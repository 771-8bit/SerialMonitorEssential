//! MCP stdio アダプタ (`--mcp` フラグで起動)
//!
//! インストール済みのアプリだけで AI 連携が完結するよう、MCP サーバを
//! アプリ本体に内蔵する（Node.js 不要）。`serial-monitor-essential --mcp` は
//! GUI を起動せず、stdin/stdout で MCP (JSON-RPC 2.0, 行区切り) を話し、
//! ローカルのブリッジ (bridge.rs, NDJSON TCP。設定画面の表示名は「MCP Server」) へ中継する。
//!
//! - 公開ツールは `mcp/server.mjs`（開発用 Node 実装）と同一の 7 種。
//!   出力テキストの形式も揃えてある。ツールを追加・変更するときは両方を
//!   同時に更新すること（docs/22 DEBT-6）。
//! - stdout は JSON-RPC 専用。ログはすべて stderr（env_logger の既定）。
//! - 接続設定は環境変数 `SME_BRIDGE_HOST` / `SME_BRIDGE_PORT` /
//!   `SME_BRIDGE_TOKEN`（Node 版と同じ名前）。
//!
//! JSON-RPC の対応メソッド: `initialize` / `notifications/initialized`(無視) /
//! `notifications/cancelled`(実行中ツールの中断) / `ping` / `tools/list` /
//! `tools/call`。他のリクエストは -32601。
//!
//! ツール呼び出しはワーカースレッドで実行し、メインスレッドは stdin を読み
//! 続ける。`serial_wait_for`（最大 10 分）の間も `ping` に即応答でき、
//! キャンセルを受け付けられる。

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::bridge::{DEFAULT_BRIDGE_PORT, DEFAULT_TAIL_BYTES, MAX_READ_LENGTH};

/// MCP serverInfo.name（Node 版と同じ）
pub const SERVER_NAME: &str = "serial-monitor-essential";
/// ブリッジ 1 リクエストのタイムアウト
const REQUEST_TIMEOUT_MS: u64 = 5000;
/// `serial_wait_for` の 1 回あたりのサーバ側待機の上限
///
/// 走査そのものはブリッジが行う（`bridge::method_wait_for`）。ここで区切るのは
/// キャンセルに気づくためだけなので、この値がキャンセル検知の遅延の上限になる。
///
/// 旧実装はクライアント側で `tail` を 500ms ごとに固定 64 KiB 読んでいた
/// ＝実効 131 KB/s しか走査できず、約 1 Mbps を超えると返信を取りこぼした
/// （docs/26 §5.1 / DEBT-8）。窓も蓄積も無くなったので、その制限は消えている。
const WAIT_CHUNK_MS: u64 = crate::bridge::WAIT_SERVER_MAX_MS;
/// この比率を超えて非印字バイトが含まれるときは hex ダンプ表示にする
const BINARY_RATIO: f64 = 0.1;
/// 読み出し系ツールが返す本文の既定上限（文字数）
///
/// トークンではなく**文字数**で課すのは、サーバがクライアントのトークナイザを
/// 知り得ず、hex や CJK では推定が必ず外れるため（docs/26 統括原則）。
/// ログ的 ASCII ではおおよそ 3.6 文字/トークン。
pub const DEFAULT_MAX_CHARS: usize = 4000;
/// `max_chars` に指定できる下限 / 上限
///
/// 下限は [`ENVELOPE_RESERVE`] より大きく取る。ヘッダとフッタは切り詰め対象に
/// しない（切り詰めた事実と続きの取り方を必ず伝えるため）ので、封筒すら
/// 入らない予算を許すと本文が空でも上限を超えてしまう。
pub const MIN_MAX_CHARS: usize = 500;
/// `serial_digest` / `serial_watch` の既定予算（実体は insight::render 側）
const DEFAULT_DIGEST_CHARS: usize = crate::insight::render::DEFAULT_DIGEST_CHARS;
const DEFAULT_WATCH_CHARS: usize = crate::insight::render::DEFAULT_WATCH_CHARS;
const DEFAULT_SAMPLE_CHARS: usize = crate::insight::render::DEFAULT_SAMPLE_CHARS;
pub const MAX_MAX_CHARS: usize = 200_000;
/// サポートする MCP プロトコル版（クライアント指定をエコーできる版）
const KNOWN_PROTOCOL_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];
/// クライアント指定が未知のときに名乗る版
const LATEST_PROTOCOL_VERSION: &str = "2025-06-18";

// ============================================================================
// ブリッジ接続（トレイトで抽象化: テストはモックを差す）
// ============================================================================

/// ブリッジへの 1 リクエスト（メソッド + params -> result）
pub trait Bridge {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, String>;
}

fn unreachable_message(host: &str, port: u16, detail: &str) -> String {
    format!(
        "Cannot reach the SerialMonitorEssential bridge at {host}:{port}.\n\
         Start the app and turn on \"MCP Server\" in its settings.\n({detail})"
    )
}

/// `serial_status` の接続状態。再接続待ちなら、エージェントが「待てば同じキャプチャに
/// 続きが来る」と分かるように書く（Node 版と同じ文言）
fn connected_label(connected: bool, reconnecting: bool) -> &'static str {
    match (connected, reconnecting) {
        (true, _) => "yes",
        (false, true) => "no (device went away; the app reconnects when it returns and the capture continues at the same offsets)",
        (false, false) => "no",
    }
}

fn bridge_error_message(method: &str, error: &str) -> String {
    format!("The bridge returned an error (method={method}): {error}")
}

/// 実 TCP のブリッジクライアント（遅延接続・自動 auth・切断時 1 回だけ再接続）
pub struct TcpBridgeClient {
    host: String,
    port: u16,
    token: Option<String>,
    timeout: Duration,
    conn: Option<BufReader<TcpStream>>,
    next_id: u64,
}

impl TcpBridgeClient {
    pub fn new(host: String, port: u16, token: Option<String>) -> Self {
        Self {
            host,
            port,
            token,
            timeout: Duration::from_millis(REQUEST_TIMEOUT_MS),
            conn: None,
            next_id: 1,
        }
    }

    /// 環境変数（Node 版と同名）から接続設定を読む
    pub fn from_env() -> Self {
        let host = std::env::var("SME_BRIDGE_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port = std::env::var("SME_BRIDGE_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .filter(|p| *p > 0)
            .unwrap_or(DEFAULT_BRIDGE_PORT);
        let token = std::env::var("SME_BRIDGE_TOKEN")
            .ok()
            .filter(|t| !t.is_empty());
        Self::new(host, port, token)
    }

    fn connect(&mut self) -> Result<(), String> {
        if self.conn.is_some() {
            return Ok(());
        }
        let addr = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|e| unreachable_message(&self.host, self.port, &e.to_string()))?
            .next()
            .ok_or_else(|| unreachable_message(&self.host, self.port, "no address"))?;
        let stream = TcpStream::connect_timeout(&addr, self.timeout)
            .map_err(|e| unreachable_message(&self.host, self.port, &e.to_string()))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|e| e.to_string())?;
        stream.set_nodelay(true).ok();
        self.conn = Some(BufReader::new(stream));

        if let Some(token) = self.token.clone() {
            if let Err(e) = self.exchange("auth", json!({ "token": token })) {
                self.conn = None;
                return Err(e);
            }
        }
        Ok(())
    }

    /// 1 往復（接続済み前提）。トランスポート断は Err("__transport: ...") で表す。
    fn exchange(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let line = format!(
            "{}\n",
            json!({ "id": id, "method": method, "params": params })
        );

        let reader = self.conn.as_mut().ok_or("__transport: not connected")?;
        reader
            .get_mut()
            .write_all(line.as_bytes())
            .map_err(|e| format!("__transport: {e}"))?;

        // このクライアントは逐次リクエストなので、対応する id の行まで読む
        loop {
            let mut response_line = String::new();
            match reader.read_line(&mut response_line) {
                Ok(0) => return Err("__transport: connection closed".to_string()),
                Ok(_) => {}
                // タイムアウトはトランスポート断と区別して「再送しない」:
                // 処理中かもしれない send を張り直して再送すると二重送信になる
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(format!(
                        "The bridge did not answer in time (method={method}, {}ms).",
                        self.timeout.as_millis()
                    ))
                }
                Err(e) => return Err(format!("__transport: {e}")),
            }
            let trimmed = response_line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let message: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue, // 壊れた行は無視する（Node 版と同じ）
            };
            // id:null のエラー行は接続レベルの拒否（例: "too many connections"）。
            // 読み飛ばすと EOF まで進んで「ブリッジに繋がらない」という誤った案内に
            // なるため、実際のエラーメッセージで即座に失敗させる（再送もしない）。
            if message.get("id").map(Value::is_null).unwrap_or(false)
                && message.get("ok").and_then(Value::as_bool) == Some(false)
            {
                let error = message
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("connection rejected");
                return Err(bridge_error_message(method, error));
            }
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue; // 他リクエストの応答や push フレームは読み飛ばす
            }
            return if message.get("ok").and_then(Value::as_bool) == Some(true) {
                Ok(message.get("result").cloned().unwrap_or(json!({})))
            } else {
                let error = message
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                Err(bridge_error_message(method, error))
            };
        }
    }
}

