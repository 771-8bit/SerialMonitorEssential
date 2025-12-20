// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod serial;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(serial::SerialState {
            port: std::sync::Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            serial::list_ports,
            serial::open_port,
            serial::close_port,
            serial::write_data
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
