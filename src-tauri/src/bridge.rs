//! ブリッジ（設定画面の表示名は「MCP Server」）- ローカル専用 NDJSON TCP サーバ (protocol v1)
//!
//! OS のシリアルポートは排他オープンなので、アプリが動いている間は外部ツール
//! （AI エージェント等）が同じ COM ポートを開けない。本モジュールはアプリ自身を
//! マルチプレクサにして、**受信中のキャプチャの読み出し**と**アプリ経由の送信**を
//! ローカルホストのソケットから提供する。
//!
//! # 接続
//!
//! - バインドは **127.0.0.1 のみ**（`Ipv4Addr::LOCALHOST`）。既定ポートは 57320。
//! - 既定は **無効**。どこからも自動起動しない（`bridge_set` の明示呼び出しのみ）。
//! - 同時接続は 4 本まで。5 本目はエラー行を 1 行返して即座に閉じる。
//!
//! # プロトコル v1 (NDJSON: 1 行 1 JSON オブジェクト, UTF-8)
//!
//! リクエスト:
//! ```text
//! {"id": <number>, "method": "<name>", "params": {...}}
//! ```
//! レスポンス（1 リクエストにつき 1 行）:
//! ```text
//! {"id": n, "ok": true,  "result": {...}}
//! {"id": n, "ok": false, "error": "..."}
//! ```
//! 壊れた JSON は `{"id": null, "ok": false, "error": "parse error"}`。
//! 未知のメソッドは `unknown method: <name>` エラー。
//!
//! ## メソッド
//!
//! | method | params | result |
//! |--------|--------|--------|
//! | `help` | - | プロトコル仕様（機械可読・認証不要） |
//! | `auth` | `{token}` | `{authenticated: true}` |
//! | `status` | - | `{connected, reconnecting, port_name, total_bytes, app_version, protocol}` |
//! | `read_range` | `{offset: u64, length: u32}` | `{base64, offset, length_read}` |
//! | `tail` | `{bytes: u32}` | `{base64, offset, total_bytes}` |
//! | `subscribe` | `{from_offset: u64?}` | `{subscribed: true, from_offset}` |
//! | `send` | `{text, line_ending}` \| `{base64}` | `{bytes_written}` |
//! | `ports` | - | `{ports: [..]}` |
//!
//! - `auth` はトークンが設定されているときのみ **最初のリクエストとして必須**。
//!   未認証で他のメソッドを呼ぶと `unauthorized`。トークン未設定なら `auth` は no-op。
//!   （現状トークンは常に `None`。設定の配線は後段のレイヤで行う。）
//! - `read_range` の `length` は 1 MiB でクランプされ、さらに利用可能バイト数
//!   （`total_bytes - offset`）でもクランプされる。実際に読めた長さは `length_read`。
//!   キャプチャが無い場合は `no capture` エラー。
//! - `tail` の `bytes` は既定 4096 / 上限 1 MiB。`offset` は返却データの開始オフセット。
//! - `send` はアプリが握っているポートへ書き込む。ポート未オープンなら `port not open`。
//!   成功時は活動ログに記録し、`bridge-activity` イベント
//!   （`{kind:"send", bytes, preview}`）を GUI へ emit する（人間が AI の送信内容を
//!   見られることが要件）。
//! - `ports` は GUI の `list_ports` と同じ列挙を返す。
//!
//! ## subscribe (push モード)
//!
//! `subscribe` を受けた接続は **以降リクエストを読まない**（片方向の push 専用に
//! なる）。サーバは 50 ms 間隔で `total_bytes` を監視し、増分を 1 フレーム
//! 最大 256 KiB で送る:
//! ```text
//! {"event":"data","offset":N,"base64":"..."}
//! ```
//! DataStore の世代が変わったとき（ポート再オープン / Clear によるインスタンス
//! 差し替え、または `total_bytes` の巻き戻り）は:
//! ```text
//! {"event":"reset"}
//! ```
//! を送り、オフセットを 0 に戻す。購読を止めたい場合は接続を閉じる。

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::Serialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::serial::data_store::DataStore;
use crate::serial::tx::OpenPort;

/// 既定の待ち受けポート
pub const DEFAULT_BRIDGE_PORT: u16 = 57320;
/// プロトコルバージョン
///
/// 2: `digest` / `watch` を追加し、`status` に索引の状態を足した（docs/26 段階 2）。
/// 新しいパラメータを送らない v1 クライアントの挙動は変わらない。
pub const PROTOCOL_VERSION: u32 = 2;
/// `read_range` / `tail` の 1 回の読み出し上限 (1 MiB)
pub const MAX_READ_LENGTH: u32 = 1024 * 1024;
/// `tail` の既定バイト数
pub const DEFAULT_TAIL_BYTES: u32 = 4096;
/// 同時接続数の上限
pub const MAX_CONNECTIONS: usize = 4;
/// push モードのポーリング間隔
const SUBSCRIBE_POLL_MS: u64 = 50;
/// push モードの 1 フレーム上限 (256 KiB)
const SUBSCRIBE_MAX_FRAME: u64 = 256 * 1024;
/// accept ループ / 接続ループが停止フラグを見に行く間隔
const POLL_INTERVAL_MS: u64 = 100;
/// 1 リクエスト行の上限。base64 1 MiB でも約 1.4 MB なので十分な余裕を取る。
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

// ============================================================================
// 共有ハンドル
// ============================================================================

/// 現在の DataStore への共有ハンドル（ポート再オープン / Clear で内側が差し替わる）
pub type SharedDataStore = Arc<Mutex<Option<Arc<DataStore>>>>;
/// 現在の SerialPort への共有ハンドル
pub type SharedSerialPort = Arc<Mutex<Option<Arc<OpenPort>>>>;

/// GUI へ流す活動イベント（Tauri から切り離すためのコールバック）
pub type EmitFn = Box<dyn Fn(BridgeActivityEvent) + Send + Sync>;

/// 直近の活動（GUI 表示用）
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BridgeActivity {
    /// 種別（現状 "send" のみ）
    pub kind: String,
    /// バイト数
    pub bytes: usize,
    /// 発生時刻（Unix epoch ms）
    pub at_ms: u64,
    /// 送信内容の先頭 64 文字（lossy）。「AI の送信内容を人間が確認できる」
    /// 要件（SYS-F-1103）の中身の部分。イベントを見逃しても `bridge_status`
    /// のポーリングで復元できるよう、活動ログ側にも持たせる。
    pub preview: String,
}

/// `bridge-activity` イベントのペイロード
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BridgeActivityEvent {
    pub kind: String,
    pub bytes: usize,
    /// 送信内容の先頭 64 文字（lossy）。人間が中身を確認できるようにする。
    pub preview: String,
}

/// プロトコル処理に必要な共有ハンドル一式
///
/// Tauri の `State` はコマンドのスコープを超えて生存できない（スレッドが状態より
/// 長生きする）ため、ブリッジは `Arc` ハンドルのみを持つ。`emit` をコールバックに
/// することで、テストは `AppHandle` 無しで全プロトコルを駆動できる。
pub struct BridgeCtx {
    pub data_store: SharedDataStore,
    pub port: SharedSerialPort,
    /// 設定トークン。`Some` のときだけ `auth` が必須になる。
    pub token: Option<String>,
    pub activity: Arc<Mutex<Option<BridgeActivity>>>,
    /// 導出情報の索引。**ブリッジの所有物**であり、GUI の窓の開閉に依存しない
    /// （docs/26 §2.2）
    pub index: Arc<crate::insight::InsightIndex>,
    /// 自動再接続の待機中か（`SerialState::reconnecting` と共有）
    pub reconnecting: Arc<AtomicBool>,
    emit: EmitFn,
}

impl BridgeCtx {
    pub fn new(
        data_store: SharedDataStore,
        port: SharedSerialPort,
        token: Option<String>,
        activity: Arc<Mutex<Option<BridgeActivity>>>,
        index: Arc<crate::insight::InsightIndex>,
        emit: EmitFn,
    ) -> Self {
        Self {
            data_store,
            port,
            token,
            activity,
            index,
            reconnecting: Arc::new(AtomicBool::new(false)),
            emit,
        }
    }

    /// 自動再接続の待機フラグを GUI の状態と共有する
    pub fn with_reconnecting(mut self, reconnecting: Arc<AtomicBool>) -> Self {
        self.reconnecting = reconnecting;
        self
    }

    /// 現在の DataStore を解決する（毎回解決: 世代が差し替わるため）
    fn store(&self) -> Option<Arc<DataStore>> {
        self.data_store.lock().ok().and_then(|guard| guard.clone())
    }

    /// 現在のポートを解決する（ロックはここで手放す。送信の完了待ちの間は握らない）
    fn port_handle(&self) -> Option<Arc<OpenPort>> {
        self.port.lock().ok().and_then(|guard| guard.clone())
    }

    /// 活動を記録し、GUI へ通知する
    fn record_send(&self, bytes: usize, preview: String) {
        if let Ok(mut slot) = self.activity.lock() {
            *slot = Some(BridgeActivity {
                kind: "send".to_string(),
                bytes,
                at_ms: now_ms(),
                preview: preview.clone(),
            });
        }
        (self.emit)(BridgeActivityEvent {
            kind: "send".to_string(),
            bytes,
            preview,
        });
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ============================================================================
// リクエスト / レスポンス
// ============================================================================

/// パース済みリクエスト
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub id: Value,
    pub method: String,
    pub params: Value,
}

impl Request {
    /// テスト用の簡易コンストラクタ
    #[cfg(test)]
    pub fn new(id: i64, method: &str, params: Value) -> Self {
        Self {
            id: json!(id),
            method: method.to_string(),
            params,
        }
    }
}

/// レスポンス本体
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Ok(Value),
    Err(String),
}

/// 1 行分のレスポンス
#[derive(Clone, Debug, PartialEq)]
pub struct Response {
    pub id: Value,
    pub outcome: Outcome,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            id,
            outcome: Outcome::Ok(result),
        }
    }

    pub fn error(id: Value, message: impl Into<String>) -> Self {
        Self {
            id,
            outcome: Outcome::Err(message.into()),
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self.outcome, Outcome::Ok(_))
    }

    pub fn into_json(self) -> Value {
        match self.outcome {
            Outcome::Ok(result) => json!({ "id": self.id, "ok": true, "result": result }),
            Outcome::Err(error) => json!({ "id": self.id, "ok": false, "error": error }),
        }
    }
}

/// 1 接続分のセッション状態
#[derive(Clone, Debug)]
pub struct Session {
    /// 認証済みか（トークン未設定なら最初から true）
    pub authed: bool,
}

impl Session {
    pub fn new(token_required: bool) -> Self {
        Self {
            authed: !token_required,
        }
    }
}

/// NDJSON の 1 行をリクエストへ変換する
///
/// 失敗時は「そのまま返せるエラーレスポンス」を返す。
pub fn parse_request(line: &str) -> Result<Request, Response> {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Err(Response::error(Value::Null, "parse error")),
    };
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let method = match value.get("method").and_then(Value::as_str) {
        Some(m) => m.to_string(),
        None => return Err(Response::error(id, "parse error")),
    };
    let params = value.get("params").cloned().unwrap_or(Value::Null);
    Ok(Request { id, method, params })
}

// ============================================================================
// パラメータ取得 / 純粋ヘルパ
// ============================================================================

fn param_u64(params: &Value, key: &str) -> Option<u64> {
    params.get(key).and_then(Value::as_u64)
}

fn param_u32(params: &Value, key: &str) -> Option<u32> {
    param_u64(params, key).and_then(|v| u32::try_from(v).ok())
}

/// 実際に読み出す長さを決める: 要求長を 1 MiB と残量でクランプする
pub fn clamp_read_length(requested: u32, available: u64) -> u32 {
    (requested.min(MAX_READ_LENGTH) as u64).min(available) as u32
}

/// `send` のペイロードを組み立てる（純粋関数: 単体テスト用に分離）
///
/// `base64` が指定されていればそれを優先し、無ければ `text` + `line_ending`。
pub fn build_send_payload(params: &Value) -> Result<Vec<u8>, String> {
    if let Some(raw) = params.get("base64") {
        if !raw.is_null() {
            let encoded = raw
                .as_str()
                .ok_or_else(|| "bad params: base64 must be a string".to_string())?;
            return BASE64
                .decode(encoded)
                .map_err(|_| "bad params: invalid base64".to_string());
        }
    }

    let text = params
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| "bad params: text or base64 required".to_string())?;

    let ending = match params.get("line_ending") {
        None | Some(Value::Null) => "none",
        Some(v) => v
            .as_str()
            .ok_or_else(|| "bad params: line_ending must be a string".to_string())?,
    };
    let suffix: &[u8] = match ending {
        "none" => b"",
        "cr" => b"\r",
        "lf" => b"\n",
        "crlf" => b"\r\n",
        other => return Err(format!("bad params: unknown line_ending '{}'", other)),
    };

    let mut bytes = text.as_bytes().to_vec();
    bytes.extend_from_slice(suffix);
    Ok(bytes)
}

/// 送信内容のプレビュー（先頭 64 文字, lossy）
pub fn preview_of(data: &[u8]) -> String {
    String::from_utf8_lossy(data).chars().take(64).collect()
}

// ============================================================================
// プロトコル処理（IO から独立）
// ============================================================================

/// 1 リクエストを処理する
///
/// IO には一切触れないので、テストは `BridgeCtx` を直接組み立てて全メソッドを
/// 検証できる。`subscribe` はここでは受理応答を返すだけで、実際の push は
/// IO 層（[`run_subscription`]）が行う。
pub fn handle_request(req: &Request, ctx: &BridgeCtx, session: &mut Session) -> Response {
    let id = req.id.clone();

    // help は自己記述のための発見用メソッド: データを一切含まないので認証不要
    if req.method == "help" {
        return Response::ok(id, method_help());
    }

    // 認証ゲート: トークン設定時は auth が最初の必須リクエスト
    if req.method == "auth" {
        return match &ctx.token {
            None => {
                session.authed = true;
                Response::ok(id, json!({ "authenticated": true }))
            }
            Some(expected) => match req.params.get("token").and_then(Value::as_str) {
                Some(got) if got == expected => {
                    session.authed = true;
                    Response::ok(id, json!({ "authenticated": true }))
                }
                _ => Response::error(id, "invalid token"),
            },
        };
    }
    if !session.authed {
        return Response::error(id, "unauthorized");
    }

    match req.method.as_str() {
        "status" => Response::ok(id, method_status(ctx)),
        "read_range" => match method_read_range(ctx, &req.params) {
            Ok(v) => Response::ok(id, v),
            Err(e) => Response::error(id, e),
        },
        "tail" => match method_tail(ctx, &req.params) {
            Ok(v) => Response::ok(id, v),
            Err(e) => Response::error(id, e),
        },
        "find" => match method_find(ctx, &req.params) {
            Ok(v) => Response::ok(id, v),
            Err(e) => Response::error(id, e),
        },
        "digest" => match method_digest(ctx, &req.params) {
            Ok(v) => Response::ok(id, v),
            Err(e) => Response::error(id, e),
        },
        "watch" => match method_watch(ctx, &req.params) {
            Ok(v) => Response::ok(id, v),
            Err(e) => Response::error(id, e),
        },
        "sample" => match method_sample(ctx, &req.params) {
            Ok(v) => Response::ok(id, v),
            Err(e) => Response::error(id, e),
        },
        "wait_for" => match method_wait_for(ctx, &req.params) {
            Ok(v) => Response::ok(id, v),
            Err(e) => Response::error(id, e),
        },
        "send" => match method_send(ctx, &req.params) {
            Ok(v) => Response::ok(id, v),
            Err(e) => Response::error(id, e),
        },
        "ports" => Response::ok(id, json!({ "ports": crate::serial::enumerate_ports() })),
        "subscribe" => match method_subscribe(&req.params) {
            Ok(v) => Response::ok(id, v),
            Err(e) => Response::error(id, e),
        },
        other => Response::error(id, format!("unknown method: {}", other)),
    }
}