impl Bridge for TcpBridgeClient {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        // トランスポート断は接続を捨てて 1 回だけ張り直す（Node 版と同じ挙動）
        for attempt in 0..2 {
            self.connect()?;
            match self.exchange(method, params.clone()) {
                Ok(v) => return Ok(v),
                Err(e) if e.starts_with("__transport:") => {
                    self.conn = None;
                    if attempt == 1 {
                        let detail = e.trim_start_matches("__transport:").trim();
                        return Err(unreachable_message(&self.host, self.port, detail));
                    }
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!("the retry loop always returns");
    }
}

// ============================================================================
// 表示ヘルパ（mcp/server.mjs の同名関数の移植）
// ============================================================================

/// 両端の切断を許した UTF-8 妥当性
///
/// tail / 窓読みは多バイト文字の**途中から始まり**、**途中で終わり**得る。
/// 先頭の継続バイト（0b10xxxxxx）を最大 3 つ読み飛ばし、末尾も最大 3 バイト
/// 削って判定する（どちらか一方だけの許容だと、正当な UTF-8 テキストが
/// まるごと hex ダンプ表示になる）。
fn is_valid_utf8_allow_truncation(buf: &[u8]) -> bool {
    if buf.is_empty() {
        return true;
    }
    let mut start = 0;
    while start < 3.min(buf.len()) && (buf[start] & 0xc0) == 0x80 {
        start += 1;
    }
    let body = &buf[start..];
    if body.is_empty() {
        return true;
    }
    for trim in 0..=3.min(body.len() - 1) {
        if std::str::from_utf8(&body[..body.len() - trim]).is_ok() {
            return true;
        }
    }
    false
}

/// 印字可能でも空白でも妥当な UTF-8 でもないバイトの比率
pub fn non_printable_ratio(buf: &[u8]) -> f64 {
    if buf.is_empty() {
        return 0.0;
    }
    let utf8 = is_valid_utf8_allow_truncation(buf);
    let bad = buf
        .iter()
        .filter(|&&b| {
            !(b == 0x09
                || b == 0x0a
                || b == 0x0d
                || (0x20..=0x7e).contains(&b)
                || (b >= 0x80 && utf8))
        })
        .count();
    bad as f64 / buf.len() as f64
}

pub fn looks_like_text(buf: &[u8]) -> bool {
    non_printable_ratio(buf) <= BINARY_RATIO
}

/// 16 バイト/行の hex ダンプ（オフセットはストリーム絶対値）
pub fn hex_dump(buf: &[u8], base_offset: u64) -> String {
    let mut rows = Vec::new();
    for (i, row) in buf.chunks(16).enumerate() {
        let mut cells: Vec<String> = row.iter().map(|b| format!("{b:02x}")).collect();
        cells.resize(16, "  ".to_string());
        let hex = format!("{}  {}", cells[..8].join(" "), cells[8..].join(" "));
        let ascii: String = row
            .iter()
            .map(|&b| {
                if (0x20..=0x7e).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        rows.push(format!(
            "{:08x}  {}  |{}|",
            base_offset + (i as u64) * 16,
            hex,
            ascii
        ));
    }
    rows.join("\n")
}

/// 出力の描画形式（ツール引数 `format`）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFormat {
    /// 中身を見てテキスト / hex を選ぶ（既定）
    Auto,
    /// 常に lossy UTF-8 テキスト
    Text,
    /// 常に hex ダンプ
    Hex,
    /// テキストだが、両端の欠けた行を捨てて完全な行だけを出す
    Lines,
}

impl RenderFormat {
    pub fn parse(v: Option<&Value>) -> Result<Self, String> {
        match v {
            None | Some(Value::Null) => Ok(Self::Auto),
            Some(v) => match v.as_str() {
                Some("auto") => Ok(Self::Auto),
                Some("text") => Ok(Self::Text),
                Some("hex") => Ok(Self::Hex),
                Some("lines") => Ok(Self::Lines),
                _ => Err("bad arguments: 'format' must be one of auto/text/hex/lines".to_string()),
            },
        }
    }

    /// このバイト列を実際にどう描画するか
    fn resolve(self, buf: &[u8]) -> Self {
        match self {
            Self::Auto if looks_like_text(buf) => Self::Text,
            Self::Auto => Self::Hex,
            other => other,
        }
    }
}

/// テキストなら lossy UTF-8、バイナリらしければ hex ダンプ
///
/// 予算を持たない小さな断片（`serial_wait_for` の末尾 256 バイトなど）専用。
/// 呼び出しの本文は必ず [`render_chunk`] を通し、予算を課すこと。
fn render_bytes(buf: &[u8], base_offset: u64) -> String {
    if buf.is_empty() {
        "(no data)".to_string()
    } else if looks_like_text(buf) {
        String::from_utf8_lossy(buf).into_owned()
    } else {
        hex_dump(buf, base_offset)
    }
}

/// hex ダンプ 1 行が消費する文字数（16 バイト分 + 改行）
///
/// `00000010  30 31 .. 66  |0123456789abcdef|` = 78 文字 + `\n`。
/// この定数のおかげで hex は「文字数 → バイト数」を厳密に逆算できる。
const HEX_ROW_CHARS: usize = 79;

/// lossy UTF-8 変換したときの文字数が `max_chars` 以下に収まる最大の先頭バイト数
///
/// `String::from_utf8_lossy` と同じ規則で数える。不正バイト列は
/// `Utf8Error::error_len()` 分をまとめて 1 個の U+FFFD にするため、
/// 「1 バイト = 1 文字以下」を仮定した概算より正確に予算を使い切れる。
pub fn head_bytes_within_char_budget(buf: &[u8], max_chars: usize) -> usize {
    let mut consumed = 0usize;
    let mut chars = 0usize;
    while consumed < buf.len() && chars < max_chars {
        let rest = &buf[consumed..];
        match std::str::from_utf8(rest) {
            Ok(s) => {
                for (i, _) in s.char_indices() {
                    if chars >= max_chars {
                        return consumed + i;
                    }
                    chars += 1;
                }
                return buf.len();
            }
            Err(e) => {
                // valid_up_to() までは妥当な UTF-8 なので from_utf8 は必ず成功する
                let valid = std::str::from_utf8(&rest[..e.valid_up_to()]).unwrap_or("");
                for (i, _) in valid.char_indices() {
                    if chars >= max_chars {
                        return consumed + i;
                    }
                    chars += 1;
                }
                consumed += e.valid_up_to();
                if chars >= max_chars {
                    return consumed;
                }
                // 不正列は error_len 分（末尾切れなら残り全部）で 1 個の U+FFFD
                let skip = e.error_len().unwrap_or(rest.len() - e.valid_up_to());
                consumed += skip;
                chars += 1;
            }
        }
    }
    consumed.min(buf.len())
}

/// 同上を末尾から数え、残すべき**末尾バイト数**を返す
pub fn tail_bytes_within_char_budget(buf: &[u8], max_chars: usize) -> usize {
    // 先頭から順に「ここから末尾までが予算に収まるか」を二分探索するより、
    // UTF-8 の先頭バイト境界を末尾から辿って数えるほうが単純で速い。
    let mut start = buf.len();
    let mut chars = 0usize;
    while start > 0 && chars < max_chars {
        // 直前の文字境界まで戻る（継続バイト 10xxxxxx を最大 3 個スキップ）
        let mut cand = start - 1;
        let mut steps = 0;
        while cand > 0 && steps < 3 && (buf[cand] & 0xc0) == 0x80 {
            cand -= 1;
            steps += 1;
        }
        start = cand;
        chars += 1;
    }
    buf.len() - start
}

/// 連続する同一行を 1 行 + 回数表示に畳む。畳んだ行数も返す
pub fn collapse_repeated_lines(text: &str) -> (String, usize) {
    let mut out: Vec<String> = Vec::new();
    let mut removed = 0usize;
    let mut prev: Option<&str> = None;
    let mut run = 0usize;
    let flush = |out: &mut Vec<String>, line: &str, run: usize| {
        if run > 1 {
            out.push(format!("{line}  [x{run}]"));
        } else {
            out.push(line.to_string());
        }
    };
    for line in text.split('\n') {
        match prev {
            Some(p) if p == line => run += 1,
            Some(p) => {
                if run > 1 {
                    removed += run - 1;
                }
                flush(&mut out, p, run);
                prev = Some(line);
                run = 1;
            }
            None => {
                prev = Some(line);
                run = 1;
            }
        }
    }
    if let Some(p) = prev {
        if run > 1 {
            removed += run - 1;
        }
        flush(&mut out, p, run);
    }
    (out.join("\n"), removed)
}

/// 予算に収まるよう切り出した描画結果
///
/// ツール経路が使うのは `text` だけだが、残りは**同じ内容がヘッダとフッタに
/// 文字列として現れる**ものの構造化版であり、テストが「本文が示すバイト範囲」と
/// 「報告したオフセット」の一致を機械的に検証するために読む。文字列を
/// パースして検証すると、書式を変えた瞬間にテストが黙って無意味になる。
pub struct Rendered {
    pub text: String,
    /// 実際に描画したバイト範囲（ストリーム絶対オフセット）
    #[allow(dead_code)]
    pub shown_start: u64,
    #[allow(dead_code)]
    pub shown_end: u64,
    #[allow(dead_code)]
    pub truncated: bool,
}

/// ヘッダ + フッタのために確保しておく文字数
const ENVELOPE_RESERVE: usize = 240;

/// `max_chars` を厳守して 1 チャンクを描画する
///
/// 予算に収まらないときは**バイト単位で切り出してから**描画するので、
/// 出したバイト範囲を絶対オフセットで正確に報告でき、続きを
/// `serial_read_range` で 1 回で取り直せる（設計原則「ペイロードではなく
/// ハンドル」）。切り詰めは必ずフッタで報告し、黙って短くしない。
#[allow(clippy::too_many_arguments)]
fn render_chunk(
    buf: &[u8],
    offset: u64,
    total_bytes: Option<u64>,
    extra: &[String],
    format: RenderFormat,
    collapse: bool,
    max_chars: usize,
    keep_tail: bool,
) -> Rendered {
    if buf.is_empty() {
        let mut header = vec![format!("offset={offset}"), "bytes=0".to_string()];
        if let Some(total) = total_bytes {
            header.push(format!("total_bytes={total}"));
        }
        header.push("rendering=empty".to_string());
        header.extend(extra.iter().cloned());
        return Rendered {
            text: format!("{}\n---\n(no data)", header.join("  ")),
            shown_start: offset,
            shown_end: offset,
            truncated: false,
        };
    }

    let mode = format.resolve(buf);
    let body_budget = max_chars.saturating_sub(ENVELOPE_RESERVE).max(80);

    // --- バイト単位で切り出す（ここが予算を守る唯一の場所） ---
    let (slice, shown_start) = match mode {
        RenderFormat::Hex => {
            let rows = (body_budget / HEX_ROW_CHARS).max(1);
            let fit = rows * 16;
            if buf.len() <= fit {
                (buf, offset)
            } else if keep_tail {
                // 行の境界を保つため 16 の倍数で切る
                let start = buf.len() - fit;
                (&buf[start..], offset + start as u64)
            } else {
                (&buf[..fit], offset)
            }
        }
        _ => {
            if keep_tail {
                let keep = tail_bytes_within_char_budget(buf, body_budget);
                let start = buf.len() - keep;
                (&buf[start..], offset + start as u64)
            } else {
                let keep = head_bytes_within_char_budget(buf, body_budget);
                (&buf[..keep], offset)
            }
        }
    };
    let mut shown_start = shown_start;
    let mut slice = slice;

    // `lines` は端の欠けた行を捨てる。捨てた分だけ shown 範囲も詰める
    if mode == RenderFormat::Lines {
        if let Some(p) = slice.iter().position(|&b| b == b'\n') {
            shown_start += p as u64 + 1;
            slice = &slice[p + 1..];
        } else {
            slice = &slice[slice.len()..];
        }
        if let Some(p) = slice.iter().rposition(|&b| b == b'\n') {
            slice = &slice[..p + 1];
        } else {
            slice = &slice[..0];
        }
    }
    let shown_end = shown_start + slice.len() as u64;
    let truncated = slice.len() < buf.len();

    // --- 描画 ---
    let mut body = match mode {
        RenderFormat::Hex => hex_dump(slice, shown_start),
        _ => String::from_utf8_lossy(slice).into_owned(),
    };
    let mut collapsed = 0usize;
    if collapse && mode != RenderFormat::Hex {
        let (c, removed) = collapse_repeated_lines(&body);
        body = c;
        collapsed = removed;
    }

    // --- ヘッダ / フッタ ---
    let mode_label = match mode {
        RenderFormat::Hex => "hex",
        RenderFormat::Lines => "lines",
        _ => "text",
    };
    let mut header = vec![format!("offset={offset}"), format!("bytes={}", buf.len())];
    if let Some(total) = total_bytes {
        header.push(format!("total_bytes={total}"));
    }
    header.push(format!("rendering={mode_label}"));
    header.extend(extra.iter().cloned());
    if truncated {
        header.push(format!(
            "shown={}..{} ({} of {} bytes)",
            shown_start,
            shown_end,
            slice.len(),
            buf.len()
        ));
    }
    if collapsed > 0 {
        header.push(format!("collapsed={collapsed} repeated lines"));
    }

    // 落とした範囲は前後どちらにも出る（tail は先頭側、range は末尾側、
    // `lines` は端の欠け行の分だけ両側）。両方を正確に示し、続きを 1 回で
    // 取り直せるようにする。黙って短くしない（設計原則 5）。
    let footer = if truncated {
        let dropped = buf.len() - slice.len();
        let end = offset + buf.len() as u64;
        let mut next: Vec<String> = Vec::new();
        if shown_start > offset {
            next.push(format!(
                "serial_read_range(offset={offset}, length={})",
                shown_start - offset
            ));
        }
        if shown_end < end {
            next.push(format!(
                "serial_read_range(offset={shown_end}, length={})",
                end - shown_end
            ));
        }
        Some(format!(
            "\n---\ntruncated: {dropped} more bytes in this read (budget {max_chars} chars). next: {}",
            next.join(" and ")
        ))
    } else {
        None
    };

    let head = header.join("  ");
    let foot = footer.unwrap_or_default();
    // 最終的な硬い上限。溢れたら**本文だけ**を削り、フッタは決して落とさない。
    let fixed = head.chars().count() + 5 + foot.chars().count();
    let allowed = max_chars.saturating_sub(fixed);
    if body.chars().count() > allowed {
        body = body.chars().take(allowed).collect();
    }

    // 予算超過を構造的に不可能にするための最後の砦。オフセットが桁外れに
    // 大きく（テラバイト級のキャプチャ）封筒だけで予算を食い潰す場合にのみ
    // 効く。通常経路では発動しない。
    let mut text = format!("{head}\n---\n{body}{foot}");
    if text.chars().count() > max_chars {
        text = text.chars().take(max_chars).collect();
    }

    Rendered {
        text,
        shown_start,
        shown_end,
        truncated,
    }
}

/// `find` の結果を予算内のテキストに落とす
///
/// 件数と少数のハンドルだけを出す。**裸のゼロを出さない**のが要点で、
/// `matches: 0` は必ずカバレッジ（走査したバイト数・欠落・打ち切り）と
/// 一緒に出す。「見つからなかった」と「見に行けなかった」を混同させない。
pub fn render_find(res: &Value, max_chars: usize) -> String {
    let g = |k: &str| res.get(k).and_then(Value::as_u64).unwrap_or(0);
    let matches = g("matches");
    let exact = res.get("exact").and_then(Value::as_bool).unwrap_or(false);
    let complete = res
        .get("complete")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let (start, end) = (g("range_start"), g("range_end"));
    let scanned = g("bytes_scanned");
    let gap = g("gap_bytes");

    // 1 行目: 件数とその確度。丸めない
    let qualifier = if exact {
        "exact".to_string()
    } else {
        let mut why = Vec::new();
        if !complete {
            why.push("scan cut short");
        }
        if gap > 0 {
            why.push("unreadable bytes skipped");
        }
        format!("AT LEAST — {}", why.join(", "))
    };
    let mut lines = vec![format!("matches: {matches} ({qualifier})")];

    // カバレッジは常に出す（原則 5: 裸のゼロを禁じる）
    let requested = end.saturating_sub(start);
    let coverage = scanned
        .saturating_mul(100)
        .checked_div(requested)
        .map_or(100, |pct| pct.min(100));
    let mut cov =
        format!("scanned {scanned} of {requested} bytes in offset {start}..{end} ({coverage}%)");
    if gap > 0 {
        cov.push_str(&format!(", {gap} bytes unreadable"));
    }
    cov.push_str(&format!(", {}ms", g("elapsed_ms")));
    lines.push(cov);

    if matches > 0 {
        let first = res.get("first").and_then(Value::as_u64);
        let last = res.get("last").and_then(Value::as_u64);
        if let (Some(f), Some(l)) = (first, last) {
            lines.push(format!("first @off={f}  last @off={l}"));
        }
    }
    if !complete && end > start {
        // 続きを 1 回で再開できるハンドルを必ず渡す
        let resume = start + scanned;
        lines.push(format!(
            "resume: serial_find(pattern=..., from={resume}, to={end})"
        ));
    }

    let hits = res.get("hits").and_then(Value::as_array);
    let shown = hits.map(|h| h.len()).unwrap_or(0);
    let mut body = lines.join("\n");
    if let Some(hits) = hits {
        for h in hits {
            let off = h.get("offset").and_then(Value::as_u64).unwrap_or(0);
            let ex = h.get("excerpt").and_then(Value::as_str).unwrap_or("");
            let cut = if h
                .get("excerpt_truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                "…"
            } else {
                ""
            };
            let line = format!("\n@off={off}  {ex}{cut}");
            // 予算を超えるヒットは足さない（末尾の注記ぶんを残す）
            if body.chars().count() + line.chars().count() + 120 > max_chars {
                break;
            }
            body.push_str(&line);
        }
    }

    let listed = body.matches("\n@off=").count() as u64;
    if matches > listed {
        body.push_str(&format!(
            "\nshowing {listed} of {matches} hits (raise max_hits, or narrow with from/to). \
             Fetch context with serial_read_range(offset=<@off>, length=200)."
        ));
    } else if shown > 0 {
        body.push_str("\nall hits listed.");
    }

    if body.chars().count() > max_chars {
        body = body.chars().take(max_chars).collect();
    }
    body
}

/// "48 65 6c" / "0x48,0x65" / "48:65-6C" などを受け付ける
pub fn parse_hex(input: &str) -> Result<Vec<u8>, String> {
    let no_prefix = input.replace("0x", "").replace("0X", "");
    let cleaned: String = no_prefix
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, ',' | ':' | ';' | '_' | '-'))
        .collect();
    if cleaned.is_empty() {
        return Err("hex string is empty".to_string());
    }
    if !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("hex string contains non-hex characters".to_string());
    }
    if !cleaned.len().is_multiple_of(2) {
        return Err("hex string must have an even number of digits".to_string());
    }
    Ok(cleaned
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            let s = std::str::from_utf8(pair).expect("hex digits are ascii");
            u8::from_str_radix(s, 16).expect("validated hex digits")
        })
        .collect())
}

// ============================================================================
// serial_wait_for
// ============================================================================

#[derive(Debug)]
pub struct WaitOutcome {
    pub matched: bool,
    pub offset: u64,
    pub excerpt: String,
    pub tail: Vec<u8>,
    pub tail_offset: u64,
    pub elapsed_ms: u64,
    /// 走査した総バイト数（再開の重なりを二重に数えない累計）
    pub bytes_scanned: u64,
    pub total_bytes: u64,
    /// キャプチャの世代が変わった（Clear / ポート再オープン）
    pub gap: bool,
    /// `notifications/cancelled` により中断された
    pub cancelled: bool,
}

