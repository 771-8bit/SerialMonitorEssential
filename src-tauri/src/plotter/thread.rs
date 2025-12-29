// Plotter Thread - Background thread for parsing serial data for plotter
//
// Reads new data from the main DataStore and parses it into plotter data points.

use crate::plotter::{PlotterDataStore, PlotterParser};
use crate::serial::data_store::DataStore;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Plotter thread for background parsing
pub struct PlotterThread {
    /// Thread handle
    handle: Option<JoinHandle<()>>,
    /// Stop flag
    stop_flag: Arc<AtomicBool>,
}

impl PlotterThread {
    /// Start a new plotter thread
    pub fn start(data_store: Arc<DataStore>, plotter_store: PlotterDataStore) -> Self {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_clone = Arc::clone(&stop_flag);

        let handle = thread::spawn(move || {
            Self::run(data_store, plotter_store, stop_flag_clone);
        });

        Self {
            handle: Some(handle),
            stop_flag,
        }
    }

    /// Stop the plotter thread
    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    /// Main thread loop
    fn run(
        data_store: Arc<DataStore>,
        plotter_store: PlotterDataStore,
        stop_flag: Arc<AtomicBool>,
    ) {
        let mut parser = PlotterParser::new();
        let mut last_processed_offset: u64 = 0;
        let start_time = Instant::now();

        // Buffer size for reading data
        const READ_BUFFER_SIZE: u32 = 4096;

        loop {
            if stop_flag.load(Ordering::SeqCst) {
                break;
            }

            // Check for new data
            let total_bytes = data_store.total_bytes();

            if total_bytes > last_processed_offset {
                let bytes_to_read =
                    (total_bytes - last_processed_offset).min(READ_BUFFER_SIZE as u64) as u32;

                // Read new data from data store
                if let Ok(data) = data_store.get_data(last_processed_offset, bytes_to_read) {
                    if !data.is_empty() {
                        // Calculate timestamp (ms since start)
                        let timestamp_ms = start_time.elapsed().as_millis() as u64;

                        // Parse the data
                        let data_points = parser.parse(&data, timestamp_ms);

                        // Add to plotter store
                        for point in data_points {
                            for (channel, value) in point.channels {
                                plotter_store.add_data_point(&channel, point.timestamp_ms, value);
                            }
                        }

                        last_processed_offset += data.len() as u64;
                    }
                }
            }

            // Sleep briefly to avoid busy-waiting
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for PlotterThread {
    fn drop(&mut self) {
        self.stop();
    }
}
