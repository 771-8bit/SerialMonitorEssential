// SerialMonitorEssential - Main library entry point
mod serial;

use serial::SerialState;
use std::sync::Mutex;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize logger
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("SerialMonitorEssential starting...");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(SerialState {
            port: Mutex::new(None),
            data_store: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            serial::list_ports,
            serial::open_port,
            serial::close_port,
            serial::write_data,
            serial::get_read_data,
            serial::get_display_rows,
            serial::export_log
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