/// 指定オフセット以降に `pattern` が現れるまで待つ
///
/// 走査は**ブリッジ側**（`bridge::method_wait_for`）が行う。ここがやるのは
/// 「区切って呼び直し、間でキャンセルを見る」だけ。
///
/// 旧実装はここで `tail` を base64 で引いて手元の蓄積を走査していたが、
/// 固定窓では 12 Mbps に読み負け、蓄積を切り詰めると長いマッチを取り逃がした
/// （docs/22 DEBT-8）。ストアを直接前方走査する形にして、窓・蓄積・base64 の
/// 3 つとも構造ごと無くした。
///
/// `chunk_ms` は 1 回のサーバ側待機の上限。ブロック中のソケット読み出しは
/// 中断できないので、キャンセルに気づくまでの遅れはこの値が上限になる。
///
/// 正確性の要点:
/// - マッチは `regex::bytes` で**生バイト**に対して行う（ブリッジ側）。lossy 変換した
///   文字列のインデックスを使うと、不正 UTF-8 バイトの置換でオフセットが膨らみ
///   `serial_read_range` と噛み合わなくなる。
/// - `total_bytes` の巻き戻り（世代交代）はブリッジが `reset` として報告する。
///   放置すると「応答が来ているのに必ずタイムアウト」になる。
/// - 再開位置 `next_from` はブリッジが重なりを含めて返す。重なりの計算が
///   1 箇所しかないので、境界を跨ぐマッチの扱いがずれない。
pub fn wait_for_pattern_from(
    bridge: &mut dyn Bridge,
    pattern: &str,
    timeout_ms: u64,
    start: u64,
    chunk_ms: u64,
    cancelled: &dyn Fn() -> bool,
) -> Result<WaitOutcome, String> {
    // ブリッジ側でも検証されるが、ここで弾くと往復せずに理由を返せる
    regex::bytes::Regex::new(&format!("(?m){pattern}"))
        .map_err(|e| format!("invalid pattern: {e}"))?;
    let started = Instant::now();

    let mut from = start;
    let mut scanned_to = start;
    let mut bytes_scanned: u64 = 0;
    let mut total_bytes = start;
    let mut gap = false;
    let mut tail: Vec<u8> = Vec::new();
    let mut tail_offset = start;

    loop {
        if cancelled() {
            return Ok(WaitOutcome {
                matched: false,
                offset: 0,
                excerpt: String::new(),
                tail,
                tail_offset,
                elapsed_ms: started.elapsed().as_millis() as u64,
                bytes_scanned,
                total_bytes,
                gap,
                cancelled: true,
            });
        }
        let elapsed = started.elapsed().as_millis() as u64;
        let remaining = timeout_ms.saturating_sub(elapsed);
        let block_ms = remaining
            .min(chunk_ms.max(1))
            .min(crate::bridge::WAIT_SERVER_MAX_MS);

        let call_started = Instant::now();
        let res = bridge.request(
            "wait_for",
            json!({ "pattern": pattern, "from": from, "timeout_ms": block_ms }),
        )?;

        if res.get("reset").and_then(Value::as_bool).unwrap_or(false) {
            // 新しいストリームが始まった。走査済みの位置も意味を失う
            gap = true;
            scanned_to = 0;
        }
        total_bytes = res
            .get("total_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(total_bytes);
        let reported = res
            .get("scanned_to")
            .and_then(Value::as_u64)
            .unwrap_or(scanned_to);
        bytes_scanned += reported.saturating_sub(scanned_to);
        scanned_to = scanned_to.max(reported);
        if let Some(encoded) = res.get("tail_base64").and_then(Value::as_str) {
            if !encoded.is_empty() {
                tail = BASE64.decode(encoded).unwrap_or_default();
                tail_offset = res
                    .get("tail_offset")
                    .and_then(Value::as_u64)
                    .unwrap_or(tail_offset);
            }
        }

        if res.get("matched").and_then(Value::as_bool).unwrap_or(false) {
            return Ok(WaitOutcome {
                matched: true,
                offset: res.get("offset").and_then(Value::as_u64).unwrap_or(0),
                excerpt: res
                    .get("excerpt")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                tail: Vec::new(),
                tail_offset: 0,
                elapsed_ms: started.elapsed().as_millis() as u64,
                bytes_scanned,
                total_bytes,
                gap,
                cancelled: false,
            });
        }
        from = res
            .get("next_from")
            .and_then(Value::as_u64)
            .unwrap_or(scanned_to);

        if started.elapsed().as_millis() as u64 >= timeout_ms {
            return Ok(WaitOutcome {
                matched: false,
                offset: 0,
                excerpt: String::new(),
                tail,
                tail_offset,
                elapsed_ms: started.elapsed().as_millis() as u64,
                bytes_scanned,
                total_bytes,
                gap,
                cancelled: false,
            });
        }
        // ブリッジは要求した時間だけブロックしてから返すのが正常。それより
        // 早く返してきた相手に対して空回りしないよう、差分だけ待つ
        let spent = call_started.elapsed().as_millis() as u64;
        if spent < block_ms {
            std::thread::sleep(Duration::from_millis(block_ms - spent));
        }
    }
}

/// 呼び出し後に届いたデータへ `pattern` がマッチするまで待つ
///
/// 開始位置を `status` から決めて [`wait_for_pattern_from`] へ委ねる。
/// `from_end=false` のときだけ、既にバッファにある末尾も対象にする。
pub fn wait_for_pattern(
    bridge: &mut dyn Bridge,
    pattern: &str,
    timeout_ms: u64,
    from_end: bool,
    chunk_ms: u64,
    cancelled: &dyn Fn() -> bool,
) -> Result<WaitOutcome, String> {
    // 不正なパターンは往復する前に弾く（開始位置を引きに行く意味がない）
    regex::bytes::Regex::new(&format!("(?m){pattern}"))
        .map_err(|e| format!("invalid pattern: {e}"))?;
    let status = bridge.request("status", json!({}))?;
    let total = status
        .get("total_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let start = if from_end {
        total
    } else {
        total.saturating_sub(DEFAULT_TAIL_BYTES as u64)
    };
    wait_for_pattern_from(bridge, pattern, timeout_ms, start, chunk_ms, cancelled)
}

/// 待機結果を AI 向けのテキストにする（`serial_wait_for` と `serial_send` で共用）
pub fn format_wait_outcome(pattern: &str, outcome: &WaitOutcome) -> String {
    let quoted = serde_json::to_string(pattern).unwrap_or_default();
    if outcome.cancelled {
        return format!(
            "CANCELLED  pattern={}  elapsed_ms={}  bytes_scanned={}",
            quoted, outcome.elapsed_ms, outcome.bytes_scanned
        );
    }
    if outcome.matched {
        let mut head = vec![
            "MATCH".to_string(),
            format!("pattern={quoted}"),
            format!("offset={}", outcome.offset),
            format!("elapsed_ms={}", outcome.elapsed_ms),
            format!("total_bytes={}", outcome.total_bytes),
        ];
        if outcome.gap {
            head.push("warning=capture_reset".to_string());
        }
        return format!("{}\n---\n{}", head.join("  "), outcome.excerpt);
    }
    let head = [
        "TIMEOUT".to_string(),
        format!("pattern={quoted}"),
        format!("elapsed_ms={}", outcome.elapsed_ms),
        format!("bytes_scanned={}", outcome.bytes_scanned),
        format!("total_bytes={}", outcome.total_bytes),
    ]
    .join("  ");
    // 「見つからなかった」と「見に行けなかった」を混同させない
    let label = if outcome.bytes_scanned == 0 {
        "no new data arrived while waiting".to_string()
    } else {
        format!(
            "last {} byte(s) seen, from offset {}",
            outcome.tail.len(),
            outcome.tail_offset
        )
    };
    let body = render_bytes(&outcome.tail, outcome.tail_offset);
    format!("{head}\n--- {label}\n{body}")
}

// ============================================================================
// MCP ツール定義 / 実行
// ============================================================================

/// `tools/list` に返すツール定義（名前・説明・スキーマは Node 版と揃える）
pub fn tool_definitions() -> Value {
    json!([
        {
            "name": "serial_status",
            "title": "Serial status",
            "description": "Report the bridge/app status: whether a serial port is open, its name, how many bytes have been received so far, and the app version.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "serial_ports",
            "title": "List serial ports",
            "description": "List the serial ports the app can see on this machine.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "serial_digest",
            "title": "Summarise the capture",
            "description": "Understand a capture in ONE call at fixed cost: line templates with counts, alarm totals, rare and brand-new lines, plus a cursor for serial_watch. Reads the app's index, not the bulk data, so 4 KB and 4 GB cost the same. Prefer it over serial_read_tail to find out what is going on. Counts are exact unless the reply says otherwise, and every extreme carries a byte offset for serial_read_range.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "window": { "type": "string", "description": "\"all\" (default), a duration like \"30s\"/\"5m\", or \"off:FROM..TO\". A duration needs timestamps; without them the call fails rather than guessing." },
                    "template": { "type": "string", "description": "A template id like \"t12\" from a previous reply. Returns that one template in detail (each field range with offsets, plus real lines) instead of the overview." },
                    "top": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Frequent templates to list (default 8). Rare and new ones come first regardless." },
                    "samples": { "type": "integer", "minimum": 0, "maximum": 8, "description": "How many recent raw lines to quote (default 3)." },
                    "buckets": { "type": "integer", "minimum": 0, "maximum": 24, "description": "Split the window into N buckets, reporting lines/err/warn each (default 0 = none)." },
                    "max_chars": { "type": "integer", "minimum": MIN_MAX_CHARS, "maximum": MAX_MAX_CHARS, "description": format!("Character cap (default {DEFAULT_DIGEST_CHARS}). Alarms are never dropped to fit; samples are.") }
                }
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "serial_watch",
            "title": "Follow what changed",
            "description": "Report only what changed since a cursor: new templates, per-template increases, and the alarm delta. Reply size does not depend on how much arrived, so following a 12 Mbps device costs the same as an idle one. Call it once without a cursor (or use serial_digest) to get one, then pass it back. A cursor from a cleared or reopened capture is not an error; the reply says so.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "cursor": { "type": "string", "description": "Cursor from a previous reply. Omit on the first call to get one." },
                    "wait_ms": { "type": "integer", "minimum": 0, "maximum": 3000, "description": "Wait server-side this long for new data before answering (default 0)." },
                    "top": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Changed templates to list (default 8)." },
                    "max_chars": { "type": "integer", "minimum": MIN_MAX_CHARS, "maximum": MAX_MAX_CHARS, "description": format!("Hard cap on the returned characters (default {DEFAULT_WATCH_CHARS}).") }
                }
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "serial_sample",
            "title": "Quote real lines",
            "description": "Quote actual received lines when the summary is not enough, capped and with consecutive duplicates folded into [xN]. Every line comes with the offset it starts at, and lines cut off by the read window are dropped rather than shown as if they were whole. Use serial_digest first; reach for this when you need to see the bytes themselves.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "mode": { "type": "string", "enum": ["tail", "head", "around", "stratified", "by_template"], "description": "tail (default): newest lines. head: oldest indexed. around: needs offset. stratified: spread evenly over the capture. by_template: recent occurrences of one template." },
                    "n": { "type": "integer", "minimum": 1, "maximum": 50, "description": "How many lines (default 10)." },
                    "offset": { "type": "integer", "minimum": 0, "description": "Centre offset for mode=around." },
                    "template": { "type": "string", "description": "Template id like \"t12\" for mode=by_template." },
                    "collapse_repeats": { "type": "boolean", "description": "Fold consecutive identical lines into [xN] (default true). Counts stay exact." },
                    "max_chars": { "type": "integer", "minimum": MIN_MAX_CHARS, "maximum": MAX_MAX_CHARS, "description": format!("Character cap (default {DEFAULT_SAMPLE_CHARS}).") }
                }
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "serial_read_tail",
            "title": "Read latest serial data",
            "description": format!(
                "Read the most recent bytes received from the device. Rendered as UTF-8 text, or as a hex dump when the data looks binary. Default {DEFAULT_TAIL_BYTES} bytes, max {MAX_READ_LENGTH}. Output is capped at max_chars ({DEFAULT_MAX_CHARS} by default); when it does not fit, the most recent part is kept and a footer reports the exact byte range shown plus a serial_read_range call for the rest."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "bytes": {
                        "type": "integer", "minimum": 1, "maximum": MAX_READ_LENGTH,
                        "description": format!("How many trailing bytes to read (default {DEFAULT_TAIL_BYTES}).")
                    },
                    "format": {
                        "type": "string", "enum": ["auto", "text", "hex", "lines"],
                        "description": "auto (default): text, or hex when the data looks binary. text: always UTF-8. hex: always a hex dump (about 79 chars per 16 bytes, so it burns budget ~5x faster than text). lines: text with the partial first and last lines dropped."
                    },
                    "collapse_repeats": {
                        "type": "boolean",
                        "description": "Collapse consecutive identical lines into one line with [xN]. Applied after the byte budget, so raise 'bytes' as well when a log is highly repetitive. Not available in hex."
                    },
                    "max_chars": {
                        "type": "integer", "minimum": MIN_MAX_CHARS, "maximum": MAX_MAX_CHARS,
                        "description": format!("Hard cap on the returned characters (default {DEFAULT_MAX_CHARS}). Roughly 3.6 chars per token for ASCII logs.")
                    }
                }
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "serial_read_range",
            "title": "Read a serial byte range",
            "description": format!(
                "Read an explicit byte range of the received stream (offsets match those reported by serial_read_tail and serial_wait_for). Rendered as text, or as a hex dump when the data looks binary. Output is capped at max_chars ({DEFAULT_MAX_CHARS} by default); when it does not fit, the start of the range is kept and a footer gives the exact offset to resume from."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "offset": { "type": "integer", "minimum": 0, "description": "Absolute start offset in the received stream." },
                    "length": { "type": "integer", "minimum": 1, "maximum": MAX_READ_LENGTH, "description": format!("How many bytes to read (max {MAX_READ_LENGTH}).") },
                    "format": {
                        "type": "string", "enum": ["auto", "text", "hex", "lines"],
                        "description": "Same as serial_read_tail: auto (default) | text | hex | lines."
                    },
                    "collapse_repeats": {
                        "type": "boolean",
                        "description": "Collapse consecutive identical lines into [xN]. Not available in hex."
                    },
                    "max_chars": {
                        "type": "integer", "minimum": MIN_MAX_CHARS, "maximum": MAX_MAX_CHARS,
                        "description": format!("Hard cap on the returned characters (default {DEFAULT_MAX_CHARS}).")
                    }
                },
                "required": ["offset", "length"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "serial_find",
            "title": "Search the capture",
            "description": "Search the whole capture for a regular expression, server-side, and get back the number of matches plus a few byte offsets. Use this INSTEAD of reading bulk data to look for something: it costs the same handful of tokens whether the capture is 4 KB or 4 GB, and it works past the 1 MiB read limit. Then fetch context for one interesting offset with serial_read_range. The match count is exact unless the reply says AT LEAST (the scan was cut short or bytes were unreadable), and a zero always comes with the coverage that produced it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regular expression (Rust regex syntax, multi-line; no lookaround or backreferences)." },
                    "from": { "type": "integer", "minimum": 0, "description": "Start offset of the search range (default 0)." },
                    "to": { "type": "integer", "minimum": 0, "description": "End offset of the search range (default: end of the capture)." },
                    "max_hits": { "type": "integer", "minimum": 0, "maximum": 200, "description": "How many matching lines to quote (default 20). The count is always complete regardless of this." },
                    "count_only": { "type": "boolean", "description": "Return only the count and coverage, quoting no lines. The cheapest possible answer." },
                    "max_chars": {
                        "type": "integer", "minimum": MIN_MAX_CHARS, "maximum": MAX_MAX_CHARS,
                        "description": format!("Hard cap on the returned characters (default {DEFAULT_MAX_CHARS}).")
                    }
                },
                "required": ["pattern"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "serial_send",
            "title": "Send text to the device",
            "description": "Send a text command to the serial device, with an optional line ending (default \"lf\"). Pass 'expect' to wait for the reply in the same call: matching starts at the byte offset just before the write, so a reply that lands immediately cannot be missed. The GUI shows every bridge send to the human.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Text to transmit (the line ending is added by the app)." },
                    "line_ending": { "type": "string", "enum": ["none", "cr", "lf", "crlf"], "description": "Line ending appended to the text. Default \"lf\"." },
                    "expect": { "type": "string", "description": "Regular expression to wait for after sending. Saves a separate serial_wait_for round trip." },
                    "expect_timeout_ms": { "type": "integer", "minimum": 100, "maximum": 600000, "description": "How long to wait for 'expect'. Default 10000." }
                },
                "required": ["text"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true }
        },
        {
            "name": "serial_send_hex",
            "title": "Send raw bytes to the device",
            "description": "Send raw bytes given as hex (e.g. \"01 03 00 00 00 0A\"). Separators, colons and 0x prefixes are ignored; the digit count must be even. Nothing is appended. Pass 'expect' to wait for the reply in the same call. The GUI shows every bridge send to the human.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "hex": { "type": "string", "description": "Hex byte string, e.g. \"DE AD BE EF\"." },
                    "expect": { "type": "string", "description": "Regular expression to wait for after sending." },
                    "expect_timeout_ms": { "type": "integer", "minimum": 100, "maximum": 600000, "description": "How long to wait for 'expect'. Default 10000." }
                },
                "required": ["hex"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true }
        },
        {
            "name": "serial_wait_for",
            "title": "Wait for a pattern",
            "description": "Wait until a regular expression (Rust regex syntax; no lookaround or backreferences) matches data received after this call started. The app scans the capture forward itself, so nothing is missed however fast the device talks. Prefer serial_send(expect=...) when you are waiting for a reply to something you just sent. Returns the matching excerpt and its stream offset, or a timeout report with the last 256 bytes seen.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regular expression, matched in multi-line mode." },
                    "timeout_ms": { "type": "integer", "minimum": 100, "maximum": 600000, "description": "How long to wait, in milliseconds. Default 10000." },
                    "from_end": { "type": "boolean", "description": "true (default): only match data that arrives after this call. false: also search the last 4096 bytes already buffered." }
                },
                "required": ["pattern"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        }
    ])
}

