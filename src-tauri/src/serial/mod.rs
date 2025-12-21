mod chunk;
mod data_store;
mod logger_thread;
pub mod port;
mod ui_notifier;
mod worker_thread;

use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDeviceRegistryPropertyW, DIGCF_PRESENT, SPDRP_FRIENDLYNAME, SP_DEVINFO_DATA,
};

use log::warn;

// Standard GUID for COM ports (Ports class)
// 4D36E978-E325-11CE-BFC1-08002BE10318
const GUID_DEVINTERFACE_COMPORT: GUID = GUID::from_u128(0x4D36E978_E325_11CE_BFC1_08002BE10318);

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
    baud_rate: u32,
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
    let port = port::SerialPort::new(port_path, baud_rate)?;
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
        if let Ok(p) = port.lock() {
            return p.write(&data);
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
fn byte_to_ascii(b: u8) -> char {
    if (0x20..=0x7e).contains(&b) {
        b as char
    } else {
        match b {
            0x00 => '␀',
            0x0a => '␊',
            0x0d => '␍',
            0x09 => '␉',
            _ => '·',
        }
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

#[tauri::command]
pub fn list_ports() -> Vec<String> {
    let mut ports = Vec::new();

    unsafe {
        let h_dev_info = SetupDiGetClassDevsW(
            Some(&GUID_DEVINTERFACE_COMPORT),
            PCWSTR::null(),
            None,
            DIGCF_PRESENT,
        );

        if h_dev_info.is_err() {
            return ports;
        }
        let h_dev_info = h_dev_info.unwrap();

        let mut device_info_data = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };

        let mut index = 0;
        while SetupDiEnumDeviceInfo(h_dev_info, index, &mut device_info_data).is_ok() {
            let mut buffer = [0u16; 256];
            let mut required_size = 0;

            // Get Friendly Name
            let res = SetupDiGetDeviceRegistryPropertyW(
                h_dev_info,
                &device_info_data,
                SPDRP_FRIENDLYNAME,
                None,
                Some(std::slice::from_raw_parts_mut(
                    buffer.as_mut_ptr() as *mut u8,
                    512,
                )),
                Some(&mut required_size),
            );

            if res.is_ok() {
                let len = (required_size / 2) as usize;
                if len > 0 {
                    let name = String::from_utf16_lossy(&buffer[..len - 1]); // remove null terminator
                                                                             // Filter for "COM"
                    if name.contains("(COM") {
                        ports.push(name);
                    }
                }
            }

            index += 1;
        }

        let _ = SetupDiDestroyDeviceInfoList(h_dev_info);
    }

    ports
}
