mod chunk;
mod data_store;
mod logger_thread;
mod object_pool;
pub mod port;
mod worker_thread;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDeviceRegistryPropertyW, DIGCF_PRESENT, SPDRP_FRIENDLYNAME, SP_DEVINFO_DATA,
};
// use windows::Win32::Foundation::INVALID_HANDLE_VALUE;
// use windows::Win32::Devices::Communication::GUID_DEVINTERFACE_COMPORT; // Might need to check where this is
use windows::core::{GUID, PCWSTR};

// Standard GUID for COM ports (Ports class)
// 4D36E978-E325-11CE-BFC1-08002BE10318
const GUID_DEVINTERFACE_COMPORT: GUID = GUID::from_u128(0x4D36E978_E325_11CE_BFC1_08002BE10318);

use data_store::DataStore;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, State};

pub struct SerialState {
    pub port: Mutex<Option<Arc<Mutex<port::SerialPort>>>>,
    pub data_store: Mutex<Option<Arc<DataStore>>>,
}

// Phase 3で使用予定: data-updateイベントのペイロード
// UIへ受信データと総バイト数を通知するために使用
#[allow(dead_code)]
#[derive(Clone, serde::Serialize)]
struct DataUpdatePayload {
    data: Vec<u8>,
    total_bytes: u64,
}

#[tauri::command]
pub fn open_port(
    state: State<'_, SerialState>,
    _app: AppHandle,
    port_name: String,
    baud_rate: u32,
) -> Result<(), String> {
    let mut port_guard = state.port.lock().map_err(|e| e.to_string())?;
    let mut store_guard = state.data_store.lock().map_err(|e| e.to_string())?;

    // Close existing port and DataStore if open
    // This will drop the old DataStore and delete its temp files
    if let Some(existing_store) = store_guard.take() {
        println!("[open_port] Stopping and dropping existing DataStore");
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

    // Create NEW DataStore and start reception
    println!("[open_port] Creating new DataStore");
    let data_store = Arc::new(DataStore::new()?);
    data_store.start_reception(port_arc.clone())?;
    *store_guard = Some(data_store.clone());

    // Note: Phase 2では、data-updateイベントはまだ実装していません
    // Phase 3で finished_queue を監視してUIイベントを送る仕組みを追加します

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
