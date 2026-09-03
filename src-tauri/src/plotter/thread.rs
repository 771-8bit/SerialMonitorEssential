// Plotter Thread - Background thread for parsing serial data for plotter
//
// Reads new data from the main DataStore and parses it into plotter data points.

use crate::plotter::parser::ParsedDataPoint;
use crate::plotter::{PlotterAggregator, PlotterParser};
use crate::serial::data_store::DataStore;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Shared handle to the (possibly absent / replaceable) serial DataStore.
///
/// The serial side replaces the inner `Arc<DataStore>` on port reopen and on
/// clear, so the plotter thread must re-resolve it every poll instead of
/// capturing one instance at start time.
pub type SharedDataStore = Arc<Mutex<Option<Arc<DataStore>>>>;

/// Maximum buffer size for a single read (1MB) - safety limit
const MAX_READ_SIZE: u64 = 1024 * 1024;
/// After ~5s of persistent failures at the same offset (500 polls at the 10ms
/// interval of the run loop), skip ahead so a permanently unreadable region
/// can't stall the plotter. Kept generous so transient failures (e.g. an
/// antivirus/indexer briefly holding the archive file) don't discard the
/// backlog.
const MAX_READ_FAILURES: u32 = 500;

/// How many bytes to read this poll, or `None` when there is nothing new.
///
/// Reads ALL available data at once, capped at `MAX_READ_SIZE` for safety.
/// (Pure - extracted for exact unit testing.)
fn bytes_to_read(total_bytes: u64, last_processed_offset: u64) -> Option<u32> {
    if total_bytes > last_processed_offset {
        Some((total_bytes - last_processed_offset).min(MAX_READ_SIZE) as u32)
    } else {
        None
    }
}

/// Whether this consecutive read failure should be logged: the first one, then
/// every 100th, so a permanently failing offset doesn't flood the log.
/// (Pure - extracted for exact unit testing.)
fn should_log_read_failure(read_failures: u32) -> bool {
    read_failures == 1 || read_failures.is_multiple_of(100)
}

/// Spread the timestamps of one parsed batch across (prev, now], ~1ms/line,
/// clamped so ordering stays monotonic with the previous batch.
/// Returns the new last_batch_ts. (Pure - extracted for exact unit testing.)
fn spread_batch_timestamps(
    data_points: &mut [ParsedDataPoint],
    last_batch_ts: u64,
    timestamp_ms: u64,
) -> u64 {
    let n = data_points.len() as u64;
    if n > 0 {
        let candidate = timestamp_ms.saturating_sub(n);
        let prev = candidate.max(last_batch_ts).min(timestamp_ms);
        let span = timestamp_ms - prev;
        for (i, point) in data_points.iter_mut().enumerate() {
            point.timestamp_ms = prev + span * (i as u64 + 1) / n;
        }
        timestamp_ms
    } else {
        last_batch_ts
    }
}

/// Plotter thread for background parsing
pub struct PlotterThread {
    /// Thread handle
    handle: Option<JoinHandle<()>>,
    /// Stop flag
    stop_flag: Arc<AtomicBool>,
}

