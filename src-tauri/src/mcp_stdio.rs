//! MCP stdio アダプタ (`--mcp` フラグで起動)
//!
//! インストール済みのアプリだけで AI 連携が完結するよう、MCP サーバを
//! アプリ本体に内蔵する（Node.js 不要）。`serial-monitor-essential --mcp` は
//! GUI を起動せず、stdin/stdout で MCP (JSON-RPC 2.0, 行区切り) を話し、
//! ローカルの AI Bridge (bridge.rs, NDJSON TCP) へ中継する。
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
/// `serial_wait_for` のポーリング間隔
const WAIT_POLL_MS: u64 = 500;
/// `serial_wait_for` が 1 回のポーリングで読む tail 窓
const WAIT_WINDOW_BYTES: u32 = 65536;
/// この比率を超えて非印字バイトが含まれるときは hex ダンプ表示にする
const BINARY_RATIO: f64 = 0.1;
/// サポートする MCP プロトコル版（クライアント指定をエコーできる版）
const KNOWN_PROTOCOL_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];
/// クライアント指定が未知のときに名乗る版
const LATEST_PROTOCOL_VERSION: &str = "2025-06-18";

// ============================================================================
// ブリッジ接続（トレイトで抽象化: テストはモックを差す）
// ============================================================================

/// AI Bridge への 1 リクエスト（メソッド + params -> result）
pub trait Bridge {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, String>;
}

fn unreachable_message(host: &str, port: u16, detail: &str) -> String {
    format!(
        "シリアルブリッジに接続できません ({host}:{port})。\n\
         SerialMonitorEssential を起動し、設定画面で「AI Bridge」を ON にしてください。\n\
         Cannot reach the SerialMonitorEssential bridge at {host}:{port}.\n\
         Start the app and turn on \"AI Bridge\" in its settings.\n({detail})"
    )
}

fn bridge_error_message(method: &str, error: &str) -> String {
    format!(
        "ブリッジがエラーを返しました (method={method}): {error}\n\
         The bridge returned an error (method={method}): {error}"
    )
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
                        "ブリッジへの要求がタイムアウトしました (method={method}, {}ms)。\n\
                         The bridge did not answer in time (method={method}, {}ms).",
                        self.timeout.as_millis(),
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

/// テキストなら lossy UTF-8、バイナリらしければ hex ダンプ
fn render_bytes(buf: &[u8], base_offset: u64) -> (&'static str, String) {
    if buf.is_empty() {
        ("empty", "(no data)".to_string())
    } else if looks_like_text(buf) {
        ("text", String::from_utf8_lossy(buf).into_owned())
    } else {
        ("hex", hex_dump(buf, base_offset))
    }
}

fn render_chunk(buf: &[u8], offset: u64, total_bytes: Option<u64>, extra: &[String]) -> String {
    let (mode, body) = render_bytes(buf, offset);
    let mut header = vec![format!("offset={offset}"), format!("bytes={}", buf.len())];
    if let Some(total) = total_bytes {
        header.push(format!("total_bytes={total}"));
    }
    header.push(format!("rendering={mode}"));
    header.extend(extra.iter().cloned());
    format!("{}\n---\n{}", header.join("  "), body)
}

/// "48 65 6c" / "0x48,0x65" / "48:65-6C" などを受け付ける
pub fn parse_hex(input: &str) -> Result<Vec<u8>, String> {
    let no_prefix = input.replace("0x", "").replace("0X", "");
    let cleaned: String = no_prefix
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, ',' | ':' | ';' | '_' | '-'))
        .collect();
    if cleaned.is_empty() {
        return Err("hex が空です / hex string is empty".to_string());
    }
    if !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(
            "16進数として不正な文字が含まれています / hex string contains non-hex characters"
                .to_string(),
        );
    }
    if !cleaned.len().is_multiple_of(2) {
        return Err(
            "16進数は2桁ずつ (バイト単位) で指定してください / hex string must have an even number of digits"
                .to_string(),
        );
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
    pub bytes_scanned: usize,
    pub total_bytes: u64,
    pub gap: bool,
    /// `notifications/cancelled` により中断された
    pub cancelled: bool,
}

