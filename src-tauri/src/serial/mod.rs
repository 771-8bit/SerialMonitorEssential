mod chunk;
pub mod data_store;
mod logger_thread;
pub mod port;
pub mod tx;
mod ui_notifier;
mod worker_thread;

use log::warn;

use chrono::{Local, TimeZone};
use data_store::DataStore;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, State};

/// シリアル通信の状態を管理する構造体
///
/// `data_store` は外側も `Arc` で包み、プロッタスレッドが「現在の」DataStore を
/// 毎ポーリングで解決できるようハンドルを共有する（クリアで内側の
/// `Arc<DataStore>` は差し替わるため）。
///
/// `port` も同じ理由で外側を `Arc` にしてある。AI ブリッジ（`bridge.rs`）の
/// サーバスレッドはコマンドのスコープより長生きするので Tauri の `State` を
/// 保持できず、ハンドルの複製が必要になる。
///
/// 内側は `OpenPort`（ADR-14）: ポートの I/O は受信ワーカーだけが行い、送信・DTR・RTS は
/// キュー経由で渡す。外側のロックは**ハンドルの解決にだけ**使い、送信の完了待ちの間は
/// 握らない（握ると close_port / clear_data が長い送信の後ろで止まる）。
pub struct SerialState {
    pub port: Arc<Mutex<Option<Arc<tx::OpenPort>>>>,
    pub data_store: Arc<Mutex<Option<Arc<DataStore>>>>,
    /// 自動再接続の待機中（デバイスが消え、戻るのを待っている）。フロントエンドが
    /// 待機の出入りで `set_reconnecting` を呼ぶ。ブリッジの `status` がこれを返し、
    /// エージェントが「ボードが再起動中なので待つべき」と判断できるようにする
    pub reconnecting: Arc<AtomicBool>,
}

/// フレンドリー名からデバイスパスを取り出す
///
/// `list_ports` は USB 機器などを "Product (COM7)" / "Product (/dev/ttyUSB0)"
/// の形で返すため、**最後の括弧の中身**をデバイスパスとして採用する。
/// 括弧がない場合（Linux の Unknown タイプ等はパスがそのまま来る）は全体を使う。
/// 旧実装は "(COM" 前提で Linux のパスを抽出できなかった（移植バグ）。
fn extract_port_path(port_name: &str) -> &str {
    match (port_name.rfind('('), port_name.rfind(')')) {
        (Some(start), Some(end)) if start < end => &port_name[start + 1..end],
        _ => port_name,
    }
}

/// Manual Connect resumes the capture. Only Clear discards received data.
#[tauri::command]
pub fn open_port(
    state: State<'_, SerialState>,
    app: AppHandle,
    port_name: String,
    config: port::SerialConfig,
) -> Result<u64, String> {
    open_port_impl(&state, app, &port_name, config)
}

/// 自動再接続。デバイスが消えて戻ってきたとき、**同じストアに追記を再開**する
/// （オフセットは続き、ブリッジに `reset` は出ない。切断前のログが残る。SYS-F-807）。
/// ストアが無い（切断中に Clear した）ときは新しいストアで始める。
///
/// `serial-status` を受けたフロントの `close_port` と順序が前後してもよいよう、
/// 切断済みの `OpenPort` が残っていても動く。ただし生きている接続を奪うことはしない
/// （待機中に利用者が手動で繋いだ場合など）。
#[tauri::command]
pub fn reopen_port(
    state: State<'_, SerialState>,
    app: AppHandle,
    port_name: String,
    config: port::SerialConfig,
) -> Result<u64, String> {
    open_port_impl(&state, app, &port_name, config)
}

/// 自動再接続の待機に入った / 出たことを記録する（ブリッジの `status.reconnecting`）
#[tauri::command]
pub fn set_reconnecting(state: State<'_, SerialState>, waiting: bool) {
    state.reconnecting.store(waiting, Ordering::Relaxed);
}

fn open_port_impl(
    state: &SerialState,
    app: AppHandle,
    port_name: &str,
    config: port::SerialConfig,
) -> Result<u64, String> {
    let port_path = extract_port_path(port_name);
    open_capture(
        state,
        || {
            let port = port::SerialPort::new(port_path, config)?;
            Ok(Arc::new(tx::OpenPort::new(port_path, Box::new(port))))
        },
        |store, port| store.start_reception(port, app, store.clone()),
    )
}

