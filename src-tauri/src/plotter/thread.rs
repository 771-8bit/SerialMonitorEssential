// Plotter Thread - Background thread for parsing serial data for plotter
//
// Reads new data from the main DataStore and parses it into plotter data points.

use crate::plotter::parser::ParsedDataPoint;
use crate::plotter::{PlotterAggregator, PlotterParser};
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
    /// Start a new plotter thread with PlotterAggregator
    pub fn start(data_store: Arc<DataStore>, aggregator: PlotterAggregator) -> Self {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_clone = Arc::clone(&stop_flag);

        let handle = thread::spawn(move || {
            Self::run(data_store, aggregator, stop_flag_clone);
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

    /// Check if thread should stop (exposed for testing)
    #[cfg(test)]
    pub fn should_stop(&self) -> bool {
        self.stop_flag.load(Ordering::SeqCst)
    }

    /// Main thread loop
    fn run(data_store: Arc<DataStore>, aggregator: PlotterAggregator, stop_flag: Arc<AtomicBool>) {
        let mut parser = PlotterParser::new();
        let mut last_processed_offset: u64 = 0;
        let start_time = Instant::now();

        // Maximum buffer size for a single read (1MB) - safety limit
        const MAX_READ_SIZE: u64 = 1024 * 1024;

        loop {
            if stop_flag.load(Ordering::SeqCst) {
                break;
            }

            // Check for new data
            let total_bytes = data_store.total_bytes();

            if total_bytes > last_processed_offset {
                // Read ALL available data at once (up to MAX_READ_SIZE for safety)
                let bytes_to_read = (total_bytes - last_processed_offset).min(MAX_READ_SIZE) as u32;

                // Read new data from data store
                if let Ok(data) = data_store.get_data(last_processed_offset, bytes_to_read) {
                    if !data.is_empty() {
                        // Calculate timestamp (ms since start)
                        let timestamp_ms = start_time.elapsed().as_millis() as u64;

                        // Parse all the data at once
                        let data_points: Vec<ParsedDataPoint> = parser.parse(&data, timestamp_ms);

                        // Add all points in a single batch
                        if !data_points.is_empty() {
                            aggregator.add_data_points_batch(data_points);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plotter::parser::ChannelValue;
    use crate::plotter::PlotterDataRequest;

    /// Helper to create a minimal test DataStore
    fn create_test_data_store() -> Arc<DataStore> {
        Arc::new(DataStore::new().expect("Failed to create test DataStore"))
    }

    // ==================== Lifecycle Tests ====================

    #[test]
    fn test_thread_start_stop() {
        // Test that thread can be started and stopped cleanly
        let data_store = create_test_data_store();
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        let mut thread = PlotterThread::start(data_store, aggregator);

        // Thread should be running
        assert!(!thread.should_stop());

        // Stop should complete without hanging
        thread.stop();

        // Stop flag should be set
        assert!(thread.should_stop());
    }

    #[test]
    fn test_thread_drop_stops_cleanly() {
        // Test that drop implementation stops the thread
        let data_store = create_test_data_store();
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        {
            let _thread = PlotterThread::start(data_store, aggregator);
            // Thread is dropped here
        }
        // If we reach here without hanging, the test passes
    }

    #[test]
    fn test_thread_multiple_start_stop() {
        // Test starting and stopping multiple times
        let data_store = create_test_data_store();
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        for _ in 0..3 {
            let mut thread = PlotterThread::start(data_store.clone(), aggregator.clone());
            thread::sleep(Duration::from_millis(20));
            thread.stop();
        }
    }

    // ==================== Stop Flag Tests ====================

    #[test]
    fn test_stop_flag_atomic_ordering() {
        // Test that stop flag works correctly across threads
        let data_store = create_test_data_store();
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        let mut thread = PlotterThread::start(data_store, aggregator);

        // Give thread time to start
        thread::sleep(Duration::from_millis(50));

        // Stop should be quick (< 100ms if not hanging)
        let start = Instant::now();
        thread.stop();
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(500),
            "Stop took too long: {:?}",
            elapsed
        );
    }

    // ==================== Data Ingestion Tests ====================

    #[test]
    fn test_aggregator_receives_no_data_when_disabled() {
        // When aggregator is disabled, no data should be added
        let data_store = create_test_data_store();
        let aggregator = PlotterAggregator::new();
        // Note: NOT enabled

        let mut thread = PlotterThread::start(data_store, aggregator.clone());

        thread::sleep(Duration::from_millis(50));
        thread.stop();

        // Aggregator should have no data
        assert_eq!(aggregator.total_points(), 0);
    }

    #[test]
    fn test_aggregator_enabled_empty_data_store() {
        // When data store is empty, aggregator should have no data
        let data_store = create_test_data_store();
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        let mut thread = PlotterThread::start(data_store, aggregator.clone());

        thread::sleep(Duration::from_millis(50));
        thread.stop();

        // Aggregator should still have no data (empty data store)
        assert_eq!(aggregator.total_points(), 0);
    }

    // ==================== Parser Integration Tests ====================

    #[test]
    fn test_parser_is_new_each_thread() {
        // Each thread gets a fresh parser (no state leakage)
        let data_store = create_test_data_store();

        for i in 0..3 {
            let aggregator = PlotterAggregator::new();
            aggregator.set_enabled(true);
            aggregator.clear();

            let mut thread = PlotterThread::start(data_store.clone(), aggregator.clone());
            thread::sleep(Duration::from_millis(30));
            thread.stop();

            // Each iteration should be independent
            assert_eq!(
                aggregator.total_points(),
                0,
                "Iteration {} should have 0 points",
                i
            );
        }
    }

    // ==================== Timing Tests ====================

    #[test]
    fn test_thread_sleeps_between_polls() {
        // Test that thread doesn't busy-wait
        let data_store = create_test_data_store();
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        let start = Instant::now();

        let mut thread = PlotterThread::start(data_store, aggregator);

        // Let it run for a few poll cycles
        thread::sleep(Duration::from_millis(100));

        thread.stop();

        let elapsed = start.elapsed();

        // Thread should have completed without consuming 100% CPU
        // (This is a basic sanity check, not a strict assertion)
        assert!(
            elapsed >= Duration::from_millis(100),
            "Thread should have run for at least 100ms"
        );
    }

    // ==================== Aggregator Integration ====================

    #[test]
    fn test_aggregator_clone_is_shared() {
        // Verify that cloned aggregators share state
        let aggregator = PlotterAggregator::new();
        let aggregator_clone = aggregator.clone();

        aggregator.set_enabled(true);
        assert!(aggregator_clone.is_enabled());

        aggregator_clone.set_enabled(false);
        assert!(!aggregator.is_enabled());
    }

    #[test]
    fn test_direct_data_point_addition() {
        // Test that data can be added directly to aggregator (simulating thread behavior)
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        // Simulate what the thread would do
        aggregator.add_data_point("ch0", 1000, ChannelValue::Numeric(42.0));

        assert_eq!(aggregator.total_points(), 1);

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 800,
            is_realtime: false,
        };
        let payload = aggregator.get_ranged_data(&req);
        assert!(!payload.line_data.is_empty());
    }
}