/// `help`: プロトコル仕様を機械可読で返す
///
/// MCP を使わず生 TCP で繋ぐ AI エージェントが、この 1 メソッドだけで
/// 使い方を自己取得できるようにする（認証不要・データ非含有）。
fn method_help() -> Value {
    json!({
        "name": "serial-monitor-essential bridge",
        "protocol": PROTOCOL_VERSION,
        "app_version": env!("CARGO_PKG_VERSION"),
        "transport": "NDJSON over TCP (127.0.0.1 only). One JSON object per line. Request: {\"id\":n,\"method\":\"...\",\"params\":{...}}. Response: {\"id\":n,\"ok\":true,\"result\":{...}} or {\"id\":n,\"ok\":false,\"error\":\"...\"}.",
        "auth": "Only required when a token is configured; then 'auth' {token} must be the first request. 'help' never requires auth.",
        "methods": {
            "help": { "params": {}, "result": "this document" },
            "auth": { "params": { "token": "string" }, "result": { "authenticated": true } },
            "status": { "params": {}, "result": { "connected": "bool", "reconnecting": "bool (the device went away and the app reopens it when it returns; the capture continues at the same offsets, no reset)", "port_name": "string|null", "total_bytes": "u64", "app_version": "string", "protocol": "u32" } },
            "read_range": { "params": { "offset": "u64", "length": "u32 (clamped to 1 MiB and to available bytes)" }, "result": { "base64": "string", "offset": "u64", "length_read": "u32" } },
            "tail": { "params": { "bytes": "u32 (default 4096, max 1 MiB)" }, "result": { "base64": "string", "offset": "u64 (stream offset of the returned data)", "total_bytes": "u64" } },
            "send": { "params": "either {text, line_ending: none|cr|lf|crlf} or {base64}", "result": { "bytes_written": "usize", "offset_before": "u64 (stream offset when the app started writing; pass it to wait_for as 'from' to catch a reply that lands immediately)" }, "note": "writes to the port the app has open; every send is shown to the human in the GUI. Reception keeps running while it writes. With hardware flow control the peer may throttle, so a large send can take long; it fails only if no byte is accepted for 3 s (the error says how many bytes went out). Do not resend on your own timeout: the bytes may already be on the wire." },
            "find": { "params": { "pattern": "string (Rust regex, multi-line; no lookaround)", "from": "u64 (optional, default 0)", "to": "u64 (optional, default end of stream)", "max_hits": format!("usize (optional, default {FIND_DEFAULT_HITS}, max {FIND_MAX_HITS})") }, "result": { "matches": "u64", "exact": "bool (false when the scan was cut short or skipped unreadable bytes; then 'matches' is a LOWER BOUND)", "complete": "bool", "hits": [{ "offset": "u64", "len": "usize", "excerpt": "string (the matching line)", "excerpt_truncated": "bool" }], "first": "u64|null", "last": "u64|null", "range_start": "u64", "range_end": "u64", "bytes_scanned": "u64", "gap_bytes": "u64", "elapsed_ms": "u64" }, "note": format!("Scans the capture server-side and returns counts plus a few offsets, never the bulk data. Gives up after {FIND_DEADLINE_MS}ms with complete=false rather than timing out.") },
            "digest": { "params": { "window": "string (optional: \"all\" (default), a duration like \"30s\"/\"5m\", or \"off:FROM..TO\")", "template": "string|u32 (optional: when given, returns the detail of that one template instead of the overview)", "top": "usize (optional)", "samples": "usize (optional)", "buckets": "usize (optional, 0 = none)", "max_chars": "usize (optional)" }, "result": { "text": "rendered summary (already inside the character budget)", "data": "structured equivalent", "meta": { "gen": "u64", "exact": "bool", "coverage_pct": "u64", "index_lag_bytes": "u64" }, "budget": { "max_chars": "usize", "used_chars": "usize", "truncated": "bool", "next": "string" } }, "note": "Summarises the capture from the index; never reads the bulk data. Cost does not grow with the arrival rate." },
            "watch": { "params": { "cursor": "string (optional, from a previous digest/watch)", "wait_ms": format!("u64 (optional, server-side long poll, max {WATCH_MAX_WAIT_MS})"), "top": "usize (optional)", "max_chars": "usize (optional)" }, "result": "same envelope as 'digest', reporting only what changed since the cursor", "note": "A stale-generation cursor is not an error: the reply says the generation changed and where it restarted." },
            "wait_for": { "params": { "pattern": "string (Rust regex, multi-line)", "from": "u64 (offset to scan forward from)", "timeout_ms": format!("u64 (how long this call may block, max {WAIT_SERVER_MAX_MS})") }, "result": { "matched": "bool", "offset": "u64|null", "match_text": "string", "excerpt": "string", "next_from": "u64 (pass back to resume; already includes the overlap)", "scanned_to": "u64", "reset": "bool (the capture generation rolled back)", "tail_base64": "string (last 256 new bytes, for a timeout report)" }, "note": "Scans the capture forward server-side, so there is no polling window to fall behind. Call repeatedly with 'next_from' until it matches or your own deadline passes." },
            "sample": { "params": { "mode": "string (tail|head|around|stratified|by_template, default tail)", "n": format!("usize (optional, default {SAMPLE_DEFAULT_N}, max {SAMPLE_MAX_N})"), "offset": "u64 (mode=around)", "template": "string|u32 (mode=by_template)", "collapse_repeats": "bool (default true)", "max_chars": "usize (optional)" }, "result": "same envelope as 'digest'", "note": "Raw lines, capped and de-duplicated, each with the offset it starts at. Partial lines at the window edges are dropped rather than shown as if they were whole." },
            "ports": { "params": {}, "result": { "ports": ["string"] } },
            "subscribe": { "params": { "from_offset": "u64 (optional)" }, "result": { "subscribed": true }, "note": "after this the connection becomes push-only: frames {event:'data',offset,base64} every <=50ms, {event:'reset'} when the capture restarts; close the socket to stop" }
        },
        "limits": {
            "max_read_length": MAX_READ_LENGTH,
            "default_tail_bytes": DEFAULT_TAIL_BYTES,
            "max_connections": MAX_CONNECTIONS,
            "subscribe_max_frame": SUBSCRIBE_MAX_FRAME,
        },
        "hints": [
            "Poll 'tail' (or use 'subscribe') to follow live data; offsets are absolute stream offsets.",
            "Typical debug flow: status -> send -> poll tail until the reply pattern appears.",
            "To search a large capture, use 'find' instead of pulling the bytes: it scans server-side and answers with counts and offsets, then you fetch only the interesting offset with 'read_range'.",
            "To understand a capture, call 'digest' once, then follow it with 'watch' + the cursor it returned: both cost a fixed number of characters no matter how fast the device talks.",
            "An MCP stdio adapter with higher-level tools is built into the app binary: run it with --mcp.",
        ],
    })
}

fn method_status(ctx: &BridgeCtx) -> Value {
    let (connected, port_name) = match ctx.port_handle() {
        // 切断（致命エラー）後はハンドルが残っていても connected=false（DEBT-1 の形を再発させない）
        Some(port) => (port.is_connected(), Some(port.name().to_string())),
        None => (false, None),
    };
    let total_bytes = ctx.store().map(|s| s.total_bytes()).unwrap_or(0);
    let snap = ctx.index.snapshot();

    json!({
        "connected": connected,
        // デバイスが消え、戻ったら同じキャプチャに続けて受信する待機中
        "reconnecting": !connected && ctx.reconnecting.load(Ordering::Relaxed),
        "port_name": port_name,
        "total_bytes": total_bytes,
        "app_version": env!("CARGO_PKG_VERSION"),
        "protocol": PROTOCOL_VERSION,
        // 索引の状態。'digest'/'watch' の答えをどこまで信じてよいかの根拠
        "gen": snap.gen,
        "index_running": snap.running,
        "indexed_bytes": snap.bytes_ingested,
        "indexed_lines": snap.lines,
        "index_lag_bytes": snap.lag_bytes(),
        "coverage_pct": snap.coverage_pct(),
    })
}

fn method_read_range(ctx: &BridgeCtx, params: &Value) -> Result<Value, String> {
    let offset = param_u64(params, "offset").ok_or_else(|| "bad params: offset".to_string())?;
    let length = param_u32(params, "length").ok_or_else(|| "bad params: length".to_string())?;

    let store = ctx.store().ok_or_else(|| "no capture".to_string())?;
    let total = store.total_bytes();
    let to_read = clamp_read_length(length, total.saturating_sub(offset));

    let data = if to_read == 0 {
        Vec::new()
    } else {
        store.get_data(offset, to_read)?
    };

    Ok(json!({
        "base64": BASE64.encode(&data),
        "offset": offset,
        "length_read": data.len(),
    }))
}

/// `find` の走査に費やしてよい時間
///
/// クライアント側のリクエストタイムアウト（`mcp_stdio::REQUEST_TIMEOUT_MS`
/// = 5000ms）より十分手前で切り上げ、打ち切ったら `complete=false` として
/// 返す。無言でタイムアウトさせて「見つからなかった」と誤解させない。
const FIND_DEADLINE_MS: u64 = 3000;
/// `find` が返すヒットの既定件数 / 上限
const FIND_DEFAULT_HITS: usize = 20;
const FIND_MAX_HITS: usize = 200;

/// `DataStore` を走査ロジックへ渡すためのアダプタ
struct StoreSource(Arc<DataStore>);

impl crate::insight::find::ByteSource for StoreSource {
    fn total_bytes(&self) -> u64 {
        self.0.total_bytes()
    }
    fn read(&self, offset: u64, len: u32) -> Result<Vec<u8>, String> {
        self.0.get_data(offset, len)
    }
}

/// `find`: ストリーム全体を正規表現で走査し、件数とハンドルを返す
///
/// 生バイトは返さない。返すのは件数・先頭/末尾オフセット・少数の抜粋で、
/// 実データが要るときは呼び出し側が `read_range` で 1 点だけ買う。
fn method_find(ctx: &BridgeCtx, params: &Value) -> Result<Value, String> {
    let pattern = params
        .get("pattern")
        .and_then(Value::as_str)
        .ok_or_else(|| "bad params: pattern".to_string())?;
    let regex = regex::bytes::Regex::new(&format!("(?m){pattern}"))
        .map_err(|e| format!("invalid pattern: {e}"))?;

    let store = ctx.store().ok_or_else(|| "no capture".to_string())?;
    let total = store.total_bytes();
    let from = param_u64(params, "from").unwrap_or(0);
    let to = params
        .get("to")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX)
        .min(total);
    let max_hits = params
        .get("max_hits")
        .and_then(Value::as_u64)
        .map(|n| (n as usize).min(FIND_MAX_HITS))
        .unwrap_or(FIND_DEFAULT_HITS);

    let started = Instant::now();
    let deadline = Duration::from_millis(FIND_DEADLINE_MS);
    let src = StoreSource(store);
    let result = crate::insight::find::find_matches(
        &src,
        &crate::insight::find::FindOptions {
            regex: &regex,
            from,
            to,
            max_hits,
        },
        &|| started.elapsed() >= deadline,
    );

    let hits: Vec<Value> = result
        .hits
        .iter()
        .map(|h| {
            json!({
                "offset": h.offset,
                "len": h.len,
                "excerpt": h.excerpt,
                "excerpt_truncated": h.excerpt_truncated,
            })
        })
        .collect();

    Ok(json!({
        "matches": result.matches,
        "exact": result.is_exact(),
        "complete": result.complete,
        "hits": hits,
        "first": result.first,
        "last": result.last,
        "range_start": result.range_start,
        "range_end": result.range_end,
        "bytes_scanned": result.bytes_scanned,
        "gap_bytes": result.gap_bytes,
        "total_bytes": total,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    }))
}

// ============================================================================
// 索引に基づく導出情報: digest / watch (docs/26 段階 2)
// ============================================================================

/// `digest` の TOP に載せる既定 / 上限件数
const DIGEST_DEFAULT_TOP: usize = 8;
const DIGEST_MAX_TOP: usize = 50;
/// `digest` の SAMPLE 行数
const DIGEST_DEFAULT_SAMPLES: usize = 3;
/// `digest` の時系列バケット数の上限
const DIGEST_MAX_BUCKETS: usize = 24;
/// `watch` のサーバ側待機上限
///
/// MCP クライアントの 1 リクエストタイムアウト（5000ms）より十分手前で
/// 切り上げる。ここを超えて待つと「ブリッジが答えない」という誤った案内になる。
const WATCH_MAX_WAIT_MS: u64 = 3000;
const WATCH_POLL_MS: u64 = 20;

fn param_usize(params: &Value, key: &str) -> Option<usize> {
    params.get(key).and_then(Value::as_u64).map(|v| v as usize)
}

/// `DataStore` の時刻索引を純粋なウィンドウ解決へ渡すアダプタ
struct StoreTime(Option<Arc<DataStore>>);

impl crate::insight::window::TimeIndex for StoreTime {
    fn bounds(&self) -> Option<(u64, u64)> {
        self.0.as_ref()?.timestamp_bounds()
    }
    fn offset_at_or_after(&self, ts: u64) -> Option<u64> {
        self.0.as_ref()?.offset_at_or_after_timestamp(ts)
    }
}

/// 1 テンプレートの表示単位（全期間の統計 + このウィンドウでの件数）
struct Entry {
    stat: crate::insight::index::TemplateStat,
    count: u64,
    is_new: bool,
}

/// 1 テンプレートを 1 行にする
///
/// `lifetime` は「この行の統計が索引全体のものか」。差分（`watch` や窓つき
/// `digest`）では first / min / max は**窓の外**を指しうるので出さない。
/// 窓の件数の隣に索引全体の極値を並べるのは、docs/26 が禁じている
/// 「要約が事象を隠す」の一種なので、書けない事実は書かない。
/// 初出テンプレートだけは例外で、全期間 = その窓なのでそのまま出せる。
///
/// docs/26 §6.2-4 のとおり、**出す極値には必ずバイトオフセットを併記する**。
fn format_entry(entry: &Entry, lifetime: bool) -> String {
    use crate::insight::render::{commas, number, template_with_slots};

    let stat = &entry.stat;
    let scoped = lifetime || entry.is_new;
    let count = if lifetime {
        format!("x{}", commas(entry.count))
    } else {
        format!("+{}", commas(entry.count))
    };
    let template = if scoped {
        template_with_slots(&stat.template, &stat.slots)
    } else {
        stat.template.clone()
    };
    let mut line = if scoped {
        format!(
            "t{} {} \"{}\" @off {}..{}",
            stat.id,
            count,
            template,
            commas(stat.first_offset),
            commas(stat.last_offset)
        )
    } else {
        format!(
            "t{} {} \"{}\" last @off={}",
            stat.id,
            count,
            template,
            commas(stat.last_offset)
        )
    };
    if !scoped {
        return line;
    }
    let numeric: Vec<_> = stat.slots.iter().filter(|s| s.numeric).collect();
    if let Some(slot) = numeric.first() {
        if slot.min != slot.max {
            line.push_str(&format!(
                " min {}@off={} max {}@off={}",
                number(slot.min),
                commas(slot.min_offset),
                number(slot.max),
                commas(slot.max_offset),
            ));
        }
        if numeric.len() > 1 {
            line.push_str(&format!(" (+{} more numeric)", numeric.len() - 1));
        }
    }
    line
}

/// 件数の確度を必ず言葉にする
///
/// docs/26 §6.2-5「裸のゼロを禁じる」。`err=0` 単独は不正な出力であり、
/// 常にカバレッジと欠落を添える。
fn accuracy_note(snap: &crate::insight::index::Snapshot) -> String {
    let coverage = snap.coverage_pct();
    if snap.is_exact() {
        return format!("exact, coverage {coverage}%");
    }
    let mut why = Vec::new();
    if snap.lag_bytes() > 0 {
        why.push(format!(
            "index {} behind",
            crate::insight::render::human_bytes(snap.lag_bytes())
        ));
    }
    if snap.gap_bytes > 0 {
        why.push(format!(
            "{} unreadable",
            crate::insight::render::human_bytes(snap.gap_bytes)
        ));
    }
    format!("AT LEAST - coverage {coverage}%, {}", why.join(", "))
}

/// 索引の健康状態。異常が無ければ何も言わない（空の行で予算を食わない）
fn index_health_lines(snap: &crate::insight::index::Snapshot) -> Vec<String> {
    use crate::insight::render::{commas, human_bytes};
    let mut notes = Vec::new();
    if !snap.running {
        notes.push("not running (turn on MCP Server to index)".to_string());
    }
    if !snap.looks_line_oriented() {
        // テンプレートと行数はこの場合まったく意味を持たない。数字だけ並べると
        // バイナリのストリームに対して自信満々に嘘をつくことになる
        notes.push(format!(
            "stream does not look line oriented ({:.1}% control bytes): the line counts and \
             templates below are NOT meaningful. Use serial_find on byte patterns and \
             serial_read_range(format=\"hex\") instead",
            snap.control_ratio() * 100.0
        ));
    }
    if snap.lag_bytes() > 0 {
        notes.push(format!(
            "{} behind the capture",
            human_bytes(snap.lag_bytes())
        ));
    }
    if snap.gaps > 0 {
        notes.push(format!(
            "{} gap(s), {} unreadable",
            snap.gaps,
            human_bytes(snap.gap_bytes)
        ));
    }
    if snap.evicted_templates > 0 {
        notes.push(format!(
            "evicted {} template(s) holding {} line(s)",
            commas(snap.evicted_templates),
            commas(snap.evicted_lines)
        ));
    }
    if notes.is_empty() {
        Vec::new()
    } else {
        vec![format!("INDEX {}", notes.join(" · "))]
    }
}

/// NEW / RARE / TOP を組み立てる
///
/// docs/26 §6.2-2「稀少イベントの床」: top-N を出す**前に**、初出のものと
/// 件数 <= [`RARE_THRESHOLD`](crate::insight::render::RARE_THRESHOLD) のものを
/// 正確な件数とオフセット付きで必ず列挙する。
fn template_blocks(
    entries: &[Entry],
    top: usize,
    lifetime: bool,
) -> Vec<crate::insight::render::Block> {
    use crate::insight::render::{Block, Section, RARE_LIMIT, RARE_THRESHOLD};

    let mut blocks = Vec::new();
    let scope = if lifetime {
        format!("count<={RARE_THRESHOLD}")
    } else {
        format!("+{RARE_THRESHOLD} or fewer since the cursor")
    };

    let new: Vec<&Entry> = entries.iter().filter(|e| e.is_new).collect();
    if !new.is_empty() {
        let mut lines = vec![format!("NEW   {} template(s) first seen here", new.len())];
        lines.extend(
            new.iter()
                .map(|e| format!("      {}", format_entry(e, lifetime))),
        );
        blocks.push(Block::new(Section::New, lines));
    }

    let rare: Vec<&Entry> = entries
        .iter()
        .filter(|e| !e.is_new && e.count <= RARE_THRESHOLD)
        .collect();
    if !rare.is_empty() {
        let shown = rare.len().min(RARE_LIMIT);
        let head = if shown == rare.len() {
            format!("RARE  {} template(s) with {scope} (all shown)", rare.len())
        } else {
            // 黙って短くしない。全件を出す呼び出しを添える
            format!(
                "RARE  {shown} of {} template(s) with {scope} shown; narrow window= to see the rest",
                rare.len()
            )
        };
        let mut lines = vec![head];
        lines.extend(
            rare.iter()
                .take(shown)
                .map(|e| format!("      {}", format_entry(e, lifetime))),
        );
        blocks.push(Block::new(Section::Rare, lines));
    }

    let common: Vec<&Entry> = entries
        .iter()
        .filter(|e| !e.is_new && e.count > RARE_THRESHOLD)
        .collect();
    if !common.is_empty() {
        let shown = common.len().min(top);
        let label = if lifetime { "TOP  " } else { "DELTA" };
        let head = if shown == common.len() {
            format!("{label} {} template(s) (all shown)", common.len())
        } else {
            format!("{label} {shown} of {} template(s) by count", common.len())
        };
        let mut lines = vec![head];
        lines.extend(
            common
                .iter()
                .take(shown)
                .map(|e| format!("      {}", format_entry(e, lifetime))),
        );
        blocks.push(Block::new(Section::Top, lines));
    }
    blocks
}