fn arg_u64(args: &Value, key: &str, min: u64, max: u64) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_u64() {
            Some(n) if (min..=max).contains(&n) => Ok(Some(n)),
            _ => Err(format!(
                "bad arguments: '{key}' must be an integer between {min} and {max}"
            )),
        },
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("bad arguments: '{key}' (string) is required"))
}

fn arg_bool(args: &Value, key: &str, default: bool) -> Result<bool, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        _ => Err(format!("bad arguments: '{key}' must be a boolean")),
    }
}

/// 読み出し系ツール共通の描画引数（`format` / `collapse_repeats` / `max_chars`）
fn render_args(args: &Value) -> Result<(RenderFormat, bool, usize), String> {
    let format = RenderFormat::parse(args.get("format"))?;
    let collapse = arg_bool(args, "collapse_repeats", false)?;
    let max_chars = arg_u64(
        args,
        "max_chars",
        MIN_MAX_CHARS as u64,
        MAX_MAX_CHARS as u64,
    )?
    .unwrap_or(DEFAULT_MAX_CHARS as u64) as usize;
    Ok((format, collapse, max_chars))
}

/// `expect` が指定されていれば、送信直前の位置から返信を待って追記する
///
/// 開始位置は**ブリッジが返した送信直前のオフセット**なので、返信が最初の
/// 走査より前に着弾しても取りこぼさない。送信 -> 待機 -> tail の 3 往復を
/// 1 ツール呼び出しに畳む（docs/26 §7 段階 4）。
fn append_expected_reply(
    bridge: &mut dyn Bridge,
    args: &Value,
    send_result: &Value,
    head: String,
    cancelled: &dyn Fn() -> bool,
) -> Result<String, String> {
    let expect = match args.get("expect") {
        None | Some(Value::Null) => return Ok(head),
        Some(v) => v
            .as_str()
            .ok_or_else(|| "bad arguments: 'expect' must be a string".to_string())?,
    };
    let timeout_ms = arg_u64(args, "expect_timeout_ms", 100, 600_000)?.unwrap_or(10_000);
    let from = send_result
        .get("offset_before")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let outcome =
        wait_for_pattern_from(bridge, expect, timeout_ms, from, WAIT_CHUNK_MS, cancelled)?;
    Ok(format!(
        "{head}
{}",
        format_wait_outcome(expect, &outcome)
    ))
}

/// 1 ツール呼び出し（テキスト応答 or エラーメッセージ）。キャンセル無視版。
/// 本番経路はワーカー側の [`run_tool_call`] → [`call_tool_with_cancel`]。
#[cfg(test)]
pub fn call_tool(bridge: &mut dyn Bridge, name: &str, args: &Value) -> Result<String, String> {
    call_tool_with_cancel(bridge, name, args, &|| false)
}

/// 1 ツール呼び出し。`cancelled` は `serial_wait_for` のポーリング中に参照され、
/// true になったら速やかに中断する（他のツールはブリッジ 5s タイムアウトで
/// 十分短いため参照しない）。
pub fn call_tool_with_cancel(
    bridge: &mut dyn Bridge,
    name: &str,
    args: &Value,
    cancelled: &dyn Fn() -> bool,
) -> Result<String, String> {
    match name {
        "serial_status" => {
            let res = bridge.request("status", json!({}))?;
            let connected = res
                .get("connected")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let port = res
                .get("port_name")
                .and_then(Value::as_str)
                .unwrap_or("(none)");
            // `raw: {全 JSON}` は上の 4 行と同じ内容を JSON で繰り返すだけで、
            // 情報ゼロのままトークンを倍にしていた（docs/26 §5）。
            let reconnecting = res
                .get("reconnecting")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(format!(
                "connected: {}\nport: {}\ntotal_bytes: {}\napp_version: {} (protocol {})",
                connected_label(connected, reconnecting),
                port,
                res.get("total_bytes").and_then(Value::as_u64).unwrap_or(0),
                res.get("app_version")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                res.get("protocol").and_then(Value::as_u64).unwrap_or(0),
            ))
        }
        "serial_ports" => {
            let res = bridge.request("ports", json!({}))?;
            let ports: Vec<String> = res
                .get("ports")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .map(|p| {
                            p.as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| p.to_string())
                        })
                        .collect()
                })
                .unwrap_or_default();
            if ports.is_empty() {
                Ok("No serial ports found.".to_string())
            } else {
                Ok(format!(
                    "{} port(s):\n{}",
                    ports.len(),
                    ports
                        .iter()
                        .map(|p| format!("- {p}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ))
            }
        }
        "serial_read_tail" => {
            let bytes = arg_u64(args, "bytes", 1, MAX_READ_LENGTH as u64)?
                .unwrap_or(DEFAULT_TAIL_BYTES as u64);
            let (format, collapse, max_chars) = render_args(args)?;
            let res = bridge.request("tail", json!({ "bytes": bytes }))?;
            let buf = BASE64
                .decode(res.get("base64").and_then(Value::as_str).unwrap_or(""))
                .map_err(|_| "bridge returned invalid base64".to_string())?;
            // tail は「最新」が欲しい呼び出しなので、溢れたら末尾を残す
            Ok(render_chunk(
                &buf,
                res.get("offset").and_then(Value::as_u64).unwrap_or(0),
                Some(res.get("total_bytes").and_then(Value::as_u64).unwrap_or(0)),
                &[],
                format,
                collapse,
                max_chars,
                true,
            )
            .text)
        }
        "serial_read_range" => {
            let offset = arg_u64(args, "offset", 0, u64::MAX)?
                .ok_or_else(|| "bad arguments: 'offset' (integer) is required".to_string())?;
            let length = arg_u64(args, "length", 1, MAX_READ_LENGTH as u64)?
                .ok_or_else(|| "bad arguments: 'length' (integer) is required".to_string())?;
            let (format, collapse, max_chars) = render_args(args)?;
            let res =
                bridge.request("read_range", json!({ "offset": offset, "length": length }))?;
            let buf = BASE64
                .decode(res.get("base64").and_then(Value::as_str).unwrap_or(""))
                .map_err(|_| "bridge returned invalid base64".to_string())?;
            let length_read = res
                .get("length_read")
                .and_then(Value::as_u64)
                .unwrap_or(buf.len() as u64);
            // range は「この位置から」が欲しい呼び出しなので、溢れたら先頭を残す
            Ok(render_chunk(
                &buf,
                res.get("offset").and_then(Value::as_u64).unwrap_or(offset),
                None,
                &[format!("length_read={length_read}")],
                format,
                collapse,
                max_chars,
                false,
            )
            .text)
        }
        "serial_find" => {
            let pattern = arg_str(args, "pattern")?;
            let count_only = arg_bool(args, "count_only", false)?;
            let max_hits =
                arg_u64(args, "max_hits", 0, 200)?.unwrap_or(if count_only { 0 } else { 20 });
            let (_, _, max_chars) = render_args(args)?;
            let mut params = json!({ "pattern": pattern, "max_hits": max_hits });
            for key in ["from", "to"] {
                if let Some(v) = arg_u64(args, key, 0, u64::MAX)? {
                    params[key] = json!(v);
                }
            }
            let res = bridge.request("find", params)?;
            Ok(render_find(&res, max_chars))
        }
        "serial_digest" | "serial_watch" | "serial_sample" => {
            // MCP 側は薄い描画器に徹する: 予算の適用も文言もブリッジの
            // insight::render が唯一の担当（docs/26 §2.3）
            let method = name.trim_start_matches("serial_");
            let mut params = json!({});
            for key in [
                "top",
                "samples",
                "buckets",
                "max_chars",
                "wait_ms",
                "n",
                "offset",
            ] {
                if let Some(v) = arg_u64(args, key, 0, u64::MAX)? {
                    params[key] = json!(v);
                }
            }
            for key in ["window", "cursor", "mode", "template"] {
                if let Some(v) = args.get(key) {
                    if !v.is_null() {
                        // template だけは "t12" と 12 の両方を許す
                        params[key] = match v {
                            Value::String(_) | Value::Number(_) if key == "template" => v.clone(),
                            Value::String(s) => json!(s),
                            _ => return Err(format!("bad arguments: '{key}' must be a string")),
                        };
                    }
                }
            }
            if let Some(v) = args.get("collapse_repeats") {
                if !v.is_null() {
                    params["collapse_repeats"] = json!(arg_bool(args, "collapse_repeats", true)?);
                }
            }
            let res = bridge.request(method, params)?;
            res.get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| "bridge returned no text".to_string())
        }
        "serial_send" => {
            let text = arg_str(args, "text")?;
            let ending = match args.get("line_ending") {
                None | Some(Value::Null) => "lf",
                Some(v) => match v.as_str() {
                    Some(e @ ("none" | "cr" | "lf" | "crlf")) => e,
                    _ => {
                        return Err(
                            "bad arguments: 'line_ending' must be one of none/cr/lf/crlf"
                                .to_string(),
                        )
                    }
                },
            };
            let res = bridge.request("send", json!({ "text": text, "line_ending": ending }))?;
            let head = format!(
                "Sent {} + {} -> bytes_written={}",
                serde_json::to_string(text).unwrap_or_else(|_| text.to_string()),
                ending,
                res.get("bytes_written")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            );
            Ok(append_expected_reply(bridge, args, &res, head, cancelled)?)
        }
        "serial_send_hex" => {
            let hex = arg_str(args, "hex")?;
            let buf = parse_hex(hex)?;
            let res = bridge.request("send", json!({ "base64": BASE64.encode(&buf) }))?;
            let spaced = buf
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            let head = format!(
                "Sent {} byte(s) [{}] -> bytes_written={}",
                buf.len(),
                spaced,
                res.get("bytes_written")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            );
            Ok(append_expected_reply(bridge, args, &res, head, cancelled)?)
        }
        "serial_wait_for" => {
            let pattern = arg_str(args, "pattern")?;
            let timeout_ms = arg_u64(args, "timeout_ms", 100, 600_000)?.unwrap_or(10_000);
            let from_end = match args.get("from_end") {
                None | Some(Value::Null) => true,
                Some(v) => v
                    .as_bool()
                    .ok_or_else(|| "bad arguments: 'from_end' must be a boolean".to_string())?,
            };
            let outcome = wait_for_pattern(
                bridge,
                pattern,
                timeout_ms,
                from_end,
                WAIT_CHUNK_MS,
                cancelled,
            )?;
            Ok(format_wait_outcome(pattern, &outcome))
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

// ============================================================================
// JSON-RPC 2.0 ディスパッチ
// ============================================================================

fn rpc_result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// stdin 1 行の振り分け結果
///
/// ツール呼び出しだけを分離するのは、`serial_wait_for` が最大 10 分ブロック
/// する間もメインスレッドが stdin を読み続け、`ping` に即応答し
/// `notifications/cancelled` を受け付けられるようにするため（MCP の ping は
/// 「速やかに応答しなければならない」）。
#[derive(Debug, PartialEq)]
pub enum Dispatch {
    /// 即座に書き出す応答（initialize / ping / tools/list / 各種エラー）
    Respond(Value),
    /// ワーカースレッドで実行するツール呼び出し
    ToolCall {
        id: Value,
        name: String,
        args: Value,
    },
    /// `notifications/cancelled`: この requestId のツール実行を中断する
    Cancelled(Value),
    /// 応答不要（その他の通知）
    Ignore,
}

/// stdin の 1 行を振り分ける（ブリッジ不要の純粋関数）
pub fn dispatch_line(line: &str) -> Dispatch {
    let message: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Dispatch::Respond(rpc_error(&Value::Null, -32700, "Parse error")),
    };
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    // 通知（id なし）: cancelled だけは意味を持つ
    let id = match message.get("id") {
        Some(id) if !id.is_null() => id.clone(),
        _ => {
            if method == "notifications/cancelled" {
                if let Some(request_id) = params.get("requestId") {
                    return Dispatch::Cancelled(request_id.clone());
                }
            }
            return Dispatch::Ignore;
        }
    };

    match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("");
            let version = if KNOWN_PROTOCOL_VERSIONS.contains(&requested) {
                requested
            } else {
                LATEST_PROTOCOL_VERSION
            };
            Dispatch::Respond(rpc_result(
                &id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "Debug a serial device through the running SerialMonitorEssential GUI app. The app owns the COM port; these tools read and write the same session the human sees. Typical flow: serial_status -> serial_send -> serial_wait_for -> serial_read_tail."
                }),
            ))
        }
        "ping" => Dispatch::Respond(rpc_result(&id, json!({}))),
        "tools/list" => Dispatch::Respond(rpc_result(&id, json!({ "tools": tool_definitions() }))),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            if !tool_exists(name) {
                return Dispatch::Respond(rpc_error(&id, -32602, &format!("Unknown tool: {name}")));
            }
            Dispatch::ToolCall {
                id,
                name: name.to_string(),
                args,
            }
        }
        other => Dispatch::Respond(rpc_error(
            &id,
            -32601,
            &format!("Method not found: {other}"),
        )),
    }
}

