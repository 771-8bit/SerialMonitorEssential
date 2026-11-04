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
/// 毎ポーリングで解決できるようハンドルを共有する（ポート再オープンや
/// クリアで内側の `Arc<DataStore>` は差し替わるため）。
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

/// 手動の Connect。新しいセッションを始める（受信データは新しいストアへ。SYS-F-803）
#[tauri::command]
pub fn open_port(
    state: State<'_, SerialState>,
    app: AppHandle,
    port_name: String,
    config: port::SerialConfig,
) -> Result<(), String> {
    open_port_impl(&state, app, &port_name, config, false)
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
) -> Result<(), String> {
    open_port_impl(&state, app, &port_name, config, true)
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
    keep_store: bool,
) -> Result<(), String> {
    let mut port_guard = state.port.lock().map_err(|e| e.to_string())?;
    let mut store_guard = state.data_store.lock().map_err(|e| e.to_string())?;

    if keep_store && port_guard.as_ref().is_some_and(|p| p.is_connected()) {
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

    // Parse the device path out of the friendly name
    let port_path = extract_port_path(port_name);

    // Try to open the new port BEFORE destroying the previous session's data.
    // If the open fails (port busy, device absent, bad config), the previous
    // capture stays intact and browsable/exportable.
    let port = port::SerialPort::new(port_path, config)?;
    let port_arc = Arc::new(tx::OpenPort::new(port_path, Box::new(port)));
    *port_guard = Some(port_arc.clone());
    state.reconnecting.store(false, Ordering::Relaxed);

    // 自動再接続: 同じストアで受信を再開する（start_reception が前回の総バイト数から続ける）
    if keep_store {
        if let Some(existing_store) = store_guard.as_ref() {
            log::info!("[reopen_port] Resuming reception on the existing DataStore");
            return existing_store.start_reception(port_arc, app, existing_store.clone());
        }
    }

    // Now it is safe to drop the old DataStore (deletes its temp files)
    if let Some(existing_store) = store_guard.take() {
        log::info!("[open_port] Stopping and dropping existing DataStore");
        existing_store.stop_reception();
        // existing_store is dropped here, temp files deleted
    }

    // Create NEW DataStore and start reception with UI event notification
    log::info!("[open_port] Creating new DataStore");
    let data_store = Arc::new(DataStore::new()?);
    data_store.start_reception(port_arc.clone(), app, data_store.clone())?;
    *store_guard = Some(data_store.clone());

    Ok(())
}

#[tauri::command]
pub fn close_port(state: State<'_, SerialState>) -> Result<(), String> {
    let mut port_guard = state.port.lock().map_err(|e| e.to_string())?;
    let store_guard = state.data_store.lock().map_err(|e| e.to_string())?;

    // Stop data reception (but keep DataStore alive to preserve temp files)
    if let Some(ref data_store) = *store_guard {
        data_store.stop_reception();
    }

    // Then close port
    if let Some(port) = port_guard.take() {
        port.close();
    }

    Ok(())
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

/// Convert bytes to ASCII string
fn bytes_to_ascii(data: &[u8]) -> String {
    data.iter().map(|&b| byte_to_ascii(b)).collect()
}

#[tauri::command]
pub fn get_display_rows(
    state: State<'_, SerialState>,
    start_row: u64,
    row_count: u32,
) -> Result<DisplayRowsPayload, String> {
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
                        let product = info.product.as_deref().unwrap_or("Unknown Device");
                        format!("{} ({})", product, p.port_name)
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
        " " | "," | "\t" => Ok(()),
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
pub fn clear_data(state: State<'_, SerialState>, app: AppHandle) -> Result<(), String> {
    let mut store_guard = state.data_store.lock().map_err(|e| e.to_string())?;
    let port_guard = state.port.lock().map_err(|e| e.to_string())?;

    if let Some(old_store) = store_guard.take() {
        log::info!("[clear_data] Stopping old DataStore");
        old_store.stop_reception();
    }

    // If port is open, restart DataStore. 切断で I/O が破棄済み（Disconnected）の
    // ポートでは受信を再開できないので、閉じている場合と同じ扱いにする。
    if let Some(port_arc) = port_guard.as_ref().filter(|p| p.is_parked()) {
        log::info!("[clear_data] Creating new DataStore");
        let new_store = Arc::new(DataStore::new()?);
        // Reuse the existing port
        new_store.start_reception(port_arc.clone(), app, new_store.clone())?;
        *store_guard = Some(new_store);
    } else {
        // If port not open, we just leave store as None (cleared) or create an empty one?
        // Actually SerialState currently holds DataStore only when open_port is called?
        // Wait, open_port creates it. close_port keeps it alive?
        // close_port stops reception but keeps DataStore handle.
        // If we clear while closed, we probably just want to drop the old one and maybe create a fresh empty one?
        // Or just None?
        // If we set it to None, then `get_display_rows` returns empty. That's fine.
        log::info!("[clear_data] Port closed, DataStore cleared (set to None)");
    }

    Ok(())
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

        // Timestamp ON in the ASCII view (issue #4): the same text as Save
        if let (Some(separator), false) = (timestamp_separator.as_deref(), mode == "hex") {
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