/// チェックポイント環から時系列バケットを作る
///
/// docs/26 §6.2-6 に従い、平均でスパイクを消さないよう**件数の実測差分**を
/// そのまま出す。素材が足りなければ黙って作らない（`None`）。
fn timeline_lines(
    checkpoints: &[(u64, u64, u64, u64, u64)],
    start: u64,
    end: u64,
    buckets: usize,
) -> Option<Vec<String>> {
    use crate::insight::render::commas;
    if buckets == 0 || end <= start {
        return None;
    }
    let points: Vec<&(u64, u64, u64, u64, u64)> = checkpoints
        .iter()
        .filter(|c| c.0 >= start && c.0 <= end)
        .collect();
    if points.len() < 2 {
        return None;
    }
    let span = end - start;
    let mut lines = vec![0u64; buckets];
    let mut errs = vec![0u64; buckets];
    let mut warns = vec![0u64; buckets];
    for pair in points.windows(2) {
        let (prev, cur) = (pair[0], pair[1]);
        let bucket = (((cur.0 - start) as u128 * buckets as u128) / span as u128) as usize;
        let bucket = bucket.min(buckets - 1);
        lines[bucket] += cur.2.saturating_sub(prev.2);
        errs[bucket] += cur.3.saturating_sub(prev.3);
        warns[bucket] += cur.4.saturating_sub(prev.4);
    }
    let join = |v: &[u64]| v.iter().map(|n| commas(*n)).collect::<Vec<_>>().join("|");
    // どこまでを素材にできたかを必ず言う。チェックポイント環は有界なので、
    // 長い窓では前半に素材が無い。それを黙って 0 として出すと
    // 「その時間帯は静かだった」という嘘になる。
    let covered_from = points.first().map(|p| p.0).unwrap_or(start);
    Some(vec![
        format!(
            "TIME  {buckets} bucket(s) of {} bytes over off {}..{} (from {} checkpoint(s) covering off {}..{})",
            commas(span / buckets as u64),
            commas(start),
            commas(end),
            points.len(),
            commas(covered_from),
            commas(points.last().map(|p| p.0).unwrap_or(end))
        ),
        format!("      lines {}", join(&lines)),
        format!("      err {} · warn {}", join(&errs), join(&warns)),
    ])
}

fn entries_to_json(entries: &[Entry], limit: usize) -> Vec<Value> {
    entries
        .iter()
        .take(limit)
        .map(|e| {
            json!({
                "id": e.stat.id,
                "template": e.stat.template,
                "count": e.count,
                "total_count": e.stat.count,
                "first_offset": e.stat.first_offset,
                "last_offset": e.stat.last_offset,
                "is_new": e.is_new,
            })
        })
        .collect()
}

/// 応答エンベロープ（docs/26 §2.3）
fn insight_envelope(
    rendered: crate::insight::render::Rendered,
    data: Value,
    meta: Value,
    max_chars: usize,
    next: Option<String>,
) -> Value {
    json!({
        "text": rendered.text,
        "data": data,
        "meta": meta,
        "budget": {
            "max_chars": max_chars,
            "used_chars": rendered.used_chars,
            "truncated": rendered.truncated,
            // 落とした行数をセクション別に。フッタの文と同じ内容を機械可読で
            "withheld": rendered
                .withheld
                .iter()
                .map(|(section, n)| json!({ "section": section.label(), "lines": n }))
                .collect::<Vec<Value>>(),
            "next": next,
        },
    })
}

/// `digest`: 1 コールで全体を把握する
///
/// `get_data` を 1 回も呼ばない。応答は索引の写しだけから作られるので、
/// コストは受信レートに依存しない。
fn method_digest(ctx: &BridgeCtx, params: &Value) -> Result<Value, String> {
    use crate::insight::render::{self, commas, human_bytes, human_ms, human_rate, Block, Section};
    use crate::insight::window::{parse_window, resolve_window, Cursor, WindowSpec};

    let spec = parse_window(
        params
            .get("window")
            .and_then(Value::as_str)
            .unwrap_or("all"),
    )?;
    let max_chars = param_usize(params, "max_chars").unwrap_or(render::DEFAULT_DIGEST_CHARS);
    let top = param_usize(params, "top")
        .unwrap_or(DIGEST_DEFAULT_TOP)
        .min(DIGEST_MAX_TOP);
    let samples = param_usize(params, "samples")
        .unwrap_or(DIGEST_DEFAULT_SAMPLES)
        .min(crate::insight::index::RECENT_LINES);
    let buckets = param_usize(params, "buckets")
        .unwrap_or(0)
        .min(DIGEST_MAX_BUCKETS);

    // 写しとカーソルは同じロックで取る。ここがずれると、次の `watch` が
    // 「基準点が無い」と言うか、間の行をどちらの応答にも載せなくなる
    let (snap, cursor) = ctx.index.snapshot_with_checkpoint(now_ms());
    let time = StoreTime(ctx.store());
    let window = resolve_window(&spec, snap.start_offset, snap.indexed_up_to, &time)?;

    if let Some(id) = param_template_id(params)? {
        return digest_detail(ctx, &snap, id, max_chars);
    }

    let whole = matches!(spec, WindowSpec::All);
    let (entries, lines, bytes, err, warn, elapsed_ms, baseline) = if whole {
        let entries: Vec<Entry> = snap
            .templates
            .iter()
            .map(|s| Entry {
                stat: s.clone(),
                count: s.count,
                is_new: false,
            })
            .collect();
        (
            entries,
            snap.lines,
            snap.bytes_ingested,
            snap.err,
            snap.warn,
            snap.last_ingest_ms.saturating_sub(snap.first_ingest_ms),
            None,
        )
    } else {
        let at = Cursor {
            gen: snap.gen,
            offset: window.start,
            lines: 0,
            epoch_ms: 0,
        };
        let delta = ctx.index.delta_since(Some(&at), now_ms());
        let entries: Vec<Entry> = delta
            .changed
            .iter()
            .map(|(stat, count, is_new)| Entry {
                stat: stat.clone(),
                count: *count,
                is_new: *is_new,
            })
            .collect();
        let baseline = if delta.exact_baseline {
            None
        } else {
            Some(format!(
                "NOTE  counts measured from off {} (window starts at {}); no checkpoint at the exact offset",
                commas(delta.from_offset),
                commas(window.start)
            ))
        };
        (
            entries,
            delta.to_lines.saturating_sub(delta.from_lines),
            window.end.saturating_sub(delta.from_offset),
            delta.err,
            delta.warn,
            delta.elapsed_ms,
            baseline,
        )
    };

    let mut blocks = vec![Block::new(
        Section::Header,
        vec![
            format!(
                "digest gen={} index={} {}",
                snap.gen,
                if snap.running { "on" } else { "off" },
                window.label
            ),
            format!(
                "{} lines · {} · {} · {}",
                commas(lines),
                human_bytes(bytes),
                human_ms(elapsed_ms),
                human_rate(bytes, elapsed_ms)
            ),
        ],
    )];

    let mut health = index_health_lines(&snap);
    if let Some(note) = baseline {
        health.push(note);
    }
    if !health.is_empty() {
        blocks.push(Block::new(Section::IndexHealth, health));
    }

    blocks.push(Block::new(
        Section::Alarm,
        vec![format!(
            "ALARM {} err · {} warn ({})",
            commas(err),
            commas(warn),
            accuracy_note(&snap)
        )],
    ));
    blocks.extend(template_blocks(&entries, top, whole));

    if let Some(lines) = timeline_lines(&snap.checkpoints, window.start, window.end, buckets) {
        blocks.push(Block::new(Section::Timeline, lines));
    }
    if samples > 0 && !snap.recent.is_empty() {
        let lines: Vec<String> = snap
            .recent
            .iter()
            .rev()
            .take(samples)
            .map(|(offset, text)| format!("LAST  @off={} {}", commas(*offset), text))
            .collect();
        blocks.push(Block::new(Section::Samples, lines));
    }
    blocks.push(Block::new(
        Section::Cursor,
        vec![format!("cursor {}", cursor.encode())],
    ));

    let next = format!(
        "serial_digest(window={:?}, top={}, max_chars={})",
        params
            .get("window")
            .and_then(Value::as_str)
            .unwrap_or("all"),
        (top * 2).min(DIGEST_MAX_TOP),
        (max_chars * 2).min(render::MAX_MAX_CHARS)
    );
    let rendered = render::render(blocks, Some(&next), max_chars);

    let data = json!({
        "gen": snap.gen,
        "window_start": window.start,
        "window_end": window.end,
        "lines": lines,
        "bytes": bytes,
        "err": err,
        "warn": warn,
        "templates": entries_to_json(&entries, top.max(crate::insight::render::RARE_LIMIT)),
        "cursor": cursor.encode(),
    });
    let meta = json!({
        "gen": snap.gen,
        "running": snap.running,
        "exact": snap.is_exact(),
        "coverage_pct": snap.coverage_pct(),
        "index_lag_bytes": snap.lag_bytes(),
        "gaps": snap.gaps,
        "gap_bytes": snap.gap_bytes,
        "evicted_templates": snap.evicted_templates,
        "evicted_lines": snap.evicted_lines,
        "template_count": entries.len(),
        "line_oriented": snap.looks_line_oriented(),
        "control_pct": snap.control_ratio() * 100.0,
    });
    Ok(insight_envelope(
        rendered,
        data,
        meta,
        max_chars,
        Some(next),
    ))
}

/// `watch`: カーソル以降の集約された変化のみを返す
///
/// コストは到着データ量に依存しない（4 KB でも 40 MB でも同じ行数）。
fn method_watch(ctx: &BridgeCtx, params: &Value) -> Result<Value, String> {
    use crate::insight::render::{self, commas, human_bytes, human_ms, human_rate, Block, Section};
    use crate::insight::window::parse_cursor;

    let cursor = match params.get("cursor") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let text = v
                .as_str()
                .ok_or_else(|| "bad params: cursor must be a string".to_string())?;
            Some(parse_cursor(text)?)
        }
    };
    let max_chars = param_usize(params, "max_chars").unwrap_or(render::DEFAULT_WATCH_CHARS);
    let top = param_usize(params, "top")
        .unwrap_or(DIGEST_DEFAULT_TOP)
        .min(DIGEST_MAX_TOP);
    let wait_ms = param_u64(params, "wait_ms")
        .unwrap_or(0)
        .min(WATCH_MAX_WAIT_MS);

    // サーバ側ロングポール: 変化するまで待つ（何も無ければ空の差分を返す）
    if let Some(at) = cursor.as_ref() {
        let deadline = Instant::now() + Duration::from_millis(wait_ms);
        while Instant::now() < deadline {
            let snap = ctx.index.snapshot();
            if snap.gen != at.gen || snap.indexed_up_to > at.offset {
                break;
            }
            std::thread::sleep(Duration::from_millis(WATCH_POLL_MS));
        }
    }

    let snap = ctx.index.snapshot();
    let delta = ctx.index.delta_since(cursor.as_ref(), now_ms());
    // `delta_since` は自分が返した終端にチェックポイントを打つので、
    // そこをそのままカーソルにすれば次回の基準点が必ず一致する
    let cursor_out = crate::insight::window::Cursor {
        gen: snap.gen,
        offset: delta.to_offset,
        lines: delta.to_lines,
        epoch_ms: delta.to_epoch_ms,
    };
    let entries: Vec<Entry> = delta
        .changed
        .iter()
        .map(|(stat, count, is_new)| Entry {
            stat: stat.clone(),
            count: *count,
            is_new: *is_new,
        })
        .collect();

    let bytes = delta.to_offset.saturating_sub(delta.from_offset);
    let new_lines = delta.to_lines.saturating_sub(delta.from_lines);
    let mut blocks = vec![Block::new(
        Section::Header,
        vec![format!(
            "watch gen={} · +{} · +{} lines · {} · {}",
            snap.gen,
            human_bytes(bytes),
            commas(new_lines),
            human_ms(delta.elapsed_ms),
            human_rate(bytes, delta.elapsed_ms)
        )],
    )];

    let mut health = index_health_lines(&snap);
    if delta.gen_changed {
        health.push(format!(
            "NOTE  generation changed (capture cleared or port reopened); restarted from off {}",
            commas(delta.from_offset)
        ));
    } else if cursor.is_none() {
        health.push(
            "NOTE  no cursor given: this covers the whole indexed range. Pass the cursor back for deltas."
                .to_string(),
        );
    } else if !delta.exact_baseline {
        health.push(format!(
            "NOTE  measured from off {} (cursor was at {}); the checkpoint for that offset had aged out",
            commas(delta.from_offset),
            commas(cursor.map(|c| c.offset).unwrap_or(0))
        ));
    }
    if !health.is_empty() {
        blocks.push(Block::new(Section::IndexHealth, health));
    }

    blocks.push(Block::new(
        Section::Alarm,
        vec![format!(
            "ALARM +{} err · +{} warn ({})",
            commas(delta.err),
            commas(delta.warn),
            accuracy_note(&snap)
        )],
    ));
    blocks.extend(template_blocks(&entries, top, false));
    blocks.push(Block::new(
        Section::Cursor,
        vec![format!("cursor {}", cursor_out.encode())],
    ));

    let next = format!(
        "serial_watch(cursor=\"{}\", top={}, max_chars={})",
        cursor_out.encode(),
        (top * 2).min(DIGEST_MAX_TOP),
        (max_chars * 2).min(render::MAX_MAX_CHARS)
    );
    let rendered = render::render(blocks, Some(&next), max_chars);

    let data = json!({
        "gen": snap.gen,
        "from_offset": delta.from_offset,
        "to_offset": delta.to_offset,
        "lines": new_lines,
        "bytes": bytes,
        "err": delta.err,
        "warn": delta.warn,
        "templates": entries_to_json(&entries, top),
        "cursor": cursor_out.encode(),
    });
    let meta = json!({
        "gen": snap.gen,
        "running": snap.running,
        "gen_changed": delta.gen_changed,
        "exact": snap.is_exact() && delta.exact_baseline,
        "exact_baseline": delta.exact_baseline,
        "coverage_pct": snap.coverage_pct(),
        "index_lag_bytes": snap.lag_bytes(),
        "waited_ms": wait_ms,
    });
    Ok(insight_envelope(
        rendered,
        data,
        meta,
        max_chars,
        Some(next),
    ))
}

/// `sample` の既定 / 上限行数
const SAMPLE_DEFAULT_N: usize = 10;
const SAMPLE_MAX_N: usize = 50;
/// 窓の大きさを見積もる 1 行あたりのバイト数（当たらなくても正しさには影響しない）
const SAMPLE_BYTES_PER_LINE: u64 = 256;
/// 1 回の抜き取りで読む上限
const SAMPLE_MAX_READ: u64 = 256 * 1024;
/// 1 点抜き（stratified / by_template）で 1 行を切り出すために読む量
const SAMPLE_CHUNK: u32 = 512;

/// `t12` / `12` のどちらでもテンプレート ID として受け取る
fn param_template_id(params: &Value) -> Result<Option<u32>, String> {
    match params.get("template") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_u64()
            .map(|v| Some(v as u32))
            .ok_or_else(|| "bad params: template".to_string()),
        Some(Value::String(s)) => s
            .trim()
            .trim_start_matches('t')
            .parse::<u32>()
            .map(Some)
            .map_err(|_| format!("bad params: template '{s}' (expected an id like \"t12\")")),
        _ => Err("bad params: template must be a string or an integer".to_string()),
    }
}

/// 指定オフセットから 1 行だけ読む
///
/// `offset` は行頭である前提（索引が記録したもの）。読めなければ `None`。
fn read_line_at(store: &DataStore, offset: u64) -> Option<crate::insight::sample::SampleLine> {
    let total = store.total_bytes();
    if offset >= total {
        return None;
    }
    let want = SAMPLE_CHUNK.min((total - offset) as u32);
    let buf = store.get_data(offset, want).ok()?;
    // 改行まで届かなかったときは、切れている事実を残したまま 1 行として出す
    let complete = crate::insight::sample::lines_in(&buf, offset, false, true);
    complete.into_iter().next().or_else(|| {
        crate::insight::sample::lines_in(&buf, offset, false, false)
            .into_iter()
            .next()
    })
}

