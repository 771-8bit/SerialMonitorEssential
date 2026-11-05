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
#[cfg(test)]
fn spread_batch_timestamps(
    data_points: &mut [ParsedDataPoint],
    last_batch_ts: u64,
    timestamp_ms: u64,
) -> u64 {
    spread_over_window(data_points, last_batch_ts, timestamp_ms, 0)
}

/// How often the store records "bytes received by when" (the UI notifier's
/// 16 ms x 6): the bytes of a recorded slice arrived within this window
const RECORD_WINDOW_MS: u64 = 100;

/// `spread_batch_timestamps` over a window of at least `window_ms` before
/// `timestamp_ms`: a slice timed by a store record holds the lines of up to
/// ~100 ms, spread evenly across it instead of bunched into its last few ms.
fn spread_over_window(
    data_points: &mut [ParsedDataPoint],
    last_batch_ts: u64,
    timestamp_ms: u64,
    window_ms: u64,
) -> u64 {
    let n = data_points.len() as u64;
    if n > 0 {
        let candidate = timestamp_ms.saturating_sub(n.max(window_ms));
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

/// Timestamp (ms since the session started) for a slice of received bytes.
///
/// `entry_wall` is the wall-clock time by which the slice had arrived, from the
/// store's timestamp index (None: no record yet, i.e. live bytes). Records and the
/// live clock (an `Instant`) are both measured from the store's creation, but the
/// wall clock is slewed and can jump, so a record that would go backwards past the
/// previous batch or ahead of now falls back to now - live reading then behaves
/// exactly as without records. (Pure - extracted for exact unit testing.)
fn slice_timestamp(
    entry_wall: Option<u64>,
    created_wall: u64,
    last_batch_ts: u64,
    now: u64,
) -> u64 {
    match entry_wall {
        Some(t) => {
            let ts = t.saturating_sub(created_wall);
            if ts < last_batch_ts || ts > now {
                now
            } else {
                ts
            }
        }
        None => now,
    }
}

/// Parse the bytes `range` (store offsets) of `data` (read from offset `base`),
/// telling the parser where a new receive segment starts (connect /
/// auto-reconnect): the line there may be cut (docs/27 PF-2). Parser state carries
/// over between calls, so a line crossing `range.end` is completed by the next one.
fn parse_range(
    parser: &mut PlotterParser,
    store: &DataStore,
    data: &[u8],
    base: u64,
    range: std::ops::Range<u64>,
    ts: u64,
) -> Vec<ParsedDataPoint> {
    let mut points = Vec::new();
    let mut pos = (range.start - base) as usize;
    let end = (range.end - base) as usize;
    for start in store.segment_starts_in(range.start, range.end) {
        let cut = (start - base) as usize;
        if cut > pos {
            points.extend(parser.parse(&data[pos..cut], ts));
        }
        parser.begin_untrusted_segment();
        pos = cut;
    }
    points.extend(parser.parse(&data[pos..end], ts));
    points
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
        Self::start_with_read_cap(store_handle, aggregator, MAX_READ_SIZE)
    }

    /// `start` with a smaller read size, so tests can make a backlog span reads
    fn start_with_read_cap(
        store_handle: SharedDataStore,
        aggregator: PlotterAggregator,
        read_cap: u64,
    ) -> Self {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_clone = Arc::clone(&stop_flag);

        let handle = thread::spawn(move || {
            Self::run(store_handle, aggregator, stop_flag_clone, read_cap);
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
        read_cap: u64,
    ) {
        let mut parser = PlotterParser::new();
        let mut last_processed_offset: u64 = 0;
        // Time origin of the plotter's timestamps: the creation of the current
        // session's store (docs/27 PF-5), not the moment the plotter attached -
        // the plotter is often opened after data has arrived, and that backlog
        // must keep its reception times. A new session (manual Connect / Clear)
        // starts again at 0; an automatic reconnect keeps the same store and
        // therefore the same clock.
        let mut start_time = Instant::now();
        // Whether this thread has ever been attached to a store: from then on a
        // swap or a detach ends a session, so the plot must be cleared
        let mut had_store = false;
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
                        // A new session (port reopen / Clear, also after a
                        // detach while closed): clear the plot so it matches the
                        // (now empty) main view instead of mixing stale channels
                        // with the new session (SYS-F-802), and restart the time
                        // axis at 0 (PF-5). The aggregator's generation changes,
                        // so the frontend resets its view too. Timestamps are
                        // monotonic within a generation (SYS-NF-602).
                        if had_store {
                            aggregator.clear();
                        }
                        had_store = true;
                        start_time = store.created_instant();
                        last_batch_ts = 0;
                        current_store = Some(Arc::clone(&store));
                    }
                    store
                }
                None => {
                    // Port not open yet, or Clear while closed: the session is
                    // over, so empty the plot now - together with the main view
                    // (SYS-F-802, SYS-F-806) - not only at the next Connect
                    if current_store.take().is_some() {
                        aggregator.clear();
                    }
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
            };

            // Check for new data
            let total_bytes = data_store.total_bytes();

            if let Some(bytes_to_read) = bytes_to_read(total_bytes, last_processed_offset)
                .map(|n| n.min(read_cap.min(u32::MAX as u64) as u32))
            {
                // Read new data from data store
                match data_store.get_data(last_processed_offset, bytes_to_read) {
                    Ok(data) => {
                        read_failures = 0;
                        if !data.is_empty() {
                            let now_ms = start_time.elapsed().as_millis() as u64;
                            let end = last_processed_offset + data.len() as u64;
                            let created_wall = data_store.created_wall_ms();

                            // Cut the read where the store recorded how many bytes
                            // had arrived by when (about every 100 ms), so a
                            // backlog - the plotter opened after data arrived, or a
                            // read capped at MAX_READ_SIZE - keeps its reception
                            // times instead of collapsing onto "now". The tail of
                            // the read takes the NEXT record's time (it may lie
                            // beyond this read); only bytes newer than every
                            // record are timed "now".
                            let mut cuts: Vec<(u64, Option<u64>)> = data_store
                                .timestamps_in(last_processed_offset, end)
                                .into_iter()
                                .filter(|e| e.cumulative_bytes < end)
                                .map(|e| (e.cumulative_bytes, Some(e.timestamp)))
                                .collect();
                            cuts.push((
                                end,
                                data_store.timestamp_at_or_after(end).map(|e| e.timestamp),
                            ));

                            // Slices that end up with the same time (e.g. several
                            // falling back to "now") are spread together, so their
                            // lines don't pile up on one instant.
                            let mut data_points: Vec<ParsedDataPoint> = Vec::new();
                            let mut pending: Vec<ParsedDataPoint> = Vec::new();
                            let mut pending_ts = last_batch_ts;
                            // Whether the pending slices are timed by a store record
                            let mut pending_recorded = false;
                            let mut from = last_processed_offset;
                            for (to, wall) in cuts {
                                if to <= from {
                                    continue;
                                }
                                // Never before the slice still pending (keeps order)
                                let floor = last_batch_ts.max(pending_ts);
                                let ts = slice_timestamp(wall, created_wall, floor, now_ms);
                                let recorded =
                                    wall.map(|t| t.saturating_sub(created_wall)) == Some(ts);
                                let points = parse_range(
                                    &mut parser,
                                    &data_store,
                                    &data,
                                    last_processed_offset,
                                    from..to,
                                    ts,
                                );
                                from = to;
                                if points.is_empty() {
                                    continue;
                                }
                                if ts != pending_ts && !pending.is_empty() {
                                    // Spread timestamps across (prev, ts] so the lines
                                    // keep distinct, monotonically increasing x
                                    // positions. Aim for ~1ms per line, but never go
                                    // below the previous batch's end (keeps
                                    // aggregator input ordered).
                                    last_batch_ts = spread_over_window(
                                        &mut pending,
                                        last_batch_ts,
                                        pending_ts,
                                        if pending_recorded {
                                            RECORD_WINDOW_MS
                                        } else {
                                            0
                                        },
                                    );
                                    data_points.append(&mut pending);
                                    pending_recorded = false;
                                }
                                pending_ts = ts;
                                pending_recorded |= recorded;
                                pending.extend(points);
                            }
                            last_batch_ts = spread_over_window(
                                &mut pending,
                                last_batch_ts,
                                pending_ts,
                                if pending_recorded {
                                    RECORD_WINDOW_MS
                                } else {
                                    0
                                },
                            );
                            data_points.append(&mut pending);

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
                            // We jumped into the middle of the stream
                            parser.begin_untrusted_segment();
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

    /// Connecting mid-line: the cut first line of a new store creates no
    /// phantom channel (docs/27 PF-2)
    #[test]
    fn test_cut_first_line_of_a_new_store_is_dropped() {
        let handle: SharedDataStore = Arc::new(Mutex::new(None));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);
        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());

        let store = Arc::new(DataStore::new().expect("create store"));
        store.push_segment_start(0);
        store.push_test_data(b"05,cos:\naccel:1,temp:2\naccel:3,temp:4\n");
        *handle.lock().unwrap() = Some(store.clone());

        wait_for(|| aggregator.total_points() >= 4, "4 samples");
        thread::sleep(Duration::from_millis(50));
        assert_eq!(aggregator.get_channels(), vec!["accel", "temp"]);
        assert_eq!(aggregator.total_points(), 4);
        thread.stop();
    }

    /// Auto-reconnect resumes on the same store: the line cut by the unplug and
    /// the cut first line after the resume are both dropped, and the data from
    /// before the unplug stays (SYS-F-807)
    #[test]
    fn test_cut_lines_around_a_resume_boundary_are_dropped() {
        let handle: SharedDataStore = Arc::new(Mutex::new(None));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);
        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());

        let store = Arc::new(DataStore::new().expect("create store"));
        store.push_test_data(b"sin:1,cos:2\nsin:3,cos:");
        *handle.lock().unwrap() = Some(store.clone());
        wait_for(|| aggregator.total_points() >= 2, "first line");

        store.push_segment_start(store.total_bytes());
        store.push_test_data(b"accel:9\nsin:5,cos:6\n");
        wait_for(|| aggregator.total_points() >= 4, "line after the resume");
        thread::sleep(Duration::from_millis(50));
        assert_eq!(aggregator.get_channels(), vec!["sin", "cos"]);
        assert_eq!(aggregator.total_points(), 4);
        thread.stop();
    }

    /// The boundary may sit in the middle of one read (the thread was behind)
    #[test]
    fn test_resume_boundary_inside_one_read() {
        let handle: SharedDataStore = Arc::new(Mutex::new(None));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        let store = Arc::new(DataStore::new().expect("create store"));
        store.push_test_data(b"sin:1,cos:2\nsin:3,cos:");
        store.push_segment_start(store.total_bytes());
        store.push_test_data(b"accel:9\nsin:5,cos:6\n");
        *handle.lock().unwrap() = Some(store.clone());

        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());
        wait_for(|| aggregator.total_points() >= 4, "both lines");
        thread::sleep(Duration::from_millis(50));
        assert_eq!(aggregator.get_channels(), vec!["sin", "cos"]);
        assert_eq!(aggregator.total_points(), 4);
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

    /// A new session (store swap) restarts the time axis at 0 (docs/27 PF-5)
    #[test]
    fn test_store_swap_resets_time_origin() {
        let handle: SharedDataStore = Arc::new(Mutex::new(None));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);
        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());

        let store_a = Arc::new(DataStore::new().expect("create store A"));
        store_a.push_test_data(b"1,2\n");
        *handle.lock().unwrap() = Some(store_a);
        wait_for(|| aggregator.total_points() >= 2, "store A");
        thread::sleep(Duration::from_millis(500));

        let store_b = Arc::new(DataStore::new().expect("create store B"));
        *handle.lock().unwrap() = Some(store_b.clone());
        thread::sleep(Duration::from_millis(50));
        store_b.push_test_data(b"3,4\n");
        wait_for(
            || {
                aggregator
                    .get_channel_info()
                    .iter()
                    .any(|c| c.latest_value.as_deref() == Some("3.00"))
            },
            "store B",
        );
        thread.stop();

        let payload = aggregator.get_chart_data(&PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 800,
            is_realtime: false,
        });
        assert_eq!(aggregator.total_points(), 2, "store A's data must be gone");
        assert!(
            payload.end_ms < 300,
            "time origin not reset: {}",
            payload.end_ms
        );
    }

    /// Clear while the port is closed (the store goes away) empties the plot
    /// at once, and the next session does not mix with the old one
    /// (SYS-F-802 / SYS-F-806)
    #[test]
    fn test_detach_clears_the_plot() {
        let handle: SharedDataStore = Arc::new(Mutex::new(None));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);
        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());

        let store_a = Arc::new(DataStore::new().expect("create store A"));
        store_a.push_test_data(b"old:1\n");
        *handle.lock().unwrap() = Some(store_a);
        wait_for(|| aggregator.total_points() >= 1, "store A");

        *handle.lock().unwrap() = None;
        wait_for(
            || aggregator.get_channels().is_empty(),
            "plot cleared on detach",
        );

        let store_b = Arc::new(DataStore::new().expect("create store B"));
        store_b.push_test_data(b"new:2\n");
        *handle.lock().unwrap() = Some(store_b);
        wait_for(|| aggregator.total_points() >= 1, "store B");
        thread.stop();
        assert_eq!(aggregator.get_channels(), vec!["new"]);
    }

    /// A store "created" `secs` ago holding `lines` lines received one per second
    /// (line i - 2 bytes - had arrived by i+1 s), as when the plotter is opened
    /// after data has been flowing
    fn store_with_backlog(secs: u64, lines: u64) -> Arc<DataStore> {
        let mut store = DataStore::new().expect("create store");
        store.backdate_for_test(Duration::from_secs(secs));
        let t0 = store.created_wall_ms();
        for i in 0..lines {
            store.push_test_data(format!("{}\n", i).as_bytes());
            store.push_test_timestamp(2 * (i + 1), t0 + 1000 * (i + 1));
        }
        Arc::new(store)
    }

    /// Timestamps (s) of all drawn points
    fn drawn_times(aggregator: &PlotterAggregator) -> Vec<f64> {
        let payload = aggregator.get_chart_data(&PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 800,
            is_realtime: false,
        });
        payload.aligned_data[0].iter().flatten().copied().collect()
    }

    fn assert_one_per_second(ts: &[f64], lines: usize) {
        assert_eq!(ts.len(), lines, "{ts:?}");
        for (i, t) in ts.iter().enumerate() {
            let want = (i + 1) as f64;
            assert!(
                (t - want).abs() <= 0.05,
                "line {i} at {t} s, want ~{want} s: {ts:?}"
            );
        }
    }

    /// Opening the plotter after data has arrived: the backlog keeps the times it
    /// was received at, instead of collapsing onto the attach instant
    #[test]
    fn test_backlog_keeps_its_reception_times() {
        let handle: SharedDataStore = Arc::new(Mutex::new(Some(store_with_backlog(11, 10))));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());
        wait_for(|| aggregator.total_points() >= 10, "10 points");
        thread.stop();

        assert_one_per_second(&drawn_times(&aggregator), 10);
    }

    /// The same when the backlog spans several reads (larger than the read cap):
    /// the tail of each read takes the next record's time, not "now"
    #[test]
    fn test_backlog_spanning_reads_keeps_its_times() {
        let handle: SharedDataStore = Arc::new(Mutex::new(Some(store_with_backlog(11, 10))));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        // 8-byte reads: 4 lines per read, cut mid-way through the backlog
        let mut thread = PlotterThread::start_with_read_cap(handle.clone(), aggregator.clone(), 8);
        wait_for(|| aggregator.total_points() >= 10, "10 points");
        thread.stop();

        assert_one_per_second(&drawn_times(&aggregator), 10);
    }

    /// Records that disagree with the live clock (wall clock ahead of it, or
    /// behind the previous batch) fall back to "now": no point is lost, order
    /// is kept and nothing lies in the future
    #[test]
    fn test_records_off_the_clock_fall_back_to_now() {
        let mut store = DataStore::new().expect("create store");
        store.backdate_for_test(Duration::from_secs(2));
        let t0 = store.created_wall_ms();
        store.push_test_data(b"1\n2\n");
        store.push_test_timestamp(4, t0 + 60_000); // a minute in the future
        store.push_test_data(b"3\n4\n");
        store.push_test_timestamp(8, t0 + 60_001);
        let handle: SharedDataStore = Arc::new(Mutex::new(Some(Arc::new(store))));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());
        wait_for(|| aggregator.total_points() >= 4, "4 points");
        thread.stop();

        let ts = drawn_times(&aggregator);
        assert_eq!(ts.len(), 4, "{ts:?}");
        assert!(ts.windows(2).all(|w| w[0] < w[1]), "{ts:?}");
        assert!(ts.iter().all(|&t| (1.9..=3.0).contains(&t)), "{ts:?}");
    }

    /// A resume boundary inside a recorded slice: the cut lines are still
    /// dropped and the times stay ordered
    #[test]
    fn test_resume_boundary_inside_a_recorded_slice() {
        let mut store = DataStore::new().expect("create store");
        store.backdate_for_test(Duration::from_secs(3));
        let t0 = store.created_wall_ms();
        store.push_test_data(b"sin:1,cos:2\nsin:3,cos:");
        store.push_segment_start(store.total_bytes());
        store.push_test_data(b"accel:9\nsin:5,cos:6\n");
        store.push_test_timestamp(store.total_bytes(), t0 + 2000);
        let handle: SharedDataStore = Arc::new(Mutex::new(Some(Arc::new(store))));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());
        wait_for(|| aggregator.total_points() >= 4, "both lines");
        thread::sleep(Duration::from_millis(50));
        thread.stop();

        assert_eq!(aggregator.get_channels(), vec!["sin", "cos"]);
        assert_eq!(aggregator.total_points(), 4);
        let ts = drawn_times(&aggregator);
        assert!(ts.windows(2).all(|w| w[0] < w[1]), "{ts:?}");
        assert!(ts.iter().all(|&t| (1.9..=2.0).contains(&t)), "{ts:?}");
    }

    /// The lines of one recorded ~100 ms slice are spread across it, not bunched
    /// into its last few ms (a backlog would otherwise draw as a staircase)
    #[test]
    fn test_recorded_slice_lines_are_spread_across_its_window() {
        let mut store = DataStore::new().expect("create store");
        store.backdate_for_test(Duration::from_secs(2));
        let t0 = store.created_wall_ms();
        store.push_test_data(b"1\n2\n3\n4\n5\n");
        store.push_test_timestamp(10, t0 + 1000);
        let handle: SharedDataStore = Arc::new(Mutex::new(Some(Arc::new(store))));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());
        wait_for(|| aggregator.total_points() >= 5, "5 points");
        thread.stop();

        let ts = drawn_times(&aggregator);
        assert_eq!(ts, vec![0.92, 0.94, 0.96, 0.98, 1.0]);
    }

    #[test]
    fn test_slice_timestamp_exact_vectors() {
        // (entry_wall, created_wall, last_batch_ts, now) -> ts
        for (entry, created, last, now, want) in [
            (Some(1500), 1000, 0, 900, 500),   // in range: the record's time
            (Some(1500), 1000, 500, 900, 500), // == last is still in range
            (Some(1500), 1000, 501, 900, 900), // behind the previous batch: now
            (Some(1500), 1000, 0, 499, 499),   // ahead of now: now
            (Some(900), 1000, 0, 300, 0),      // before the store: 0
            (None, 1000, 100, 300, 300),       // no record: now
        ] {
            assert_eq!(
                slice_timestamp(entry, created, last, now),
                want,
                "{entry:?} {created} {last} {now}"
            );
        }
    }

    /// The same when the store is found late (the flaky case of
    /// test_batch_timestamps_are_spread under load): lines already waiting are
    /// placed by the session clock, not all at the attach instant
    #[test]
    fn test_lines_waiting_at_attach_stay_distinct() {
        let handle: SharedDataStore = Arc::new(Mutex::new(None));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        let store = Arc::new(DataStore::new().expect("create store"));
        thread::sleep(Duration::from_millis(60));
        store.push_test_data(b"1\n2\n3\n4\n5\n6\n7\n8\n");
        *handle.lock().unwrap() = Some(store);

        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());
        wait_for(|| aggregator.total_points() >= 8, "8 points");
        thread.stop();

        let payload = aggregator.get_chart_data(&PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 800,
            is_realtime: false,
        });
        assert_eq!(payload.aligned_data[0].len(), 8, "points collapsed");
    }

    #[test]
    fn test_batch_timestamps_are_spread() {
        // Lines read in one poll must not collapse onto a single timestamp
        let handle: SharedDataStore = Arc::new(Mutex::new(None));
        let aggregator = PlotterAggregator::new();
        aggregator.set_enabled(true);

        let mut thread = PlotterThread::start(handle.clone(), aggregator.clone());

        // As in the app: the session's store is attached (its clock starts at
        // 0, PF-5), and lines arrive a little later, 8 of them in one poll
        let store = Arc::new(DataStore::new().expect("create store"));
        *handle.lock().unwrap() = Some(store.clone());
        thread::sleep(Duration::from_millis(60));
        // 8 lines, each kept as its own sample (1 ms cells of tier 0)
        store.push_test_data(b"1\n2\n3\n4\n5\n6\n7\n8\n");

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