/// Shared by manual and automatic opens. Keep the capture independently of the
/// connection, including when opening the device or starting reception fails.
fn open_capture(
    state: &SerialState,
    open: impl FnOnce() -> Result<Arc<tx::OpenPort>, String>,
    start: impl FnOnce(&Arc<DataStore>, Arc<tx::OpenPort>) -> Result<(), String>,
) -> Result<u64, String> {
    let mut port_guard = state.port.lock().map_err(|e| e.to_string())?;
    let mut store_guard = state.data_store.lock().map_err(|e| e.to_string())?;

    if port_guard.as_ref().is_some_and(|p| p.is_connected()) {
        return Err("a port is already connected".to_string());
    }

    // Stop the old session's reception FIRST (without destroying its capture):
    // the worker thread holds an Arc to the old SerialPort, so the OS COM
    // handle is only released once the worker joins. Without this, reopening
    // the same COM port fails with an exclusive-access error.
    if let Some(ref existing_store) = *store_guard {
        existing_store.stop_reception();
    }
    if let Some(existing) = port_guard.take() {
        // ワーカーは停止済みで I/O は退避されている。ここで OS ハンドルを閉じる
        // （送信中の呼び出し側が Arc を握っていても、ハンドルはここで確実に解放される）
        existing.close();
    }

    let port = open()?;
    let data_store = match store_guard.as_ref() {
        Some(store) => store.clone(),
        None => Arc::new(DataStore::new()?),
    };
    if let Err(error) = start(&data_store, port.clone()) {
        data_store.stop_reception();
        port.close();
        return Err(error);
    }
    let connection_id = port.id();
    *store_guard = Some(data_store);
    *port_guard = Some(port);
    state.reconnecting.store(false, Ordering::Relaxed);
    Ok(connection_id)
}

#[tauri::command]
pub fn close_port(
    state: State<'_, SerialState>,
    connection_id: Option<u64>,
) -> Result<bool, String> {
    close_capture(&state, connection_id)
}

fn close_capture(state: &SerialState, connection_id: Option<u64>) -> Result<bool, String> {
    let mut port_guard = state.port.lock().map_err(|e| e.to_string())?;
    // A delayed disconnect event must never stop a subsequently opened port.
    if let Some(id) = connection_id {
        if port_guard.as_ref().is_none_or(|port| port.id() != id) {
            return Ok(false);
        }
    }
    let store_guard = state.data_store.lock().map_err(|e| e.to_string())?;

    // Stop data reception (but keep DataStore alive to preserve temp files)
    if let Some(ref data_store) = *store_guard {
        data_store.stop_reception();
    }

    // Then close port
    if let Some(port) = port_guard.take() {
        port.close();
    }

    Ok(true)
}

/// 現在のポートを解決する（外側のロックはここで手放す）
fn current_port(state: &SerialState) -> Result<Arc<tx::OpenPort>, String> {
    state
        .port
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or_else(|| "Port not open".to_string())
}