/// `sample`: 生の行が本当に要るときの、代表・重複畳み込み・上限付きの抜き取り
///
/// 索引が無くても `tail` / `head` / `around` / `stratified` は動く
/// （`by_template` だけが索引を要る）。
fn method_sample(ctx: &BridgeCtx, params: &Value) -> Result<Value, String> {
    use crate::insight::render::{self, commas, Block, Section};
    use crate::insight::sample::{collapse, lines_in, stratified_offsets, SampleLine};

    let mode = params
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("tail")
        .to_string();
    let n = param_usize(params, "n")
        .unwrap_or(SAMPLE_DEFAULT_N)
        .clamp(1, SAMPLE_MAX_N);
    let max_chars = param_usize(params, "max_chars").unwrap_or(render::DEFAULT_SAMPLE_CHARS);
    let fold = params
        .get("collapse_repeats")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let store = ctx.store().ok_or_else(|| "no capture".to_string())?;
    let total = store.total_bytes();
    let snap = ctx.index.snapshot();
    let window = (n as u64)
        .saturating_mul(SAMPLE_BYTES_PER_LINE)
        .min(SAMPLE_MAX_READ);

    // (説明, 行) を作る。読み方はモードごとに違うが、行の作り方は 1 箇所
    let (label, mut lines): (String, Vec<SampleLine>) = match mode.as_str() {
        "tail" => {
            let start = total.saturating_sub(window);
            let buf = store
                .get_data(start, (total - start) as u32)
                .unwrap_or_default();
            let mut lines = lines_in(&buf, start, start > 0, false);
            if lines.len() > n {
                lines.drain(..lines.len() - n);
            }
            (format!("mode=tail n={n}"), lines)
        }
        "head" => {
            let start = snap.start_offset.min(total);
            let len = window.min(total.saturating_sub(start)) as u32;
            let buf = store.get_data(start, len).unwrap_or_default();
            let mut lines = lines_in(&buf, start, false, true);
            lines.truncate(n);
            (format!("mode=head n={n}"), lines)
        }
        "around" => {
            let at = param_u64(params, "offset")
                .ok_or_else(|| "bad params: mode=around needs 'offset'".to_string())?;
            let start = at.saturating_sub(window / 2);
            let len = window.min(total.saturating_sub(start)) as u32;
            let buf = store.get_data(start, len).unwrap_or_default();
            let lines = lines_in(&buf, start, start > 0, true);
            // 指定オフセットを含む行を中心に n 行
            let center = lines
                .iter()
                .position(|l| l.offset >= at)
                .unwrap_or(lines.len().saturating_sub(1));
            let from = center.saturating_sub(n / 2);
            let to = (from + n).min(lines.len());
            (
                format!("mode=around offset={} n={n}", commas(at)),
                lines[from.min(to)..to].to_vec(),
            )
        }
        "stratified" => {
            let start = snap.start_offset.min(total);
            let mut lines = Vec::new();
            for at in stratified_offsets(start, total, n, SAMPLE_CHUNK as u64) {
                let buf = store.get_data(at, SAMPLE_CHUNK).unwrap_or_default();
                // 位置は行の途中なので、最初の完全な行を採る
                if let Some(line) = lines_in(&buf, at, at > start, true).into_iter().next() {
                    lines.push(line);
                }
            }
            (
                format!(
                    "mode=stratified n={} over off {}..{}",
                    lines.len(),
                    commas(start),
                    commas(total)
                ),
                lines,
            )
        }
        "by_template" => {
            let id = param_template_id(params)?
                .ok_or_else(|| "bad params: mode=by_template needs 'template'".to_string())?;
            let stat = snap.templates.iter().find(|t| t.id == id).ok_or_else(|| {
                format!("unknown template t{id} (it may have been evicted; run digest again)")
            })?;
            let lines: Vec<SampleLine> = stat
                .recent_offsets
                .iter()
                .rev()
                .take(n)
                .filter_map(|off| read_line_at(&store, *off))
                .collect();
            (
                format!(
                    "mode=by_template t{id} x{} (showing the last {} occurrence(s) the index kept)",
                    commas(stat.count),
                    lines.len()
                ),
                lines,
            )
        }
        other => {
            return Err(format!(
                "bad params: mode '{other}' (use tail|head|around|stratified|by_template)"
            ))
        }
    };

    if fold {
        lines = collapse(lines);
    }
    let shown: u32 = lines.iter().map(|l| l.repeats).sum();

    let mut blocks = vec![Block::new(
        Section::Header,
        vec![format!(
            "sample gen={} {label} · {} line(s) shown · capture is {} byte(s)",
            snap.gen,
            commas(shown as u64),
            commas(total)
        )],
    )];
    if lines.is_empty() {
        // 裸の空応答を返さない: 何も出せなかった理由を必ず言う
        blocks.push(Block::new(
            Section::IndexHealth,
            vec![format!(
                "NOTE  no complete line in the window that was read \
                 (partial lines at the edges are dropped on purpose). \
                 Try a larger n, or serial_read_range near the offset you care about."
            )],
        ));
    }
    blocks.push(Block::new(
        Section::Samples,
        lines
            .iter()
            .map(|l| {
                let repeats = if l.repeats > 1 {
                    format!("  [x{}]", l.repeats)
                } else {
                    String::new()
                };
                let cut = if l.truncated { "\u{2026}" } else { "" };
                format!("@off={}  {}{cut}{repeats}", commas(l.offset), l.text)
            })
            .collect(),
    ));
    blocks.push(Block::new(
        Section::Cursor,
        vec![format!("cursor {}", snap.cursor().encode())],
    ));

    let next = format!(
        "serial_sample(mode={mode:?}, n={}, max_chars={})",
        (n * 2).min(SAMPLE_MAX_N),
        (max_chars * 2).min(render::MAX_MAX_CHARS)
    );
    let rendered = render::render(blocks, Some(&next), max_chars);

    let data = json!({
        "mode": mode,
        "lines": lines.iter().map(|l| json!({
            "offset": l.offset,
            "text": l.text,
            "repeats": l.repeats,
            "truncated": l.truncated,
        })).collect::<Vec<Value>>(),
        "total_bytes": total,
    });
    let meta = json!({
        "gen": snap.gen,
        "running": snap.running,
        "collapsed": fold,
        "line_count": lines.len(),
        "occurrences": shown,
    });
    Ok(insight_envelope(
        rendered,
        data,
        meta,
        max_chars,
        Some(next),
    ))
}

/// `digest(template=...)`: 1 テンプレートの詳細
///
/// 概要モードと違い、ここは**数百バイトだけ**キャプチャを読む
/// （索引が覚えている直近の出現オフセットから実際の行を引くため）。
fn digest_detail(
    ctx: &BridgeCtx,
    snap: &crate::insight::index::Snapshot,
    id: u32,
    max_chars: usize,
) -> Result<Value, String> {
    use crate::insight::render::{self, commas, number, template_with_slots, Block, Section};

    let stat = snap.templates.iter().find(|t| t.id == id).ok_or_else(|| {
        format!(
            "unknown template t{id}. It may never have existed, or it was evicted \
                 ({} template(s) evicted so far). Run serial_digest() to see the current ids.",
            commas(snap.evicted_templates)
        )
    })?;

    let share = if snap.lines == 0 {
        0.0
    } else {
        stat.count as f64 * 100.0 / snap.lines as f64
    };
    let mut blocks = vec![Block::new(
        Section::Header,
        vec![
            format!(
                "detail gen={} t{} x{} ({:.1}% of {} indexed lines)",
                snap.gen,
                stat.id,
                commas(stat.count),
                share,
                commas(snap.lines)
            ),
            format!("\"{}\"", template_with_slots(&stat.template, &stat.slots)),
            format!(
                "first @off={}  last @off={}",
                commas(stat.first_offset),
                commas(stat.last_offset)
            ),
        ],
    )];

    let health = index_health_lines(snap);
    if !health.is_empty() {
        blocks.push(Block::new(Section::IndexHealth, health));
    }
    blocks.push(Block::new(
        Section::Alarm,
        vec![format!(
            "ALARM {} err · {} warn across the whole index ({})",
            commas(snap.err),
            commas(snap.warn),
            accuracy_note(snap)
        )],
    ));

    // 全スロットの範囲。docs/26 §6.2-4 のとおり極値にはオフセットを添える
    let slot_lines: Vec<String> = stat
        .slots
        .iter()
        .enumerate()
        .map(|(i, slot)| {
            let tag = slot.kind.placeholder();
            if !slot.numeric {
                return format!("      {} {tag} (not numeric)", i + 1);
            }
            if slot.min == slot.max {
                return format!(
                    "      {} {tag} constant {} (first @off={})",
                    i + 1,
                    number(slot.min),
                    commas(slot.min_offset)
                );
            }
            format!(
                "      {} {tag} min {}@off={} max {}@off={} last {}",
                i + 1,
                number(slot.min),
                commas(slot.min_offset),
                number(slot.max),
                commas(slot.max_offset),
                number(slot.last)
            )
        })
        .collect();
    if !slot_lines.is_empty() {
        let mut lines = vec![format!("SLOT  {} field(s)", stat.slots.len())];
        lines.extend(slot_lines);
        blocks.push(Block::new(Section::Top, lines));
    }

    // 実際の行。索引はオフセットしか覚えていないので、ここで読み直す
    let samples: Vec<String> = match ctx.store() {
        Some(store) => stat
            .recent_offsets
            .iter()
            .rev()
            .filter_map(|off| read_line_at(&store, *off))
            .map(|line| {
                format!(
                    "LINE  @off={}  {}{}",
                    commas(line.offset),
                    line.text,
                    if line.truncated { "\u{2026}" } else { "" }
                )
            })
            .collect(),
        None => vec!["LINE  (no capture attached; offsets above are still valid)".to_string()],
    };
    if !samples.is_empty() {
        blocks.push(Block::new(Section::Samples, samples));
    }
    blocks.push(Block::new(
        Section::Cursor,
        vec![format!("cursor {}", snap.cursor().encode())],
    ));

    let next = format!("serial_sample(mode=\"by_template\", template=\"t{id}\")");
    let rendered = render::render(blocks, Some(&next), max_chars);

    let data = json!({
        "gen": snap.gen,
        "id": stat.id,
        "template": stat.template,
        "count": stat.count,
        "share_pct": share,
        "first_offset": stat.first_offset,
        "last_offset": stat.last_offset,
        "recent_offsets": stat.recent_offsets,
        "slots": stat.slots.iter().map(|s| json!({
            "kind": s.kind.placeholder(),
            "numeric": s.numeric,
            "min": s.min,
            "max": s.max,
            "last": s.last,
            "min_offset": s.min_offset,
            "max_offset": s.max_offset,
        })).collect::<Vec<Value>>(),
    });
    let meta = json!({
        "gen": snap.gen,
        "running": snap.running,
        "exact": snap.is_exact(),
        "coverage_pct": snap.coverage_pct(),
        "evicted_templates": snap.evicted_templates,
    });
    Ok(insight_envelope(
        rendered,
        data,
        meta,
        max_chars,
        Some(next),
    ))
}

// ============================================================================
// サーバ側の待ち受け (docs/26 段階 4 / DEBT-8 の恒久対策)
// ============================================================================

/// `wait_for` が 1 回の呼び出しでブロックしてよい上限
///
/// クライアントの 1 リクエストタイムアウト（`mcp_stdio::REQUEST_TIMEOUT_MS`
/// = 5000ms）より十分手前で必ず返す。短く区切るのは、MCP の
/// `notifications/cancelled` に反応できる余地を残すため
/// （ブロック中のソケット読み出しは中断できないので、区切り目でしか見られない）。
pub const WAIT_SERVER_MAX_MS: u64 = 1000;
/// 待機中に新着を見に行く間隔
const WAIT_SERVER_POLL_MS: u64 = 5;
/// 再開時に重ねて走査する量。これを超える長さの単一マッチは境界で取り逃がし得る
const WAIT_SCAN_OVERLAP: u64 = crate::insight::find::SCAN_OVERLAP;
/// マッチの前後に付ける文脈
const WAIT_CONTEXT_BEFORE: u64 = 80;
const WAIT_CONTEXT_AFTER: u64 = 160;
/// タイムアウト報告に添える末尾バイト数
const WAIT_TAIL_BYTES: u64 = 256;
/// 1 回の走査が費やしてよい時間の安全弁（巨大な積み残しでブロックしないため）
const WAIT_SCAN_DEADLINE_MS: u64 = 2000;

/// `wait_for`: 指定オフセット以降に `pattern` が現れるまでサーバ側で待つ
///
/// 旧実装はクライアントが `tail` を base64 で引いて手元で走査していた
/// （docs/22 DEBT-8）。固定窓・重なり・蓄積の切り詰めという失敗モードは、
/// **ストアを直接前方走査する**ことで構造ごと消える:
///
/// - 窓が無いので「読み負け」が無い。到着レートに依存しない。
/// - 蓄積を持たないので `WAIT_RETAIN_BYTES` を超える単一マッチも取り逃がさない
///   （残るのは再開の重なり [`WAIT_SCAN_OVERLAP`] = 4 KiB を超える場合のみ）。
/// - base64 の往復が無い。
///
/// 呼び出し側は `next_from` を渡して再開する。重なりの計算はここ 1 箇所。
fn method_wait_for(ctx: &BridgeCtx, params: &Value) -> Result<Value, String> {
    let pattern = params
        .get("pattern")
        .and_then(Value::as_str)
        .ok_or_else(|| "bad params: pattern".to_string())?;
    let regex = regex::bytes::Regex::new(&format!("(?m){pattern}"))
        .map_err(|e| format!("invalid pattern: {e}"))?;
    let requested_from = param_u64(params, "from").unwrap_or(0);
    let timeout_ms = param_u64(params, "timeout_ms")
        .unwrap_or(0)
        .min(WAIT_SERVER_MAX_MS);

    let store = ctx.store().ok_or_else(|| "no capture".to_string())?;
    let started = Instant::now();
    let deadline = Duration::from_millis(timeout_ms);

    // 世代交代したら「元の開始位置」も意味を失うので、床も 0 に落とす
    let mut floor = requested_from;
    let mut from = requested_from;
    let mut scanned_to = requested_from;
    let mut gap_bytes = 0u64;
    let mut reset = false;
    let mut total;

    loop {
        total = store.total_bytes();
        if total < from {
            // Clear / ポート再オープンでオフセットが巻き戻った。
            // 黙って待ち続けると「応答が来ているのに必ずタイムアウト」になる
            reset = true;
            floor = 0;
            from = 0;
            scanned_to = 0;
        }
        if total > from {
            let src = StoreSource(store.clone());
            let scan_deadline = Duration::from_millis(WAIT_SCAN_DEADLINE_MS);
            let scan_started = Instant::now();
            let result = crate::insight::find::find_matches(
                &src,
                &crate::insight::find::FindOptions {
                    regex: &regex,
                    from,
                    to: total,
                    max_hits: 1,
                },
                &|| scan_started.elapsed() >= scan_deadline,
            );
            gap_bytes += result.gap_bytes;
            scanned_to = scanned_to.max(total);

            if let Some(hit) = result.hits.first() {
                let context_start = hit.offset.saturating_sub(WAIT_CONTEXT_BEFORE);
                let context_end =
                    (hit.offset + hit.len as u64 + WAIT_CONTEXT_AFTER).min(store.total_bytes());
                let context = store
                    .get_data(
                        context_start,
                        context_end.saturating_sub(context_start) as u32,
                    )
                    .unwrap_or_default();
                let excerpt = String::from_utf8_lossy(&context).into_owned();
                // マッチ部分そのもの。オフセットと長さから切り出すので、
                // 呼び出し側が lossy 変換で数え直してずれることがない
                let match_start = (hit.offset - context_start) as usize;
                let match_text = context
                    .get(match_start..(match_start + hit.len).min(context.len()))
                    .map(|b| String::from_utf8_lossy(b).into_owned())
                    .unwrap_or_default();
                return Ok(json!({
                    "matched": true,
                    "offset": hit.offset,
                    "len": hit.len,
                    "match_text": match_text,
                    "excerpt": excerpt,
                    "from": requested_from,
                    "scanned_to": scanned_to,
                    "next_from": scanned_to,
                    "total_bytes": total,
                    "gap_bytes": gap_bytes,
                    "reset": reset,
                    "tail_base64": "",
                    "tail_offset": 0,
                    "elapsed_ms": started.elapsed().as_millis() as u64,
                }));
            }
            // 次はこの続きから。境界を跨ぐマッチのために少しだけ重ねる
            from = total.saturating_sub(WAIT_SCAN_OVERLAP).max(floor);
        }
        if started.elapsed() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(WAIT_SERVER_POLL_MS));
    }

    // 見つからなかった: 何を見に行けたのかを必ず添える
    let tail_start = total.saturating_sub(WAIT_TAIL_BYTES).max(requested_from);
    let tail = if total > tail_start {
        store
            .get_data(tail_start, (total - tail_start) as u32)
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok(json!({
        "matched": false,
        "offset": Value::Null,
        "len": 0,
        "match_text": "",
        "excerpt": "",
        "from": requested_from,
        "scanned_to": scanned_to,
        "next_from": from,
        "total_bytes": total,
        "gap_bytes": gap_bytes,
        "reset": reset,
        "tail_base64": BASE64.encode(&tail),
        "tail_offset": tail_start,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    }))
}

fn method_tail(ctx: &BridgeCtx, params: &Value) -> Result<Value, String> {
    let requested = match params.get("bytes") {
        None | Some(Value::Null) => DEFAULT_TAIL_BYTES,
        Some(_) => param_u32(params, "bytes").ok_or_else(|| "bad params: bytes".to_string())?,
    };

    let store = ctx.store().ok_or_else(|| "no capture".to_string())?;
    let total = store.total_bytes();
    let window = requested.min(MAX_READ_LENGTH) as u64;
    let offset = total.saturating_sub(window);
    let to_read = (total - offset) as u32;

    let data = if to_read == 0 {
        Vec::new()
    } else {
        store.get_data(offset, to_read)?
    };

    Ok(json!({
        "base64": BASE64.encode(&data),
        "offset": offset,
        "total_bytes": total,
    }))
}

