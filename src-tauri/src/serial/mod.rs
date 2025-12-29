mod chunk;
pub mod data_store;
mod logger_thread;
pub mod port;
mod ui_notifier;
mod worker_thread;

use log::warn;

use chrono::{Local, TimeZone};
use data_store::DataStore;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, State};

/// シリアル通信の状態を管理する構造体
pub struct SerialState {
    pub port: Mutex<Option<Arc<Mutex<port::SerialPort>>>>,
    pub data_store: Mutex<Option<Arc<DataStore>>>,
}

#[tauri::command]
pub fn open_port(
    state: State<'_, SerialState>,
    app: AppHandle,
    port_name: String,
    config: port::SerialConfig,
) -> Result<(), String> {
    let mut port_guard = state.port.lock().map_err(|e| e.to_string())?;
    let mut store_guard = state.data_store.lock().map_err(|e| e.to_string())?;

    // Close existing port and DataStore if open
    // This will drop the old DataStore and delete its temp files
    if let Some(existing_store) = store_guard.take() {
        log::info!("[open_port] Stopping and dropping existing DataStore");
        existing_store.stop_reception();
        // existing_store is dropped here, temp files deleted
    }
    if let Some(existing) = port_guard.take() {
        if let Ok(mut p) = existing.lock() {
            p.close();
        }
    }

    // Parse port name from friendly name "Device (COMx)" -> "COMx"
    let port_path = if let Some(start) = port_name.rfind("(COM") {
        if let Some(end) = port_name[start..].find(')') {
            &port_name[start + 1..start + end]
        } else {
            &port_name
        }
    } else {
        &port_name
    };

    // Create SerialPort
    let port = port::SerialPort::new(port_path, config)?;
    let port_arc = Arc::new(Mutex::new(port));
    *port_guard = Some(port_arc.clone());

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
        if let Ok(mut p) = port.lock() {
            p.close();
        }
    }

    Ok(())
}

#[tauri::command]
pub fn write_data(state: State<'_, SerialState>, data: Vec<u8>) -> Result<usize, String> {
    let port_guard = state.port.lock().map_err(|e| e.to_string())?;

    if let Some(port) = port_guard.as_ref() {
        if let Ok(mut p) = port.lock() {
            return p.write(&data);
        }
    }
    Err("Port not open".to_string())
}

#[tauri::command]
pub fn write_dtr(state: State<'_, SerialState>, level: bool) -> Result<(), String> {
    let port_guard = state.port.lock().map_err(|e| e.to_string())?;

    if let Some(port) = port_guard.as_ref() {
        if let Ok(mut p) = port.lock() {
            return p.write_dtr(level).map_err(|e| e.to_string());
        }
    }
    Err("Port not open".to_string())
}

#[tauri::command]
pub fn write_rts(state: State<'_, SerialState>, level: bool) -> Result<(), String> {
    let port_guard = state.port.lock().map_err(|e| e.to_string())?;

    if let Some(port) = port_guard.as_ref() {
        if let Ok(mut p) = port.lock() {
            return p.write_rts(level).map_err(|e| e.to_string());
        }
    }
    Err("Port not open".to_string())
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
            });
        }

        // Phase 2: line_index is now updated by Worker Thread in real-time
        // No need to call update_line_index here

        let total_lines = data_store.total_lines();

        if total_lines == 0 {
            return Ok(AsciiLinesPayload {
                lines: vec![],
                total_lines: 0,
            });
        }

        // Get line offsets for requested range
        let line_offsets = data_store.get_line_offsets(start_line, line_count);

        if line_offsets.is_empty() {
            return Ok(AsciiLinesPayload {
                lines: vec![],
                total_lines,
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
                let timestamp = if show_timestamp {
                    if let Some(ts_millis) = data_store.get_timestamp_for_offset(line_start) {
                        // Use chrono to convert UTC timestamp (millis) to Local time
                        // timestamp_millis_opt handles the conversion safely
                        if let Some(dt) = Local.timestamp_millis_opt(ts_millis as i64).single() {
                            let time_str = dt.format("%H:%M:%S").to_string();
                            let d = (ts_millis % 1000) / 100;
                            Some(format!("{}.{}", time_str, d))
                        } else {
                            Some("--:--:--.0".to_string())
                        }
                    } else {
                        Some("--:--:--.0".to_string())
                    }
                } else {
                    None
                };

                lines.push(AsciiLine {
                    offset: line_start,
                    text,
                    timestamp,
                });
            }
        }

        Ok(AsciiLinesPayload { lines, total_lines })
    } else {
        Ok(AsciiLinesPayload {
            lines: vec![],
            total_lines: 0,
        })
    }
}

#[tauri::command]
pub fn list_ports() -> Vec<String> {
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

#[tauri::command]
pub fn clear_data(state: State<'_, SerialState>, app: AppHandle) -> Result<(), String> {
    let mut store_guard = state.data_store.lock().map_err(|e| e.to_string())?;
    let port_guard = state.port.lock().map_err(|e| e.to_string())?;

    if let Some(old_store) = store_guard.take() {
        log::info!("[clear_data] Stopping old DataStore");
        old_store.stop_reception();
    }

    // If port is open, restart DataStore
    if let Some(port_arc) = port_guard.as_ref() {
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
pub fn get_clipboard_text(state: State<'_, SerialState>, mode: String) -> Result<String, String> {
    let store_guard = state.data_store.lock().map_err(|e| e.to_string())?;

    if let Some(ref data_store) = *store_guard {
        let total = data_store.total_bytes();
        if total == 0 {
            return Ok(String::new());
        }

        // Limit for clipboard to avoid crash?
        // User handles confirmation. We just try to read.
        // Fetch all data
        // WARNING: If total is huge (e.g. 100MB), Vec<u8> is 100MB, String is another 100MB+.
        // Ideally we stream, but we return a String.
        // For now, read all.
        let to_read = total as u32; // Limit to u32
        let data = data_store.get_data(0, to_read)?;

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