/// ツール呼び出しを実行して JSON-RPC 応答に包む
pub fn run_tool_call(
    bridge: &mut dyn Bridge,
    id: &Value,
    name: &str,
    args: &Value,
    cancelled: &dyn Fn() -> bool,
) -> Value {
    let result = match call_tool_with_cancel(bridge, name, args, cancelled) {
        Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
        Err(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": true }),
    };
    rpc_result(id, result)
}

/// stdin の 1 行を同期処理する（テスト用の合成: dispatch + 即時実行）
#[cfg(test)]
pub fn handle_line(bridge: &mut dyn Bridge, line: &str) -> Option<Value> {
    match dispatch_line(line) {
        Dispatch::Respond(v) => Some(v),
        Dispatch::ToolCall { id, name, args } => {
            Some(run_tool_call(bridge, &id, &name, &args, &|| false))
        }
        Dispatch::Cancelled(_) | Dispatch::Ignore => None,
    }
}

fn tool_exists(name: &str) -> bool {
    matches!(
        name,
        "serial_status"
            | "serial_ports"
            | "serial_read_tail"
            | "serial_read_range"
            | "serial_send"
            | "serial_send_hex"
            | "serial_wait_for"
    )
}

/// 応答 1 行を stdout へ書く（スレッド安全: `Stdout` はプロセス全体で排他）
fn write_response(response: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{response}");
    let _ = out.flush();
}

/// GUI サブシステム (release) の exe がコンソールから直接起動されたとき、
/// 親コンソールへ stdio を繋ぐ。
///
/// GUI サブシステムでは標準ハンドルが null のため、素で `--mcp` を叩くと
/// stdin が即 EOF になり無言で終了して「壊れている」ように見える。
/// MCP クライアントがパイプ付きで spawn した場合は STARTF_USESTDHANDLES で
/// ハンドルが渡っているため、AttachConsole は既存ハンドルを上書きしない。
#[cfg(windows)]
fn attach_parent_console() {
    extern "system" {
        fn AttachConsole(dw_process_id: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// `--mcp` のエントリポイント: stdin が閉じるまで JSON-RPC を処理する
///
/// - stdout には JSON-RPC 以外を一切書かない（ログは stderr = env_logger 既定）。
/// - ツール呼び出しはワーカースレッドで逐次実行し、メインスレッドは stdin を
///   読み続ける。`serial_wait_for` の長い待ちの間も `ping` に即応答でき、
///   `notifications/cancelled` で待ちを中断できる。
/// - キャンセル済み要求の応答は送らない（クライアントは既に破棄している）。
pub fn run_stdio() {
    #[cfg(windows)]
    attach_parent_console();

    let bridge_cfg = TcpBridgeClient::from_env();
    log::info!(
        "[mcp] stdio server ready (bridge {}:{}{})",
        bridge_cfg.host,
        bridge_cfg.port,
        if bridge_cfg.token.is_some() {
            ", token set"
        } else {
            ""
        }
    );

    // requestId(文字列化) の集合。cancelled 通知で追加、ツール完了時に除去。
    let cancels: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let (tx, rx) = mpsc::channel::<(Value, String, Value)>();

    let worker_cancels = cancels.clone();
    let worker = thread::spawn(move || {
        let mut bridge = bridge_cfg;
        while let Ok((id, name, args)) = rx.recv() {
            let key = id.to_string();
            let is_cancelled = || {
                worker_cancels
                    .lock()
                    .map(|s| s.contains(&key))
                    .unwrap_or(false)
            };
            let response = run_tool_call(&mut bridge, &id, &name, &args, &is_cancelled);
            // 完了時に必ず集合から除去（残すと際限なく溜まる）。
            // 除去できた = キャンセル済みなので応答は送らない。
            let was_cancelled = worker_cancels
                .lock()
                .map(|mut s| s.remove(&key))
                .unwrap_or(false);
            if !was_cancelled {
                write_response(&response);
            }
        }
    });

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        match dispatch_line(&line) {
            Dispatch::Respond(response) => write_response(&response),
            Dispatch::ToolCall { id, name, args } => {
                let _ = tx.send((id, name, args));
            }
            Dispatch::Cancelled(request_id) => {
                if let Ok(mut set) = cancels.lock() {
                    set.insert(request_id.to_string());
                }
            }
            Dispatch::Ignore => {}
        }
    }
    // stdin EOF: キューを閉じ、実行中のツールが終わるのを待って終了する
    drop(tx);
    let _ = worker.join();
    log::info!("[mcp] stdin closed, exiting");
}

// ============================================================================
// テスト
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// 記録付きモックブリッジ。応答は前詰めのキューで与える。
    struct MockBridge {
        calls: Vec<(String, Value)>,
        responses: VecDeque<Result<Value, String>>,
    }

    impl MockBridge {
        fn new(responses: Vec<Result<Value, String>>) -> Self {
            Self {
                calls: Vec::new(),
                responses: responses.into(),
            }
        }
    }

    impl Bridge for MockBridge {
        fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
            self.calls.push((method.to_string(), params));
            self.responses
                .pop_front()
                .unwrap_or_else(|| Err("mock exhausted".to_string()))
        }
    }

    fn tail_response(data: &[u8], offset: u64, total: u64) -> Result<Value, String> {
        Ok(json!({ "base64": BASE64.encode(data), "offset": offset, "total_bytes": total }))
    }

    // ---------------- 表示ヘルパ ----------------

    #[test]
    fn test_non_printable_ratio_and_looks_like_text() {
        assert_eq!(non_printable_ratio(b""), 0.0);
        assert_eq!(non_printable_ratio(b"hello\r\n\tworld"), 0.0);
        assert!(looks_like_text("温度:25.5\n".as_bytes()));
        // ほぼ全部が制御バイト -> バイナリ
        assert!(!looks_like_text(&[0x00, 0x01, 0x02, 0x03]));
        // 1 割以下の混入はテキスト扱い
        let mut mostly_text = vec![b'a'; 99];
        mostly_text.push(0x00);
        assert!(looks_like_text(&mostly_text));
    }

    // ---------------- 描画予算 (docs/26 §7 段階 0) ----------------

    fn render_default(buf: &[u8], offset: u64, keep_tail: bool, max_chars: usize) -> Rendered {
        render_chunk(
            buf,
            offset,
            None,
            &[],
            RenderFormat::Auto,
            false,
            max_chars,
            keep_tail,
        )
    }

    #[test]
    fn test_render_format_parse() {
        assert_eq!(RenderFormat::parse(None).unwrap(), RenderFormat::Auto);
        assert_eq!(
            RenderFormat::parse(Some(&json!("hex"))).unwrap(),
            RenderFormat::Hex
        );
        assert_eq!(
            RenderFormat::parse(Some(&json!("lines"))).unwrap(),
            RenderFormat::Lines
        );
        assert!(RenderFormat::parse(Some(&json!("yaml"))).is_err());
    }

    #[test]
    fn test_char_budget_counts_like_lossy_utf8() {
        // ASCII は 1 バイト = 1 文字
        assert_eq!(head_bytes_within_char_budget(b"hello world", 5), 5);
        assert_eq!(head_bytes_within_char_budget(b"hello", 99), 5);
        // CJK は 3 バイト = 1 文字。バイト数で概算すると 1/3 しか使えない
        let jp = "温度センサ".as_bytes();
        assert_eq!(jp.len(), 15);
        assert_eq!(head_bytes_within_char_budget(jp, 2), 6, "2 chars = 6 bytes");
        assert_eq!(head_bytes_within_char_budget(jp, 5), 15);
        // 不正バイト列は error_len 分をまとめて 1 個の U+FFFD
        let bad = [b'a', 0xff, 0xfe, b'b'];
        let lossy = String::from_utf8_lossy(&bad);
        assert_eq!(lossy.chars().count(), 4, "a + 2x U+FFFD + b");
        assert_eq!(head_bytes_within_char_budget(&bad, 4), 4);
        assert_eq!(head_bytes_within_char_budget(&bad, 2), 2);
    }

    #[test]
    fn test_tail_char_budget_does_not_split_multibyte() {
        let jp = "温度センサ".as_bytes();
        // 末尾 2 文字 = 6 バイト。境界を割ると U+FFFD が出るので割らないこと
        let keep = tail_bytes_within_char_budget(jp, 2);
        assert_eq!(keep, 6);
        let tail = &jp[jp.len() - keep..];
        assert_eq!(std::str::from_utf8(tail).unwrap(), "ンサ");
    }

    #[test]
    fn test_collapse_repeated_lines() {
        let (out, removed) = collapse_repeated_lines("a\na\na\nb\nc\nc");
        assert_eq!(out, "a  [x3]\nb\nc  [x2]");
        assert_eq!(removed, 3);
        let (out, removed) = collapse_repeated_lines("x\ny");
        assert_eq!(out, "x\ny");
        assert_eq!(removed, 0);
    }

    #[test]
    fn test_render_chunk_never_exceeds_max_chars() {
        // 受入基準: どんな入力・どんな予算でも、返す文字数は max_chars 以下。
        let ascii = vec![b'A'; 40_000];
        let binary: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
        let jp = "温度 25.5 度\n".repeat(3000).into_bytes();
        let mut lines = Vec::new();
        for i in 0..4000 {
            lines.extend_from_slice(format!("line {i}\n").as_bytes());
        }
        for buf in [&ascii, &binary, &jp, &lines] {
            for &budget in &[MIN_MAX_CHARS, 500, 1000, DEFAULT_MAX_CHARS, 20_000] {
                for keep_tail in [true, false] {
                    for format in [
                        RenderFormat::Auto,
                        RenderFormat::Text,
                        RenderFormat::Hex,
                        RenderFormat::Lines,
                    ] {
                        for collapse in [false, true] {
                            let r = render_chunk(
                                buf,
                                1000,
                                None,
                                &[],
                                format,
                                collapse,
                                budget,
                                keep_tail,
                            );
                            let n = r.text.chars().count();
                            assert!(
                                n <= budget,
                                "budget {budget} exceeded: {n} chars (format={format:?} \
                                 collapse={collapse} keep_tail={keep_tail} len={})",
                                buf.len()
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn test_render_chunk_hex_pathological_case_is_bounded() {
        // docs/26 の動機となったケース: looks_like_text が false になった瞬間、
        // 既定 4096 バイトの要求が約 20 KB(6〜8k トークン) を返していた。
        let binary: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        assert!(!looks_like_text(&binary), "this must render as hex");

        let unbounded = hex_dump(&binary, 0).chars().count();
        assert!(
            unbounded > 19_000,
            "the old behaviour really was ~20 KB, got {unbounded}"
        );

        let r = render_default(&binary, 0, true, DEFAULT_MAX_CHARS);
        assert!(r.truncated);
        assert!(
            r.text.chars().count() <= DEFAULT_MAX_CHARS,
            "must fit the default budget"
        );
        assert!(
            r.text.contains("truncated:"),
            "truncation must be reported, never silent"
        );
    }

    #[test]
    fn test_render_chunk_truncation_footer_is_resumable() {
        // range 読み (keep_tail=false): 先頭を残し、続きの正確な offset を出す
        let buf = vec![b'x'; 10_000];
        let r = render_chunk(
            &buf,
            5_000,
            None,
            &[],
            RenderFormat::Text,
            false,
            1000,
            false,
        );
        assert!(r.truncated);
        assert_eq!(r.shown_start, 5_000, "head keeps the start of the range");
        let dropped = 10_000 - (r.shown_end - r.shown_start);
        assert!(r.text.contains(&format!(
            "serial_read_range(offset={}, length={})",
            r.shown_end, dropped
        )));

        // tail 読み (keep_tail=true): 末尾を残し、落とした先頭側を指す
        let r = render_chunk(
            &buf,
            5_000,
            None,
            &[],
            RenderFormat::Text,
            false,
            1000,
            true,
        );
        assert!(r.truncated);
        assert_eq!(r.shown_end, 15_000, "tail keeps the most recent bytes");
        assert!(r.text.contains(&format!(
            "serial_read_range(offset=5000, length={})",
            r.shown_start - 5_000
        )));
    }

    #[test]
    fn test_render_chunk_shown_range_matches_the_bytes_rendered() {
        // 報告した shown 範囲と実際に描いたバイトが一致すること（ハンドルの信頼性）
        let mut buf = Vec::new();
        for i in 0..2000 {
            buf.extend_from_slice(format!("{i:06}\n").as_bytes());
        }
        let r = render_chunk(&buf, 700, None, &[], RenderFormat::Text, false, 1200, true);
        assert!(r.truncated);
        let body = r.text.split("\n---\n").nth(1).unwrap();
        let expected = String::from_utf8_lossy(
            &buf[(r.shown_start - 700) as usize..(r.shown_end - 700) as usize],
        );
        assert_eq!(body, expected, "body must be exactly the shown byte range");
    }

    #[test]
    fn test_render_chunk_lines_format_drops_partial_lines() {
        let buf = b"tail of a cut line\nfull line 1\nfull line 2\npartial".to_vec();
        let r = render_chunk(
            &buf,
            0,
            None,
            &[],
            RenderFormat::Lines,
            false,
            DEFAULT_MAX_CHARS,
            false,
        );
        let body = r.text.split("\n---\n").nth(1).unwrap();
        assert_eq!(body, "full line 1\nfull line 2\n");
        assert_eq!(r.shown_start, 19, "offset advances past the dropped prefix");
        assert!(r.truncated, "dropping partial lines is reported");
    }

    #[test]
    fn test_render_chunk_untruncated_output_has_no_footer() {
        let r = render_default(b"short\n", 42, true, DEFAULT_MAX_CHARS);
        assert!(!r.truncated);
        assert!(!r.text.contains("truncated:"));
        assert!(!r.text.contains("shown="));
        assert_eq!(r.shown_start, 42);
        assert_eq!(r.shown_end, 48);
    }

    #[test]
    fn test_render_chunk_collapse_shrinks_repetitive_logs() {
        let buf = "WDT tick\n".repeat(200).into_bytes();
        let plain = render_chunk(
            &buf,
            0,
            None,
            &[],
            RenderFormat::Text,
            false,
            DEFAULT_MAX_CHARS,
            true,
        );
        let collapsed = render_chunk(
            &buf,
            0,
            None,
            &[],
            RenderFormat::Text,
            true,
            DEFAULT_MAX_CHARS,
            true,
        );
        assert!(collapsed.text.chars().count() < plain.text.chars().count() / 4);
        assert!(collapsed.text.contains("[x"));
        assert!(
            collapsed.text.contains("collapsed="),
            "collapsing must be disclosed in the header"
        );
    }

    // ---------------- serial_find (docs/26 §7 段階 1) ----------------

    fn find_reply(matches: u64, exact: bool, complete: bool, gap: u64, hits: Value) -> Value {
        json!({
            "matches": matches, "exact": exact, "complete": complete,
            "hits": hits, "first": 1204, "last": 52110993,
            "range_start": 0, "range_end": 52400000,
            "bytes_scanned": if complete { 52400000u64 } else { 8000000 },
            "gap_bytes": gap, "total_bytes": 52400000u64, "elapsed_ms": 812,
        })
    }

    #[test]
    fn test_find_reports_exact_counts_with_coverage() {
        let res = find_reply(
            1284,
            true,
            true,
            0,
            json!([{ "offset": 1204, "len": 3, "excerpt": "E: i2c nak addr=0x48", "excerpt_truncated": false }]),
        );
        let text = render_find(&res, DEFAULT_MAX_CHARS);
        assert!(text.starts_with("matches: 1284 (exact)"));
        assert!(text.contains("100%"), "coverage is always stated: {text}");
        assert!(text.contains("@off=1204  E: i2c nak addr=0x48"));
        assert!(text.contains("first @off=1204  last @off=52110993"));
    }

    #[test]
    fn test_find_never_reports_a_bare_zero() {
        // 「見つからなかった」と「見に行けなかった」を混同させないこと。
        // 完走した 0 件でもカバレッジを必ず添える（docs/26 §6.2 規則 5）。
        let clean = render_find(&find_reply(0, true, true, 0, json!([])), DEFAULT_MAX_CHARS);
        assert!(clean.starts_with("matches: 0 (exact)"));
        assert!(
            clean.contains("scanned") && clean.contains("100%"),
            "even an exact zero must carry its coverage: {clean}"
        );

        // 打ち切り / 欠落があるときは「正確」と言ってはいけない
        let partial = render_find(
            &find_reply(0, false, false, 0, json!([])),
            DEFAULT_MAX_CHARS,
        );
        assert!(
            partial.contains("AT LEAST") && partial.contains("scan cut short"),
            "a truncated scan must not look like a definitive zero: {partial}"
        );
        assert!(!partial.contains("(exact)"));

        let gappy = render_find(
            &find_reply(3, false, true, 4096, json!([])),
            DEFAULT_MAX_CHARS,
        );
        assert!(gappy.contains("AT LEAST"));
        assert!(gappy.contains("4096 bytes unreadable"));
    }

    #[test]
    fn test_find_incomplete_scan_offers_a_resume_handle() {
        let text = render_find(
            &find_reply(9, false, false, 0, json!([])),
            DEFAULT_MAX_CHARS,
        );
        // bytes_scanned=8000000, range_start=0 -> 続きは 8000000 から
        assert!(
            text.contains("resume: serial_find(pattern=..., from=8000000, to=52400000)"),
            "an interrupted scan must hand back a way to continue: {text}"
        );
    }

    #[test]
    fn test_find_discloses_that_hits_were_capped() {
        let hits: Vec<Value> = (0..20)
            .map(|i| json!({ "offset": 1000 + i * 10, "len": 3, "excerpt": format!("E: line {i}"), "excerpt_truncated": false }))
            .collect();
        let text = render_find(
            &find_reply(1284, true, true, 0, json!(hits)),
            DEFAULT_MAX_CHARS,
        );
        assert!(text.contains("showing 20 of 1284 hits"));
        assert!(text.contains("serial_read_range"));

        // 全件出せたときは「これで全部」と言い切る
        let few =
            json!([{ "offset": 7, "len": 3, "excerpt": "E: only", "excerpt_truncated": false }]);
        let text = render_find(&find_reply(1, true, true, 0, few), DEFAULT_MAX_CHARS);
        assert!(text.contains("all hits listed."));
    }

    #[test]
    fn test_find_output_stays_within_budget() {
        let hits: Vec<Value> = (0..200)
            .map(|i| json!({ "offset": 1_000_000 + i * 977, "len": 5, "excerpt": "E: ".to_string() + &"x".repeat(115), "excerpt_truncated": true }))
            .collect();
        for budget in [MIN_MAX_CHARS, 1000, DEFAULT_MAX_CHARS] {
            let text = render_find(&find_reply(9999, true, true, 0, json!(hits)), budget);
            assert!(
                text.chars().count() <= budget,
                "budget {budget} exceeded: {} chars",
                text.chars().count()
            );
            assert!(
                text.starts_with("matches: 9999"),
                "the count must survive any budget"
            );
        }
    }

    #[test]
    fn test_find_tool_forwards_range_and_count_only() {
        let mut bridge = MockBridge::new(vec![Ok(find_reply(2, true, true, 0, json!([])))]);
        let out = call_tool(
            &mut bridge,
            "serial_find",
            &json!({ "pattern": "E: ", "from": 100, "to": 900, "count_only": true }),
        )
        .unwrap();
        assert!(out.starts_with("matches: 2 (exact)"));
        let (method, params) = &bridge.calls[0];
        assert_eq!(method, "find");
        assert_eq!(params["pattern"], json!("E: "));
        assert_eq!(params["from"], json!(100));
        assert_eq!(params["to"], json!(900));
        assert_eq!(
            params["max_hits"],
            json!(0),
            "count_only must not pull any excerpts"
        );
    }

    #[test]
    fn test_find_tool_requires_a_pattern() {
        let mut bridge = MockBridge::new(vec![]);
        let err = call_tool(&mut bridge, "serial_find", &json!({})).unwrap_err();
        assert!(err.contains("pattern"));
    }

    #[test]
    fn test_hex_dump_format() {
        let dump = hex_dump(b"0123456789abcdefG", 0x10);
        let lines: Vec<&str> = dump.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            "00000010  30 31 32 33 34 35 36 37  38 39 61 62 63 64 65 66  |0123456789abcdef|"
        );
        assert!(lines[1].starts_with("00000020  47 "));
        assert!(lines[1].ends_with("|G|"));
    }

    #[test]
    fn test_parse_hex_variants() {
        assert_eq!(parse_hex("48 65 6c").unwrap(), vec![0x48, 0x65, 0x6c]);
        assert_eq!(parse_hex("0x48,0X65:6C").unwrap(), vec![0x48, 0x65, 0x6c]);
        assert_eq!(
            parse_hex("de-ad_be;ef").unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
        assert!(parse_hex("").unwrap_err().contains("empty"));
        assert!(parse_hex("zz").unwrap_err().contains("non-hex"));
        assert!(parse_hex("abc").unwrap_err().contains("even"));
    }

    #[test]
    fn test_utf8_validity_allows_truncation_at_both_ends() {
        // "あいう" (9 バイト) の途中から途中まで: 先頭は継続バイト、末尾は欠け
        let full = "あいう".as_bytes();
        let window = &full[1..8]; // 継続バイト2つ + "い" + "う"の先頭2バイト
        assert!(
            is_valid_utf8_allow_truncation(window),
            "tail 窓は多バイト文字の途中から始まり得る"
        );
        // 正当なテキストが hex ダンプ扱いにならないこと
        assert!(looks_like_text(window));
        // 本物のバイナリは依然としてバイナリ判定
        assert!(!is_valid_utf8_allow_truncation(&[
            0x80, 0xff, 0xfe, 0xfd, 0xfc
        ]));
    }

    // ---------------- JSON-RPC ----------------

    fn rpc(bridge: &mut dyn Bridge, line: &str) -> Value {
        handle_line(bridge, line).expect("expected a response")
    }

    #[test]
    fn test_initialize_echoes_known_version() {
        let mut bridge = MockBridge::new(vec![]);
        let res = rpc(
            &mut bridge,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}"#,
        );
        assert_eq!(res["id"], json!(1));
        assert_eq!(res["result"]["protocolVersion"], json!("2024-11-05"));
        assert_eq!(res["result"]["serverInfo"]["name"], json!(SERVER_NAME));
        assert!(res["result"]["capabilities"]["tools"].is_object());

        // 未知の版は最新を名乗る
        let res = rpc(
            &mut bridge,
            r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
        );
        assert_eq!(
            res["result"]["protocolVersion"],
            json!(LATEST_PROTOCOL_VERSION)
        );
    }

    #[test]
    fn test_notifications_and_ping() {
        let mut bridge = MockBridge::new(vec![]);
        assert!(handle_line(
            &mut bridge,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
        )
        .is_none());
        let res = rpc(&mut bridge, r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#);
        assert_eq!(res["result"], json!({}));
    }

    #[test]
    fn test_parse_error_and_method_not_found() {
        let mut bridge = MockBridge::new(vec![]);
        let res = rpc(&mut bridge, "{broken");
        assert_eq!(res["error"]["code"], json!(-32700));
        let res = rpc(
            &mut bridge,
            r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#,
        );
        assert_eq!(res["error"]["code"], json!(-32601));
    }

    #[test]
    fn test_tools_list_is_complete_and_well_formed() {
        let mut bridge = MockBridge::new(vec![]);
        let res = rpc(
            &mut bridge,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        );
        let tools = res["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            vec![
                "serial_status",
                "serial_ports",
                "serial_digest",
                "serial_watch",
                "serial_sample",
                "serial_read_tail",
                "serial_read_range",
                "serial_find",
                "serial_send",
                "serial_send_hex",
                "serial_wait_for",
            ],
            "tool set and order must stay in sync with mcp/server.mjs (DEBT-6)"
        );
        // 全ツールにスキーマと説明がある
        for tool in tools {
            assert!(tool["inputSchema"]["type"] == json!("object"));
            assert!(!tool["description"].as_str().unwrap().is_empty());
        }
        // 常駐コスト（スキーマ全体）が肥大していないこと。
        // 段階 2 で digest / watch、段階 3 で sample を足して 7.1k -> 約 10.5k 文字
        // （約 +900 トークン）になった。docs/26 の「約 1.0k トークン」という目標は
        // 達成できていない（既存 8 ツールの時点で約 1.9k トークンあった）。
        // 1 回の tail が 25 万トークンだったことに対する取引としては妥当。
        // 段階 3 で `serial_patterns` を別ツールにせず `serial_digest(template=)`
        // へ畳んだのは、この常駐コストを増やさないため（docs/26 §7.4）。
        let schema_chars = serde_json::to_string(&res["result"]["tools"])
            .unwrap()
            .chars()
            .count();
        assert!(
            schema_chars < 11_500,
            "tools/list schema is {schema_chars} chars; it is resident in every request"
        );
    }

    #[test]
    fn test_tools_call_unknown_tool_is_protocol_error() {
        let mut bridge = MockBridge::new(vec![]);
        let res = rpc(
            &mut bridge,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
        );
        assert_eq!(res["error"]["code"], json!(-32602));
    }

    /// 再接続待ちは「待てば続きが来る」と分かる書き方で出る（ブリッジ未対応の古い
    /// 応答では `reconnecting` が無いので従来どおり "no"）
    #[test]
    fn test_tools_call_status_reports_reconnecting() {
        let mut bridge = MockBridge::new(vec![
            Ok(
                json!({ "connected": false, "reconnecting": true, "port_name": "COM3", "total_bytes": 42, "app_version": "0.1.0", "protocol": 1 }),
            ),
            Ok(
                json!({ "connected": false, "port_name": null, "total_bytes": 0, "app_version": "0.1.0", "protocol": 1 }),
            ),
        ]);
        let call =
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"serial_status"}}"#;
        let res = rpc(&mut bridge, call);
        let text = res["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("connected: no (device went away;"), "{text}");
        let res = rpc(&mut bridge, call);
        let text = res["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("connected: no\n"), "{text}");
    }

    #[test]
    fn test_tools_call_status_success_and_bridge_error() {
        let mut bridge = MockBridge::new(vec![
            Ok(
                json!({ "connected": true, "port_name": "COM3", "total_bytes": 42, "app_version": "0.1.0", "protocol": 1 }),
            ),
            Err("boom".to_string()),
        ]);
        let res = rpc(
            &mut bridge,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"serial_status"}}"#,
        );
        let text = res["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("connected: yes"));
        assert!(text.contains("port: COM3"));
        assert!(res["result"].get("isError").is_none());
        // 以前は同じ内容を `raw: {全 JSON}` で繰り返しており、情報ゼロのまま
        // トークンを倍にしていた（docs/26 §5）
        assert!(
            !text.contains("raw:"),
            "the duplicate JSON dump must stay gone: {text}"
        );
        assert!(
            text.chars().count() < 120,
            "status must stay tiny, got {} chars",
            text.chars().count()
        );

        // ブリッジ側エラーは isError のテキストになる（プロトコルエラーではない）
        let res = rpc(
            &mut bridge,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"serial_status"}}"#,
        );
        assert_eq!(res["result"]["isError"], json!(true));
        assert!(res["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("boom"));
    }

    // ---------------- 各ツール ----------------

    #[test]
    fn test_serial_ports_rendering() {
        let mut bridge = MockBridge::new(vec![Ok(json!({ "ports": ["COM3", "COM7"] }))]);
        let text = call_tool(&mut bridge, "serial_ports", &json!({})).unwrap();
        assert_eq!(text, "2 port(s):\n- COM3\n- COM7");

        let mut bridge = MockBridge::new(vec![Ok(json!({ "ports": [] }))]);
        let text = call_tool(&mut bridge, "serial_ports", &json!({})).unwrap();
        assert_eq!(text, "No serial ports found.");
    }

    #[test]
    fn test_serial_read_tail_text_and_defaults() {
        let mut bridge = MockBridge::new(vec![tail_response(b"hello\n", 10, 16)]);
        let text = call_tool(&mut bridge, "serial_read_tail", &json!({})).unwrap();
        assert!(text.starts_with("offset=10  bytes=6  total_bytes=16  rendering=text"));
        assert!(text.ends_with("hello\n"));
        // 既定バイト数がブリッジへ渡る
        assert_eq!(bridge.calls[0].1["bytes"], json!(DEFAULT_TAIL_BYTES));

        // 範囲外の bytes は拒否
        let mut bridge = MockBridge::new(vec![]);
        let err = call_tool(&mut bridge, "serial_read_tail", &json!({ "bytes": 0 })).unwrap_err();
        assert!(err.contains("bytes"));
    }

    #[test]
    fn test_serial_read_tail_binary_renders_hex() {
        let data = [0u8, 1, 2, 3, 0xff, 0xfe];
        let mut bridge = MockBridge::new(vec![tail_response(&data, 0, 6)]);
        let text = call_tool(&mut bridge, "serial_read_tail", &json!({})).unwrap();
        assert!(text.contains("rendering=hex"));
        assert!(text.contains("00 01 02 03 ff fe"));
    }

    #[test]
    fn test_serial_read_range_requires_args() {
        let mut bridge = MockBridge::new(vec![]);
        assert!(call_tool(&mut bridge, "serial_read_range", &json!({}))
            .unwrap_err()
            .contains("offset"));
        assert!(
            call_tool(&mut bridge, "serial_read_range", &json!({ "offset": 0 }))
                .unwrap_err()
                .contains("length")
        );

        let mut bridge = MockBridge::new(vec![Ok(
            json!({ "base64": BASE64.encode(b"abc"), "offset": 5, "length_read": 3 }),
        )]);
        let text = call_tool(
            &mut bridge,
            "serial_read_range",
            &json!({ "offset": 5, "length": 3 }),
        )
        .unwrap();
        assert!(text.contains("offset=5"));
        assert!(text.contains("length_read=3"));
    }

    #[test]
    fn test_serial_send_defaults_to_lf() {
        let mut bridge = MockBridge::new(vec![Ok(json!({ "bytes_written": 3 }))]);
        let text = call_tool(&mut bridge, "serial_send", &json!({ "text": "AT" })).unwrap();
        assert_eq!(bridge.calls[0].1["line_ending"], json!("lf"));
        assert_eq!(text, "Sent \"AT\" + lf -> bytes_written=3");

        let mut bridge = MockBridge::new(vec![]);
        assert!(call_tool(
            &mut bridge,
            "serial_send",
            &json!({ "text": "AT", "line_ending": "wat" })
        )
        .unwrap_err()
        .contains("line_ending"));
    }

    #[test]
    fn test_serial_send_hex_roundtrip() {
        let mut bridge = MockBridge::new(vec![Ok(json!({ "bytes_written": 2 }))]);
        let text = call_tool(
            &mut bridge,
            "serial_send_hex",
            &json!({ "hex": "0xDE 0xAD" }),
        )
        .unwrap();
        assert_eq!(
            bridge.calls[0].1["base64"],
            json!(BASE64.encode([0xdeu8, 0xad]))
        );
        assert_eq!(text, "Sent 2 byte(s) [de ad] -> bytes_written=2");
    }

    // ---------------- serial_wait_for（走査はブリッジ側。docs/26 段階 4） ----------------

    /// ブリッジ `wait_for` の応答（マッチ無し）
    fn wait_pending(scanned_to: u64, next_from: u64, total: u64) -> Result<Value, String> {
        Ok(json!({
            "matched": false,
            "offset": Value::Null,
            "len": 0,
            "excerpt": "",
            "from": 0,
            "scanned_to": scanned_to,
            "next_from": next_from,
            "total_bytes": total,
            "gap_bytes": 0,
            "reset": false,
            "tail_base64": "",
            "tail_offset": 0,
            "elapsed_ms": 1,
        }))
    }

    fn wait_match(offset: u64, excerpt: &str, total: u64) -> Result<Value, String> {
        Ok(json!({
            "matched": true,
            "offset": offset,
            "len": 2,
            "excerpt": excerpt,
            "from": 0,
            "scanned_to": total,
            "next_from": total,
            "total_bytes": total,
            "gap_bytes": 0,
            "reset": false,
            "tail_base64": "",
            "tail_offset": 0,
            "elapsed_ms": 3,
        }))
    }

    #[test]
    fn test_wait_starts_after_the_bytes_already_captured() {
        let mut bridge = MockBridge::new(vec![
            Ok(json!({ "total_bytes": 10 })),
            wait_match(10, "OK\n", 13),
        ]);
        let outcome = wait_for_pattern(&mut bridge, "OK", 1000, true, 1, &|| false).unwrap();
        assert!(outcome.matched);
        assert_eq!(outcome.offset, 10);
        // 走査開始位置は「呼び出し時点の総バイト数」= 既存データは対象外
        assert_eq!(bridge.calls[1].0, "wait_for");
        assert_eq!(bridge.calls[1].1["from"], json!(10));
    }

    #[test]
    fn test_wait_from_end_false_includes_the_recent_backlog() {
        let mut bridge = MockBridge::new(vec![
            Ok(json!({ "total_bytes": 10_000 })),
            wait_match(9_000, "OK", 10_000),
        ]);
        wait_for_pattern(&mut bridge, "OK", 1000, false, 1, &|| false).unwrap();
        assert_eq!(
            bridge.calls[1].1["from"],
            json!(10_000 - DEFAULT_TAIL_BYTES as u64)
        );
    }

    #[test]
    fn test_wait_resumes_where_the_bridge_says_and_counts_bytes_once() {
        // 2 回に分けて走査。next_from は重なりを含んでいるので、走査量は
        // scanned_to の前進分だけを数える（重なりを二重に数えない）
        let mut bridge = MockBridge::new(vec![
            Ok(json!({ "total_bytes": 100 })),
            wait_pending(1_000, 900, 1_000),
            wait_match(1_500, "OK", 2_000),
        ]);
        let outcome = wait_for_pattern(&mut bridge, "OK", 5_000, true, 1, &|| false).unwrap();
        assert!(outcome.matched);
        assert_eq!(bridge.calls[1].1["from"], json!(100));
        assert_eq!(
            bridge.calls[2].1["from"],
            json!(900),
            "must resume at next_from"
        );
        // 100 -> 1000 -> 2000 で 1900 バイト（900..1000 の重なりは 1 回だけ）
        assert_eq!(outcome.bytes_scanned, 1_900);
    }

    #[test]
    fn test_wait_reports_a_capture_reset_instead_of_waiting_forever() {
        let reset = Ok(json!({
            "matched": false, "offset": Value::Null, "len": 0, "excerpt": "",
            "from": 5_000, "scanned_to": 0, "next_from": 0, "total_bytes": 20,
            "gap_bytes": 0, "reset": true, "tail_base64": "", "tail_offset": 0, "elapsed_ms": 1,
        }));
        let mut bridge = MockBridge::new(vec![
            Ok(json!({ "total_bytes": 5_000 })),
            reset,
            wait_match(10, "OK", 40),
        ]);
        let outcome = wait_for_pattern(&mut bridge, "OK", 5_000, true, 1, &|| false).unwrap();
        assert!(outcome.matched);
        assert!(outcome.gap, "a generation change must be reported");
        assert_eq!(outcome.offset, 10);
    }

    #[test]
    fn test_wait_timeout_carries_the_tail_the_bridge_saw() {
        let pending = Ok(json!({
            "matched": false, "offset": Value::Null, "len": 0, "excerpt": "",
            "from": 0, "scanned_to": 7, "next_from": 7, "total_bytes": 7,
            "gap_bytes": 0, "reset": false,
            "tail_base64": BASE64.encode(b"partial"), "tail_offset": 0, "elapsed_ms": 1,
        }));
        let mut responses = vec![Ok(json!({ "total_bytes": 0 }))];
        responses.extend((0..200).map(|_| pending.clone()));
        let mut bridge = MockBridge::new(responses);
        let outcome = wait_for_pattern(&mut bridge, "NEVER", 30, true, 1, &|| false).unwrap();
        assert!(!outcome.matched);
        assert_eq!(outcome.tail, b"partial");
        assert_eq!(outcome.bytes_scanned, 7);
        let text = format_wait_outcome("NEVER", &outcome);
        assert!(text.starts_with("TIMEOUT"), "{text}");
        assert!(text.contains("last 7 byte(s) seen"), "{text}");
    }

    #[test]
    fn test_wait_timeout_says_when_nothing_arrived_at_all() {
        let mut responses = vec![Ok(json!({ "total_bytes": 0 }))];
        responses.extend((0..200).map(|_| wait_pending(0, 0, 0)));
        let mut bridge = MockBridge::new(responses);
        let outcome = wait_for_pattern(&mut bridge, "NEVER", 30, true, 1, &|| false).unwrap();
        let text = format_wait_outcome("NEVER", &outcome);
        // 「見つからなかった」と「見に行けなかった」を混同させない
        assert!(text.contains("no new data arrived while waiting"), "{text}");
    }

    #[test]
    fn test_wait_invalid_pattern_fails_without_a_round_trip() {
        let mut bridge = MockBridge::new(vec![]);
        assert!(wait_for_pattern(&mut bridge, "([", 100, true, 1, &|| false)
            .unwrap_err()
            .contains("invalid pattern"));
        assert!(
            bridge.calls.is_empty(),
            "a bad pattern must not hit the bridge"
        );
    }

    #[test]
    fn test_wait_cancellation_stops_promptly() {
        use std::sync::atomic::{AtomicU32, Ordering};
        // 2 回目の区切り以降キャンセル扱いにする
        let polls = AtomicU32::new(0);
        let cancelled = || polls.fetch_add(1, Ordering::SeqCst) >= 1;
        let mut responses = vec![Ok(json!({ "total_bytes": 0 }))];
        responses.extend((0..100).map(|_| wait_pending(0, 0, 0)));
        let mut bridge = MockBridge::new(responses);
        // タイムアウトは 60 秒だがキャンセルで即座に戻る
        let outcome = wait_for_pattern(&mut bridge, "NEVER", 60_000, true, 10, &cancelled).unwrap();
        assert!(outcome.cancelled);
        assert!(!outcome.matched);
        assert!(
            outcome.elapsed_ms < 5_000,
            "must return well before timeout"
        );
        assert!(format_wait_outcome("NEVER", &outcome).starts_with("CANCELLED"));
    }

    #[test]
    fn test_wait_never_asks_the_bridge_to_block_longer_than_it_may() {
        let mut responses = vec![Ok(json!({ "total_bytes": 0 }))];
        responses.extend((0..200).map(|_| wait_pending(0, 0, 0)));
        let mut bridge = MockBridge::new(responses);
        // 呼び出し側が 10 分を要求しても、1 往復は上限で切られる
        // （ブリッジの応答がクライアントのタイムアウトを越えないため）
        use std::sync::atomic::{AtomicU32, Ordering};
        let seen = AtomicU32::new(0);
        let cancelled = || seen.fetch_add(1, Ordering::SeqCst) >= 1;
        let _ = wait_for_pattern(&mut bridge, "NEVER", 600_000, true, 60_000, &cancelled);
        let asked = bridge.calls[1].1["timeout_ms"].as_u64().unwrap();
        assert!(
            asked <= crate::bridge::WAIT_SERVER_MAX_MS,
            "asked for {asked}ms"
        );
    }

    // ---------------- serial_send(expect=)（送信と待機を 1 呼び出しに） ----------------

    #[test]
    fn test_send_without_expect_is_unchanged() {
        let mut bridge =
            MockBridge::new(vec![Ok(json!({ "bytes_written": 3, "offset_before": 99 }))]);
        let text = call_tool(&mut bridge, "serial_send", &json!({ "text": "AT" })).unwrap();
        assert_eq!(text, "Sent \"AT\" + lf -> bytes_written=3");
        assert_eq!(bridge.calls.len(), 1, "no wait when 'expect' is absent");
    }

    #[test]
    fn test_send_with_expect_scans_from_just_before_the_write() {
        let mut bridge = MockBridge::new(vec![
            Ok(json!({ "bytes_written": 7, "offset_before": 4_096 })),
            wait_match(4_100, "AT+GMR\nOK\n", 4_110),
        ]);
        let text = call_tool(
            &mut bridge,
            "serial_send",
            &json!({ "text": "AT+GMR", "expect": "OK|ERROR" }),
        )
        .unwrap();
        assert!(
            text.starts_with("Sent \"AT+GMR\" + lf -> bytes_written=7"),
            "{text}"
        );
        assert!(text.contains("MATCH"), "{text}");
        assert!(text.contains("offset=4100"), "{text}");
        // 走査は送信の**直前**から始まる: 即答する機器でも取りこぼさない
        assert_eq!(bridge.calls[1].0, "wait_for");
        assert_eq!(bridge.calls[1].1["from"], json!(4_096));
        // status を引かない（送信が返したオフセットで足りる）
        assert_eq!(bridge.calls.len(), 2);
    }

    #[test]
    fn test_send_hex_with_expect_and_bad_expect_argument() {
        let mut bridge = MockBridge::new(vec![
            Ok(json!({ "bytes_written": 2, "offset_before": 10 })),
            wait_match(12, "\x06", 14),
        ]);
        let text = call_tool(
            &mut bridge,
            "serial_send_hex",
            &json!({ "hex": "DE AD", "expect": "\\x06" }),
        )
        .unwrap();
        assert!(
            text.starts_with("Sent 2 byte(s) [de ad] -> bytes_written=2"),
            "{text}"
        );
        assert!(text.contains("MATCH"), "{text}");

        let mut bridge =
            MockBridge::new(vec![Ok(json!({ "bytes_written": 1, "offset_before": 0 }))]);
        assert!(call_tool(
            &mut bridge,
            "serial_send",
            &json!({ "text": "A", "expect": 7 })
        )
        .unwrap_err()
        .contains("'expect' must be a string"));
    }

    #[test]
    fn test_send_with_expect_reports_a_timeout_without_hiding_it() {
        let pending = Ok(json!({
            "matched": false, "offset": Value::Null, "len": 0, "excerpt": "",
            "from": 0, "scanned_to": 5, "next_from": 5, "total_bytes": 5,
            "gap_bytes": 0, "reset": false,
            "tail_base64": BASE64.encode(b"noise"), "tail_offset": 0, "elapsed_ms": 1,
        }));
        let mut responses = vec![Ok(json!({ "bytes_written": 1, "offset_before": 0 }))];
        responses.extend((0..200).map(|_| pending.clone()));
        let mut bridge = MockBridge::new(responses);
        let text = call_tool(
            &mut bridge,
            "serial_send",
            &json!({ "text": "AT", "expect": "OK", "expect_timeout_ms": 100 }),
        )
        .unwrap();
        assert!(text.contains("bytes_written=1"), "{text}");
        assert!(text.contains("TIMEOUT"), "{text}");
        assert!(text.contains("last 5 byte(s) seen"), "{text}");
    }

    // ---------------- dispatch（非同期化のための振り分け） ----------------

    #[test]
    fn test_dispatch_separates_tool_calls_from_immediate_responses() {
        // ping / initialize / tools/list は即応答（ワーカーを経由しない）
        assert!(matches!(
            dispatch_line(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#),
            Dispatch::Respond(_)
        ));
        // tools/call はワーカー行き
        match dispatch_line(
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"serial_status","arguments":{}}}"#,
        ) {
            Dispatch::ToolCall { id, name, .. } => {
                assert_eq!(id, json!(7));
                assert_eq!(name, "serial_status");
            }
            other => panic!("expected ToolCall, got {:?}", other),
        }
        // 未知ツールはワーカーへ行かず即エラー
        assert!(matches!(
            dispatch_line(
                r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"nope"}}"#
            ),
            Dispatch::Respond(_)
        ));
    }

    #[test]
    fn test_dispatch_cancelled_notification() {
        match dispatch_line(
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":42,"reason":"user"}}"#,
        ) {
            Dispatch::Cancelled(id) => assert_eq!(id, json!(42)),
            other => panic!("expected Cancelled, got {:?}", other),
        }
        // requestId 無しの cancelled や他の通知は無視
        assert_eq!(
            dispatch_line(r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#),
            Dispatch::Ignore
        );
        assert_eq!(
            dispatch_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            Dispatch::Ignore
        );
    }

    #[test]
    fn test_dispatch_null_id_is_ignored() {
        // id:null + method は通知扱い（Ignore）。`!id.is_null()` を true に固定する
        // 退行を検出する（null id を通常リクエストとして処理してしまう）。
        assert_eq!(
            dispatch_line(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#),
            Dispatch::Ignore
        );
    }

    #[test]
    fn test_from_env_parses_and_filters() {
        // 直列化して env の競合を避ける（他テストはこれらの変数を触らない）
        std::env::set_var("SME_BRIDGE_HOST", "10.0.0.5");
        std::env::set_var("SME_BRIDGE_PORT", "60000");
        std::env::set_var("SME_BRIDGE_TOKEN", "tok");
        let c = TcpBridgeClient::from_env();
        assert_eq!(c.host, "10.0.0.5");
        assert_eq!(c.port, 60000);
        assert_eq!(c.token.as_deref(), Some("tok"));

        // port=0 は無効 → 既定へフォールバック（`> 0` フィルタの退行検出）
        std::env::set_var("SME_BRIDGE_PORT", "0");
        // token 空 → None（`!t.is_empty()` の退行検出）
        std::env::set_var("SME_BRIDGE_TOKEN", "");
        let c = TcpBridgeClient::from_env();
        assert_eq!(c.port, DEFAULT_BRIDGE_PORT);
        assert_eq!(c.token, None);

        // 数値でない → 既定
        std::env::set_var("SME_BRIDGE_PORT", "abc");
        assert_eq!(TcpBridgeClient::from_env().port, DEFAULT_BRIDGE_PORT);

        std::env::remove_var("SME_BRIDGE_HOST");
        std::env::remove_var("SME_BRIDGE_PORT");
        std::env::remove_var("SME_BRIDGE_TOKEN");
    }

    #[test]
    fn test_is_valid_utf8_truncation_bounds() {
        // 末尾 3 バイトまでの切断を許すが、4 バイト以上は不正のまま
        // （trim ループ境界 `<`/`<=` と長さ演算の退行検出）
        let base = "AAAA".as_bytes(); // 4 バイトの妥当 ASCII
        assert!(is_valid_utf8_allow_truncation(base));
        // 末尾に 3 バイトの継続バイト（不正だが末尾切断として許容）
        let mut trunc3 = base.to_vec();
        trunc3.extend_from_slice(&[0xe3, 0x81, 0x82][..2]); // "あ" の先頭2バイト
        assert!(is_valid_utf8_allow_truncation(&trunc3));
        // 4 バイト以上の不正列は許容しない
        assert!(!is_valid_utf8_allow_truncation(&[
            0xff, 0xff, 0xff, 0xff, 0x41
        ]));
        // 空は妥当
        assert!(is_valid_utf8_allow_truncation(&[]));
    }

    #[test]
    fn test_serial_send_all_line_endings() {
        // none/cr/lf/crlf それぞれで正しい line_ending がブリッジへ渡ることを確認。
        // match アーム削除（"none"|"cr"|"lf"|"crlf"）の退行を検出する。
        for ending in ["none", "cr", "lf", "crlf"] {
            let mut bridge = MockBridge::new(vec![Ok(json!({ "bytes_written": 2 }))]);
            let text = call_tool(
                &mut bridge,
                "serial_send",
                &json!({ "text": "AT", "line_ending": ending }),
            )
            .unwrap();
            assert_eq!(bridge.calls[0].1["line_ending"], json!(ending));
            assert!(text.contains(&format!("+ {ending} ->")));
        }
    }

    // ---------------- 実ソケット統合（bridge.rs のサーバに接続） ----------------

    #[test]
    fn test_tcp_client_against_real_bridge() {
        use crate::bridge::{BridgeCtx, BridgeServer};
        use crate::serial::data_store::DataStore;
        use std::sync::atomic::AtomicUsize;
        use std::sync::{Arc, Mutex};

        let store = Arc::new(DataStore::new().expect("DataStore::new"));
        store.push_test_data(b"INTEGRATION");
        let ctx = Arc::new(BridgeCtx::new(
            Arc::new(Mutex::new(Some(store))),
            Arc::new(Mutex::new(None)),
            None,
            Arc::new(Mutex::new(None)),
            Arc::new(crate::insight::InsightIndex::new()),
            Box::new(|_| {}),
        ));
        let connections = Arc::new(AtomicUsize::new(0));
        let server = BridgeServer::start(0, ctx, connections).expect("start");

        let mut client = TcpBridgeClient::new("127.0.0.1".to_string(), server.port(), None);

        // MCP レイヤ経由でステータスとデータが読める
        let text = call_tool(&mut client, "serial_status", &json!({})).unwrap();
        assert!(text.contains("total_bytes: 11"));
        let text = call_tool(&mut client, "serial_read_tail", &json!({ "bytes": 5 })).unwrap();
        assert!(text.ends_with("ATION"));
        // ポート未オープンの send はブリッジエラーとして返る
        let err = call_tool(&mut client, "serial_send", &json!({ "text": "X" })).unwrap_err();
        assert!(err.contains("port not open"));
    }

    #[test]
    fn test_tcp_client_unreachable_bridge_friendly_error() {
        // 誰も聞いていないポート -> 接続失敗の案内文（両言語）
        let mut client = TcpBridgeClient::new("127.0.0.1".to_string(), 1, None);
        let err = call_tool(&mut client, "serial_status", &json!({})).unwrap_err();
        assert!(err.contains("MCP Server"));
        assert!(err.contains("Cannot reach"));
    }

    #[test]
    fn test_tcp_client_surfaces_connection_rejection() {
        // 接続上限時、ブリッジは id:null のエラー行を 1 行返して閉じる。
        // これを読み飛ばすと「ブリッジに繋がらない」という誤案内になる。
        use std::net::TcpListener;
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            // 1 接続だけ処理して閉じる（非リトライの検証も兼ねる）。
            // 実サーバ同様、クライアントのリクエスト行を読んでから拒否行を
            // 返す: 未読データを残して閉じると RST で拒否行が破棄されることが
            // ある（Windows で顕著。本番の拒否パスも同じ理由で読み捨てる）。
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            let _ = reader.read_line(&mut request_line);
            let _ = stream
                .write_all(b"{\"id\":null,\"ok\":false,\"error\":\"too many connections\"}\n");
            let _ = stream.shutdown(std::net::Shutdown::Write);
        });

        let mut client = TcpBridgeClient::new("127.0.0.1".to_string(), port, None);
        let err = call_tool(&mut client, "serial_status", &json!({})).unwrap_err();
        assert!(
            err.contains("too many connections"),
            "must surface the real rejection reason, got: {err}"
        );
        assert!(!err.contains("Cannot reach"), "must not claim unreachable");
        server.join().unwrap();
    }

    // ---------------- serial_digest / serial_watch（docs/26 段階 2） ----------------

    #[test]
    fn test_digest_is_a_thin_pass_through_of_the_bridge_text() {
        // 予算も文言もブリッジの insight::render が唯一の担当。MCP 層は
        // 何も足さず何も削らない（docs/26 §2.3）
        let rendered = "digest gen=3 index=on window=all -> off 0..100
ALARM 0 err · 0 warn (exact, coverage 100%)
cursor v1:g3:o100:l4:e1
- nothing withheld";
        let mut bridge = MockBridge::new(vec![Ok(json!({
            "text": rendered,
            "data": { "cursor": "v1:g3:o100:l4:e1" },
            "meta": { "gen": 3 },
            "budget": { "max_chars": 1800, "used_chars": 120, "truncated": false }
        }))]);
        let out = call_tool(&mut bridge, "serial_digest", &json!({})).unwrap();
        assert_eq!(out, rendered);
        assert_eq!(bridge.calls[0].0, "digest");
        // 省略時のパラメータは送らない（既定はブリッジ側が持つ）
        assert_eq!(bridge.calls[0].1, json!({}));
    }

    #[test]
    fn test_digest_and_watch_forward_their_arguments() {
        let ok = || Ok(json!({ "text": "x", "data": {}, "meta": {}, "budget": {} }));
        let mut bridge = MockBridge::new(vec![ok(), ok()]);
        call_tool(
            &mut bridge,
            "serial_digest",
            &json!({ "window": "30s", "top": 20, "buckets": 6, "max_chars": 3000 }),
        )
        .unwrap();
        assert_eq!(
            bridge.calls[0].1,
            json!({ "window": "30s", "top": 20, "buckets": 6, "max_chars": 3000 })
        );
        call_tool(
            &mut bridge,
            "serial_watch",
            &json!({ "cursor": "v1:g1:o1:l1:e1", "wait_ms": 1500 }),
        )
        .unwrap();
        assert_eq!(bridge.calls[1].0, "watch");
        assert_eq!(
            bridge.calls[1].1,
            json!({ "cursor": "v1:g1:o1:l1:e1", "wait_ms": 1500 })
        );
    }

    #[test]
    fn test_digest_rejects_a_non_string_window() {
        let mut bridge = MockBridge::new(vec![]);
        let err = call_tool(&mut bridge, "serial_digest", &json!({ "window": 30 })).unwrap_err();
        assert!(err.contains("'window' must be a string"), "{err}");
    }

    #[test]
    fn test_digest_reports_a_bridge_reply_without_text() {
        // 沈黙して空文字を返すより、はっきり失敗させる
        let mut bridge = MockBridge::new(vec![Ok(json!({ "data": {} }))]);
        let err = call_tool(&mut bridge, "serial_digest", &json!({})).unwrap_err();
        assert!(err.contains("no text"), "{err}");
    }

    /// 実ソケット + 実索引スレッドを通した digest / watch の一気通し
    ///
    /// `IndexThread` -> `InsightIndex` -> ブリッジ `digest`/`watch` -> MCP ツールと、
    /// 段階 2 の全レイヤを 1 本で通す。モックでは「索引スレッドが実際に
    /// `DataStore` を読めていない」を見逃すので、ここだけは本物を使う。
    #[test]
    fn test_digest_and_watch_against_a_real_bridge_and_index() {
        use crate::bridge::{BridgeCtx, BridgeServer};
        use crate::insight::{IndexThread, InsightIndex};
        use crate::serial::data_store::DataStore;
        use std::sync::atomic::AtomicUsize;
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        let store = Arc::new(DataStore::new().expect("DataStore::new"));
        let handle = Arc::new(Mutex::new(Some(store.clone())));
        let index = Arc::new(InsightIndex::new());
        let mut indexer = IndexThread::start(handle.clone(), index.clone());

        let ctx = Arc::new(BridgeCtx::new(
            handle,
            Arc::new(Mutex::new(None)),
            None,
            Arc::new(Mutex::new(None)),
            index.clone(),
            Box::new(|_| {}),
        ));
        let server = BridgeServer::start(0, ctx, Arc::new(AtomicUsize::new(0))).expect("start");
        let mut client = TcpBridgeClient::new("127.0.0.1".to_string(), server.port(), None);

        let mut payload = String::new();
        for i in 0..500 {
            payload.push_str(&format!("T: {}.5 C\n", 20 + i % 4));
        }
        payload.push_str("E: i2c nak addr=0x48\n");
        store.push_test_data(payload.as_bytes());

        // 索引が追いつくのを待つ（実スレッドなので即時ではない）
        let deadline = Instant::now() + Duration::from_secs(5);
        while index.snapshot().lines < 501 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }

        let digest = call_tool(&mut client, "serial_digest", &json!({})).unwrap();
        assert!(digest.starts_with("digest gen="), "{digest}");
        assert!(digest.contains("index=on"), "{digest}");
        assert!(digest.contains("T: <F"), "{digest}");
        assert!(digest.contains("i2c nak addr=<X>"), "{digest}");
        assert!(digest.contains("coverage 100%"), "{digest}");
        // 501 行を索引しても、返るのは予算内の要約だけ
        assert!(digest.chars().count() <= DEFAULT_DIGEST_CHARS, "{digest}");
        assert!(!digest.contains("base64"), "{digest}");

        let cursor = digest
            .lines()
            .find_map(|l| l.strip_prefix("cursor "))
            .expect("digest must hand out a cursor")
            .to_string();

        store.push_test_data(b"W: rx overflow q=64\n");
        let deadline = Instant::now() + Duration::from_secs(5);
        while index.snapshot().lines < 502 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }

        let watch = call_tool(&mut client, "serial_watch", &json!({ "cursor": cursor })).unwrap();
        assert!(watch.starts_with("watch gen="), "{watch}");
        assert!(watch.contains("ALARM +0 err"), "{watch}");
        assert!(watch.contains("+1 warn"), "{watch}");
        // 500 行の既知テンプレートは増えていないので出ない
        assert!(!watch.contains("T: <F"), "{watch}");
        assert!(watch.contains("rx overflow"), "{watch}");

        indexer.stop();
    }
}