fn method_send(ctx: &BridgeCtx, params: &Value) -> Result<Value, String> {
    let payload = build_send_payload(params)?;
    let port = ctx
        .port_handle()
        .ok_or_else(|| "port not open".to_string())?;

    // 受信ワーカーへキューで渡し、完了を待つ（ADR-14）。書き込みは読みと交互に
    // 1 スライスずつ行われるので、大きな送信でも受信は止まらない。
    // offset_before はワーカーが要求を取り出した時点の公開済み位置: ここから前方走査
    // すれば、返信が最初のポーリングより前に着弾しても取りこぼさず（docs/26 §5.1）、
    // キュー待ちの間に届いたデータを返信と取り違えることもない
    let preview = preview_of(&payload);
    let receipt = port.send(payload)?;
    let written = receipt.bytes_written;
    let offset_before = receipt.offset_before;

    // GUI 可視化要件: 人間が AI の送信内容を見られるようにする
    ctx.record_send(written, preview);

    Ok(json!({ "bytes_written": written, "offset_before": offset_before }))
}

fn method_subscribe(params: &Value) -> Result<Value, String> {
    let from_offset = match params.get("from_offset") {
        None | Some(Value::Null) => None,
        Some(_) => Some(
            param_u64(params, "from_offset")
                .ok_or_else(|| "bad params: from_offset".to_string())?,
        ),
    };
    Ok(json!({
        "subscribed": true,
        "from_offset": from_offset,
    }))
}

// ============================================================================
// IO 層
// ============================================================================

fn is_timeout(err: &std::io::Error) -> bool {
    matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

fn write_line(writer: &mut TcpStream, value: &Value) -> std::io::Result<()> {
    let mut line = serde_json::to_string(value)
        .unwrap_or_else(|_| r#"{"id":null,"ok":false,"error":"encode error"}"#.to_string());
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()
}

/// 接続数カウンタを必ず戻すためのガード
struct ConnectionGuard(Arc<AtomicUsize>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 動作中のブリッジサーバ（listener スレッド + 接続スレッド群）
pub struct BridgeServer {
    stop_flag: Arc<AtomicBool>,
    listener_thread: Option<JoinHandle<()>>,
    connection_threads: Arc<Mutex<Vec<JoinHandle<()>>>>,
    local_addr: SocketAddr,
}

impl BridgeServer {
    /// 127.0.0.1 の指定ポートで待ち受けを開始する
    ///
    /// `port` に 0 を渡すと OS が空きポートを割り当てる（テスト用）。実際に
    /// バインドされたアドレスは [`BridgeServer::local_addr`] で取得できる。
    pub fn start(
        port: u16,
        ctx: Arc<BridgeCtx>,
        connections: Arc<AtomicUsize>,
    ) -> Result<Self, String> {
        // ループバック固定: 外部からは絶対に到達できない
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .map_err(|e| format!("Failed to bind 127.0.0.1:{}: {}", port, e))?;
        let local_addr = listener
            .local_addr()
            .map_err(|e| format!("Failed to resolve local address: {}", e))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("Failed to set nonblocking: {}", e))?;

        let stop_flag = Arc::new(AtomicBool::new(false));
        let connection_threads: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));

        let thread_stop = stop_flag.clone();
        let thread_conns = connection_threads.clone();
        let listener_thread = thread::spawn(move || {
            log::info!("[bridge] Listening on {}", local_addr);
            loop {
                if thread_stop.load(Ordering::SeqCst) {
                    break;
                }
                match listener.accept() {
                    Ok((stream, peer)) => {
                        // 二重の安全策: ループバック以外は即座に切る
                        if !peer.ip().is_loopback() {
                            log::warn!("[bridge] Rejected non-loopback peer: {}", peer);
                            continue;
                        }
                        if connections.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                            let mut stream = stream;
                            let _ = write_line(
                                &mut stream,
                                &Response::error(Value::Null, "too many connections").into_json(),
                            );
                            // クライアントは接続直後に最初のリクエストを書き込んで
                            // いることが多い。未読データを残したまま drop すると
                            // （特に Windows で）RST が送信され、上のエラー行が
                            // クライアントへ届く前に破棄され得る。書き込み側だけ
                            // 閉じて、相手が閉じるまで短時間読み捨てる。
                            let _ = stream.shutdown(std::net::Shutdown::Write);
                            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                            let mut sink = [0u8; 256];
                            use std::io::Read as _;
                            while matches!(stream.read(&mut sink), Ok(n) if n > 0) {}
                            log::warn!("[bridge] Connection limit reached, rejected {}", peer);
                            continue;
                        }
                        connections.fetch_add(1, Ordering::SeqCst);
                        let guard = ConnectionGuard(connections.clone());
                        let conn_ctx = ctx.clone();
                        let conn_stop = thread_stop.clone();
                        let handle = thread::spawn(move || {
                            let _guard = guard;
                            handle_connection(stream, conn_ctx, conn_stop);
                        });
                        if let Ok(mut list) = thread_conns.lock() {
                            // 終了済みハンドルが溜まらないよう、生存中のものだけ残す
                            list.retain(|h| !h.is_finished());
                            list.push(handle);
                        }
                    }
                    Err(ref e) if is_timeout(e) => {
                        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
                    }
                    Err(e) => {
                        log::warn!("[bridge] accept error: {}", e);
                        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
                    }
                }
            }
            log::info!("[bridge] Listener stopped ({})", local_addr);
        });

        Ok(Self {
            stop_flag,
            listener_thread: Some(listener_thread),
            connection_threads,
            local_addr,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn port(&self) -> u16 {
        self.local_addr.port()
    }

    /// 停止フラグを立て、listener と全接続スレッドの終了を待つ
    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.listener_thread.take() {
            let _ = handle.join();
        }
        let handles: Vec<JoinHandle<()>> = match self.connection_threads.lock() {
            Ok(mut list) => list.drain(..).collect(),
            Err(_) => Vec::new(),
        };
        for handle in handles {
            let _ = handle.join();
        }
    }
}

impl Drop for BridgeServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 1 接続の処理ループ
fn handle_connection(stream: TcpStream, ctx: Arc<BridgeCtx>, stop_flag: Arc<AtomicBool>) {
    // 読み取りにタイムアウトを付けて、停止フラグを定期的に見に行けるようにする
    let _ = stream.set_read_timeout(Some(Duration::from_millis(POLL_INTERVAL_MS)));
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(e) => {
            log::warn!("[bridge] Failed to clone stream: {}", e);
            return;
        }
    };
    let mut reader = BufReader::new(stream);
    let mut session = Session::new(ctx.token.is_some());
    let mut line = String::new();

    loop {
        if stop_flag.load(Ordering::SeqCst) {
            break;
        }
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(ref e) if is_timeout(e) => {
                // タイムアウト時は行が途中まで積まれている可能性があるので
                // クリアせず、そのまま続きを読む
                if line.len() > MAX_LINE_BYTES {
                    break;
                }
                continue;
            }
            Err(e) => {
                log::debug!("[bridge] read error: {}", e);
                break;
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            line.clear();
            continue;
        }
        if trimmed.len() > MAX_LINE_BYTES {
            let _ = write_line(
                &mut writer,
                &Response::error(Value::Null, "parse error").into_json(),
            );
            break;
        }

        match parse_request(trimmed) {
            Err(response) => {
                if write_line(&mut writer, &response.into_json()).is_err() {
                    break;
                }
            }
            Ok(request) => {
                let is_subscribe = request.method == "subscribe";
                let response = handle_request(&request, &ctx, &mut session);
                let accepted = response.is_ok();
                if write_line(&mut writer, &response.into_json()).is_err() {
                    break;
                }
                if is_subscribe && accepted {
                    // 以降この接続はリクエストを読まず push 専用になる
                    let from_offset = param_u64(&request.params, "from_offset");
                    run_subscription(&mut writer, &ctx, &stop_flag, from_offset);
                    break;
                }
            }
        }
        line.clear();
    }
}

/// push モードのループ
///
/// 50 ms ごとに `total_bytes` を見て、増分を 1 フレーム 256 KiB までで送る。
/// DataStore のインスタンスが差し替わった / `total_bytes` が巻き戻ったときは
/// `{"event":"reset"}` を送ってオフセットを 0 に戻す。
fn run_subscription(
    writer: &mut TcpStream,
    ctx: &BridgeCtx,
    stop_flag: &Arc<AtomicBool>,
    from_offset: Option<u64>,
) {
    let mut current_store: Option<Arc<DataStore>> = None;
    let mut next_offset: u64 = 0;
    let mut attached = false;

    // 切断検知用: peek をほぼノンブロッキングにする（データが無くても 1ms で戻る）
    let _ = writer.set_read_timeout(Some(Duration::from_millis(1)));

    loop {
        if stop_flag.load(Ordering::SeqCst) {
            break;
        }

        // クライアントが閉じたら接続スロットを解放する
        let mut probe = [0u8; 1];
        match writer.peek(&mut probe) {
            Ok(0) => break, // 対向が close
            Ok(_) => {}     // 購読後のリクエストは読まない（プロトコル定義）
            Err(ref e) if is_timeout(e) => {}
            Err(_) => break,
        }

        match ctx.store() {
            Some(store) => {
                let changed = current_store
                    .as_ref()
                    .map(|c| !Arc::ptr_eq(c, &store))
                    .unwrap_or(true);
                if changed {
                    if attached {
                        // 世代交代: クライアントへリセットを通知
                        if write_line(writer, &json!({ "event": "reset" })).is_err() {
                            break;
                        }
                        next_offset = 0;
                    } else {
                        next_offset = from_offset.unwrap_or(0);
                        attached = true;
                    }
                    current_store = Some(store.clone());
                }

                let total = store.total_bytes();
                if total < next_offset {
                    // 同一インスタンスでの巻き戻り（論理的なリセット）
                    if write_line(writer, &json!({ "event": "reset" })).is_err() {
                        break;
                    }
                    next_offset = 0;
                }
                if total > next_offset {
                    let length = (total - next_offset).min(SUBSCRIBE_MAX_FRAME) as u32;
                    match store.get_data(next_offset, length) {
                        Ok(data) if !data.is_empty() => {
                            let frame = json!({
                                "event": "data",
                                "offset": next_offset,
                                "base64": BASE64.encode(&data),
                            });
                            if write_line(writer, &frame).is_err() {
                                break;
                            }
                            next_offset += data.len() as u64;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            log::debug!("[bridge] subscription read failed: {}", e);
                        }
                    }
                }
            }
            None => {
                if attached {
                    if write_line(writer, &json!({ "event": "reset" })).is_err() {
                        break;
                    }
                    attached = false;
                    current_store = None;
                    next_offset = 0;
                }
            }
        }

        thread::sleep(Duration::from_millis(SUBSCRIBE_POLL_MS));
    }
}

// ============================================================================
// Tauri 状態 / コマンド
// ============================================================================

/// GUI へ返すブリッジの状態
#[derive(Clone, Debug, Serialize)]
pub struct BridgeStatusInfo {
    pub enabled: bool,
    pub port: u16,
    pub connections: u32,
    pub last_activity: Option<BridgeActivity>,
}

struct BridgeConfig {
    enabled: bool,
    port: u16,
    /// 認証トークン。現状は常に `None`（設定の配線は後段レイヤ）。
    token: Option<String>,
    server: Option<BridgeServer>,
}

/// Tauri が管理するブリッジ状態
pub struct BridgeState {
    config: Mutex<BridgeConfig>,
    activity: Arc<Mutex<Option<BridgeActivity>>>,
    connections: Arc<AtomicUsize>,
    /// 導出情報の索引。**ブリッジが所有する**ので、プロッタ窓やメイン窓の
    /// 開閉で中身が消えたりしない（docs/26 §2.2）
    index: Arc<crate::insight::InsightIndex>,
    /// 索引スレッド。`bridge_set(enabled=true)` の間だけ動く
    index_thread: Mutex<Option<crate::insight::IndexThread>>,
}

impl Default for BridgeState {
    fn default() -> Self {
        Self {
            config: Mutex::new(BridgeConfig {
                enabled: false, // 既定 OFF: 自動起動しない
                port: DEFAULT_BRIDGE_PORT,
                token: None,
                server: None,
            }),
            activity: Arc::new(Mutex::new(None)),
            connections: Arc::new(AtomicUsize::new(0)),
            index: Arc::new(crate::insight::InsightIndex::new()),
            index_thread: Mutex::new(None),
        }
    }
}

impl BridgeState {
    pub fn new() -> Self {
        Self::default()
    }

    fn snapshot(&self, config: &BridgeConfig) -> BridgeStatusInfo {
        BridgeStatusInfo {
            enabled: config.enabled,
            port: config.port,
            connections: self.connections.load(Ordering::SeqCst) as u32,
            last_activity: self.activity.lock().ok().and_then(|a| a.clone()),
        }
    }
}

/// AI 連携ガイドの表示に必要な情報
///
/// `exe_path` は実行中バイナリの絶対パス。ガイドはこれを埋め込んだ
/// コピペ可能な MCP 登録コマンドを表示する（インストール先はユーザーごとに
/// 異なるため、実行時に解決するしかない）。
#[derive(Clone, Debug, Serialize)]
pub struct BridgeGuideInfo {
    pub exe_path: String,
    pub bridge_enabled: bool,
    pub bridge_port: u16,
    pub app_version: String,
}

