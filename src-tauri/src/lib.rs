// SerialMonitorEssential - Main library entry point
mod plotter;
mod serial;

use plotter::{
    AggregationMode, PlotterAggregator, PlotterDataRequest, PlotterRangedPayload, PlotterThread,
};
use serial::SerialState;
use std::sync::Mutex;

/// Plotter state accessible across the application
pub struct PlotterState {
    /// Data aggregator for plotter (stores all parsed data with lazy aggregation)
    pub aggregator: PlotterAggregator,
    pub thread: Mutex<Option<PlotterThread>>,
}

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
        .manage(PlotterState {
            aggregator: PlotterAggregator::new(),
            thread: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            serial::list_ports,
            serial::open_port,
            serial::close_port,
            serial::write_data,
            serial::get_read_data,
            serial::get_display_rows,
            serial::get_ascii_lines,
            serial::export_log,
            serial::clear_data,
            serial::get_clipboard_text,
            serial::get_line_index,
            serial::write_dtr,
            serial::write_rts,
            open_plotter_window,
            get_plotter_data_ranged,
            set_plotter_enabled,
            set_aggregation_mode,
            start_plotter_thread,
            stop_plotter_thread
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Open the plotter window (single instance)
#[tauri::command]
async fn open_plotter_window(
    app: tauri::AppHandle,
    plotter_state: tauri::State<'_, PlotterState>,
) -> Result<String, String> {
    use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

    const PLOTTER_LABEL: &str = "plotter";

    // Check if plotter window already exists
    if let Some(window) = app.get_webview_window(PLOTTER_LABEL) {
        // Focus existing window
        window.set_focus().map_err(|e| e.to_string())?;
        log::info!("[open_plotter_window] Focused existing window");
        return Ok(PLOTTER_LABEL.to_string());
    }

    // Enable plotter aggregator
    plotter_state.aggregator.set_enabled(true);

    // Create new window
    let _window =
        WebviewWindowBuilder::new(&app, PLOTTER_LABEL, WebviewUrl::App("plotter.html".into()))
            .title("Serial Plotter")
            .inner_size(800.0, 600.0)
            .min_inner_size(600.0, 400.0)
            .build()
            .map_err(|e| format!("Failed to create plotter window: {}", e))?;

    log::info!("[open_plotter_window] Created window: {}", PLOTTER_LABEL);

    Ok(PLOTTER_LABEL.to_string())
}

/// Get ranged plotter data with dynamic aggregation
#[tauri::command]
fn get_plotter_data_ranged(
    plotter_state: tauri::State<'_, PlotterState>,
    request: PlotterDataRequest,
) -> Result<PlotterRangedPayload, String> {
    Ok(plotter_state.aggregator.get_ranged_data(&request))
}

/// Enable or disable plotter data collection
#[tauri::command]
fn set_plotter_enabled(
    plotter_state: tauri::State<'_, PlotterState>,
    enabled: bool,
) -> Result<(), String> {
    plotter_state.aggregator.set_enabled(enabled);
    Ok(())
}

/// Set the aggregation mode for downsampling
#[tauri::command]
fn set_aggregation_mode(
    plotter_state: tauri::State<'_, PlotterState>,
    mode: AggregationMode,
) -> Result<(), String> {
    plotter_state.aggregator.set_aggregation_mode(mode);
    log::info!("[set_aggregation_mode] Mode changed");
    Ok(())
}

/// Start the plotter thread to process serial data
#[tauri::command]
fn start_plotter_thread(
    serial_state: tauri::State<'_, SerialState>,
    plotter_state: tauri::State<'_, PlotterState>,
) -> Result<(), String> {
    // Get data store from serial state
    let store_guard = serial_state.data_store.lock().map_err(|e| e.to_string())?;
    let data_store = store_guard.as_ref().ok_or("Serial port not open")?;

    // Stop existing thread if any
    {
        let mut thread_guard = plotter_state.thread.lock().map_err(|e| e.to_string())?;
        if let Some(mut thread) = thread_guard.take() {
            thread.stop();
        }
    }

    // Clear old data and enable
    plotter_state.aggregator.clear();
    plotter_state.aggregator.set_enabled(true);

    // Start new plotter thread
    let thread = PlotterThread::start(data_store.clone(), plotter_state.aggregator.clone());

    {
        let mut thread_guard = plotter_state.thread.lock().map_err(|e| e.to_string())?;
        *thread_guard = Some(thread);
    }

    log::info!("[start_plotter_thread] Plotter thread started");
    Ok(())
}

/// Stop the plotter thread
#[tauri::command]
fn stop_plotter_thread(plotter_state: tauri::State<'_, PlotterState>) -> Result<(), String> {
    let mut thread_guard = plotter_state.thread.lock().map_err(|e| e.to_string())?;
    if let Some(mut thread) = thread_guard.take() {
        thread.stop();
        log::info!("[stop_plotter_thread] Plotter thread stopped");
    }
    Ok(())
}