impl PlotterThread {
    /// Start a new plotter thread with PlotterAggregator
    pub fn start(store_handle: SharedDataStore, aggregator: PlotterAggregator) -> Self {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_clone = Arc::clone(&stop_flag);

        let handle = thread::spawn(move || {
            Self::run(store_handle, aggregator, stop_flag_clone);
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
    fn run(
        store_handle: SharedDataStore,
        aggregator: PlotterAggregator,
        stop_flag: Arc<AtomicBool>,
    ) {
        let mut parser = PlotterParser::new();
        let mut last_processed_offset: u64 = 0;
        let start_time = Instant::now();
        // The DataStore instance we are currently reading from. The serial side
        // swaps in a NEW instance on port reopen / clear, so track identity.
        let mut current_store: Option<Arc<DataStore>> = None;
        // End timestamp of the previous batch; used to spread per-line
        // timestamps across the polling interval so simultaneous lines don't
        // collapse onto one x position.
        let mut last_batch_ts: u64 = 0;
        // Consecutive read failures for the same offset (see below)
        let mut read_failures: u32 = 0;

        loop {
            if stop_flag.load(Ordering::SeqCst) {
                break;
            }

            // Re-resolve the current DataStore (it may be replaced or absent)
            let store = store_handle.lock().ok().and_then(|guard| guard.clone());

            let data_store = match store {
                Some(store) => {
                    let changed = current_store
                        .as_ref()
                        .map(|c| !Arc::ptr_eq(c, &store))
                        .unwrap_or(true);
                    if changed {
                        // New session: restart from the beginning of the new store
                        log::info!("[PlotterThread] DataStore changed, resetting read offset");
                        last_processed_offset = 0;
                        parser.reset();
                        read_failures = 0;
                        // On an actual swap (port reopen / clear), the underlying
                        // capture was replaced: clear plotted data so the plot
                        // matches the (now empty) main view instead of mixing
                        // stale channels with the new session. Keep timestamps
                        // monotonic. On the initial attach leave last_batch_ts
                        // at 0 so a backlog replay can spread backwards
                        // (~1ms per line) instead of collapsing onto a single
                        // timestamp.
                        if current_store.is_some() {
                            aggregator.clear();
                            last_batch_ts = start_time.elapsed().as_millis() as u64;
                        }
                        current_store = Some(Arc::clone(&store));
                    }
                    store
                }
                None => {
                    // Port not open yet (or cleared while closed) - wait for one
                    current_store = None;
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
            };

            // Check for new data
            let total_bytes = data_store.total_bytes();

            if let Some(bytes_to_read) = bytes_to_read(total_bytes, last_processed_offset) {
                // Read new data from data store
                match data_store.get_data(last_processed_offset, bytes_to_read) {
                    Ok(data) => {
                        read_failures = 0;
                        if !data.is_empty() {
                            // Calculate timestamp (ms since start)
                            let timestamp_ms = start_time.elapsed().as_millis() as u64;

                            // Parse all the data at once
                            let mut data_points: Vec<ParsedDataPoint> =
                                parser.parse(&data, timestamp_ms);

                            // Spread timestamps across (prev, timestamp_ms] so
                            // multiple lines from one read keep distinct,
                            // monotonically increasing x positions instead of
                            // collapsing onto a single point. Aim for ~1ms per
                            // line, but never go below the previous batch's end
                            // (keeps aggregator input ordered).
                            last_batch_ts = spread_batch_timestamps(
                                &mut data_points,
                                last_batch_ts,
                                timestamp_ms,
                            );

                            // Add all points in a single batch
                            if !data_points.is_empty() {
                                aggregator.add_data_points_batch(data_points);
                            }

                            last_processed_offset += data.len() as u64;
                        }
                    }
                    Err(e) => {
                        read_failures += 1;
                        if should_log_read_failure(read_failures) {
                            log::warn!(
                                "[PlotterThread] get_data failed at offset {} (attempt {}): {}",
                                last_processed_offset,
                                read_failures,
                                e
                            );
                        }
                        if read_failures >= MAX_READ_FAILURES {
                            // Skip the unreadable region instead of stalling forever
                            log::warn!(
                                "[PlotterThread] Skipping unreadable region {}..{}",
                                last_processed_offset,
                                total_bytes
                            );
                            last_processed_offset = total_bytes;
                            read_failures = 0;
                        }
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
    use std::collections::HashMap;

    /// Helper to create a minimal test DataStore wrapped in the shared handle
    fn create_test_data_store() -> SharedDataStore {
        Arc::new(Mutex::new(Some(Arc::new(
            DataStore::new().expect("Failed to create test DataStore"),
        ))))
    }

    /// `n` placeholder points; only `timestamp_ms` matters for spread tests.
    fn dummy_points(n: usize) -> Vec<ParsedDataPoint> {
        (0..n)
            .map(|_| ParsedDataPoint {
                timestamp_ms: u64::MAX, // poison: must be overwritten by the spread
                channels: HashMap::new(),
                channel_order: Vec::new(),
            })
            .collect()
    }

    /// Poll `cond` until true or `timeout` elapses. Returns the final result.
    fn wait_until(mut cond: impl FnMut() -> bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            thread::sleep(Duration::from_millis(25));
        }
        cond()
    }

    /// Every numeric value currently held by the aggregator, across all channels.
    fn all_values(aggregator: &PlotterAggregator) -> Vec<f64> {
        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 800,
            is_realtime: false,
        };
        aggregator
            .get_chart_data(&req)
            .aligned_data
            .iter()
            .skip(1) // index 0 is the timestamp row
            .flatten()
            .filter_map(|v| *v)
            .collect()
    }

    /// Keep appending readable data at the end of the store until the plotter
    /// thread produces points, or `timeout` elapses.
    ///
    /// Used by the read-failure tests: while the thread is stuck on an
    /// unreadable offset nothing can be parsed, so the first point only appears
    /// once the skip-ahead has moved the offset past the bad region. Returns
    /// how long that took.
    fn drive_until_points(
        store: &Arc<DataStore>,
        aggregator: &PlotterAggregator,
        line: &[u8],
        timeout: Duration,
    ) -> Option<Duration> {
        let start = Instant::now();
        while start.elapsed() < timeout {
            store.push_test_data(line);
            if aggregator.total_points() > 0 {
                return Some(start.elapsed());
            }
            thread::sleep(Duration::from_millis(100));
        }
        None
    }

    // ==================== Pure Helper Tests ====================
    //
    // These pin the exact arithmetic of the run loop, which integration tests
    // can only observe indirectly.

    #[test]
    fn test_bytes_to_read_exact() {
        // Nothing new -> no read at all.
        // (kills `total_bytes > offset` -> `>=`, which would issue a 0-byte read)
        assert_eq!(bytes_to_read(0, 0), None);
        assert_eq!(bytes_to_read(104, 104), None);
        // Offset ahead of the store (shrunk/rewound store) -> still no read
        assert_eq!(bytes_to_read(99, 100), None);

        // Exactly the unread remainder.
        // (kills `total_bytes - offset` -> `+`, which would ask for 140)
        assert_eq!(bytes_to_read(100, 40), Some(60));
        assert_eq!(bytes_to_read(100, 0), Some(100));
        assert_eq!(bytes_to_read(101, 100), Some(1));

        // Capped at MAX_READ_SIZE.
        // (kills `1024 * 1024` -> `1024 / 1024`, and `.min` -> `.max`)
        assert_eq!(MAX_READ_SIZE, 1_048_576);
        assert_eq!(bytes_to_read(MAX_READ_SIZE, 0), Some(1_048_576));
        assert_eq!(bytes_to_read(MAX_READ_SIZE + 1, 0), Some(1_048_576));
        assert_eq!(bytes_to_read(u64::MAX, 0), Some(1_048_576));
    }

    #[test]
    fn test_should_log_read_failure() {
        // First failure is always logged, then every 100th.
        // (kills `read_failures == 1` -> `!=` and `||` -> `&&`)
        assert!(should_log_read_failure(1), "first failure must be logged");
        assert!(!should_log_read_failure(2), "2nd failure must stay quiet");
        assert!(!should_log_read_failure(99));
        assert!(should_log_read_failure(100), "every 100th is logged");
        assert!(!should_log_read_failure(101));
        assert!(should_log_read_failure(200));
        assert!(should_log_read_failure(MAX_READ_FAILURES));
        assert_eq!(MAX_READ_FAILURES, 500);
    }

    #[test]
    fn test_spread_batch_timestamps_exact_vectors() {
        struct Case {
            name: &'static str,
            n: usize,
            last_batch_ts: u64,
            now: u64,
            expected: Vec<u64>,
            expected_return: u64,
        }

        let cases = [
            // Single line lands exactly on `now`.
            // Kills: `timestamp_ms - prev` -> `+` (span 199 -> ts 298),
            //        `prev + span * ..` -> `prev * ..` (99),
            //        `(i as u64 + 1)` -> `* 1` (99).
            Case {
                name: "single line",
                n: 1,
                last_batch_ts: 0,
                now: 100,
                expected: vec![100],
                expected_return: 100,
            },
            // candidate == last_batch_ts: 1ms per line, ending on `now`.
            // Kills: `span * (i+1) / n` -> `* n` (90 + 100*(i+1)).
            Case {
                name: "1ms per line, candidate == last",
                n: 10,
                last_batch_ts: 90,
                now: 100,
                expected: (91..=100).collect(),
                expected_return: 100,
            },
            // Backlog replay spreads BACKWARDS from `now` (~1ms/line), it must
            // not stretch across the whole elapsed time.
            // Kills: `candidate.max(last_batch_ts)` -> `.min` (prev 0 -> steps of 100).
            Case {
                name: "backlog spreads ~1ms per line, not across the whole gap",
                n: 10,
                last_batch_ts: 0,
                now: 1000,
                expected: (991..=1000).collect(),
                expected_return: 1000,
            },
            // now < n: saturating_sub clamps candidate to 0, span is the whole
            // elapsed time and integer division repeats timestamps.
            // Kills: `/ n` -> `* n` (3*(i+1)*5).
            Case {
                name: "clamp low: fewer ms than lines",
                n: 5,
                last_batch_ts: 0,
                now: 3,
                expected: vec![0, 1, 1, 2, 3], // 3*(i+1)/5
                expected_return: 3,
            },
            // Monotonic guard: never start below the previous batch's end.
            Case {
                name: "monotonic guard: last > candidate",
                n: 3,
                last_batch_ts: 500,
                now: 502,
                expected: vec![500, 501, 502], // 500 + 2*(i+1)/3
                expected_return: 502,
            },
            // prev never exceeds now (a clock that appears to go backwards
            // relative to last_batch_ts collapses to a zero-width span).
            // Kills: `.min(timestamp_ms)` -> `.max` (prev 700 > now 600 would
            // underflow `timestamp_ms - prev` and panic).
            Case {
                name: "clamp high: last > now",
                n: 4,
                last_batch_ts: 700,
                now: 600,
                expected: vec![600; 4],
                expected_return: 600,
            },
            // Empty batch: nothing to place, last_batch_ts must NOT move.
            // Kills: `n > 0` -> `n >= 0` (would return `now` = 1000).
            Case {
                name: "empty batch leaves last_batch_ts alone",
                n: 0,
                last_batch_ts: 42,
                now: 1000,
                expected: vec![],
                expected_return: 42,
            },
        ];

        for case in cases {
            let mut points = dummy_points(case.n);
            let returned = spread_batch_timestamps(&mut points, case.last_batch_ts, case.now);
            let actual: Vec<u64> = points.iter().map(|p| p.timestamp_ms).collect();

            assert_eq!(
                actual, case.expected,
                "case '{}': timestamps (last={}, now={})",
                case.name, case.last_batch_ts, case.now
            );
            assert_eq!(
                returned, case.expected_return,
                "case '{}': returned last_batch_ts",
                case.name
            );
        }
    }

    #[test]
    fn test_spread_batch_timestamps_is_monotonic_and_bounded() {
        // Property check over the same formula: output is non-decreasing, stays
        // within [prev, now], and ends exactly on `now` whenever there is room.
        for (last, now, n) in [
            (0u64, 5u64, 3usize),
            (0, 1000, 1),
            (90, 100, 10),
            (500, 502, 3),
            (0, 3, 5),
            (10, 10, 4),
            (700, 600, 4),
        ] {
            let mut points = dummy_points(n);
            spread_batch_timestamps(&mut points, last, now);
            let ts: Vec<u64> = points.iter().map(|p| p.timestamp_ms).collect();

            assert!(
                ts.windows(2).all(|w| w[0] <= w[1]),
                "non-monotonic for last={last} now={now} n={n}: {ts:?}"
            );
            assert!(
                ts.iter().all(|t| *t <= now),
                "timestamp beyond now for last={last} now={now} n={n}: {ts:?}"
            );
            assert_eq!(
                *ts.last().unwrap(),
                now,
                "last point must land on now for last={last} now={now} n={n}"
            );
        }
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

    // ==================== Store Lifecycle Tests ====================

    /// Wait until `cond` is true, or panic after ~2 seconds
    fn wait_for(mut cond: impl FnMut() -> bool, what: &str) {
        for _ in 0..200 {
            if cond() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("Timed out waiting for: {}", what);
    }

    #[test]
    fn test_thread_reads_data_from_store() {
        let handle: SharedDataStore = Arc::new(Mutex::new(None));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());

        // No store yet -> no data
        thread::sleep(Duration::from_millis(60));
        assert_eq!(aggregator.total_points(), 0);

        // Provide a store with data; thread should pick it up dynamically
        let store = Arc::new(DataStore::new().expect("create store"));
        store.push_test_data(b"1,2\n3,4\n");
        *handle.lock().unwrap() = Some(store.clone());

        wait_for(
            || aggregator.total_points() >= 4,
            "4 points from first store",
        );

        thread.stop();
    }

    #[test]
    fn test_thread_survives_store_swap() {
        // Simulates port reopen / clear_data: the inner Arc<DataStore> is
        // REPLACED with a fresh instance whose offsets restart at 0. The
        // aggregator is cleared on swap so the plot matches the new session.
        let store_a = Arc::new(DataStore::new().expect("create store A"));
        store_a.push_test_data(b"10,20\n30,40\n");

        let handle: SharedDataStore = Arc::new(Mutex::new(Some(store_a)));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());
        wait_for(|| aggregator.total_points() >= 4, "points from store A");

        // Swap in a new store (fresh offsets); thread must clear old data,
        // reset its offset and keep reading from the new store
        let store_b = Arc::new(DataStore::new().expect("create store B"));
        store_b.push_test_data(b"50,60\n");
        *handle.lock().unwrap() = Some(store_b);

        wait_for(
            || aggregator.total_points() == 2,
            "only store B's points after swap (old data cleared)",
        );

        thread.stop();
    }

    #[test]
    fn test_batch_timestamps_are_spread() {
        // Lines read in one poll must not collapse onto a single timestamp
        let handle: SharedDataStore = Arc::new(Mutex::new(None));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());

        // Give the thread some elapsed time so the backlog can spread backwards
        thread::sleep(Duration::from_millis(60));

        // 8 lines: below the default bucket_size (10) so points stay raw
        let store = Arc::new(DataStore::new().expect("create store"));
        store.push_test_data(b"1\n2\n3\n4\n5\n6\n7\n8\n");
        *handle.lock().unwrap() = Some(store);

        wait_for(|| aggregator.total_points() >= 8, "8 points");
        thread.stop();

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 800,
            is_realtime: false,
        };
        let payload = aggregator.get_chart_data(&req);
        // All 8 points must remain visible (distinct timestamps)
        assert_eq!(
            payload.aligned_data[0].len(),
            8,
            "spread timestamps should keep all 8 points distinct"
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

    // ==================== Read-Failure / Fault Injection ====================
    //
    // `push_test_data_at(offset, ..)` parks a chunk at an arbitrary global
    // offset. A chunk at offset K > 0 leaves the range [0, K) with no backing
    // data, so `get_data(0, ..)` fails with "Insufficient data" while
    // `total_bytes()` still reports K + len - exactly the stall the
    // read_failures / skip-ahead logic exists for.

    #[test]
    fn test_read_failure_then_recovery() {
        let store = Arc::new(DataStore::new().expect("create store"));
        store.push_test_data_at(100, b"1,2\n"); // gap at [0, 100)
        assert_eq!(store.total_bytes(), 104);
        assert!(
            store.get_data(0, 104).is_err(),
            "fault injection precondition: the read must fail across the gap"
        );

        let handle: SharedDataStore = Arc::new(Mutex::new(Some(store.clone())));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);
        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());

        // ~30 failing polls: nothing may be produced, and the offset must stay
        // put (no silent advance past unreadable bytes).
        thread::sleep(Duration::from_millis(300));
        assert_eq!(
            aggregator.total_points(),
            0,
            "no data may flow while every read fails"
        );

        // Heal the gap: a chunk at offset 0 covering all 104 bytes makes
        // get_data(0, 104) succeed again. Value 9 exists ONLY in the healed
        // region, so seeing it proves the thread was still reading from
        // offset 0 - it never skipped ahead over the bytes it could not read.
        let padding: Vec<u8> = b"9,9\n".repeat(26); // 26 lines = 104 bytes
        assert_eq!(padding.len(), 104);
        store.push_test_data_at(0, &padding);
        assert!(
            store.get_data(0, 104).is_ok(),
            "healed store must be readable from 0"
        );

        assert!(
            wait_until(|| aggregator.total_points() > 0, Duration::from_secs(2)),
            "thread must resume parsing once the read succeeds again"
        );
        // Let the whole batch land, then freeze the state.
        thread::sleep(Duration::from_millis(100));
        thread.stop();

        let values = all_values(&aggregator);
        assert!(!values.is_empty(), "healed region should have been parsed");
        assert!(
            values.iter().all(|v| *v == 9.0),
            "only the healed region's data (value 9) may appear, got {values:?}"
        );
    }

    #[test]
    fn test_read_failure_skip_ahead() {
        // SLOW BY DESIGN - measured ~6.0s: the skip cannot fire before
        // MAX_READ_FAILURES (500) consecutive failures at the run loop's 10ms
        // poll interval. Deliberately NOT #[ignore]d - this is the only
        // coverage of the anti-stall skip-ahead, a data-loss-prevention path:
        // without it a permanently unreadable region freezes the plotter
        // forever. It runs in parallel with the rest of the suite, so the whole
        // `plotter::thread` module still finishes in ~7s.
        let store = Arc::new(DataStore::new().expect("create store"));
        store.push_test_data_at(100, b"1,2\n"); // gap at [0, 100), never healed

        let handle: SharedDataStore = Arc::new(Mutex::new(Some(store.clone())));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);
        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());

        // Feed readable data at the end of the store while waiting. Until the
        // skip fires, every read still starts at offset 0 and spans the gap, so
        // nothing can be parsed; once the offset jumps to total_bytes, the next
        // appended chunk is readable and flows.
        let recovery = drive_until_points(&store, &aggregator, b"7,8\n", Duration::from_secs(20));
        thread.stop();

        let elapsed = recovery
            .expect("skip-ahead never fired: the plotter stayed stalled on the unreadable region");
        assert!(
            elapsed >= Duration::from_secs(4),
            "skip fired after {elapsed:?}: it must be gated by MAX_READ_FAILURES \
             (500 polls ~= 5s), not by the first failure"
        );

        // Recovered: only post-skip data is parsable, and it is.
        let values = all_values(&aggregator);
        assert!(
            values.contains(&7.0),
            "post-skip data must flow, got {values:?}"
        );
    }

    #[test]
    fn test_partial_read_when_gap_ahead() {
        // CHARACTERIZATION - measured ~5.5s (same skip-ahead wait as above, and
        // it runs concurrently with it).
        //
        // Layout: readable chunk [0, 50), gap [50, 200), chunk [200, 204).
        // The run loop reads (total_bytes - offset).min(MAX_READ_SIZE) in ONE
        // call, so it asks for [0, 204) - which crosses the gap. DataStore
        // returns Err for the whole request instead of the readable prefix, so
        // the readable [0, 50) is NOT delivered piecewise: the thread stalls
        // and eventually skips the entire region, losing both chunks.
        let store = Arc::new(DataStore::new().expect("create store"));
        let prefix: Vec<u8> = b"11,2\n".repeat(10); // 50 bytes, values 11 and 2
        assert_eq!(prefix.len(), 50);
        store.push_test_data_at(0, &prefix);
        store.push_test_data_at(200, b"3,4\n");
        assert_eq!(store.total_bytes(), 204);

        // Documented DataStore semantics this behavior rests on:
        assert!(store.get_data(0, 50).is_ok(), "prefix alone is readable");
        assert!(
            store.get_data(0, 204).is_err(),
            "a request crossing the gap fails whole - no partial read"
        );

        let handle: SharedDataStore = Arc::new(Mutex::new(Some(store.clone())));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);
        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());

        // Stalls despite the first 50 bytes being perfectly readable.
        thread::sleep(Duration::from_millis(300));
        assert_eq!(
            aggregator.total_points(),
            0,
            "readable prefix is not delivered while the same read spans a gap"
        );

        // ... and recovers only via the skip-ahead, which drops the prefix.
        let recovery = drive_until_points(&store, &aggregator, b"7,8\n", Duration::from_secs(20));
        thread.stop();

        assert!(
            recovery.is_some(),
            "skip-ahead must eventually get the thread reading again"
        );
        let values = all_values(&aggregator);
        assert!(
            values.contains(&7.0),
            "post-skip data must flow, got {values:?}"
        );
        assert!(
            !values.contains(&11.0) && !values.contains(&3.0),
            "characterization: the skipped region (values 11 and 3) is lost, got {values:?}"
        );
    }

    #[test]
    fn test_drop_stops_thread() {
        // `impl Drop` must stop AND join the worker; a dropped handle that left
        // the thread running would keep consuming data behind the caller's back.
        let store = Arc::new(DataStore::new().expect("create store"));
        store.push_test_data(b"1,2\n");

        let handle: SharedDataStore = Arc::new(Mutex::new(Some(store.clone())));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);
        let thread = PlotterThread::start(handle.clone(), aggregator.clone());

        wait_for(|| aggregator.total_points() >= 2, "initial points");
        let at_drop = aggregator.total_points();

        drop(thread); // no explicit stop(): Drop must do it

        // Anything appended after the drop must never be consumed.
        store.push_test_data(b"3,4\n5,6\n");
        thread::sleep(Duration::from_millis(300));
        let sample_1 = aggregator.total_points();
        store.push_test_data(b"7,8\n");
        thread::sleep(Duration::from_millis(300));
        let sample_2 = aggregator.total_points();

        assert_eq!(
            sample_1, at_drop,
            "point count grew after drop: the thread was not stopped"
        );
        assert_eq!(
            sample_2, sample_1,
            "point growth must have ceased permanently after drop"
        );
    }

    #[test]
    fn test_data_is_consumed_exactly_once() {
        // The read offset must advance by exactly the bytes consumed, otherwise
        // the same bytes are re-parsed on every poll.
        // (kills `last_processed_offset += data.len()` -> `*=`, which pins the
        // offset at 0 and duplicates every line ~100x/second)
        let store = Arc::new(DataStore::new().expect("create store"));
        store.push_test_data(b"1,2\n3,4\n"); // 2 lines x 2 channels = 4 points

        let handle: SharedDataStore = Arc::new(Mutex::new(Some(store.clone())));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);
        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());

        wait_for(|| aggregator.total_points() >= 4, "4 points");
        assert_eq!(aggregator.total_points(), 4, "no duplicates on first read");

        // ~20 further polls with no new data: the count must not move.
        thread::sleep(Duration::from_millis(200));
        assert_eq!(
            aggregator.total_points(),
            4,
            "already-read bytes must not be parsed again"
        );

        thread.stop();
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