/// AI 連携ガイド用の情報を返す
#[tauri::command]
pub fn bridge_guide_info(state: tauri::State<'_, BridgeState>) -> BridgeGuideInfo {
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    // AppImage では current_exe() が一時マウントされた squashfs 内のパスを返し、
    // アプリ終了とともに消える。AppImage ランタイムが設定する $APPIMAGE
    // （.AppImage 本体のパス）を優先する。
    let exe_path = std::env::var("APPIMAGE")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| {
            std::env::current_exe()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    BridgeGuideInfo {
        exe_path,
        bridge_enabled: config.enabled,
        bridge_port: config.port,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// ブリッジの現在状態を取得する
#[tauri::command]
pub fn bridge_status(state: tauri::State<'_, BridgeState>) -> BridgeStatusInfo {
    // 状態取得は失敗させない: 毒された Mutex も中身をそのまま読む
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    state.snapshot(&config)
}

/// ブリッジの起動 / 停止
///
/// - `enabled=false`: 動いていれば停止する
/// - `enabled=true`: 同じポートで既に動いていれば no-op、ポートが変われば再起動
#[tauri::command]
pub fn bridge_set(
    app: tauri::AppHandle,
    state: tauri::State<'_, BridgeState>,
    serial: tauri::State<'_, crate::serial::SerialState>,
    enabled: bool,
    port: Option<u16>,
) -> Result<BridgeStatusInfo, String> {
    use tauri::Emitter;

    let mut config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    let desired_port = port.unwrap_or(config.port);

    if !enabled {
        if let Some(mut server) = config.server.take() {
            server.stop();
            log::info!("[bridge] Stopped");
        }
        // 索引も止める: OFF の利用者は CPU もメモリも払わない（docs/26 §2.2）
        if let Ok(mut thread) = state.index_thread.lock() {
            if let Some(mut running) = thread.take() {
                running.stop();
            }
        }
        config.enabled = false;
        config.port = desired_port;
        return Ok(state.snapshot(&config));
    }

    // 同じ設定での再起動要求は no-op
    let already_running = config
        .server
        .as_ref()
        .map(|s| s.port() == desired_port)
        .unwrap_or(false);
    if already_running {
        config.enabled = true;
        return Ok(state.snapshot(&config));
    }
    // ポート変更は listener の作り直しが必要
    if let Some(mut server) = config.server.take() {
        server.stop();
    }

    let app_handle = app.clone();
    let ctx = Arc::new(
        BridgeCtx::new(
            serial.data_store.clone(),
            serial.port.clone(),
            config.token.clone(),
            state.activity.clone(),
            state.index.clone(),
            Box::new(move |event: BridgeActivityEvent| {
                if let Err(e) = app_handle.emit("bridge-activity", event) {
                    log::warn!("[bridge] Failed to emit bridge-activity: {}", e);
                }
            }),
        )
        .with_reconnecting(serial.reconnecting.clone()),
    );

    // 待ち受けが立ってから索引を起動する。listener の bind に失敗したときに
    // 索引だけが動き続けると、ブリッジ OFF なのに CPU とメモリを払うことになる
    let server = BridgeServer::start(desired_port, ctx, state.connections.clone())?;
    log::info!("[bridge] Started on {}", server.local_addr());
    if let Ok(mut thread) = state.index_thread.lock() {
        if thread.is_none() {
            *thread = Some(crate::insight::IndexThread::start(
                serial.data_store.clone(),
                state.index.clone(),
            ));
        }
    }
    config.port = server.port();
    config.enabled = true;
    config.server = Some(server);

    Ok(state.snapshot(&config))
}

// ============================================================================
// テスト
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用 ctx を組み立てる。`emit` は呼び出しを記録する。
    fn test_ctx(
        store: Option<Arc<DataStore>>,
        token: Option<String>,
    ) -> (Arc<BridgeCtx>, Arc<Mutex<Vec<BridgeActivityEvent>>>) {
        let emitted: Arc<Mutex<Vec<BridgeActivityEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = emitted.clone();
        let ctx = BridgeCtx::new(
            Arc::new(Mutex::new(store)),
            Arc::new(Mutex::new(None)),
            token,
            Arc::new(Mutex::new(None)),
            Arc::new(crate::insight::InsightIndex::new()),
            Box::new(move |event| {
                if let Ok(mut list) = sink.lock() {
                    list.push(event);
                }
            }),
        );
        (Arc::new(ctx), emitted)
    }

    fn store_with(data: &[u8]) -> Arc<DataStore> {
        let store = Arc::new(DataStore::new().expect("DataStore::new"));
        if !data.is_empty() {
            store.push_test_data(data);
        }
        store
    }

    fn call(ctx: &BridgeCtx, method: &str, params: Value) -> Response {
        let mut session = Session::new(ctx.token.is_some());
        handle_request(&Request::new(1, method, params), ctx, &mut session)
    }

    fn result_of(response: Response) -> Value {
        match response.outcome {
            Outcome::Ok(v) => v,
            Outcome::Err(e) => panic!("expected ok, got error: {}", e),
        }
    }

    fn error_of(response: Response) -> String {
        match response.outcome {
            Outcome::Ok(v) => panic!("expected error, got ok: {}", v),
            Outcome::Err(e) => e,
        }
    }

    fn decode(value: &Value, key: &str) -> Vec<u8> {
        BASE64
            .decode(value.get(key).and_then(Value::as_str).unwrap())
            .unwrap()
    }

    // ---------------- 純粋ヘルパ ----------------

    #[test]
    fn test_clamp_read_length_caps_at_1mib() {
        // 要求が上限を超えても 1 MiB でクランプされる
        assert_eq!(clamp_read_length(u32::MAX, u64::MAX), MAX_READ_LENGTH);
        assert_eq!(clamp_read_length(2_000_000, 10_000_000), MAX_READ_LENGTH);
        // 残量が少なければ残量でクランプ
        assert_eq!(clamp_read_length(1000, 10), 10);
        assert_eq!(clamp_read_length(10, 1000), 10);
        assert_eq!(clamp_read_length(100, 0), 0);
    }

    #[test]
    fn test_build_send_payload_line_endings() {
        let p = |ending: &str| {
            build_send_payload(&json!({ "text": "AT", "line_ending": ending })).unwrap()
        };
        assert_eq!(p("none"), b"AT".to_vec());
        assert_eq!(p("cr"), b"AT\r".to_vec());
        assert_eq!(p("lf"), b"AT\n".to_vec());
        assert_eq!(p("crlf"), b"AT\r\n".to_vec());
        // line_ending 省略時は none
        assert_eq!(
            build_send_payload(&json!({ "text": "AT" })).unwrap(),
            b"AT".to_vec()
        );
    }

    #[test]
    fn test_build_send_payload_base64_and_errors() {
        assert_eq!(
            build_send_payload(&json!({ "base64": BASE64.encode([0u8, 0xff, 0x10]) })).unwrap(),
            vec![0u8, 0xff, 0x10]
        );
        // base64 優先
        assert_eq!(
            build_send_payload(&json!({ "base64": BASE64.encode(b"X"), "text": "ignored" }))
                .unwrap(),
            b"X".to_vec()
        );
        assert!(build_send_payload(&json!({})).unwrap_err().contains("text"));
        assert!(build_send_payload(&json!({ "base64": "!!!" }))
            .unwrap_err()
            .contains("invalid base64"));
        assert!(
            build_send_payload(&json!({ "text": "A", "line_ending": "wat" }))
                .unwrap_err()
                .contains("line_ending")
        );
    }

    #[test]
    fn test_preview_of_truncates_to_64_chars() {
        let long = vec![b'a'; 200];
        assert_eq!(preview_of(&long).chars().count(), 64);
        // 不正な UTF-8 でも panic しない
        assert!(!preview_of(&[0xff, 0xfe]).is_empty());
    }

    #[test]
    fn test_parse_request_errors() {
        let err = parse_request("not json").unwrap_err();
        assert_eq!(err.id, Value::Null);
        assert_eq!(error_of(err), "parse error");
        // method が無い場合も parse error（id はエコーする）
        let err = parse_request(r#"{"id":7}"#).unwrap_err();
        assert_eq!(err.id, json!(7));
        let req = parse_request(r#"{"id":3,"method":"status"}"#).unwrap();
        assert_eq!(req.method, "status");
        assert_eq!(req.params, Value::Null);
    }

    // ---------------- プロトコル単体 ----------------

    #[test]
    fn test_status_without_store() {
        let (ctx, _) = test_ctx(None, None);
        let result = result_of(call(&ctx, "status", json!({})));
        assert_eq!(result["connected"], json!(false));
        assert_eq!(result["port_name"], Value::Null);
        assert_eq!(result["total_bytes"], json!(0));
        assert_eq!(result["protocol"], json!(PROTOCOL_VERSION));
        assert_eq!(result["app_version"], json!(env!("CARGO_PKG_VERSION")));
    }

    /// 自動再接続の待機中は `reconnecting: true`（GUI と同じフラグを見る）。
    /// 待機していなければ false
    #[test]
    fn test_status_reports_reconnecting() {
        let (ctx, _) = test_ctx(Some(store_with(b"boot")), None);
        assert_eq!(
            result_of(call(&ctx, "status", json!({})))["reconnecting"],
            json!(false)
        );
        ctx.reconnecting.store(true, Ordering::Relaxed);
        let result = result_of(call(&ctx, "status", json!({})));
        assert_eq!(result["reconnecting"], json!(true));
        assert_eq!(result["connected"], json!(false));
        assert_eq!(result["total_bytes"], json!(4));
    }

    #[test]
    fn test_status_reports_total_bytes() {
        let (ctx, _) = test_ctx(Some(store_with(b"hello world")), None);
        let result = result_of(call(&ctx, "status", json!({})));
        assert_eq!(result["total_bytes"], json!(11));
        assert_eq!(result["connected"], json!(false));
    }

    #[test]
    fn test_read_range_basic_and_clamping() {
        let (ctx, _) = test_ctx(Some(store_with(b"0123456789")), None);

        let result = result_of(call(&ctx, "read_range", json!({"offset":2,"length":4})));
        assert_eq!(decode(&result, "base64"), b"2345".to_vec());
        assert_eq!(result["offset"], json!(2));
        assert_eq!(result["length_read"], json!(4));

        // 残量を超える要求は残量でクランプされる（エラーではない）
        let result = result_of(call(&ctx, "read_range", json!({"offset":8,"length":999})));
        assert_eq!(decode(&result, "base64"), b"89".to_vec());
        assert_eq!(result["length_read"], json!(2));

        // 末尾より後ろは空
        let result = result_of(call(&ctx, "read_range", json!({"offset":100,"length":10})));
        assert_eq!(result["length_read"], json!(0));
        assert_eq!(result["base64"], json!(""));
    }

    #[test]
    fn test_read_range_bad_params_and_no_store() {
        let (ctx, _) = test_ctx(Some(store_with(b"abc")), None);
        assert!(error_of(call(&ctx, "read_range", json!({"length": 4}))).contains("offset"));
        assert!(error_of(call(&ctx, "read_range", json!({"offset": 0}))).contains("length"));
        // 負値 / 型違いも bad params
        assert!(
            error_of(call(&ctx, "read_range", json!({"offset":-1,"length":4})))
                .contains("bad params")
        );
        assert!(
            error_of(call(&ctx, "read_range", json!({"offset":0,"length":"4"})))
                .contains("bad params")
        );

        let (empty, _) = test_ctx(None, None);
        assert_eq!(
            error_of(call(&empty, "read_range", json!({"offset":0,"length":4}))),
            "no capture"
        );
    }

    #[test]
    fn test_tail_default_and_window() {
        let (ctx, _) = test_ctx(Some(store_with(b"abcdefghij")), None);

        // 既定 4096 バイト -> 全部返る
        let result = result_of(call(&ctx, "tail", json!({})));
        assert_eq!(decode(&result, "base64"), b"abcdefghij".to_vec());
        assert_eq!(result["offset"], json!(0));
        assert_eq!(result["total_bytes"], json!(10));

        // 末尾 3 バイト
        let result = result_of(call(&ctx, "tail", json!({ "bytes": 3 })));
        assert_eq!(decode(&result, "base64"), b"hij".to_vec());
        assert_eq!(result["offset"], json!(7));

        // 上限を超える要求でも落ちない
        let result = result_of(call(&ctx, "tail", json!({ "bytes": 99_999_999u32 })));
        assert_eq!(result["offset"], json!(0));
        assert_eq!(result["total_bytes"], json!(10));

        // 空ストア
        let (empty_store, _) = test_ctx(Some(store_with(b"")), None);
        let result = result_of(call(&empty_store, "tail", json!({})));
        assert_eq!(result["total_bytes"], json!(0));
        assert_eq!(result["base64"], json!(""));

        // ストア無し
        let (no_store, _) = test_ctx(None, None);
        assert_eq!(error_of(call(&no_store, "tail", json!({}))), "no capture");
    }

    #[test]
    fn test_tail_bad_params() {
        let (ctx, _) = test_ctx(Some(store_with(b"abc")), None);
        assert!(error_of(call(&ctx, "tail", json!({ "bytes": -5 }))).contains("bad params"));
    }

    #[test]
    fn test_find_counts_server_side_without_returning_bulk_data() {
        let mut data = Vec::new();
        for i in 0..500 {
            data.extend_from_slice(format!("tick {i}\n").as_bytes());
            if i % 50 == 0 {
                data.extend_from_slice(b"E: i2c nak\n");
            }
        }
        let (ctx, _) = test_ctx(Some(store_with(&data)), None);

        let result = result_of(call(&ctx, "find", json!({ "pattern": "E: i2c nak" })));
        assert_eq!(result["matches"], json!(10));
        assert_eq!(result["exact"], json!(true));
        assert_eq!(result["complete"], json!(true));
        assert_eq!(result["gap_bytes"], json!(0));
        assert_eq!(result["bytes_scanned"], json!(data.len()));
        assert_eq!(result["hits"][0]["excerpt"], json!("E: i2c nak"));

        // 返答に生データ（base64）が混ざらないこと。これが find の存在意義
        let raw = serde_json::to_string(&result).unwrap();
        assert!(!raw.contains("base64"));
        assert!(
            raw.len() < 2000,
            "the reply must stay small regardless of capture size, got {} bytes",
            raw.len()
        );

        // オフセットは read_range にそのまま渡せる本物であること
        let first = result["first"].as_u64().unwrap();
        let chunk = result_of(call(
            &ctx,
            "read_range",
            json!({ "offset": first, "length": 10 }),
        ));
        assert_eq!(decode(&chunk, "base64"), b"E: i2c nak".to_vec());
    }

    #[test]
    fn test_find_zero_matches_is_reported_with_full_coverage() {
        let (ctx, _) = test_ctx(Some(store_with(b"all good\n")), None);
        let result = result_of(call(&ctx, "find", json!({ "pattern": "PANIC" })));
        assert_eq!(result["matches"], json!(0));
        assert_eq!(
            result["exact"],
            json!(true),
            "a clean scan yields an exact 0"
        );
        assert_eq!(result["bytes_scanned"], json!(9));
    }

    #[test]
    fn test_find_honours_range_and_max_hits() {
        let (ctx, _) = test_ctx(Some(store_with(b"E a\nE b\nE c\n")), None);
        let result = result_of(call(
            &ctx,
            "find",
            json!({ "pattern": "E ", "from": 4, "to": 8 }),
        ));
        assert_eq!(result["matches"], json!(1));
        assert_eq!(result["first"], json!(4));

        let result = result_of(call(
            &ctx,
            "find",
            json!({ "pattern": "E ", "max_hits": 1 }),
        ));
        assert_eq!(result["matches"], json!(3), "the count ignores max_hits");
        assert_eq!(result["hits"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_find_bad_params() {
        let (ctx, _) = test_ctx(Some(store_with(b"abc")), None);
        assert!(error_of(call(&ctx, "find", json!({}))).contains("pattern"));
        assert!(error_of(call(&ctx, "find", json!({ "pattern": "(" }))).contains("invalid pattern"));

        let (no_store, _) = test_ctx(None, None);
        assert_eq!(
            error_of(call(&no_store, "find", json!({ "pattern": "x" }))),
            "no capture"
        );
    }

    #[test]
    fn test_help_is_self_describing_and_needs_no_auth() {
        // トークン設定時でも help は未認証で通る（発見用・データ非含有）
        let (ctx, _) = test_ctx(None, Some("s3cret".to_string()));
        let mut session = Session::new(true);
        let response = handle_request(&Request::new(1, "help", json!({})), &ctx, &mut session);
        let result = result_of(response);
        assert_eq!(result["protocol"], json!(PROTOCOL_VERSION));
        // 全メソッドが仕様に載っている
        let methods = result["methods"].as_object().unwrap();
        for name in [
            "help",
            "auth",
            "status",
            "read_range",
            "tail",
            "send",
            "ports",
            "subscribe",
        ] {
            assert!(methods.contains_key(name), "missing method doc: {}", name);
        }
        assert_eq!(result["limits"]["max_read_length"], json!(MAX_READ_LENGTH));
        // help を呼んでも認証状態にはならない
        assert!(!session.authed);
    }

    #[test]
    fn test_unknown_method() {
        let (ctx, _) = test_ctx(None, None);
        assert_eq!(
            error_of(call(&ctx, "explode", json!({}))),
            "unknown method: explode"
        );
    }

    #[test]
    fn test_send_without_port_is_error() {
        // 実ポートはテストで開けないので、未オープン時のエラー経路を固定する
        let (ctx, emitted) = test_ctx(None, None);
        assert_eq!(
            error_of(call(
                &ctx,
                "send",
                json!({"text":"AT","line_ending":"crlf"})
            )),
            "port not open"
        );
        // 失敗時は活動ログもイベントも出さない
        assert!(emitted.lock().unwrap().is_empty());
        assert!(ctx.activity.lock().unwrap().is_none());
    }

    #[test]
    fn test_send_bad_params_before_port_check() {
        let (ctx, _) = test_ctx(None, None);
        assert!(error_of(call(&ctx, "send", json!({}))).contains("bad params"));
    }

    #[test]
    fn test_record_send_updates_activity_and_emits() {
        let (ctx, emitted) = test_ctx(None, None);
        ctx.record_send(5, "hello".to_string());

        let activity = ctx.activity.lock().unwrap().clone().unwrap();
        assert_eq!(activity.kind, "send");
        assert_eq!(activity.bytes, 5);
        // 実時刻であること（2023-01-01 以降）を確認する。単に > 0 だと now_ms が
        // 定数を返す退行（ミューテーション now_ms->1）を検出できない。
        assert!(
            activity.at_ms > 1_672_531_200_000,
            "at_ms must be a real epoch ms"
        );
        // ポーリング側（bridge_status）でも送信内容を復元できる
        assert_eq!(activity.preview, "hello");

        let events = emitted.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].preview, "hello");
        assert_eq!(events[0].bytes, 5);
    }

    #[test]
    fn test_ports_returns_list() {
        let (ctx, _) = test_ctx(None, None);
        let result = result_of(call(&ctx, "ports", json!({})));
        // 実機の有無に依存しないので、配列であることだけ確認する
        assert!(result["ports"].is_array());
    }

    #[test]
    fn test_subscribe_params() {
        let (ctx, _) = test_ctx(None, None);
        let result = result_of(call(&ctx, "subscribe", json!({})));
        assert_eq!(result["subscribed"], json!(true));
        assert_eq!(result["from_offset"], Value::Null);

        let result = result_of(call(&ctx, "subscribe", json!({ "from_offset": 42 })));
        assert_eq!(result["from_offset"], json!(42));

        assert!(
            error_of(call(&ctx, "subscribe", json!({ "from_offset": -1 }))).contains("bad params")
        );
    }

    // ---------------- 認証 ----------------

    #[test]
    fn test_auth_required_when_token_configured() {
        let (ctx, _) = test_ctx(Some(store_with(b"abc")), Some("s3cret".to_string()));
        let mut session = Session::new(true);
        assert!(!session.authed);

        // 未認証では他メソッドは弾かれる
        let response = handle_request(&Request::new(1, "status", json!({})), &ctx, &mut session);
        assert_eq!(error_of(response), "unauthorized");

        // 誤ったトークン
        let response = handle_request(
            &Request::new(2, "auth", json!({ "token": "wrong" })),
            &ctx,
            &mut session,
        );
        assert_eq!(error_of(response), "invalid token");
        assert!(!session.authed);

        // 正しいトークン -> 以降通る
        let response = handle_request(
            &Request::new(3, "auth", json!({ "token": "s3cret" })),
            &ctx,
            &mut session,
        );
        assert!(response.is_ok());
        assert!(session.authed);
        let response = handle_request(&Request::new(4, "status", json!({})), &ctx, &mut session);
        assert!(response.is_ok());
    }

    #[test]
    fn test_auth_is_noop_without_token() {
        let (ctx, _) = test_ctx(None, None);
        let mut session = Session::new(false);
        assert!(session.authed);
        let response = handle_request(
            &Request::new(1, "auth", json!({ "token": "anything" })),
            &ctx,
            &mut session,
        );
        assert!(response.is_ok());
    }

    #[test]
    fn test_response_json_shapes() {
        let ok = Response::ok(json!(1), json!({ "a": 1 })).into_json();
        assert_eq!(ok["id"], json!(1));
        assert_eq!(ok["ok"], json!(true));
        assert_eq!(ok["result"]["a"], json!(1));

        let err = Response::error(Value::Null, "boom").into_json();
        assert_eq!(err["id"], Value::Null);
        assert_eq!(err["ok"], json!(false));
        assert_eq!(err["error"], json!("boom"));
        assert!(err.get("result").is_none());
    }

    #[test]
    fn test_is_ok_reflects_outcome() {
        // is_ok が常に true を返す退行を検出する（subscribe の push 判定に効く）
        assert!(Response::ok(json!(1), json!({})).is_ok());
        assert!(!Response::error(Value::Null, "x").is_ok());
    }

    #[test]
    fn test_size_constants() {
        // クランプ・フレーム上限の定数値を固定する。相対量のテストだけでは
        // `1024 * 1024` → `1024 + 1024` のような定数退行を検出できない。
        assert_eq!(MAX_READ_LENGTH, 1_048_576); // 1 MiB
        assert_eq!(SUBSCRIBE_MAX_FRAME, 262_144); // 256 KiB
        assert_eq!(MAX_LINE_BYTES, 4_194_304); // 4 MiB
    }

    // ---------------- 統合（実ソケット） ----------------

    struct Client {
        reader: BufReader<TcpStream>,
        writer: TcpStream,
    }

    impl Client {
        fn connect(addr: SocketAddr) -> Self {
            let stream = TcpStream::connect(addr).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let writer = stream.try_clone().unwrap();
            Self {
                reader: BufReader::new(stream),
                writer,
            }
        }

        fn send(&mut self, line: &str) {
            self.writer.write_all(line.as_bytes()).unwrap();
            self.writer.write_all(b"\n").unwrap();
            self.writer.flush().unwrap();
        }

        /// 1 行読む。EOF なら None。
        fn recv(&mut self) -> Option<Value> {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => None,
                Ok(_) => Some(serde_json::from_str(&line).expect("valid json line")),
                Err(e) => panic!("read failed: {}", e),
            }
        }
    }

    fn start_test_server(
        store: Option<Arc<DataStore>>,
    ) -> (BridgeServer, Arc<BridgeCtx>, Arc<AtomicUsize>) {
        let (ctx, _) = test_ctx(store, None);
        let connections = Arc::new(AtomicUsize::new(0));
        // ポート 0 = OS 割り当て（テストの並行実行で衝突しない）
        let server = BridgeServer::start(0, ctx.clone(), connections.clone()).expect("start");
        (server, ctx, connections)
    }

    #[test]
    fn test_server_binds_loopback_only() {
        let (server, _ctx, _c) = start_test_server(None);
        let addr = server.local_addr();
        assert!(addr.ip().is_loopback(), "must bind loopback, got {}", addr);
        assert_eq!(addr.ip(), Ipv4Addr::LOCALHOST);
        assert_ne!(addr.port(), 0);
    }

    #[test]
    fn test_integration_status_read_range_tail() {
        let store = store_with(b"ABCDEFGHIJ");
        let (server, _ctx, _c) = start_test_server(Some(store));
        let mut client = Client::connect(server.local_addr());

        client.send(r#"{"id":1,"method":"status"}"#);
        let response = client.recv().unwrap();
        assert_eq!(response["id"], json!(1));
        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["result"]["total_bytes"], json!(10));
        assert_eq!(response["result"]["protocol"], json!(PROTOCOL_VERSION));

        client.send(r#"{"id":2,"method":"read_range","params":{"offset":1,"length":3}}"#);
        let response = client.recv().unwrap();
        assert_eq!(response["id"], json!(2));
        assert_eq!(decode(&response["result"], "base64"), b"BCD".to_vec());
        assert_eq!(response["result"]["length_read"], json!(3));

        client.send(r#"{"id":3,"method":"tail","params":{"bytes":4}}"#);
        let response = client.recv().unwrap();
        assert_eq!(decode(&response["result"], "base64"), b"GHIJ".to_vec());
        assert_eq!(response["result"]["offset"], json!(6));

        // 壊れた JSON -> id null の parse error
        client.send("{not json");
        let response = client.recv().unwrap();
        assert_eq!(response["id"], Value::Null);
        assert_eq!(response["ok"], json!(false));
        assert_eq!(response["error"], json!("parse error"));

        // 未知メソッド
        client.send(r#"{"id":9,"method":"nope"}"#);
        let response = client.recv().unwrap();
        assert_eq!(response["ok"], json!(false));
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("unknown method"));
    }

    #[test]
    fn test_integration_send_without_port() {
        let (server, _ctx, _c) = start_test_server(None);
        let mut client = Client::connect(server.local_addr());
        client.send(r#"{"id":1,"method":"send","params":{"text":"AT","line_ending":"crlf"}}"#);
        let response = client.recv().unwrap();
        assert_eq!(response["ok"], json!(false));
        assert_eq!(response["error"], json!("port not open"));
    }

    #[test]
    fn test_integration_subscribe_pushes_new_data() {
        let store = store_with(b"");
        let (server, ctx, _c) = start_test_server(Some(store));

        // 1 本目: 通常のリクエスト/レスポンス
        let mut request_client = Client::connect(server.local_addr());
        request_client.send(r#"{"id":1,"method":"status"}"#);
        assert_eq!(request_client.recv().unwrap()["ok"], json!(true));

        // 2 本目: push モードへ切り替え
        let mut push_client = Client::connect(server.local_addr());
        push_client.send(r#"{"id":2,"method":"subscribe","params":{"from_offset":0}}"#);
        let ack = push_client.recv().unwrap();
        assert_eq!(ack["id"], json!(2));
        assert_eq!(ack["result"]["subscribed"], json!(true));

        // 新しいデータを投入 -> push フレームが届く
        ctx.store().unwrap().push_test_data(b"streamed!");
        let frame = push_client.recv().unwrap();
        assert_eq!(frame["event"], json!("data"));
        assert_eq!(frame["offset"], json!(0));
        assert_eq!(decode(&frame, "base64"), b"streamed!".to_vec());

        // 1 本目は引き続き普通に応答できる
        request_client.send(r#"{"id":3,"method":"status"}"#);
        let response = request_client.recv().unwrap();
        assert_eq!(response["result"]["total_bytes"], json!(9));
    }

    #[test]
    fn test_integration_subscribe_emits_reset_on_store_swap() {
        let (server, ctx, _c) = start_test_server(Some(store_with(b"old")));
        let mut client = Client::connect(server.local_addr());
        client.send(r#"{"id":1,"method":"subscribe"}"#);
        assert!(client.recv().unwrap()["ok"].as_bool().unwrap());

        // 初回データ
        let frame = client.recv().unwrap();
        assert_eq!(frame["event"], json!("data"));
        assert_eq!(decode(&frame, "base64"), b"old".to_vec());

        // ストアを差し替え（ポート再オープン / Clear 相当）
        let fresh = store_with(b"new capture");
        *ctx.data_store.lock().unwrap() = Some(fresh);

        let frame = client.recv().unwrap();
        assert_eq!(frame["event"], json!("reset"));
        let frame = client.recv().unwrap();
        assert_eq!(frame["event"], json!("data"));
        assert_eq!(decode(&frame, "base64"), b"new capture".to_vec());
    }

    #[test]
    fn test_integration_connection_limit() {
        let (server, _ctx, connections) = start_test_server(None);
        let addr = server.local_addr();

        let mut clients = Vec::new();
        for i in 0..MAX_CONNECTIONS {
            let mut client = Client::connect(addr);
            // ハンドシェイク代わりに 1 往復してサーバ側の受理を確定させる
            client.send(&format!(r#"{{"id":{},"method":"status"}}"#, i));
            assert_eq!(client.recv().unwrap()["ok"], json!(true));
            clients.push(client);
        }
        assert_eq!(connections.load(Ordering::SeqCst), MAX_CONNECTIONS);

        // 5 本目: エラー行を 1 行受け取って閉じられる
        let mut rejected = Client::connect(addr);
        let response = rejected.recv().unwrap();
        assert_eq!(response["ok"], json!(false));
        assert_eq!(response["error"], json!("too many connections"));
        assert!(
            rejected.recv().is_none(),
            "server must close the connection"
        );
    }

    #[test]
    fn test_integration_stop_closes_connections() {
        let (mut server, _ctx, _c) = start_test_server(Some(store_with(b"xyz")));
        let mut client = Client::connect(server.local_addr());
        client.send(r#"{"id":1,"method":"status"}"#);
        assert_eq!(client.recv().unwrap()["ok"], json!(true));

        let addr = server.local_addr();
        server.stop();

        // 既存接続は閉じられる（EOF）
        assert!(client.recv().is_none(), "connection must be closed on stop");

        // 新規接続も受け付けない（拒否されるか、受理されても誰も応答しない）
        if let Ok(stream) = TcpStream::connect(addr) {
            stream
                .set_read_timeout(Some(Duration::from_millis(300)))
                .unwrap();
            let mut writer = stream.try_clone().unwrap();
            let _ = writer.write_all(b"{\"id\":1,\"method\":\"status\"}\n");
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            let answered = matches!(reader.read_line(&mut line), Ok(n) if n > 0);
            assert!(!answered, "stopped server must not answer: {}", line);
        }
    }

    // ---------------- digest / watch（docs/26 段階 2） ----------------

    /// 索引へ直接流し込む。実運用では `IndexThread` がこれを行う
    fn index_feed(ctx: &BridgeCtx, text: &str, at: u64, now_ms: u64) -> u64 {
        ctx.index.ingest(at, text.as_bytes(), now_ms);
        at + text.len() as u64
    }

    fn text_of(result: &Value) -> String {
        result
            .get("text")
            .and_then(Value::as_str)
            .expect("envelope must carry text")
            .to_string()
    }

    #[test]
    fn test_digest_answers_from_the_index_without_raw_bytes() {
        let (ctx, _) = test_ctx(Some(store_with(b"")), None);
        let mut at = 0;
        for i in 0..300 {
            at = index_feed(&ctx, &format!("T: {}.4 C\n", 20 + i % 5), at, 1000);
        }
        at = index_feed(&ctx, "E: i2c nak addr=0x48\n", at, 1000);
        index_feed(&ctx, "W: rx overflow q=64\n", at, 1000);

        let result = result_of(call(&ctx, "digest", json!({})));
        let text = text_of(&result);

        // 生データは 1 バイトも返さない（find と同じく、これが存在意義）
        assert!(!text.contains("base64"), "{text}");
        assert!(result.get("base64").is_none());
        // 300 行が 1 テンプレートに畳まれる
        assert!(text.contains("T: <F"), "{text}");
        // 稀な事象は支配的な行より先に、正確な件数とオフセット付きで出る
        let rare_at = text.find("RARE").expect("RARE section");
        let top_at = text.find("TOP").expect("TOP section");
        assert!(
            rare_at < top_at,
            "rare events must come before top-N:\n{text}"
        );
        assert!(text.contains("i2c nak addr=<X>"), "{text}");
        assert!(text.contains("@off"), "{text}");
        // 追随用のカーソルが必ず付く
        assert!(text.contains("\ncursor v1:g"), "{text}");
    }

    #[test]
    fn test_digest_never_prints_a_bare_zero() {
        let (ctx, _) = test_ctx(Some(store_with(b"")), None);
        index_feed(&ctx, "hello\n", 0, 1000);
        let text = text_of(&result_of(call(&ctx, "digest", json!({}))));
        let alarm = text
            .lines()
            .find(|l| l.starts_with("ALARM"))
            .expect("ALARM is mandatory");
        // 「見つからなかった」と「見に行けなかった」を混同させない
        assert!(alarm.contains("0 err"), "{alarm}");
        assert!(
            alarm.contains("exact") || alarm.contains("AT LEAST"),
            "{alarm}"
        );
        assert!(alarm.contains("coverage"), "{alarm}");
    }

    #[test]
    fn test_digest_size_does_not_grow_with_the_capture() {
        let small = {
            let (ctx, _) = test_ctx(Some(store_with(b"")), None);
            index_feed(&ctx, "T: 1.0 C\n", 0, 1000);
            text_of(&result_of(call(&ctx, "digest", json!({}))))
                .chars()
                .count()
        };
        let large = {
            let (ctx, _) = test_ctx(Some(store_with(b"")), None);
            let mut at = 0;
            for i in 0..20_000 {
                at = index_feed(&ctx, &format!("T: {}.0 C\n", i % 7), at, 1000);
            }
            text_of(&result_of(call(&ctx, "digest", json!({}))))
                .chars()
                .count()
        };
        // 2 万行を索引しても応答は同じ桁に収まる（トークン/洞察であって、
        // バイト/呼び出しではない）
        assert!(large < small * 3, "small={small} large={large}");
        assert!(large <= crate::insight::render::DEFAULT_DIGEST_CHARS);
    }

    #[test]
    fn test_digest_respects_max_chars_and_reports_what_it_dropped() {
        let (ctx, _) = test_ctx(Some(store_with(b"")), None);
        let mut at = 0;
        for i in 0..80 {
            at = index_feed(
                &ctx,
                &format!("k{i}: many distinct shapes here\n"),
                at,
                1000,
            );
        }
        let result = result_of(call(&ctx, "digest", json!({ "max_chars": 600, "top": 50 })));
        let text = text_of(&result);
        assert!(text.chars().count() <= 600, "{}", text.chars().count());
        assert!(text.contains("withheld"), "{text}");
        assert_eq!(result["budget"]["truncated"], json!(true));
        assert!(!result["budget"]["withheld"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_digest_time_window_without_timestamps_fails_loudly() {
        let (ctx, _) = test_ctx(Some(store_with(b"")), None);
        index_feed(&ctx, "A: 1\n", 0, 1000);
        // 時刻索引が空なので、時刻を捏造せずに失敗する
        let err = error_of(call(&ctx, "digest", json!({ "window": "30s" })));
        assert!(err.contains("no timing data"), "{err}");
        assert!(error_of(call(&ctx, "digest", json!({ "window": "nope" }))).contains("bad window"));
    }

    #[test]
    fn test_digest_offset_window_reports_the_resolved_range() {
        let (ctx, _) = test_ctx(Some(store_with(b"")), None);
        let mut at = 0;
        for _ in 0..10 {
            at = index_feed(&ctx, "A: 1\n", at, 1000);
        }
        let text = text_of(&result_of(call(
            &ctx,
            "digest",
            json!({ "window": "off:20..40" }),
        )));
        // エージェントが serial_read_range で再導出できる形で範囲を出す
        assert!(
            text.contains("off 20..40") || text.contains("off 20.."),
            "{text}"
        );
    }

    #[test]
    fn test_watch_returns_only_the_delta_and_a_reusable_cursor() {
        let (ctx, _) = test_ctx(Some(store_with(b"")), None);
        let mut at = index_feed(&ctx, "T: 1.0 C\n", 0, 1000);

        let first = result_of(call(&ctx, "watch", json!({})));
        let cursor = first["data"]["cursor"].as_str().unwrap().to_string();
        assert!(
            text_of(&first).contains("no cursor given"),
            "{}",
            text_of(&first)
        );

        index_feed(&ctx, "T: 2.0 C\nW: rx overflow\n", at, 2000);
        at = 0;
        let _ = at;

        let second = result_of(call(&ctx, "watch", json!({ "cursor": cursor })));
        let text = text_of(&second);
        assert!(text.starts_with("watch gen="), "{text}");
        assert_eq!(second["data"]["lines"], json!(2));
        assert_eq!(second["data"]["warn"], json!(1));
        assert_eq!(second["meta"]["exact_baseline"], json!(true));
        // 件数は「窓での増分」であって累計ではない（t1 は全期間 2 回だが +1）
        assert!(text.contains("t1 +1 \"T: <F> C\""), "{text}");
        // 窓の外を指しうる first / min / max は差分では出さない
        assert!(!text.contains("min 1@off"), "{text}");

        // 何も来ていなければ差分はゼロ（ただし裸のゼロにはしない）
        let cursor2 = second["data"]["cursor"].as_str().unwrap().to_string();
        let third = result_of(call(&ctx, "watch", json!({ "cursor": cursor2 })));
        assert_eq!(third["data"]["lines"], json!(0));
        assert!(text_of(&third).contains("ALARM +0 err"));
        assert!(text_of(&third).contains("coverage"));
    }

    #[test]
    fn test_watch_with_a_stale_generation_is_not_an_error() {
        let (ctx, _) = test_ctx(Some(store_with(b"")), None);
        index_feed(&ctx, "A: 1\n", 0, 1000);
        let cursor = result_of(call(&ctx, "watch", json!({})))["data"]["cursor"]
            .as_str()
            .unwrap()
            .to_string();

        // Clear / ポート再オープン相当
        ctx.index.reset(0);
        index_feed(&ctx, "B: 2\n", 0, 2000);

        let result = result_of(call(&ctx, "watch", json!({ "cursor": cursor })));
        let text = text_of(&result);
        assert_eq!(result["meta"]["gen_changed"], json!(true));
        assert!(text.contains("generation changed"), "{text}");
        assert!(text.contains("restarted from off"), "{text}");
    }

    #[test]
    fn test_watch_rejects_a_malformed_cursor() {
        let (ctx, _) = test_ctx(Some(store_with(b"")), None);
        assert!(error_of(call(&ctx, "watch", json!({ "cursor": "junk" }))).contains("bad cursor"));
        assert!(error_of(call(&ctx, "watch", json!({ "cursor": 7 })))
            .contains("cursor must be a string"));
    }

    #[test]
    fn test_watch_long_poll_returns_promptly_when_data_is_already_there() {
        let (ctx, _) = test_ctx(Some(store_with(b"")), None);
        index_feed(&ctx, "A: 1\n", 0, 1000);
        let cursor = result_of(call(&ctx, "watch", json!({})))["data"]["cursor"]
            .as_str()
            .unwrap()
            .to_string();
        index_feed(&ctx, "A: 2\n", 5, 2000);

        let started = Instant::now();
        let result = result_of(call(
            &ctx,
            "watch",
            json!({ "cursor": cursor, "wait_ms": 3000 }),
        ));
        // 既に進んでいるので待たずに返る
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(result["data"]["lines"], json!(1));
    }

    #[test]
    fn test_status_reports_the_index_state() {
        let (ctx, _) = test_ctx(Some(store_with(b"hello")), None);
        index_feed(&ctx, "hello", 0, 1000);
        let result = result_of(call(&ctx, "status", json!({})));
        assert_eq!(result["protocol"], json!(2));
        assert_eq!(result["indexed_bytes"], json!(5));
        assert_eq!(result["coverage_pct"], json!(100));
        assert_eq!(result["index_running"], json!(false));
    }

    #[test]
    fn test_help_documents_digest_and_watch() {
        let (ctx, _) = test_ctx(None, None);
        let result = result_of(call(&ctx, "help", json!({})));
        assert!(result["methods"]["digest"].is_object());
        assert!(result["methods"]["watch"].is_object());
        assert_eq!(result["protocol"], json!(PROTOCOL_VERSION));
    }

    // ---------------- sample / template detail（docs/26 段階 3） ----------------

    /// ストアと索引の両方に同じデータを入れる（実運用では IndexThread の役目）
    fn feed_both(ctx: &BridgeCtx, store: &Arc<DataStore>, text: &str, at: u64, now_ms: u64) -> u64 {
        store.push_test_data(text.as_bytes());
        ctx.index.ingest(at, text.as_bytes(), now_ms);
        at + text.len() as u64
    }

    #[test]
    fn test_sample_quotes_real_lines_with_offsets() {
        let store = store_with(b"");
        let (ctx, _) = test_ctx(Some(store.clone()), None);
        let mut at = 0;
        for i in 0..40 {
            at = feed_both(&ctx, &store, &format!("L{i}: hello\n"), at, 1000);
        }
        let result = result_of(call(&ctx, "sample", json!({ "n": 3 })));
        let text = text_of(&result);
        assert!(text.starts_with("sample gen="), "{text}");
        // 末尾 3 行が、それぞれ検証できるオフセット付きで出る
        assert!(text.contains("L39: hello"), "{text}");
        assert!(text.contains("L37: hello"), "{text}");
        assert!(!text.contains("L36: hello"), "{text}");
        assert_eq!(result["data"]["lines"].as_array().unwrap().len(), 3);
        let first_offset = result["data"]["lines"][0]["offset"].as_u64().unwrap();
        // オフセットは read_range にそのまま渡せる
        let raw = result_of(call(
            &ctx,
            "read_range",
            json!({ "offset": first_offset, "length": 4 }),
        ));
        assert_eq!(decode(&raw, "base64"), b"L37:".to_vec());
    }

    #[test]
    fn test_sample_folds_repeats_without_rounding() {
        let store = store_with(b"");
        let (ctx, _) = test_ctx(Some(store.clone()), None);
        let mut at = 0;
        for _ in 0..7 {
            at = feed_both(&ctx, &store, "same line\n", at, 1000);
        }
        let text = text_of(&result_of(call(&ctx, "sample", json!({ "n": 20 }))));
        assert!(text.contains("[x7]"), "{text}");
        // 畳まないよう頼めば 7 行出る
        let raw = text_of(&result_of(call(
            &ctx,
            "sample",
            json!({ "n": 20, "collapse_repeats": false }),
        )));
        assert_eq!(raw.matches("same line").count(), 7, "{raw}");
    }

    #[test]
    fn test_sample_modes_and_bad_arguments() {
        let store = store_with(b"");
        let (ctx, _) = test_ctx(Some(store.clone()), None);
        let mut at = 0;
        for i in 0..200 {
            at = feed_both(&ctx, &store, &format!("L{i}: x\n"), at, 1000);
        }
        assert!(text_of(&result_of(call(
            &ctx,
            "sample",
            json!({ "mode": "head", "n": 2 })
        )))
        .contains("L0: x"));
        let around = text_of(&result_of(call(
            &ctx,
            "sample",
            json!({ "mode": "around", "offset": 100, "n": 3 }),
        )));
        assert!(around.contains("mode=around offset=100"), "{around}");
        let spread = text_of(&result_of(call(
            &ctx,
            "sample",
            json!({ "mode": "stratified", "n": 4 }),
        )));
        assert!(spread.contains("mode=stratified"), "{spread}");
        // 全体に散らばっている（先頭と末尾の両方から採れている）
        assert!(
            spread.contains("L0: x") || spread.contains("L1: x"),
            "{spread}"
        );

        assert!(
            error_of(call(&ctx, "sample", json!({ "mode": "around" }))).contains("needs 'offset'")
        );
        assert!(
            error_of(call(&ctx, "sample", json!({ "mode": "nope" }))).contains("tail|head|around")
        );
        assert!(
            error_of(call(&ctx, "sample", json!({ "mode": "by_template" })))
                .contains("needs 'template'")
        );
    }

    #[test]
    fn test_sample_by_template_reads_the_offsets_the_index_kept() {
        let store = store_with(b"");
        let (ctx, _) = test_ctx(Some(store.clone()), None);
        let mut at = 0;
        for i in 0..10 {
            at = feed_both(&ctx, &store, &format!("T: {i}.0 C\n"), at, 1000);
            at = feed_both(&ctx, &store, &format!("S: state={i}\n"), at, 1000);
        }
        let digest = text_of(&result_of(call(&ctx, "digest", json!({}))));
        assert!(digest.contains("T: <F"), "{digest}");

        let result = result_of(call(
            &ctx,
            "sample",
            json!({ "mode": "by_template", "template": "t1", "n": 4 }),
        ));
        let text = text_of(&result);
        assert!(text.contains("mode=by_template t1 x10"), "{text}");
        // 索引が覚えているのは直近 4 件だけ。数を水増しせず、そう言う
        assert!(text.contains("showing the last 4 occurrence"), "{text}");
        for line in result["data"]["lines"].as_array().unwrap() {
            assert!(line["text"].as_str().unwrap().starts_with("T: "), "{text}");
        }
        assert!(error_of(call(
            &ctx,
            "sample",
            json!({ "mode": "by_template", "template": "t999" })
        ))
        .contains("unknown template t999"));
    }

    #[test]
    fn test_sample_without_a_capture_is_an_explicit_error() {
        let (ctx, _) = test_ctx(None, None);
        assert_eq!(error_of(call(&ctx, "sample", json!({}))), "no capture");
    }

    #[test]
    fn test_digest_template_detail_shows_every_field_with_offsets() {
        let store = store_with(b"");
        let (ctx, _) = test_ctx(Some(store.clone()), None);
        let mut at = 0;
        for i in 0..5 {
            at = feed_both(
                &ctx,
                &store,
                &format!("T: {}.0 C rh={}%\n", 20 + i, 40),
                at,
                1000,
            );
        }
        let result = result_of(call(&ctx, "digest", json!({ "template": "t1" })));
        let text = text_of(&result);
        assert!(text.starts_with("detail gen="), "{text}");
        assert!(text.contains("t1 x5 (100.0% of 5 indexed lines)"), "{text}");
        assert!(text.contains("SLOT  2 field(s)"), "{text}");
        // 変動する項目は min/max とその発生オフセット、固定の項目は constant
        assert!(text.contains("min 20@off=0"), "{text}");
        assert!(text.contains("constant 40"), "{text}");
        // 実際の行も出る（索引が覚えているオフセットから読み直す）
        assert!(text.contains("LINE  @off="), "{text}");
        assert_eq!(result["data"]["id"], json!(1));
        assert_eq!(result["data"]["slots"].as_array().unwrap().len(), 2);

        // 数値 id でも受け付ける
        assert!(
            text_of(&result_of(call(&ctx, "digest", json!({ "template": 1 }))))
                .starts_with("detail gen=")
        );
        // 存在しない id は、追い出しの可能性まで含めて説明する
        let err = error_of(call(&ctx, "digest", json!({ "template": "t42" })));
        assert!(err.contains("unknown template t42"), "{err}");
        assert!(err.contains("evicted"), "{err}");
    }

    // ---------------- wait_for（走査はここ。docs/26 段階 4 / DEBT-8） ----------------

    #[test]
    fn test_wait_for_matches_data_already_present_after_from() {
        let store = store_with(b"OLD OK\nAT\nOK\n");
        let (ctx, _) = test_ctx(Some(store), None);
        // from より前の "OLD OK" は対象外
        let result = result_of(call(
            &ctx,
            "wait_for",
            json!({ "pattern": "OK", "from": 10, "timeout_ms": 0 }),
        ));
        assert_eq!(result["matched"], json!(true));
        assert_eq!(result["offset"], json!(10));
        assert!(result["excerpt"].as_str().unwrap().contains("OK"));
        assert_eq!(result["reset"], json!(false));
    }

    #[test]
    fn test_wait_for_offset_is_byte_accurate_with_invalid_utf8() {
        // lossy 変換基準だと 1 バイトが U+FFFD(3 バイト) に膨らんでオフセットがずれる。
        // 走査は生バイトに対して行うので、返るオフセットは read_range と噛み合う
        let mut data = vec![0xffu8; 10];
        data.extend_from_slice(b"OK\n");
        let store = store_with(&data);
        let (ctx, _) = test_ctx(Some(store), None);
        let result = result_of(call(
            &ctx,
            "wait_for",
            json!({ "pattern": "OK", "from": 0, "timeout_ms": 0 }),
        ));
        assert_eq!(result["offset"], json!(10));
    }

    #[test]
    fn test_wait_for_multiline_anchor() {
        let store = store_with(b"line1\nERROR: x\n");
        let (ctx, _) = test_ctx(Some(store), None);
        let result = result_of(call(
            &ctx,
            "wait_for",
            json!({ "pattern": "^ERROR", "from": 0, "timeout_ms": 0 }),
        ));
        assert_eq!(result["offset"], json!(6));
    }

    #[test]
    fn test_wait_for_sees_data_that_arrives_while_it_waits() {
        let store = store_with(b"");
        let (ctx, _) = test_ctx(Some(store.clone()), None);
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            store.push_test_data(b"PONG 42\n");
        });
        let started = Instant::now();
        let result = result_of(call(
            &ctx,
            "wait_for",
            json!({ "pattern": "PONG (\\d+)", "from": 0, "timeout_ms": 900 }),
        ));
        writer.join().unwrap();
        assert_eq!(result["matched"], json!(true));
        assert_eq!(result["offset"], json!(0));
        // 待ち上限より十分早く返る（着弾したら即座に）
        assert!(started.elapsed() < Duration::from_millis(800));
    }

    #[test]
    fn test_wait_for_timeout_reports_the_tail_it_saw() {
        let store = store_with(b"before");
        let (ctx, _) = test_ctx(Some(store.clone()), None);
        store.push_test_data(b"noise");
        let result = result_of(call(
            &ctx,
            "wait_for",
            json!({ "pattern": "NEVER", "from": 6, "timeout_ms": 20 }),
        ));
        assert_eq!(result["matched"], json!(false));
        assert_eq!(result["offset"], Value::Null);
        // 末尾は「from 以降に届いた分」だけ: 送信前のデータを返信と誤認させない
        assert_eq!(decode(&result, "tail_base64"), b"noise".to_vec());
        assert_eq!(result["tail_offset"], json!(6));
        assert_eq!(result["scanned_to"], json!(11));
    }

    #[test]
    fn test_wait_for_next_from_overlaps_so_a_split_match_is_still_found() {
        let store = store_with(b"");
        let (ctx, _) = test_ctx(Some(store.clone()), None);
        store.push_test_data(b"PON");
        let first = result_of(call(
            &ctx,
            "wait_for",
            json!({ "pattern": "PONG", "from": 0, "timeout_ms": 0 }),
        ));
        assert_eq!(first["matched"], json!(false));
        let next_from = first["next_from"].as_u64().unwrap();
        // 重なりのぶん戻っているので、続きが来れば跨いだマッチを拾える
        assert_eq!(next_from, 0);

        store.push_test_data(b"G ok\n");
        let second = result_of(call(
            &ctx,
            "wait_for",
            json!({ "pattern": "PONG", "from": next_from, "timeout_ms": 0 }),
        ));
        assert_eq!(second["matched"], json!(true));
        assert_eq!(second["offset"], json!(0));
    }

    #[test]
    fn test_wait_for_reports_a_capture_reset_instead_of_waiting_forever() {
        let store = store_with(b"short");
        let (ctx, _) = test_ctx(Some(store), None);
        // 世代交代でオフセットが巻き戻った状況（from がストア末尾より先）
        let result = result_of(call(
            &ctx,
            "wait_for",
            json!({ "pattern": "short", "from": 100_000, "timeout_ms": 0 }),
        ));
        assert_eq!(result["reset"], json!(true));
        assert_eq!(result["matched"], json!(true));
        assert_eq!(result["offset"], json!(0));
    }

    #[test]
    fn test_wait_for_bad_params() {
        let (ctx, _) = test_ctx(Some(store_with(b"x")), None);
        assert!(error_of(call(&ctx, "wait_for", json!({}))).contains("pattern"));
        assert!(
            error_of(call(&ctx, "wait_for", json!({ "pattern": "(" }))).contains("invalid pattern")
        );
        let (no_store, _) = test_ctx(None, None);
        assert_eq!(
            error_of(call(&no_store, "wait_for", json!({ "pattern": "x" }))),
            "no capture"
        );
    }

    #[test]
    fn test_wait_for_never_blocks_longer_than_the_server_cap() {
        let (ctx, _) = test_ctx(Some(store_with(b"")), None);
        let started = Instant::now();
        // 10 分を頼まれても上限で切る（クライアントの 5s タイムアウトを越えない）
        let result = result_of(call(
            &ctx,
            "wait_for",
            json!({ "pattern": "NEVER", "from": 0, "timeout_ms": 600_000 }),
        ));
        assert_eq!(result["matched"], json!(false));
        assert!(
            started.elapsed() < Duration::from_millis(WAIT_SERVER_MAX_MS + 500),
            "blocked for {:?}",
            started.elapsed()
        );
    }

    /// 送信の成功経路: 受信ワーカー役のスレッドが偽ポートで要求を処理し、
    /// bytes_written / offset_before（取り出し時の公開済み位置）/ 活動ログが揃う。
    /// 切断後は status が connected=false になる（ADR-14 で単体検証が可能になった経路）
    #[test]
    fn test_send_through_the_port_worker_reports_receipt_and_activity() {
        use crate::serial::tx::fake::{FakePort, WriteMode};
        use crate::serial::tx::OpenPort;

        let fake = FakePort::new(115_200, WriteMode::Accept);
        let port = Arc::new(OpenPort::new("COM99", Box::new(fake.clone())));
        let mut parked = port.take_io().unwrap();
        let worker = std::thread::spawn(move || {
            for _ in 0..200 {
                parked.engine.step(parked.io.as_mut(), &parked.rx, 42);
                std::thread::sleep(Duration::from_millis(1));
            }
        });

        let (ctx, emitted) = test_ctx(Some(store_with(b"1234")), None);
        *ctx.port.lock().unwrap() = Some(port.clone());
        let result = result_of(call(
            &ctx,
            "send",
            json!({ "text": "AT", "line_ending": "crlf" }),
        ));
        assert_eq!(result["bytes_written"], json!(4));
        assert_eq!(result["offset_before"], json!(42));
        assert_eq!(fake.written(), b"AT\r\n");
        assert_eq!(ctx.activity.lock().unwrap().clone().unwrap().bytes, 4);
        assert_eq!(emitted.lock().unwrap().len(), 1);

        let status = result_of(call(&ctx, "status", json!({})));
        assert_eq!(status["connected"], json!(true));
        assert_eq!(status["port_name"], json!("COM99"));
        port.mark_disconnected();
        let status = result_of(call(&ctx, "status", json!({})));
        assert_eq!(status["connected"], json!(false));
        worker.join().unwrap();
    }

    #[test]
    fn test_send_without_port_does_not_touch_the_store() {
        // ポートが無いので送信自体は失敗するが、no-port でも壊れないことを固定する
        let (ctx, _) = test_ctx(Some(store_with(b"1234")), None);
        assert_eq!(
            error_of(call(&ctx, "send", json!({ "text": "AT" }))),
            "port not open"
        );
    }

    #[test]
    fn test_digest_says_when_the_stream_is_not_line_oriented() {
        let (ctx, _) = test_ctx(Some(store_with(b"")), None);
        let blob: Vec<u8> = (0..4096u32).map(|i| (i % 256) as u8).collect();
        ctx.index.ingest(0, &blob, 1000);
        let result = result_of(call(&ctx, "digest", json!({})));
        let text = text_of(&result);
        // 黙って「N 行」と言わない。固定枠（INDEX）なので予算に押し出されもしない
        assert!(text.contains("does not look line oriented"), "{text}");
        assert!(text.contains("NOT meaningful"), "{text}");
        assert_eq!(result["meta"]["line_oriented"], json!(false));

        // 通常のテキストでは出さない（誤検知でノイズを増やさない）
        let (text_ctx, _) = test_ctx(Some(store_with(b"")), None);
        text_ctx
            .index
            .ingest(0, "温度: 23.4 度\n".repeat(60).as_bytes(), 1000);
        let plain = text_of(&result_of(call(&text_ctx, "digest", json!({}))));
        assert!(!plain.contains("does not look line oriented"), "{plain}");
    }
}