/// 送信。線上の時間（115200 bps で 16 KiB なら約 1.4 秒）だけ完了を待つので、
/// async コマンドにしてメインスレッドを塞がない（INV-11 と同じ理由）。
#[tauri::command]
pub async fn write_data(state: State<'_, SerialState>, data: Vec<u8>) -> Result<usize, String> {
    let port = current_port(&state)?;
    tauri::async_runtime::spawn_blocking(move || port.send(data).map(|r| r.bytes_written))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn write_dtr(state: State<'_, SerialState>, level: bool) -> Result<(), String> {
    let port = current_port(&state)?;
    tauri::async_runtime::spawn_blocking(move || port.set_dtr(level))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn write_rts(state: State<'_, SerialState>, level: bool) -> Result<(), String> {
    let port = current_port(&state)?;
    tauri::async_runtime::spawn_blocking(move || port.set_rts(level))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn get_read_data(
    state: State<'_, SerialState>,
    offset: u64,
    length: u32,
) -> Result<Vec<u8>, String> {
    let store_guard = state.data_store.lock().map_err(|e| e.to_string())?;
    if let Some(ref data_store) = *store_guard {
        data_store.get_data(offset, length)
    } else {
        Err("No data available".to_string())
    }
}

const BYTES_PER_ROW: usize = 16;

/// A single row of hex display data
#[derive(Clone, serde::Serialize)]
pub struct DisplayRow {
    offset: u64,
    hex: String,
    ascii: String,
    /// Reception time of the row's first byte (`HH:MM:SS.d`) when requested
    timestamp: Option<String>,
}

/// Payload for get_display_rows command
#[derive(Clone, serde::Serialize)]
pub struct DisplayRowsPayload {
    rows: Vec<DisplayRow>,
    total_rows: u64,
}

/// Convert a byte to its ASCII representation (printable or placeholder)
/// Control characters (0x00-0x1F) use Unicode Control Pictures (U+2400-U+241F)
/// DEL (0x7F) uses U+2421, non-ASCII (>0x7F) uses middle dot
fn byte_to_ascii(b: u8) -> char {
    if (0x20..=0x7e).contains(&b) {
        // Printable ASCII
        b as char
    } else if b <= 0x1f {
        // Control characters 0x00-0x1F -> Unicode Control Pictures U+2400-U+241F
        char::from_u32(0x2400 + b as u32).unwrap_or('·')
    } else if b == 0x7f {
        // DEL -> U+2421
        '␡'
    } else {
        // Non-ASCII (0x80-0xFF) -> middle dot
        '·'
    }
}

/// Convert bytes to hex string with spaces
fn bytes_to_hex(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Hex rows of `BYTES_PER_ROW` bytes as in the Hex view, each preceded by the
/// reception time of its first byte (`data` starts at offset 0), one row per line
fn hex_rows_with_timestamps(
    data: &[u8],
    separator: &str,
    timestamp_at: impl Fn(u64) -> String,
) -> String {
    let mut out = String::with_capacity(data.len() / BYTES_PER_ROW * 72 + 72);
    for (i, row) in data.chunks(BYTES_PER_ROW).enumerate() {
        out.push_str(&timestamp_at((i * BYTES_PER_ROW) as u64));
        out.push_str(separator);
        out.push_str(&bytes_to_hex(row));
        out.push_str("\r\n");
    }
    out
}

/// Convert bytes to ASCII string
fn bytes_to_ascii(data: &[u8]) -> String {
    data.iter().map(|&b| byte_to_ascii(b)).collect()
}

#[tauri::command]
pub fn get_display_rows(
    state: State<'_, SerialState>,
    start_row: u64,
    row_count: u32,
    show_timestamp: Option<bool>,
) -> Result<DisplayRowsPayload, String> {
    let show_timestamp = show_timestamp.unwrap_or(false);
    let store_guard = state.data_store.lock().map_err(|e| e.to_string())?;

    if let Some(ref data_store) = *store_guard {
        let total_bytes = data_store.total_bytes();
        let total_rows = total_bytes.div_ceil(BYTES_PER_ROW as u64);

        // Calculate byte range
        let start_offset = start_row * BYTES_PER_ROW as u64;
        let bytes_needed = (row_count as usize) * BYTES_PER_ROW;

        // Clamp to available data
        if start_offset >= total_bytes {
            return Ok(DisplayRowsPayload {
                rows: vec![],
                total_rows,
            });
        }

        let actual_length = std::cmp::min(
            bytes_needed as u64,
            total_bytes.saturating_sub(start_offset),
        ) as u32;

        // Get raw data
        let data = if actual_length > 0 {
            match data_store.get_data(start_offset, actual_length) {
                Ok(d) => {
                    if d.len() != actual_length as usize {
                        warn!(
                            "[get_display_rows] Warning: requested {} bytes at offset {}, got {} bytes",
                            actual_length, start_offset, d.len()
                        );
                    }
                    d
                }
                Err(e) => {
                    warn!(
                        "[get_display_rows] Error fetching data at offset {}, length {}: {}",
                        start_offset, actual_length, e
                    );
                    vec![]
                }
            }
        } else {
            vec![]
        };

        // Convert to display rows
        let mut rows = Vec::new();
        for i in 0..(row_count as usize) {
            let row_offset = start_offset + (i * BYTES_PER_ROW) as u64;
            if row_offset >= total_bytes {
                break;
            }

            let data_start = i * BYTES_PER_ROW;
            let data_end = std::cmp::min(data_start + BYTES_PER_ROW, data.len());

            if data_start < data.len() {
                let row_data = &data[data_start..data_end];
                rows.push(DisplayRow {
                    offset: row_offset,
                    hex: bytes_to_hex(row_data),
                    ascii: bytes_to_ascii(row_data),
                    timestamp: show_timestamp.then(|| {
                        format_line_timestamp(data_store.get_timestamp_for_offset(row_offset))
                    }),
                });
            }
        }

        Ok(DisplayRowsPayload { rows, total_rows })
    } else {
        Ok(DisplayRowsPayload {
            rows: vec![],
            total_rows: 0,
        })
    }
}

/// A single line of ASCII display data
#[derive(Clone, serde::Serialize)]
pub struct AsciiLine {
    offset: u64,
    text: String,
    timestamp: Option<String>,
}

#[derive(Clone, serde::Serialize)]
pub struct AsciiLinesPayload {
    lines: Vec<AsciiLine>,
    total_lines: u64,
    /// Line index of `lines[0]` actually served. A request past the end is clamped to
    /// the last `line_count` lines, so a viewer that asks for "the tail" gets it
    /// atomically even while the line count grows (issue #3).
    start_line: u64,
    /// Whether the served range runs to the end of the data. Not derivable from
    /// `lines.len()`: after a trailing LF the line index holds an empty last line,
    /// which is not sent.
    reaches_end: bool,
}

/// Clamp a requested start line so that a request past the end serves the last
/// `line_count` lines instead of nothing.
fn clamp_start_line(start_line: u64, line_count: u32, total_lines: u64) -> u64 {
    start_line.min(total_lines.saturating_sub(line_count as u64))
}

/// Timestamp text for a line, the same everywhere (view, Copy, Save): local
/// `HH:MM:SS.d`. Timestamps are recorded about every 100 ms, so tenths are the real
/// precision; lines received before the first record show `--:--:--.0`.
pub(crate) fn format_line_timestamp(ts_millis: Option<u64>) -> String {
    ts_millis
        .and_then(|ms| {
            Local
                .timestamp_millis_opt(ms as i64)
                .single()
                .map(|dt| (ms, dt))
        })
        .map(|(ms, dt)| format!("{}.{}", dt.format("%H:%M:%S"), (ms % 1000) / 100))
        .unwrap_or_else(|| "--:--:--.0".to_string())
}

#[derive(Clone, serde::Serialize)]
pub struct LineIndexPayload {
    line_index: u64,
}

#[tauri::command]
pub fn get_line_index(
    state: State<'_, SerialState>,
    offset: u64,
) -> Result<LineIndexPayload, String> {
    let store_guard = state.data_store.lock().map_err(|e| e.to_string())?;

    if let Some(ref data_store) = *store_guard {
        let line_index = data_store.get_line_index_for_offset(offset);
        Ok(LineIndexPayload { line_index })
    } else {
        Ok(LineIndexPayload { line_index: 0 })
    }
}

#[tauri::command]
pub fn get_ascii_lines(
    state: State<'_, SerialState>,
    start_line: u64,
    line_count: u32,
    _show_ctrl: bool, // Unused: all control chars now use Unicode Control Pictures like HexViewer
    show_timestamp: bool,
) -> Result<AsciiLinesPayload, String> {
    let store_guard = state.data_store.lock().map_err(|e| e.to_string())?;

    if let Some(ref data_store) = *store_guard {
        let total_bytes = data_store.total_bytes();
        if total_bytes == 0 {
            return Ok(AsciiLinesPayload {
                lines: vec![],
                total_lines: 0,
                start_line: 0,
                reaches_end: true,
            });
        }

        // Phase 2: line_index is now updated by Worker Thread in real-time
        // No need to call update_line_index here

        let total_lines = data_store.total_lines();

        if total_lines == 0 {
            return Ok(AsciiLinesPayload {
                lines: vec![],
                total_lines: 0,
                start_line: 0,
                reaches_end: true,
            });
        }

        let start_line = clamp_start_line(start_line, line_count, total_lines);

        // Get line offsets for requested range
        let line_offsets = data_store.get_line_offsets(start_line, line_count);
        let reaches_end = start_line + line_offsets.len() as u64 >= total_lines;

        if line_offsets.is_empty() {
            return Ok(AsciiLinesPayload {
                lines: vec![],
                total_lines,
                start_line,
                reaches_end,
            });
        }

        // Phase 1 optimization: Batch fetch all lines data at once instead of per-line
        let first_offset = line_offsets[0].0;
        let last_end = line_offsets.last().unwrap().1;
        // Use saturating_sub to prevent overflow when data changes during fetch
        let batch_length = last_end.saturating_sub(first_offset).min(1024 * 1024) as u32; // Cap at 1MB

        // Early return if batch_length is 0 (race condition: data changed)
        if batch_length == 0 {
            return Ok(AsciiLinesPayload {
                lines: vec![],
                total_lines,
                start_line,
                reaches_end,
            });
        }

        // Single batch fetch
        let batch_data = match data_store.get_data(first_offset, batch_length) {
            Ok(data) => data,
            Err(_) => {
                // Fallback: if batch fails, return empty
                return Ok(AsciiLinesPayload {
                    lines: vec![],
                    total_lines,
                    start_line,
                    reaches_end,
                });
            }
        };

        // Extract each line from batch data
        let mut lines = Vec::with_capacity(line_offsets.len());
        for (line_start, line_end) in line_offsets {
            let local_start = (line_start - first_offset) as usize;
            let local_end =
                ((line_end - first_offset).min(batch_length as u64) as usize).min(batch_data.len());

            if local_start < batch_data.len() && local_start <= local_end {
                let line_data = &batch_data[local_start..local_end];
                let text = bytes_to_ascii(line_data);

                // Get timestamp from timestamp_index using binary search
                let timestamp = show_timestamp.then(|| {
                    format_line_timestamp(data_store.get_timestamp_for_offset(line_start))
                });

                lines.push(AsciiLine {
                    offset: line_start,
                    text,
                    timestamp,
                });
            }
        }

        Ok(AsciiLinesPayload {
            lines,
            total_lines,
            start_line,
            reaches_end,
        })
    } else {
        Ok(AsciiLinesPayload {
            lines: vec![],
            total_lines: 0,
            start_line: 0,
            reaches_end: true,
        })
    }
}

#[tauri::command]
pub fn list_ports() -> Vec<String> {
    enumerate_ports()
}

/// List entry for a USB port: "Product (COM5)".
///
/// On Windows the product string is the device's friendly name, which already ends
/// in " (COM5)"; that suffix is dropped so the entry does not read
/// "USB Serial Port (COM5) (COM5)".
fn usb_port_label(product: Option<&str>, port_name: &str) -> String {
    let product = product.unwrap_or("Unknown Device");
    let suffix = format!(" ({port_name})");
    let product = product.strip_suffix(suffix.as_str()).unwrap_or(product);
    format!("{product} ({port_name})")
}

/// 利用可能なポートの列挙（`list_ports` コマンドの実体）
///
/// AI ブリッジの `ports` メソッドが GUI と同じ一覧を返せるよう、コマンド属性の
/// 付いていない素の関数として切り出してある。
pub(crate) fn enumerate_ports() -> Vec<String> {
    match serialport::available_ports() {
        Ok(ports) => ports
            .into_iter()
            .map(|p| {
                // Include friendly name for USB devices, or just the port name
                match &p.port_type {
                    serialport::SerialPortType::UsbPort(info) => {
                        usb_port_label(info.product.as_deref(), &p.port_name)
                    }
                    serialport::SerialPortType::PciPort => {
                        format!("PCI Device ({})", p.port_name)
                    }
                    serialport::SerialPortType::BluetoothPort => {
                        format!("Bluetooth ({})", p.port_name)
                    }
                    serialport::SerialPortType::Unknown => p.port_name.clone(),
                }
            })
            .collect(),
        Err(e) => {
            warn!("Failed to enumerate ports: {:?}", e);
            vec![]
        }
    }
}

/// Export received data to a file
#[tauri::command]
pub fn export_log(state: State<'_, SerialState>, path: String) -> Result<u64, String> {
    use std::fs::File;
    use std::io::Write;

    let store_guard = state.data_store.lock().map_err(|e| e.to_string())?;

    if let Some(ref data_store) = *store_guard {
        let total = data_store.total_bytes();
        if total == 0 {
            return Err("No data to export".to_string());
        }

        // Read all data in chunks to avoid memory issues with large files
        const CHUNK_SIZE: u32 = 1024 * 1024; // 1MB chunks
        let mut file = File::create(&path)
            .map_err(|e| format!("Failed to create file '{}': {:?}", path, e))?;

        let mut offset = 0u64;
        while offset < total {
            let to_read = std::cmp::min(CHUNK_SIZE as u64, total - offset) as u32;
            let data = data_store.get_data(offset, to_read)?;
            file.write_all(&data)
                .map_err(|e| format!("Failed to write data: {:?}", e))?;
            offset += data.len() as u64;
        }

        log::info!("[export_log] Exported {} bytes to {}", total, path);
        Ok(total)
    } else {
        Err("No data available".to_string())
    }
}

/// Separator between the timestamp and the data, as offered in the Receive panel
fn validate_timestamp_separator(separator: &str) -> Result<(), String> {
    match separator {
        " > " | " " | "," | "\t" => Ok(()),
        other => Err(format!("Unsupported timestamp separator: {:?}", other)),
    }
}

/// Save the received data as text with a timestamp at the start of every line
/// (issue #4: Save while Timestamp is ON). The data bytes are written unchanged;
/// `export_log` remains the raw-bytes export.
#[tauri::command]
pub fn export_text_with_timestamps(
    state: State<'_, SerialState>,
    path: String,
    separator: String,
) -> Result<u64, String> {
    use std::io::{BufWriter, Write};

    validate_timestamp_separator(&separator)?;
    let store_guard = state.data_store.lock().map_err(|e| e.to_string())?;
    let Some(ref data_store) = *store_guard else {
        return Err("No data available".to_string());
    };
    if data_store.total_bytes() == 0 {
        return Err("No data to export".to_string());
    }
    let file = std::fs::File::create(&path)
        .map_err(|e| format!("Failed to create file '{}': {:?}", path, e))?;
    let mut out = BufWriter::new(file);
    let total = data_store.write_timestamped(&mut out, &separator, format_line_timestamp)?;
    out.flush()
        .map_err(|e| format!("Failed to write data: {:?}", e))?;
    log::info!(
        "[export_text_with_timestamps] Exported {} bytes to {}",
        total,
        path
    );
    Ok(total)
}

#[tauri::command]
pub fn clear_data(state: State<'_, SerialState>, app: AppHandle) -> Result<u64, String> {
    clear_capture(&state, |store, port| {
        store.start_reception(port, app, store.clone())
    })
}

fn clear_capture(
    state: &SerialState,
    start: impl FnOnce(&Arc<DataStore>, Arc<tx::OpenPort>) -> Result<(), String>,
) -> Result<u64, String> {
    let port_guard = state.port.lock().map_err(|e| e.to_string())?;
    let mut store_guard = state.data_store.lock().map_err(|e| e.to_string())?;

    // Allocate before stopping reception so that a failed allocation leaves the
    // previous capture and its reception intact.
    let mut new_store = port_guard
        .as_ref()
        .filter(|port| port.is_connected())
        .map(|_| DataStore::new().map(Arc::new))
        .transpose()?;

    if let Some(old_store) = store_guard.as_ref() {
        log::info!("[clear_data] Stopping old DataStore");
        old_store.stop_reception();
    }

    // A worker can lose the port before returning it to the parked state.
    match (port_guard.as_ref().filter(|p| p.is_parked()), &new_store) {
        (Some(port), Some(store)) => start(store, port.clone())?,
        _ => new_store = None,
    }

    // Clear while disconnected also invalidates already-queued notifications.
    let capture_id = new_store
        .as_ref()
        .map_or_else(DataStore::next_capture_id, |store| store.capture_id());
    *store_guard = new_store;
    Ok(capture_id)
}

#[tauri::command]
pub fn get_clipboard_text(
    state: State<'_, SerialState>,
    mode: String,
    timestamp_separator: Option<String>,
) -> Result<String, String> {
    let store_guard = state.data_store.lock().map_err(|e| e.to_string())?;

    if let Some(ref data_store) = *store_guard {
        let total = data_store.total_bytes();
        if total == 0 {
            return Ok(String::new());
        }

        // Limit for clipboard to avoid crash?
        // User handles confirmation. We just try to read.
        // WARNING: If total is huge (e.g. 100MB), Vec<u8> is 100MB, String is another 100MB+.
        // Cap at u32::MAX instead of wrapping (`total as u32` would silently
        // truncate/wrap for > 4GB captures).
        let to_read = total.min(u32::MAX as u64) as u32;
        let data = data_store.get_data(0, to_read)?;

        // Timestamp ON in the Hex view: the rows as shown, each with its time
        if let (Some(separator), true) = (timestamp_separator.as_deref(), mode == "hex") {
            validate_timestamp_separator(separator)?;
            return Ok(hex_rows_with_timestamps(&data, separator, |offset| {
                format_line_timestamp(data_store.get_timestamp_for_offset(offset))
            }));
        }

        // Timestamp ON in the ASCII view (issue #4): the same text as Save
        if let Some(separator) = timestamp_separator.as_deref() {
            validate_timestamp_separator(separator)?;
            let mut out = Vec::new();
            data_store.write_timestamped(&mut out, separator, format_line_timestamp)?;
            return Ok(String::from_utf8_lossy(&out).to_string());
        }

        if mode == "hex" {
            // Hex mode: space-separated hex string
            Ok(bytes_to_hex(&data))
        } else {
            // ASCII mode: lossy UTF-8 conversion
            Ok(String::from_utf8_lossy(&data).to_string())
        }
    } else {
        Ok(String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture_state() -> SerialState {
        SerialState {
            port: Arc::new(Mutex::new(None)),
            data_store: Arc::new(Mutex::new(None)),
            reconnecting: Arc::new(AtomicBool::new(false)),
        }
    }

    fn fake_port() -> Arc<tx::OpenPort> {
        Arc::new(tx::OpenPort::new(
            "COM4",
            Box::new(tx::fake::FakePort::new(115200, tx::fake::WriteMode::Accept)),
        ))
    }

    #[test]
    fn reconnect_keeps_capture_and_failed_opens_do_not_discard_it() {
        let state = capture_state();
        let first = fake_port();
        let id = open_capture(
            &state,
            || Ok(first.clone()),
            |store, _| {
                store.push_test_data(b"before\r\n");
                Ok(())
            },
        )
        .unwrap();
        let capture = state.data_store.lock().unwrap().as_ref().unwrap().clone();
        assert!(close_capture(&state, Some(id)).unwrap());
        assert!(!first.is_connected());
        assert!(!first.is_parked());
        assert!(open_capture(
            &state,
            || Err("device absent".into()),
            |_, _| { panic!("failed opens must not start reception") }
        )
        .is_err());
        assert_eq!(capture.get_data(0, 8).unwrap(), b"before\r\n");
        let second = fake_port();
        let resumed_id = open_capture(
            &state,
            || Ok(second.clone()),
            |store, _| {
                assert!(Arc::ptr_eq(store, &capture));
                assert_eq!(store.total_bytes(), 8);
                store.push_test_data(b"after\r\n");
                Ok(())
            },
        )
        .unwrap();
        assert_ne!(id, resumed_id);
        assert_eq!(capture.get_data(0, 15).unwrap(), b"before\r\nafter\r\n");
        // A stale event or an already queued cleanup cannot touch the new open.
        assert!(!close_capture(&state, Some(id)).unwrap());
        assert!(second.is_connected());
        assert!(Arc::ptr_eq(
            state.data_store.lock().unwrap().as_ref().unwrap(),
            &capture
        ));
        assert!(close_capture(&state, Some(resumed_id)).unwrap());
    }

    #[test]
    fn clear_keeps_connection_identity_but_never_restores_the_old_capture() {
        let state = capture_state();
        let port = fake_port();
        let id = open_capture(
            &state,
            || Ok(port.clone()),
            |store, _| {
                store.push_test_data(b"old\n");
                Ok(())
            },
        )
        .unwrap();
        let old = state.data_store.lock().unwrap().as_ref().unwrap().clone();
        let capture_id = clear_capture(&state, |store, same_port| {
            assert_eq!(same_port.id(), id);
            assert_eq!(store.total_bytes(), 0);
            Ok(())
        })
        .unwrap();
        let cleared = state.data_store.lock().unwrap().as_ref().unwrap().clone();
        assert!(!Arc::ptr_eq(&old, &cleared));
        assert!(capture_id > old.capture_id());
        assert_eq!(capture_id, cleared.capture_id());
        close_capture(&state, Some(id)).unwrap();
        open_capture(
            &state,
            || Ok(fake_port()),
            |store, _| {
                assert!(Arc::ptr_eq(store, &cleared));
                store.push_test_data(b"new\n");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(cleared.get_data(0, 4).unwrap(), b"new\n");
        close_capture(&state, None).unwrap();
        let disconnected_capture_id =
            clear_capture(&state, |_, _| panic!("closed port must stay closed")).unwrap();
        assert!(disconnected_capture_id > capture_id);
        assert!(state.data_store.lock().unwrap().is_none());
    }

    #[test]
    fn reception_start_failure_releases_port_and_retains_capture() {
        let state = capture_state();
        let capture = Arc::new(DataStore::new().unwrap());
        capture.push_test_data(b"keep");
        *state.data_store.lock().unwrap() = Some(capture.clone());
        let port = fake_port();
        assert!(open_capture(
            &state,
            || Ok(port.clone()),
            |_, _| Err("cannot start".into())
        )
        .is_err());
        assert!(state.port.lock().unwrap().is_none());
        assert!(!port.is_parked());
        assert!(!port.is_connected());
        assert_eq!(capture.get_data(0, 4).unwrap(), b"keep");
    }

    #[test]
    fn clamp_start_line_serves_the_tail_for_requests_past_the_end() {
        // In range: unchanged
        assert_eq!(clamp_start_line(10, 50, 1000), 10);
        assert_eq!(clamp_start_line(950, 50, 1000), 950);
        // Past the end (a viewer asking for "the tail"): the last line_count lines
        assert_eq!(clamp_start_line(u64::MAX, 50, 1000), 950);
        assert_eq!(clamp_start_line(990, 50, 1000), 950);
        // Fewer lines than requested: everything from line 0
        assert_eq!(clamp_start_line(u64::MAX, 50, 20), 0);
        assert_eq!(clamp_start_line(0, 50, 0), 0);
    }

    #[test]
    fn line_timestamp_has_tenths_and_a_fallback() {
        assert_eq!(format_line_timestamp(None), "--:--:--.0");
        let s = format_line_timestamp(Some(1_700_000_000_987));
        assert_eq!(s.len(), "HH:MM:SS.d".len());
        assert!(s.ends_with(".9"), "{s}");
    }

    #[test]
    fn test_extract_port_path() {
        // Windows friendly names
        assert_eq!(extract_port_path("USB Serial Device (COM7)"), "COM7");
        assert_eq!(extract_port_path("PCI Device (COM3)"), "COM3");
        assert_eq!(extract_port_path("Bluetooth (COM12)"), "COM12");
        // Bare names (Unknown port type, or user-provided)
        assert_eq!(extract_port_path("COM1"), "COM1");
        // Linux friendly names (the old "(COM"-anchored parser broke these)
        assert_eq!(
            extract_port_path("CP2102 USB to UART (/dev/ttyUSB0)"),
            "/dev/ttyUSB0"
        );
        assert_eq!(extract_port_path("/dev/ttyACM0"), "/dev/ttyACM0");
        // Product name itself contains parentheses: LAST pair wins
        assert_eq!(extract_port_path("FTDI (rev 2) Adapter (COM9)"), "COM9");
        // Degenerate inputs fall back to the whole string
        assert_eq!(extract_port_path("weird ) ( order"), "weird ) ( order");
        assert_eq!(extract_port_path(""), "");
    }

    #[test]
    fn hex_rows_with_timestamps_puts_each_row_on_its_own_line() {
        let data: Vec<u8> = (0u8..20).collect();
        let text = hex_rows_with_timestamps(&data, " > ", |offset| format!("t{offset}"));
        assert_eq!(
            text,
            "t0 > 00 01 02 03 04 05 06 07 08 09 0A 0B 0C 0D 0E 0F\r\nt16 > 10 11 12 13\r\n"
        );
        assert_eq!(hex_rows_with_timestamps(&[], ",", |_| String::new()), "");
    }

    #[test]
    fn usb_port_label_drops_the_duplicated_port_suffix() {
        assert_eq!(
            usb_port_label(Some("USB Serial Port (COM5)"), "COM5"),
            "USB Serial Port (COM5)"
        );
        assert_eq!(
            usb_port_label(Some("CP2102 USB to UART"), "/dev/ttyUSB0"),
            "CP2102 USB to UART (/dev/ttyUSB0)"
        );
        assert_eq!(usb_port_label(None, "COM7"), "Unknown Device (COM7)");
        // Another port's number in the name is kept
        assert_eq!(
            usb_port_label(Some("Bridge (COM4)"), "COM5"),
            "Bridge (COM4) (COM5)"
        );
        assert_eq!(
            extract_port_path(&usb_port_label(Some("X (COM5)"), "COM5")),
            "COM5"
        );
    }

    #[test]
    fn test_byte_to_ascii_printable() {
        // Printable ASCII characters (0x20-0x7e)
        assert_eq!(byte_to_ascii(b'A'), 'A');
        assert_eq!(byte_to_ascii(b'z'), 'z');
        assert_eq!(byte_to_ascii(b' '), ' ');
        assert_eq!(byte_to_ascii(b'~'), '~');
        assert_eq!(byte_to_ascii(b'0'), '0');
    }

    #[test]
    fn test_byte_to_ascii_special_chars() {
        // Special control characters with dedicated symbols
        assert_eq!(byte_to_ascii(0x00), '␀');
        assert_eq!(byte_to_ascii(0x0a), '␊'); // LF
        assert_eq!(byte_to_ascii(0x0d), '␍'); // CR
        assert_eq!(byte_to_ascii(0x09), '␉'); // TAB
    }

    #[test]
    fn test_byte_to_ascii_other_non_printable() {
        // All control characters (0x00-0x1F) get Unicode Control Pictures
        assert_eq!(byte_to_ascii(0x01), '␁'); // SOH -> U+2401
        assert_eq!(byte_to_ascii(0x1f), '␟'); // US -> U+241F
                                              // DEL gets special symbol
        assert_eq!(byte_to_ascii(0x7f), '␡'); // DEL -> U+2421
                                              // Non-ASCII should be '·'
        assert_eq!(byte_to_ascii(0xff), '·');
    }

    #[test]
    fn test_bytes_to_hex() {
        assert_eq!(bytes_to_hex(&[]), "");
        assert_eq!(bytes_to_hex(&[0x00]), "00");
        assert_eq!(bytes_to_hex(&[0xff]), "FF");
        assert_eq!(
            bytes_to_hex(&[0x48, 0x65, 0x6c, 0x6c, 0x6f]),
            "48 65 6C 6C 6F"
        ); // "Hello"
        assert_eq!(bytes_to_hex(&[0x01, 0x23, 0x45]), "01 23 45");
    }

    #[test]
    fn test_bytes_to_ascii() {
        assert_eq!(bytes_to_ascii(&[]), "");
        assert_eq!(bytes_to_ascii(b"Hello"), "Hello");
        assert_eq!(bytes_to_ascii(&[0x00, 0x0a, 0x0d]), "␀␊␍");
        assert_eq!(bytes_to_ascii(&[0x48, 0x69, 0x00, 0x21]), "Hi␀!");
        // All control chars get symbols
        assert_eq!(bytes_to_ascii(&[0x01, 0x02, 0x03]), "␁␂␃");
    }
}
