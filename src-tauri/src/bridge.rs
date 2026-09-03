//! AI Bridge - ローカル専用 NDJSON TCP サーバ (protocol v1)
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
//! | `auth` | `{token}` | `{authenticated: true}` |
//! | `status` | - | `{connected, port_name, total_bytes, app_version, protocol}` |
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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::serial::data_store::DataStore;
use crate::serial::port::SerialPort;

/// 既定の待ち受けポート
pub const DEFAULT_BRIDGE_PORT: u16 = 57320;
/// プロトコルバージョン
pub const PROTOCOL_VERSION: u32 = 1;
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
pub type SharedSerialPort = Arc<Mutex<Option<Arc<Mutex<SerialPort>>>>>;

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
    emit: EmitFn,
}

impl BridgeCtx {
    pub fn new(
        data_store: SharedDataStore,
        port: SharedSerialPort,
        token: Option<String>,
        activity: Arc<Mutex<Option<BridgeActivity>>>,
        emit: EmitFn,
    ) -> Self {
        Self {
            data_store,
            port,
            token,
            activity,
            emit,
        }
    }

    /// 現在の DataStore を解決する（毎回解決: 世代が差し替わるため）
    fn store(&self) -> Option<Arc<DataStore>> {
        self.data_store.lock().ok().and_then(|guard| guard.clone())
    }

    /// 現在の SerialPort を解決する
    fn port_handle(&self) -> Option<Arc<Mutex<SerialPort>>> {
        self.port.lock().ok().and_then(|guard| guard.clone())
    }

    /// 活動を記録し、GUI へ通知する
    fn record_send(&self, bytes: usize, preview: String) {
        if let Ok(mut slot) = self.activity.lock() {
            *slot = Some(BridgeActivity {
                kind: "send".to_string(),
                bytes,
                at_ms: now_ms(),
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

fn method_status(ctx: &BridgeCtx) -> Value {
    let (connected, port_name) = match ctx.port_handle() {
        Some(port) => {
            let name = port.lock().ok().map(|p| p.name().to_string());
            (true, name)
        }
        None => (false, None),
    };
    let total_bytes = ctx.store().map(|s| s.total_bytes()).unwrap_or(0);

    json!({
        "connected": connected,
        "port_name": port_name,
        "total_bytes": total_bytes,
        "app_version": env!("CARGO_PKG_VERSION"),
        "protocol": PROTOCOL_VERSION,
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

    let written = {
        let mut guard = port.lock().map_err(|_| "port lock poisoned".to_string())?;
        guard.write(&payload)?
    };

    // GUI 可視化要件: 人間が AI の送信内容を見られるようにする
    ctx.record_send(written, preview_of(&payload));

    Ok(json!({ "bytes_written": written }))
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
    let ctx = Arc::new(BridgeCtx::new(
        serial.data_store.clone(),
        serial.port.clone(),
        config.token.clone(),
        state.activity.clone(),
        Box::new(move |event: BridgeActivityEvent| {
            if let Err(e) = app_handle.emit("bridge-activity", event) {
                log::warn!("[bridge] Failed to emit bridge-activity: {}", e);
            }
        }),
    ));

    let server = BridgeServer::start(desired_port, ctx, state.connections.clone())?;
    log::info!("[bridge] Started on {}", server.local_addr());
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
        assert!(activity.at_ms > 0);

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
        assert_eq!(response["result"]["protocol"], json!(1));

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
}