/// 呼び出し後に届いたデータへ `pattern` がマッチするまで tail をポーリングする
///
/// 「コマンドを送って応答を待つ」ための基本要素。`poll_ms` はテストから
/// 短縮できるよう引数にしてある。`cancelled` が true を返したら速やかに
/// 中断する（MCP の `notifications/cancelled` 対応）。
///
/// 正確性の要点:
/// - マッチは `regex::bytes` で**生バイト**に対して行う。lossy 変換した文字列の
///   インデックスを使うと、不正 UTF-8 バイト（1 バイト）が U+FFFD（3 バイト）に
///   置換されてオフセットが膨らみ、`serial_read_range` と噛み合わなくなる。
/// - `total_bytes` の巻き戻り（ポート再オープン / Clear の世代交代）を検知したら
///   蓄積をリセットする。放置すると cursor が新ストリームの先を指したまま
///   何も取り込めず、応答が来ているのに必ずタイムアウトする。
/// - 窓から流れ出た（gap）ときは蓄積へ**追記せず置き換える**。不連続なバイト列を
///   連結すると、境界を跨いだ偽マッチが起き、以降のオフセットも全部ずれる。
pub fn wait_for_pattern(
    bridge: &mut dyn Bridge,
    pattern: &str,
    timeout_ms: u64,
    from_end: bool,
    poll_ms: u64,
    cancelled: &dyn Fn() -> bool,
) -> Result<WaitOutcome, String> {
    // JS 実装の 'm' フラグ相当。Rust regex は lookaround 非対応（説明文に明記）。
    let regex = regex::bytes::Regex::new(&format!("(?m){pattern}"))
        .map_err(|e| format!("パターンが不正です / invalid pattern: {e}"))?;
    let started = Instant::now();

    let status = bridge.request("status", json!({}))?;
    let initial_total = status
        .get("total_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut cursor = if from_end {
        initial_total
    } else {
        initial_total.saturating_sub(DEFAULT_TAIL_BYTES as u64)
    };

    let mut acc: Vec<u8> = Vec::new();
    let mut acc_start = cursor;
    let mut total_bytes = initial_total;
    let mut gap = false;

    let unmatched =
        |acc_len: usize, total_bytes: u64, gap: bool, was_cancelled: bool| WaitOutcome {
            matched: false,
            offset: 0,
            excerpt: String::new(),
            tail: Vec::new(),
            tail_offset: 0,
            elapsed_ms: started.elapsed().as_millis() as u64,
            bytes_scanned: acc_len,
            total_bytes,
            gap,
            cancelled: was_cancelled,
        };

    loop {
        if cancelled() {
            return Ok(unmatched(acc.len(), total_bytes, gap, true));
        }

        let res = bridge.request("tail", json!({ "bytes": WAIT_WINDOW_BYTES }))?;
        let new_total = res
            .get("total_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(total_bytes);
        // 世代交代（巻き戻り）検知: 蓄積もカーソルも新ストリーム基準でやり直す
        if new_total < cursor {
            acc.clear();
            cursor = 0;
            acc_start = 0;
            gap = true;
        }
        total_bytes = new_total;

        let chunk = BASE64
            .decode(res.get("base64").and_then(Value::as_str).unwrap_or(""))
            .unwrap_or_default();
        let chunk_offset = res.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let chunk_end = chunk_offset + chunk.len() as u64;

        if chunk_end > cursor {
            if chunk_offset > cursor {
                // ポーリングの間に窓から流れ出た（取りこぼしあり）。
                // 不連続を跨いだ偽マッチとオフセット破壊を防ぐため置き換える。
                gap = true;
                acc = chunk;
                acc_start = chunk_offset;
                cursor = chunk_end;
            } else {
                let take = &chunk[(cursor - chunk_offset) as usize..];
                if !take.is_empty() {
                    if acc.is_empty() {
                        acc_start = cursor;
                    }
                    acc.extend_from_slice(take);
                    cursor = chunk_end;
                }
            }
        }

        if let Some(found) = regex.find(&acc) {
            // found.start() は生バイトのインデックス = ストリームオフセットに直結
            let from = found.start().saturating_sub(80);
            let to = (found.end() + 160).min(acc.len());
            return Ok(WaitOutcome {
                matched: true,
                offset: acc_start + found.start() as u64,
                excerpt: String::from_utf8_lossy(&acc[from..to]).into_owned(),
                tail: Vec::new(),
                tail_offset: 0,
                elapsed_ms: started.elapsed().as_millis() as u64,
                bytes_scanned: acc.len(),
                total_bytes,
                gap,
                cancelled: false,
            });
        }

        let elapsed = started.elapsed().as_millis() as u64;
        if elapsed >= timeout_ms {
            let tail_len = acc.len().min(256);
            let tail = acc[acc.len() - tail_len..].to_vec();
            let tail_offset = acc_start + (acc.len() - tail_len) as u64;
            return Ok(WaitOutcome {
                matched: false,
                offset: 0,
                excerpt: String::new(),
                tail,
                tail_offset,
                elapsed_ms: elapsed,
                bytes_scanned: acc.len(),
                total_bytes,
                gap,
                cancelled: false,
            });
        }
        std::thread::sleep(Duration::from_millis(poll_ms.min(timeout_ms - elapsed)));
    }
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
            "name": "serial_read_tail",
            "title": "Read latest serial data",
            "description": "Read the most recent bytes received from the device. Rendered as UTF-8 text, or as a hex dump when the data looks binary. Default 4096 bytes, max 1048576.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "bytes": {
                        "type": "integer", "minimum": 1, "maximum": MAX_READ_LENGTH,
                        "description": format!("How many trailing bytes to read (default {DEFAULT_TAIL_BYTES}).")
                    }
                }
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "serial_read_range",
            "title": "Read a serial byte range",
            "description": "Read an explicit byte range of the received stream (offsets match those reported by serial_read_tail and serial_wait_for). Rendered as text, or as a hex dump when the data looks binary.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "offset": { "type": "integer", "minimum": 0, "description": "Absolute start offset in the received stream." },
                    "length": { "type": "integer", "minimum": 1, "maximum": MAX_READ_LENGTH, "description": format!("How many bytes to read (max {MAX_READ_LENGTH}).") }
                },
                "required": ["offset", "length"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "serial_send",
            "title": "Send text to the device",
            "description": "Send a text command to the serial device, with an optional line ending (default \"lf\"). The GUI shows every bridge send to the human, so the operator always sees what the agent transmitted.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Text to transmit (the line ending is added by the app)." },
                    "line_ending": { "type": "string", "enum": ["none", "cr", "lf", "crlf"], "description": "Line ending appended to the text. Default \"lf\"." }
                },
                "required": ["text"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true }
        },
        {
            "name": "serial_send_hex",
            "title": "Send raw bytes to the device",
            "description": "Send raw bytes given as hex (e.g. \"01 03 00 00 00 0A\" or \"0103000000 0A\"). Separators, colons and 0x prefixes are ignored; the digit count must be even. Nothing is appended. The GUI shows every bridge send to the human.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "hex": { "type": "string", "description": "Hex byte string, e.g. \"DE AD BE EF\"." }
                },
                "required": ["hex"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true }
        },
        {
            "name": "serial_wait_for",
            "title": "Wait for a pattern",
            "description": "Wait until a regular expression (Rust regex syntax; no lookaround or backreferences) matches data received after this call started (polls every 500ms). Use it right after serial_send to capture the device reply. Returns the matching excerpt and its stream offset, or a timeout report with the last 256 bytes seen.",
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
            Ok(format!(
                "connected: {}\nport: {}\ntotal_bytes: {}\napp_version: {} (protocol {})\n\nraw: {}",
                if connected { "yes" } else { "no" },
                port,
                res.get("total_bytes").and_then(Value::as_u64).unwrap_or(0),
                res.get("app_version").and_then(Value::as_str).unwrap_or("unknown"),
                res.get("protocol").and_then(Value::as_u64).unwrap_or(0),
                res
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
            let res = bridge.request("tail", json!({ "bytes": bytes }))?;
            let buf = BASE64
                .decode(res.get("base64").and_then(Value::as_str).unwrap_or(""))
                .map_err(|_| "bridge returned invalid base64".to_string())?;
            Ok(render_chunk(
                &buf,
                res.get("offset").and_then(Value::as_u64).unwrap_or(0),
                Some(res.get("total_bytes").and_then(Value::as_u64).unwrap_or(0)),
                &[],
            ))
        }
        "serial_read_range" => {
            let offset = arg_u64(args, "offset", 0, u64::MAX)?
                .ok_or_else(|| "bad arguments: 'offset' (integer) is required".to_string())?;
            let length = arg_u64(args, "length", 1, MAX_READ_LENGTH as u64)?
                .ok_or_else(|| "bad arguments: 'length' (integer) is required".to_string())?;
            let res =
                bridge.request("read_range", json!({ "offset": offset, "length": length }))?;
            let buf = BASE64
                .decode(res.get("base64").and_then(Value::as_str).unwrap_or(""))
                .map_err(|_| "bridge returned invalid base64".to_string())?;
            let length_read = res
                .get("length_read")
                .and_then(Value::as_u64)
                .unwrap_or(buf.len() as u64);
            Ok(render_chunk(
                &buf,
                res.get("offset").and_then(Value::as_u64).unwrap_or(offset),
                None,
                &[format!("length_read={length_read}")],
            ))
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
            Ok(format!(
                "Sent {} + {} -> bytes_written={}",
                serde_json::to_string(text).unwrap_or_else(|_| text.to_string()),
                ending,
                res.get("bytes_written")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            ))
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
            Ok(format!(
                "Sent {} byte(s) [{}] -> bytes_written={}",
                buf.len(),
                spaced,
                res.get("bytes_written")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            ))
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
                WAIT_POLL_MS,
                cancelled,
            )?;
            if outcome.cancelled {
                return Ok(format!(
                    "CANCELLED  pattern={}  elapsed_ms={}  bytes_scanned={}",
                    serde_json::to_string(pattern).unwrap_or_default(),
                    outcome.elapsed_ms,
                    outcome.bytes_scanned
                ));
            }
            if outcome.matched {
                let mut head = vec![
                    "MATCH".to_string(),
                    format!(
                        "pattern={}",
                        serde_json::to_string(pattern).unwrap_or_default()
                    ),
                    format!("offset={}", outcome.offset),
                    format!("elapsed_ms={}", outcome.elapsed_ms),
                    format!("total_bytes={}", outcome.total_bytes),
                ];
                if outcome.gap {
                    head.push("warning=data_gap".to_string());
                }
                Ok(format!("{}\n---\n{}", head.join("  "), outcome.excerpt))
            } else {
                let head = [
                    "TIMEOUT".to_string(),
                    format!(
                        "pattern={}",
                        serde_json::to_string(pattern).unwrap_or_default()
                    ),
                    format!("elapsed_ms={}", outcome.elapsed_ms),
                    format!("bytes_scanned={}", outcome.bytes_scanned),
                    format!("total_bytes={}", outcome.total_bytes),
                ]
                .join("  ");
                let tail_label = if outcome.bytes_scanned == 0 {
                    "no new data arrived while waiting".to_string()
                } else {
                    format!(
                        "last {} byte(s) seen, from offset {}",
                        outcome.tail.len(),
                        outcome.tail_offset
                    )
                };
                let (_, body) = render_bytes(&outcome.tail, outcome.tail_offset);
                Ok(format!("{head}\n--- {tail_label}\n{body}"))
            }
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
    fn test_tools_list_has_all_seven() {
        let mut bridge = MockBridge::new(vec![]);
        let res = rpc(
            &mut bridge,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        );
        let tools = res["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 7);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"serial_wait_for"));
        // 全ツールにスキーマと説明がある
        for tool in tools {
            assert!(tool["inputSchema"]["type"] == json!("object"));
            assert!(!tool["description"].as_str().unwrap().is_empty());
        }
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

    // ---------------- serial_wait_for ----------------

    #[test]
    fn test_wait_for_matches_new_data_only() {
        // status: total=10 -> 呼び出し前のデータ "OLD OK" は無視される
        let mut bridge = MockBridge::new(vec![
            Ok(json!({ "total_bytes": 10 })),
            tail_response(b"OLD OK\nAT\nOK\n", 0, 13),
        ]);
        let outcome = wait_for_pattern(&mut bridge, "OK", 1000, true, 1, &|| false).unwrap();
        assert!(outcome.matched);
        // 新規部分は offset 10 の "OK\n" のみ
        assert_eq!(outcome.offset, 10);
        assert!(outcome.excerpt.contains("OK"));
        assert!(!outcome.gap);
    }

    #[test]
    fn test_wait_for_timeout_reports_tail() {
        // ポーリング回数はスケジューリング次第なので応答は余裕を持って積む
        let mut responses = vec![Ok(json!({ "total_bytes": 0 }))];
        responses.extend((0..100).map(|_| tail_response(b"partial", 0, 7)));
        let mut bridge = MockBridge::new(responses);
        let outcome = wait_for_pattern(&mut bridge, "NEVER", 30, true, 1, &|| false).unwrap();
        assert!(!outcome.matched);
        assert_eq!(outcome.tail, b"partial");
        assert_eq!(outcome.bytes_scanned, 7);
    }

    #[test]
    fn test_wait_for_detects_gap() {
        // 窓が流れて cursor より先から始まった -> gap 警告
        let mut bridge = MockBridge::new(vec![
            Ok(json!({ "total_bytes": 0 })),
            tail_response(b"...OK", 100, 105),
        ]);
        let outcome = wait_for_pattern(&mut bridge, "OK", 1000, true, 1, &|| false).unwrap();
        assert!(outcome.matched);
        assert!(outcome.gap);
        assert_eq!(outcome.offset, 103);
    }

    #[test]
    fn test_wait_for_invalid_pattern() {
        let mut bridge = MockBridge::new(vec![]);
        assert!(wait_for_pattern(&mut bridge, "([", 100, true, 1, &|| false)
            .unwrap_err()
            .contains("invalid pattern"));
    }

    #[test]
    fn test_wait_for_multiline_anchor() {
        let mut bridge = MockBridge::new(vec![
            Ok(json!({ "total_bytes": 0 })),
            tail_response(b"line1\nERROR: x\n", 0, 15),
        ]);
        let outcome = wait_for_pattern(&mut bridge, "^ERROR", 1000, true, 1, &|| false).unwrap();
        assert!(outcome.matched);
        assert_eq!(outcome.offset, 6);
    }

    #[test]
    fn test_wait_for_offset_is_byte_accurate_with_invalid_utf8() {
        // マッチ前に不正 UTF-8 バイトが 10 個: lossy 変換基準だと各 1 バイトが
        // U+FFFD (3 バイト) になり offset が +20 ずれる。生バイトマッチなら正確。
        let mut data = vec![0xffu8; 10];
        data.extend_from_slice(b"OK\r\n");
        let mut bridge = MockBridge::new(vec![
            Ok(json!({ "total_bytes": 0 })),
            tail_response(&data, 0, 14),
        ]);
        let outcome = wait_for_pattern(&mut bridge, "OK", 1000, true, 1, &|| false).unwrap();
        assert!(outcome.matched);
        assert_eq!(outcome.offset, 10, "offset must index the raw stream bytes");
    }

    #[test]
    fn test_wait_for_detects_capture_reset() {
        // 呼び出し時 total=100 → ポート再オープンで total が巻き戻る。
        // リセットを検知しないと cursor=100 のまま何も取り込めず必ずタイムアウトする。
        let mut bridge = MockBridge::new(vec![
            Ok(json!({ "total_bytes": 100 })),
            tail_response(b"BOOT OK\n", 0, 8), // 新世代: total=8 < cursor=100
        ]);
        let outcome = wait_for_pattern(&mut bridge, "BOOT OK", 1000, true, 1, &|| false).unwrap();
        assert!(outcome.matched, "reset must be detected, not time out");
        assert_eq!(outcome.offset, 0);
        assert!(outcome.gap, "reset is reported as a discontinuity");
    }

    #[test]
    fn test_wait_for_gap_replaces_accumulator() {
        // 1 回目: offset 0..5 を取り込み。2 回目: 窓が 100 まで流れた（gap）。
        // 連結すると "HELLO" + "WORLD OK" が連続に見えて跨ぎマッチやオフセット
        // 破壊が起きる。置き換え後は offset が新窓基準で正確になる。
        let mut bridge = MockBridge::new(vec![
            Ok(json!({ "total_bytes": 0 })),
            tail_response(b"HELLO", 0, 5),
            tail_response(b"WORLD OK", 100, 108),
        ]);
        let outcome = wait_for_pattern(&mut bridge, "OK", 1000, true, 1, &|| false).unwrap();
        assert!(outcome.matched);
        assert!(outcome.gap);
        assert_eq!(
            outcome.offset, 106,
            "offset must be relative to the new window"
        );

        // 跨ぎ偽マッチの検証: "LOWO" は旧末尾+新頭でしか成立しない
        let mut responses = vec![
            Ok(json!({ "total_bytes": 0 })),
            tail_response(b"HELLO", 0, 5),
        ];
        responses.extend((0..100).map(|_| tail_response(b"WORLD", 100, 105)));
        let mut bridge = MockBridge::new(responses);
        let outcome = wait_for_pattern(&mut bridge, "LOWO", 40, true, 10, &|| false).unwrap();
        assert!(
            !outcome.matched,
            "a pattern spanning the discontinuity must not match"
        );
    }

    #[test]
    fn test_wait_for_cancellation_stops_promptly() {
        use std::sync::atomic::{AtomicU32, Ordering};
        // 2 回目のポーリング以降キャンセル扱いにする
        let polls = AtomicU32::new(0);
        let cancelled = || polls.fetch_add(1, Ordering::SeqCst) >= 1;
        let mut responses = vec![Ok(json!({ "total_bytes": 0 }))];
        responses.extend((0..100).map(|_| tail_response(b"", 0, 0)));
        let mut bridge = MockBridge::new(responses);
        // タイムアウトは 60 秒だがキャンセルで即座に戻る
        let outcome = wait_for_pattern(&mut bridge, "NEVER", 60_000, true, 10, &cancelled).unwrap();
        assert!(outcome.cancelled);
        assert!(!outcome.matched);
        assert!(
            outcome.elapsed_ms < 5_000,
            "must return well before timeout"
        );
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
        assert!(err.contains("AI Bridge"));
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
}
