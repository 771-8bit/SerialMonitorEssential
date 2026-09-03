// SerialMonitorEssential - Main library entry point
mod plotter;
mod serial;
#[cfg(test)]
mod state_transition_tests;

use plotter::{
    AggregationMode, PlotterAggregator, PlotterChartPayload, PlotterDataRequest, PlotterThread,
    PlotterVersionInfo,
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
        // ウィンドウ破棄イベントの処理:
        // - plotter: X ボタン等で破棄されたとき React の unmount クリーンアップ
        //   （stop_plotter_thread 呼び出し）は実行されないため、バックエンド側で
        //   確実にスレッド停止と収集停止を行う。
        // - main: メインウィンドウを閉じたらプロッタも道連れに閉じる
        //   （プロッタが残るとアプリが終了しない）。
        .on_window_event(|window, event| {
            use tauri::Manager;
            if !matches!(event, tauri::WindowEvent::Destroyed) {
                return;
            }
            match window.label() {
                "plotter" => {
                    let plotter_state = window.app_handle().state::<PlotterState>();
                    if let Ok(mut thread_guard) = plotter_state.thread.lock() {
                        if let Some(mut thread) = thread_guard.take() {
                            thread.stop();
                        }
                    }
                    plotter_state.aggregator.set_enabled(false);
                    log::info!("[plotter] Window destroyed: thread stopped, collection disabled");
                }
                "main" => {
                    if let Some(plotter) = window.app_handle().get_webview_window("plotter") {
                        log::info!("[main] Window destroyed: closing plotter window too");
                        let _ = plotter.close();
                    }
                }
                _ => {}
            }
        })
        .manage(SerialState {
            port: Mutex::new(None),
            data_store: std::sync::Arc::new(Mutex::new(None)),
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
            get_plotter_chart_data,
            check_plotter_version,
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

/// Get plotter chart data in uPlot-ready format
///
/// This API returns data pre-aligned for direct uPlot consumption,
/// eliminating the need for per-frame data transformation in the frontend.
#[tauri::command]
async fn get_plotter_chart_data(
    plotter_state: tauri::State<'_, PlotterState>,
    request: PlotterDataRequest,
) -> Result<PlotterChartPayload, String> {
    // async: runs on the thread pool instead of blocking the main thread
    // (this is called at up to 60Hz and does the heavy aggregation work)
    Ok(plotter_state.aggregator.get_chart_data(&request))
}

/// Lightweight version check for smart polling
///
/// Returns only version number and has_data flag, avoiding heavy data processing.
/// Frontend calls this at 60Hz and only fetches full data when version changes.
#[tauri::command]
async fn check_plotter_version(
    plotter_state: tauri::State<'_, PlotterState>,
) -> Result<PlotterVersionInfo, String> {
    Ok(plotter_state.aggregator.check_version())
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
///
/// The thread receives a shared handle to the serial DataStore slot, so it
/// keeps working across port reopen / clear (the serial side swaps the inner
/// store instance) and also when the port is opened AFTER the plotter window.
#[tauri::command]
fn start_plotter_thread(
    serial_state: tauri::State<'_, SerialState>,
    plotter_state: tauri::State<'_, PlotterState>,
) -> Result<(), String> {
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

    // Start new plotter thread with the shared store handle
    let store_handle = serial_state.data_store.clone();
    let thread = PlotterThread::start(store_handle, plotter_state.aggregator.clone());

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
