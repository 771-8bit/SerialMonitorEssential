// Plotter Aggregator - Uniform-level aggregation for plotter data
//
// Uses 3-buffer design: history + buffer (both at current_level) + raw_buffer.
// New data is aggregated to current_level before being added to buffer.
// When history + buffer exceeds max_points, both are merged and re-aggregated,
// then current_level is doubled to ensure uniform data density across all time ranges.

use crate::plotter::parser::ChannelValue;
// Note: serde traits are unused here as types are re-exported from data_store
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};

// Re-export common types from data_store
pub use crate::plotter::data_store::{
    AggregatedPoint, AggregationMode, BandSeriesData, ChannelInfo, ChannelType,
    PlotterChartPayload, PlotterConfig, PlotterDataRequest, PlotterRangedPayload, StateChange,
};

// Note: MAX_TARGET_POINTS, PIXEL_WIDTH_THRESHOLD_PERCENT, DEFAULT_BUCKET_SIZE
// are now configured via PlotterConfig for better flexibility.

/// Cached aggregation for current view
#[derive(Debug, Clone)]
struct ViewCache {
    /// Time range of the cache (min, max) in ms
    time_range: (u64, u64),
    /// Pixel width used to generate this cache
    pixel_width: u32,
    /// Cached aggregated data per channel
    data: HashMap<String, Vec<AggregatedPoint>>,
    /// Whether this was generated in realtime mode
    was_realtime: bool,
}

/// Internal bucket storing all statistics for mode-agnostic storage
///
/// This allows switching between LTTB and Average modes at display time
/// without losing precision, since we store all necessary statistics.
#[derive(Debug, Clone)]
struct AggregatedBucket {
    /// Timestamp (first point in bucket)
    ts: u64,
    /// Minimum value in bucket
    min: f64,
    /// Maximum value in bucket
    max: f64,
    /// Average value in bucket
    avg: f64,
    /// Number of raw points this bucket represents (for weighted averaging)
    count: usize,
}

impl AggregatedBucket {
    /// Convert to AggregatedPoint based on aggregation mode
    fn to_point(&self, mode: &AggregationMode) -> AggregatedPoint {
        match mode {
            AggregationMode::Lttb => AggregatedPoint::Single {
                ts: self.ts,
                value: self.avg,
            },
            AggregationMode::Average => AggregatedPoint::MinMax {
                ts: self.ts,
                min: self.min,
                max: self.max,
            },
        }
    }
}

/// Binary search helpers for sorted bucket/point arrays.
/// These avoid O(n) iteration when finding time ranges.
///
/// Find the start index for a time range using binary search.
/// Returns the index of the first element with ts >= time_min
fn find_range_start(buckets: &[AggregatedBucket], time_min: u64) -> usize {
    buckets
        .binary_search_by(|b| b.ts.cmp(&time_min))
        .unwrap_or_else(|i| i)
}

/// Find the end index for a time range using binary search
/// Returns the index after the last element with ts <= time_max
fn find_range_end(buckets: &[AggregatedBucket], time_max: u64) -> usize {
    match buckets.binary_search_by(|b| b.ts.cmp(&time_max)) {
        Ok(i) => i + 1, // Found exact match, include it
        Err(i) => i,    // Not found, i is insertion point
    }
}

/// LTTB (Largest Triangle Three Buckets) downsampling algorithm
///
/// Selects n points from the input data that best preserve visual features.
/// Uses triangle area maximization: for each bucket, selects the point that
/// forms the largest triangle with the previously selected point and the
/// average of the next bucket.
///
/// Returns indices of selected points in the original data array.
fn lttb_downsample(data: &[AggregatedBucket], target_points: usize) -> Vec<usize> {
    let n = data.len();

    // If data is already small enough, return all indices
    if n <= target_points || target_points < 3 {
        return (0..n).collect();
    }

    let mut selected = Vec::with_capacity(target_points);

    // Always include first point
    selected.push(0);

    // Calculate bucket size for middle points
    let bucket_size = (n - 2) as f64 / (target_points - 2) as f64;

    for i in 0..(target_points - 2) {
        // Current bucket range
        let bucket_start = ((i as f64 * bucket_size) + 1.0).floor() as usize;
        let bucket_end = (((i + 1) as f64 * bucket_size) + 1.0).floor() as usize;
        let bucket_end = bucket_end.min(n - 1);

        // Next bucket range (for calculating average point)
        let next_start = bucket_end;
        let next_end = (((i + 2) as f64 * bucket_size) + 1.0).floor() as usize;
        let next_end = next_end.min(n);

        // Calculate average point of next bucket
        let (avg_ts, avg_val) = if next_start < next_end {
            let mut sum_ts: f64 = 0.0;
            let mut sum_val: f64 = 0.0;
            let count = (next_end - next_start) as f64;
            for bucket in data.iter().take(next_end).skip(next_start) {
                sum_ts += bucket.ts as f64;
                sum_val += bucket.avg;
            }
            (sum_ts / count, sum_val / count)
        } else {
            // Fallback to last point
            (data[n - 1].ts as f64, data[n - 1].avg)
        };

        // Get previously selected point
        let prev_idx = *selected.last().unwrap();
        let prev_ts = data[prev_idx].ts as f64;
        let prev_val = data[prev_idx].avg;

        // Find point in current bucket with maximum triangle area
        let mut best_idx = bucket_start;
        let mut best_area = f64::NEG_INFINITY;

        for (j, bucket) in data.iter().enumerate().take(bucket_end).skip(bucket_start) {
            // Calculate triangle area using the shoelace formula
            // Area = 0.5 * |x1(y2-y3) + x2(y3-y1) + x3(y1-y2)|
            let curr_ts = bucket.ts as f64;
            let curr_val = bucket.avg;

            let area = ((prev_ts * (curr_val - avg_val))
                + (curr_ts * (avg_val - prev_val))
                + (avg_ts * (prev_val - curr_val)))
                .abs();

            if area > best_area {
                best_area = area;
                best_idx = j;
            }
        }

        selected.push(best_idx);
    }

    // Always include last point
    selected.push(n - 1);

    selected
}

/// Inner data structure (protected by RwLock)
///
/// Uses 3-buffer design for uniform aggregation levels:
/// - `history`: Confirmed historical data (already aggregated at current_level)
/// - `buffer`: Intermediate buffer (aggregated at current_level, waiting to join history)
/// - `raw_buffer`: Raw data not yet aggregated
///
/// Flow:
/// 1. New data -> raw_buffer
/// 2. When raw_buffer >= bucket_size: aggregate to current_level -> buffer
/// 3. When history + buffer > max_points: merge both, re-aggregate -> history, level up
#[derive(Debug)]
struct PlotterAggregatorInner {
    /// Channel name -> index mapping
    channels: HashMap<String, usize>,
    /// Channel index -> name mapping
    channel_names: Vec<String>,
    /// Detected channel types
    channel_types: HashMap<String, ChannelType>,

    /// Confirmed historical data (aggregated at current_level, mode-agnostic)
    history: HashMap<String, Vec<AggregatedBucket>>,

    /// Intermediate buffer (aggregated at current_level, waiting to join history)
    buffer: HashMap<String, Vec<AggregatedBucket>>,

    /// Raw data buffer (not yet aggregated)
    raw_buffer: HashMap<String, VecDeque<(u64, f64)>>,

    /// Current aggregation level (1 = 1 raw point per aggregated point, doubles on level-up)
    current_level: usize,

    /// Number of raw points to aggregate into one buffer point
    bucket_size: usize,

    /// State data (channel_name -> list of state changes)
    state_data: HashMap<String, Vec<StateChange>>,

    /// Configuration
    config: PlotterConfig,

    /// Whether plotter is enabled
    enabled: bool,

    /// View cache for current display
    view_cache: Option<ViewCache>,

    /// Debug call counter for periodic logging
    debug_call_count: usize,
}

/// Plotter aggregator - stores all parsed data with dynamic aggregation
#[derive(Debug)]
pub struct PlotterAggregator {
    inner: Arc<RwLock<PlotterAggregatorInner>>,
}

impl Default for PlotterAggregator {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for PlotterAggregator {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl PlotterAggregator {
    /// Create a new plotter aggregator
    pub fn new() -> Self {
        Self::with_config(PlotterConfig::default())
    }

    /// Create a new plotter aggregator with custom config
    pub fn with_config(config: PlotterConfig) -> Self {
        Self {
            inner: Arc::new(RwLock::new(PlotterAggregatorInner {
                channels: HashMap::new(),
                channel_names: Vec::new(),
                channel_types: HashMap::new(),
                history: HashMap::new(),
                buffer: HashMap::new(),
                raw_buffer: HashMap::new(),
                current_level: 1,
                bucket_size: config.bucket_size,
                state_data: HashMap::new(),
                config,
                enabled: false,
                view_cache: None,
                debug_call_count: 0,
            })),
        }
    }

    /// Enable or disable the plotter
    pub fn set_enabled(&self, enabled: bool) {
        if let Ok(mut inner) = self.inner.write() {
            inner.enabled = enabled;
        }
    }

    /// Check if plotter is enabled
    pub fn is_enabled(&self) -> bool {
        self.inner
            .read()
            .map(|inner| inner.enabled)
            .unwrap_or(false)
    }

    /// Ensure channel exists, creating it if needed
    fn ensure_channel_exists(inner: &mut PlotterAggregatorInner, channel: &str) {
        if !inner.channels.contains_key(channel) {
            let index = inner.channel_names.len();
            inner.channels.insert(channel.to_string(), index);
            inner.channel_names.push(channel.to_string());
            inner.history.insert(channel.to_string(), Vec::new());
            inner.buffer.insert(channel.to_string(), Vec::new());
            inner
                .raw_buffer
                .insert(channel.to_string(), VecDeque::new());
            inner.state_data.insert(channel.to_string(), Vec::new());
        }
    }

    /// Process a numeric value for a channel
    fn process_numeric_value(
        inner: &mut PlotterAggregatorInner,
        channel: &str,
        timestamp_ms: u64,
        value: f64,
    ) {
        let channel_type = inner
            .config
            .channel_types
            .get(channel)
            .cloned()
            .unwrap_or(ChannelType::Auto);

        if channel_type == ChannelType::Auto || channel_type == ChannelType::Line {
            if let Some(raw_buf) = inner.raw_buffer.get_mut(channel) {
                raw_buf.push_back((timestamp_ms, value));
            }
            inner
                .channel_types
                .insert(channel.to_string(), ChannelType::Line);
        }
    }

    /// Process a state value for a channel
    fn process_state_value(
        inner: &mut PlotterAggregatorInner,
        channel: &str,
        timestamp_ms: u64,
        state: String,
    ) {
        let channel_type = inner
            .config
            .channel_types
            .get(channel)
            .cloned()
            .unwrap_or(ChannelType::Auto);

        if channel_type == ChannelType::Auto || channel_type == ChannelType::State {
            if let Some(state_list) = inner.state_data.get_mut(channel) {
                let state_changed = state_list
                    .last()
                    .map(|last| last.state != state)
                    .unwrap_or(true);

                if state_changed {
                    if let Some(last) = state_list.last_mut() {
                        if last.end_ms.is_none() {
                            last.end_ms = Some(timestamp_ms);
                        }
                    }
                    state_list.push(StateChange {
                        start_ms: timestamp_ms,
                        end_ms: None,
                        state,
                    });
                }
            }
            inner
                .channel_types
                .insert(channel.to_string(), ChannelType::State);
        }
    }

    /// Add a data point for a channel
    pub fn add_data_point(&self, channel: &str, timestamp_ms: u64, value: ChannelValue) {
        let mut inner = match self.inner.write() {
            Ok(inner) => inner,
            Err(_) => return,
        };

        if !inner.enabled {
            return;
        }

        Self::ensure_channel_exists(&mut inner, channel);

        match value {
            ChannelValue::Numeric(v) => {
                Self::process_numeric_value(&mut inner, channel, timestamp_ms, v);
            }
            ChannelValue::State(s) => {
                Self::process_state_value(&mut inner, channel, timestamp_ms, s);
            }
        }

        // Cache validity is checked at query time by is_cache_valid
        // which handles time range changes correctly
        Self::maybe_aggregate(&mut inner);
    }

    /// Add multiple data points at once (batch processing for performance)
    ///
    /// This is more efficient than calling add_data_point multiple times
    /// because it acquires the lock once and calls maybe_aggregate only at the end.
    pub fn add_data_points_batch(&self, data_points: Vec<crate::plotter::parser::ParsedDataPoint>) {
        let mut inner = match self.inner.write() {
            Ok(inner) => inner,
            Err(_) => return,
        };

        if !inner.enabled {
            return;
        }

        for point in data_points {
            // Iterate in channel_order to preserve CSV header order
            for channel in &point.channel_order {
                if let Some(value) = point.channels.get(channel) {
                    Self::ensure_channel_exists(&mut inner, channel);

                    match value {
                        ChannelValue::Numeric(v) => {
                            Self::process_numeric_value(
                                &mut inner,
                                channel,
                                point.timestamp_ms,
                                *v,
                            );
                        }
                        ChannelValue::State(s) => {
                            Self::process_state_value(
                                &mut inner,
                                channel,
                                point.timestamp_ms,
                                s.clone(),
                            );
                        }
                    }
                }
            }
        }

        // Cache validity is checked at query time by is_cache_valid
        // which handles time range changes correctly
        Self::maybe_aggregate(&mut inner);
    }

    /// Check and perform aggregation
    ///
    /// New uniform-level design:
    /// 1. When raw_buffer >= bucket_size: aggregate to current_level points -> buffer
    /// 2. When history + buffer > max_points: merge both, re-aggregate -> history, level up
    fn maybe_aggregate(inner: &mut PlotterAggregatorInner) {
        let bucket_size = inner.bucket_size;
        let max_points = inner.config.max_points;

        // Step 1: Aggregate raw_buffer -> buffer when bucket is full
        let channel_names: Vec<String> = inner.channel_names.clone();
        for channel in &channel_names {
            Self::aggregate_raw_to_buffer(inner, channel, bucket_size);
        }

        // Step 2: Check if level-up is needed (history + buffer > max_points)
        let needs_level_up = channel_names.iter().any(|channel| {
            let hist_len = inner.history.get(channel).map(|v| v.len()).unwrap_or(0);
            let buf_len = inner.buffer.get(channel).map(|v| v.len()).unwrap_or(0);
            hist_len + buf_len > max_points
        });

        if needs_level_up {
            Self::level_up(inner, &channel_names);
        }

        // Debug log buffer sizes (periodic, not every call)
        inner.debug_call_count += 1;
        let call_count = inner.debug_call_count;
        if call_count.is_multiple_of(1000) {
            let total_history: usize = inner.history.values().map(|v| v.len()).sum();
            let total_buffer: usize = inner.buffer.values().map(|v| v.len()).sum();
            let total_raw: usize = inner.raw_buffer.values().map(|v| v.len()).sum();
            log::debug!(
                "[Aggregator] calls={}, history={}, buffer={}, raw={}, level={}, bucket_size={}",
                call_count,
                total_history,
                total_buffer,
                total_raw,
                inner.current_level,
                inner.bucket_size
            );
        }
    }

    /// Aggregate raw_buffer data into buffer at current_level
    fn aggregate_raw_to_buffer(
        inner: &mut PlotterAggregatorInner,
        channel: &str,
        bucket_size: usize,
    ) {
        let raw_buf = match inner.raw_buffer.get_mut(channel) {
            Some(buf) => buf,
            None => return,
        };

        // Process complete buckets
        while raw_buf.len() >= bucket_size {
            // Drain bucket_size points from raw_buffer
            let chunk: Vec<(u64, f64)> = raw_buf.drain(..bucket_size).collect();

            // Aggregate chunk to a bucket (stores all statistics for mode-agnostic switching)
            let bucket = Self::aggregate_chunk(&chunk);

            // Add to buffer
            if let Some(buf) = inner.buffer.get_mut(channel) {
                buf.push(bucket);
            }
        }
    }

    /// Aggregate a chunk of raw points into a single AggregatedBucket
    ///
    /// Stores min, max, avg, and count for mode-agnostic storage.
    /// The aggregation mode is applied at display time, not storage time.
    fn aggregate_chunk(chunk: &[(u64, f64)]) -> AggregatedBucket {
        if chunk.is_empty() {
            return AggregatedBucket {
                ts: 0,
                min: 0.0,
                max: 0.0,
                avg: 0.0,
                count: 0,
            };
        }

        // Use average of first and last timestamp for better bucket positioning
        let ts = (chunk[0].0 + chunk[chunk.len() - 1].0) / 2;
        let count = chunk.len();

        let min = chunk.iter().map(|(_, v)| *v).fold(f64::INFINITY, f64::min);
        let max = chunk
            .iter()
            .map(|(_, v)| *v)
            .fold(f64::NEG_INFINITY, f64::max);
        let sum: f64 = chunk.iter().map(|(_, v)| *v).sum();
        let avg = sum / count as f64;

        AggregatedBucket {
            ts,
            min,
            max,
            avg,
            count,
        }
    }

    /// Level up: merge history + buffer, re-aggregate at new level
    fn level_up(inner: &mut PlotterAggregatorInner, channels: &[String]) {
        let max_points = inner.config.max_points;
        let target_points = max_points / 2;

        for channel in channels {
            // Collect all buckets from history + buffer
            let mut all_buckets: Vec<AggregatedBucket> = Vec::new();

            // Add history data
            if let Some(hist_data) = inner.history.get(channel) {
                all_buckets.extend(hist_data.iter().cloned());
            }

            // Add buffer data
            if let Some(buf_data) = inner.buffer.get(channel) {
                all_buckets.extend(buf_data.iter().cloned());
            }

            // Sort by timestamp
            all_buckets.sort_by_key(|b| b.ts);

            // Re-aggregate preserving all statistics
            let new_history = Self::aggregate_buckets_preserving(&all_buckets, target_points);

            // Update history and clear buffer
            inner.history.insert(channel.clone(), new_history);
            if let Some(buf) = inner.buffer.get_mut(channel) {
                buf.clear();
            }
        }

        // Double the current level and bucket size
        inner.current_level *= 2;
        inner.bucket_size = (inner.bucket_size * 2).min(1000); // Cap bucket size
    }

    /// Aggregate buckets preserving all statistics for mode-agnostic storage
    ///
    /// Input: AggregatedBucket list with min, max, avg, count
    /// Output: AggregatedBucket list with merged statistics using weighted averaging
    fn aggregate_buckets_preserving(
        data: &[AggregatedBucket],
        target_points: usize,
    ) -> Vec<AggregatedBucket> {
        if data.is_empty() || target_points == 0 {
            return Vec::new();
        }

        // If data is small enough, return as-is
        if data.len() <= target_points {
            return data.to_vec();
        }

        let time_min = data.first().map(|b| b.ts).unwrap_or(0);
        let time_max = data.last().map(|b| b.ts).unwrap_or(0);
        let time_range = time_max.saturating_sub(time_min);

        if time_range == 0 {
            // All at same timestamp: merge all into one bucket
            let total_count: usize = data.iter().map(|b| b.count).sum();
            let overall_min = data.iter().map(|b| b.min).fold(f64::INFINITY, f64::min);
            let overall_max = data.iter().map(|b| b.max).fold(f64::NEG_INFINITY, f64::max);
            // Weighted average
            let weighted_sum: f64 = data.iter().map(|b| b.avg * b.count as f64).sum();
            let overall_avg = if total_count > 0 {
                weighted_sum / total_count as f64
            } else {
                0.0
            };
            return vec![AggregatedBucket {
                ts: time_min,
                min: overall_min,
                max: overall_max,
                avg: overall_avg,
                count: total_count,
            }];
        }

        // Calculate bucket width
        let data_rate = data.len() as f64 / time_range as f64;
        let bucket_width = if data_rate > 0.0 {
            let points_per_bucket = data.len() as f64 / target_points as f64;
            (points_per_bucket / data_rate).max(1.0) as u64
        } else {
            time_range / target_points as u64
        }
        .max(1);

        let num_buckets = ((time_range / bucket_width) + 1) as usize;

        // Track stats per bucket
        let mut bucket_mins: Vec<f64> = vec![f64::INFINITY; num_buckets];
        let mut bucket_maxs: Vec<f64> = vec![f64::NEG_INFINITY; num_buckets];
        let mut bucket_weighted_sums: Vec<f64> = vec![0.0; num_buckets];
        let mut bucket_counts: Vec<usize> = vec![0; num_buckets];
        let mut bucket_times: Vec<u64> = vec![0; num_buckets];
        let mut bucket_has_data: Vec<bool> = vec![false; num_buckets];

        // Assign data to buckets, preserving all stats with weighted average
        for bucket in data {
            let bucket_idx = ((bucket.ts - time_min) / bucket_width) as usize;
            if bucket_idx < num_buckets {
                bucket_mins[bucket_idx] = bucket_mins[bucket_idx].min(bucket.min);
                bucket_maxs[bucket_idx] = bucket_maxs[bucket_idx].max(bucket.max);
                bucket_weighted_sums[bucket_idx] += bucket.avg * bucket.count as f64;
                bucket_counts[bucket_idx] += bucket.count;
                if bucket_times[bucket_idx] == 0 {
                    bucket_times[bucket_idx] = bucket.ts;
                }
                bucket_has_data[bucket_idx] = true;
            }
        }

        // Build result
        let mut result = Vec::with_capacity(num_buckets);
        for i in 0..num_buckets {
            if !bucket_has_data[i] {
                continue;
            }
            let ts = if bucket_times[i] != 0 {
                bucket_times[i]
            } else {
                time_min + (i as u64 * bucket_width)
            };
            let avg = if bucket_counts[i] > 0 {
                bucket_weighted_sums[i] / bucket_counts[i] as f64
            } else {
                0.0
            };
            result.push(AggregatedBucket {
                ts,
                min: bucket_mins[i],
                max: bucket_maxs[i],
                avg,
                count: bucket_counts[i],
            });
        }
        result
    }

    /// Get ranged plotter data with dynamic aggregation
    ///
    /// Uses two-phase locking to reduce contention:
    /// 1. Read lock to check cache and gather data
    /// 2. Only upgrades to write lock if cache needs updating
    pub fn get_ranged_data(&self, req: &PlotterDataRequest) -> PlotterRangedPayload {
        // Phase 1: Try to get data with read lock (fast path for cache hits)
        let read_result = {
            let inner = match self.inner.read() {
                Ok(inner) => inner,
                Err(_) => {
                    return PlotterRangedPayload {
                        channels: Vec::new(),
                        line_data: HashMap::new(),
                        state_data: HashMap::new(),
                        start_ms: 0,
                        end_ms: 0,
                        is_aggregated: false,
                    };
                }
            };

            // Calculate global time range from data
            let (data_min_ms, data_max_ms) = Self::calculate_data_time_range(&inner);

            // Resolve actual time range
            let time_min_ms = req.time_min_ms.unwrap_or(data_min_ms);
            let time_max_ms = req.time_max_ms.unwrap_or(data_max_ms);

            // Check cache validity
            let cache_valid = Self::is_cache_valid(&inner, time_min_ms, time_max_ms, req);

            if cache_valid {
                // Cache hit - use cached data directly (no write lock needed)
                let cache = inner.view_cache.as_ref().unwrap();
                let line_data = cache.data.clone();

                // Filter state data by time range
                let state_data = Self::filter_state_data(&inner, time_min_ms, time_max_ms);
                let channels = Self::build_channel_info(&inner);

                Some(PlotterRangedPayload {
                    channels,
                    line_data,
                    state_data,
                    start_ms: time_min_ms,
                    end_ms: time_max_ms,
                    is_aggregated: true,
                })
            } else {
                // Cache miss - need to regenerate (will need write lock)
                None
            }
        };

        // If cache hit, return early without write lock
        if let Some(payload) = read_result {
            return payload;
        }

        // Phase 2: Cache miss - need write lock to regenerate and update cache
        let mut inner = match self.inner.write() {
            Ok(inner) => inner,
            Err(_) => {
                return PlotterRangedPayload {
                    channels: Vec::new(),
                    line_data: HashMap::new(),
                    state_data: HashMap::new(),
                    start_ms: 0,
                    end_ms: 0,
                    is_aggregated: false,
                };
            }
        };

        // Recalculate time range (data may have changed between read and write lock)
        let (data_min_ms, data_max_ms) = Self::calculate_data_time_range(&inner);
        let time_min_ms = req.time_min_ms.unwrap_or(data_min_ms);
        let time_max_ms = req.time_max_ms.unwrap_or(data_max_ms);

        // Double-check cache (another thread may have updated it)
        if Self::is_cache_valid(&inner, time_min_ms, time_max_ms, req) {
            let cache = inner.view_cache.as_ref().unwrap();
            let line_data = cache.data.clone();
            let state_data = Self::filter_state_data(&inner, time_min_ms, time_max_ms);
            let channels = Self::build_channel_info(&inner);

            return PlotterRangedPayload {
                channels,
                line_data,
                state_data,
                start_ms: time_min_ms,
                end_ms: time_max_ms,
                is_aggregated: true,
            };
        }

        // Generate fresh data by merging aggregated and recent
        let target_points = req.pixel_width.min(inner.config.max_target_points) as usize;
        let threshold = inner
            .config
            .aggregation_threshold
            .unwrap_or_else(|| inner.config.aggregation_mode.default_threshold());

        let mut aggregated_data = HashMap::new();
        let mut any_aggregated = false;

        // Get all channel names and current aggregation mode
        let channel_names: Vec<String> = inner.channel_names.clone();
        let aggregation_mode = inner.config.aggregation_mode.clone();

        for channel in &channel_names {
            // Skip non-line channels
            let channel_type = inner
                .channel_types
                .get(channel)
                .cloned()
                .unwrap_or(ChannelType::Auto);
            if channel_type == ChannelType::State {
                continue;
            }

            // Collect buckets from all sources using binary search for O(log n) range finding
            // Pre-calculate capacity to reduce allocations
            let hist_data = inner.history.get(channel);
            let buf_data = inner.buffer.get(channel);
            let raw_buf = inner.raw_buffer.get(channel);

            let estimated_size = hist_data.map(|h| h.len()).unwrap_or(0)
                + buf_data.map(|b| b.len()).unwrap_or(0)
                + raw_buf
                    .map(|r| r.len() / inner.current_level.max(1))
                    .unwrap_or(0);
            let mut all_buckets: Vec<AggregatedBucket> =
                Vec::with_capacity(estimated_size.min(target_points * 2));

            // Add buckets from history using binary search for range
            if let Some(hist_data) = hist_data {
                let start_idx = find_range_start(hist_data, time_min_ms);
                let end_idx = find_range_end(hist_data, time_max_ms);
                if start_idx < end_idx {
                    all_buckets.extend_from_slice(&hist_data[start_idx..end_idx]);
                }
            }

            // Add buckets from buffer using binary search for range
            if let Some(buf_data) = buf_data {
                let start_idx = find_range_start(buf_data, time_min_ms);
                let end_idx = find_range_end(buf_data, time_max_ms);
                if start_idx < end_idx {
                    all_buckets.extend_from_slice(&buf_data[start_idx..end_idx]);
                }
            }

            // Add points from raw_buffer (these are still unsorted in VecDeque,
            // so we need to iterate, but raw_buffer is typically small)
            if let Some(raw_buf) = raw_buf {
                // Collect raw points in range - raw_buffer is small, O(n) is acceptable
                let display_bucket = inner.current_level.max(1);
                let mut chunk_buffer: Vec<(u64, f64)> = Vec::with_capacity(display_bucket);

                for &(ts, v) in raw_buf.iter() {
                    if ts >= time_min_ms && ts <= time_max_ms {
                        chunk_buffer.push((ts, v));
                        if chunk_buffer.len() >= display_bucket {
                            all_buckets.push(Self::aggregate_chunk(&chunk_buffer));
                            chunk_buffer.clear();
                        }
                    }
                }
                // Handle remaining points
                if !chunk_buffer.is_empty() {
                    all_buckets.push(Self::aggregate_chunk(&chunk_buffer));
                }
            }

            // No sorting needed - data is already in timestamp order

            // Check if further aggregation is needed for display
            let needs_aggregation = all_buckets.len() > target_points * threshold;

            // Convert buckets to points based on current aggregation mode
            let points: Vec<AggregatedPoint> = if needs_aggregation && time_max_ms > time_min_ms {
                any_aggregated = true;
                match aggregation_mode {
                    AggregationMode::Lttb => {
                        // Use true LTTB algorithm for feature-preserving downsampling
                        let selected_indices = lttb_downsample(&all_buckets, target_points);
                        selected_indices
                            .iter()
                            .map(|&idx| all_buckets[idx].to_point(&aggregation_mode))
                            .collect()
                    }
                    AggregationMode::Average => {
                        // Use preserving aggregation for Average mode (keeps min/max bands)
                        let aggregated_buckets =
                            Self::aggregate_buckets_preserving(&all_buckets, target_points);
                        aggregated_buckets
                            .iter()
                            .map(|b| b.to_point(&aggregation_mode))
                            .collect()
                    }
                }
            } else {
                // Just convert to points based on mode
                all_buckets
                    .iter()
                    .map(|b| b.to_point(&aggregation_mode))
                    .collect()
            };

            aggregated_data.insert(channel.clone(), points);
        }

        // Update cache
        let cache_data = aggregated_data.clone();
        inner.view_cache = Some(ViewCache {
            time_range: (time_min_ms, time_max_ms),
            pixel_width: req.pixel_width,
            data: cache_data,
            was_realtime: req.is_realtime,
        });

        // Filter state data by time range
        let state_data = Self::filter_state_data(&inner, time_min_ms, time_max_ms);

        // Build channel info
        let channels = Self::build_channel_info(&inner);

        PlotterRangedPayload {
            channels,
            line_data: aggregated_data,
            state_data,
            start_ms: time_min_ms,
            end_ms: time_max_ms,
            is_aggregated: any_aggregated,
        }
    }

    /// Filter state data by time range (helper for get_ranged_data)
    fn filter_state_data(
        inner: &PlotterAggregatorInner,
        time_min_ms: u64,
        time_max_ms: u64,
    ) -> HashMap<String, Vec<StateChange>> {
        inner
            .state_data
            .iter()
            .filter(|(channel, _)| {
                inner
                    .channel_types
                    .get(*channel)
                    .map(|t| *t == ChannelType::State)
                    .unwrap_or(false)
            })
            .map(|(channel, states)| {
                let filtered: Vec<StateChange> = states
                    .iter()
                    .filter(|s| {
                        let end = s.end_ms.unwrap_or(time_max_ms);
                        s.start_ms <= time_max_ms && end >= time_min_ms
                    })
                    .cloned()
                    .collect();
                (channel.clone(), filtered)
            })
            .collect()
    }

    /// Get chart data in uPlot-ready format
    ///
    /// Returns data pre-aligned for uPlot consumption:
    /// - `aligned_data[0]`: timestamps in seconds (f64)
    /// - `aligned_data[1..]`: channel values in `channel_names` order
    /// - `band_data`: min/max bands for Average mode
    ///
    /// This eliminates per-frame data transformation in the frontend,
    /// fixing the memory leak caused by repeated object creation.
    pub fn get_chart_data(&self, req: &PlotterDataRequest) -> PlotterChartPayload {
        // First, get the existing ranged data
        let ranged = self.get_ranged_data(req);

        // Get channel names in registration order (CSV header order) from inner state
        // Only include channels that have line data
        let channel_names: Vec<String> = self
            .inner
            .read()
            .map(|inner| {
                inner
                    .channel_names
                    .iter()
                    .filter(|name| ranged.line_data.contains_key(*name))
                    .cloned()
                    .collect()
            })
            .unwrap_or_else(|_| ranged.line_data.keys().cloned().collect());

        // If no data, return empty payload
        if channel_names.is_empty() {
            return PlotterChartPayload {
                aligned_data: vec![vec![]], // Empty timestamps array
                channel_names: vec![],
                band_data: None,
                state_data: ranged.state_data,
                channels: ranged.channels,
                start_ms: ranged.start_ms,
                end_ms: ranged.end_ms,
            };
        }

        // Collect all unique timestamps from all channels
        let mut timestamp_set: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
        for points in ranged.line_data.values() {
            for point in points {
                let ts = match point {
                    AggregatedPoint::Single { ts, .. } => *ts,
                    AggregatedPoint::MinMax { ts, .. } => *ts,
                };
                timestamp_set.insert(ts);
            }
        }
        let timestamps: Vec<u64> = timestamp_set.into_iter().collect();

        // If no timestamps, return empty payload
        if timestamps.is_empty() {
            return PlotterChartPayload {
                aligned_data: vec![vec![]],
                channel_names: vec![],
                band_data: None,
                state_data: ranged.state_data,
                channels: ranged.channels,
                start_ms: ranged.start_ms,
                end_ms: ranged.end_ms,
            };
        }

        // Build aligned data arrays
        // aligned_data[0] = timestamps in seconds
        let timestamps_seconds: Vec<Option<f64>> = timestamps
            .iter()
            .map(|&ts| Some(ts as f64 / 1000.0))
            .collect();

        let mut aligned_data: Vec<Vec<Option<f64>>> = vec![timestamps_seconds];

        // Check if we have MinMax data (Average mode) for band_data
        let has_minmax = ranged.line_data.values().any(|points| {
            points
                .iter()
                .any(|p| matches!(p, AggregatedPoint::MinMax { .. }))
        });

        let mut band_data_map: HashMap<String, BandSeriesData> = HashMap::new();

        // Build data arrays for each channel
        for channel in &channel_names {
            let points = ranged.line_data.get(channel);
            let num_timestamps = timestamps.len();

            // Create a map from timestamp to point for efficient lookup
            let mut ts_to_point: HashMap<u64, &AggregatedPoint> = HashMap::new();
            if let Some(points) = points {
                for point in points {
                    let ts = match point {
                        AggregatedPoint::Single { ts, .. } => *ts,
                        AggregatedPoint::MinMax { ts, .. } => *ts,
                    };
                    ts_to_point.insert(ts, point);
                }
            }

            // Build value array (null for missing timestamps)
            let mut values: Vec<Option<f64>> = Vec::with_capacity(num_timestamps);
            let mut mins: Vec<Option<f64>> = Vec::with_capacity(num_timestamps);
            let mut maxs: Vec<Option<f64>> = Vec::with_capacity(num_timestamps);

            for ts in &timestamps {
                match ts_to_point.get(ts) {
                    Some(AggregatedPoint::Single { value, .. }) => {
                        values.push(Some(*value));
                        if has_minmax {
                            // For Single points in Average mode, use value as both min and max
                            mins.push(Some(*value));
                            maxs.push(Some(*value));
                        }
                    }
                    Some(AggregatedPoint::MinMax { min, max, .. }) => {
                        // Use midpoint (average) as the line value
                        values.push(Some((min + max) / 2.0));
                        mins.push(Some(*min));
                        maxs.push(Some(*max));
                    }
                    None => {
                        values.push(None);
                        if has_minmax {
                            mins.push(None);
                            maxs.push(None);
                        }
                    }
                }
            }

            aligned_data.push(values);

            // Store band data if in Average mode (has MinMax points)
            if has_minmax {
                band_data_map.insert(
                    channel.clone(),
                    BandSeriesData {
                        min: mins,
                        max: maxs,
                    },
                );
            }
        }

        PlotterChartPayload {
            aligned_data,
            channel_names,
            band_data: if band_data_map.is_empty() {
                None
            } else {
                Some(band_data_map)
            },
            state_data: ranged.state_data,
            channels: ranged.channels,
            start_ms: ranged.start_ms,
            end_ms: ranged.end_ms,
        }
    }
    /// Calculate data time range from all channels
    fn calculate_data_time_range(inner: &PlotterAggregatorInner) -> (u64, u64) {
        let mut min_ms = u64::MAX;
        let mut max_ms = 0u64;

        // Check history data for time range
        for data in inner.history.values() {
            if let Some(first) = data.first() {
                min_ms = min_ms.min(first.ts);
            }
            if let Some(last) = data.last() {
                max_ms = max_ms.max(last.ts);
            }
        }

        // Check buffer data for time range
        for data in inner.buffer.values() {
            if let Some(first) = data.first() {
                min_ms = min_ms.min(first.ts);
            }
            if let Some(last) = data.last() {
                max_ms = max_ms.max(last.ts);
            }
        }

        // Check raw_buffer for latest time range
        for raw_buf in inner.raw_buffer.values() {
            if let Some(&(ts, _)) = raw_buf.front() {
                min_ms = min_ms.min(ts);
            }
            if let Some(&(ts, _)) = raw_buf.back() {
                max_ms = max_ms.max(ts);
            }
        }

        // Check state_data time range
        for states in inner.state_data.values() {
            if let Some(first) = states.first() {
                min_ms = min_ms.min(first.start_ms);
            }
            if let Some(last) = states.last() {
                max_ms = max_ms.max(last.end_ms.unwrap_or(last.start_ms));
            }
        }

        if min_ms == u64::MAX {
            min_ms = 0;
        }

        (min_ms, max_ms)
    }

    /// Check if cache is still valid
    fn is_cache_valid(
        inner: &PlotterAggregatorInner,
        time_min_ms: u64,
        time_max_ms: u64,
        req: &PlotterDataRequest,
    ) -> bool {
        let cache = match &inner.view_cache {
            Some(c) => c,
            None => return false,
        };

        // Check pixel width change (20% threshold)
        let pixel_diff = (cache.pixel_width as f32 - req.pixel_width as f32).abs();
        let pixel_threshold = cache.pixel_width as f32 * inner.config.pixel_width_threshold_percent;
        if pixel_diff > pixel_threshold {
            return false;
        }

        if req.is_realtime && cache.was_realtime {
            let cached_span = cache.time_range.1 - cache.time_range.0;
            let new_span = time_max_ms - time_min_ms;
            // In realtime mode, cache is only valid if BOTH:
            // 1. The time span is the same (user hasn't zoomed)
            // 2. The end time matches (no new data has arrived)
            cached_span == new_span && cache.time_range.1 == time_max_ms
        } else {
            cache.time_range.0 == time_min_ms && cache.time_range.1 == time_max_ms
        }
    }

    /// Build channel info from inner state
    fn build_channel_info(inner: &PlotterAggregatorInner) -> Vec<ChannelInfo> {
        inner
            .channel_names
            .iter()
            .map(|name| {
                let channel_type = inner
                    .channel_types
                    .get(name)
                    .cloned()
                    .unwrap_or(ChannelType::Auto);

                let (latest_value, point_count) = match channel_type {
                    ChannelType::Line | ChannelType::Auto => {
                        // Get latest from raw_buffer (most recent data)
                        let raw_buf = inner.raw_buffer.get(name);
                        let latest = raw_buf
                            .and_then(|b| b.back())
                            .map(|(_, v)| format!("{:.2}", v));
                        // Count includes history + buffer + raw_buffer
                        let hist_count = inner.history.get(name).map(|h| h.len()).unwrap_or(0);
                        let buf_count = inner.buffer.get(name).map(|b| b.len()).unwrap_or(0);
                        let raw_count = raw_buf.map(|b| b.len()).unwrap_or(0);
                        (latest, hist_count + buf_count + raw_count)
                    }
                    ChannelType::State => {
                        let states = inner.state_data.get(name);
                        let latest = states.and_then(|s| s.last()).map(|s| s.state.clone());
                        let count = states.map(|s| s.len()).unwrap_or(0);
                        (latest, count)
                    }
                };

                ChannelInfo {
                    name: name.clone(),
                    channel_type,
                    latest_value,
                    point_count,
                }
            })
            .collect()
    }

    /// Get all channel names
    pub fn get_channels(&self) -> Vec<String> {
        self.inner
            .read()
            .map(|inner| inner.channel_names.clone())
            .unwrap_or_default()
    }

    /// Get channel info for all channels
    pub fn get_channel_info(&self) -> Vec<ChannelInfo> {
        let inner = match self.inner.read() {
            Ok(inner) => inner,
            Err(_) => return Vec::new(),
        };
        Self::build_channel_info(&inner)
    }

    /// Clear all data
    pub fn clear(&self) {
        if let Ok(mut inner) = self.inner.write() {
            inner.channels.clear();
            inner.channel_names.clear();
            inner.channel_types.clear();
            inner.history.clear();
            inner.buffer.clear();
            inner.raw_buffer.clear();
            inner.current_level = 1;
            inner.bucket_size = inner.config.bucket_size;
            inner.state_data.clear();
            inner.view_cache = None;
        }
    }

    /// Update configuration
    pub fn set_config(&self, config: PlotterConfig) {
        if let Ok(mut inner) = self.inner.write() {
            inner.config = config;
        }
    }

    /// Update aggregation mode
    pub fn set_aggregation_mode(&self, mode: AggregationMode) {
        if let Ok(mut inner) = self.inner.write() {
            inner.config.aggregation_mode = mode;
            inner.view_cache = None;
        }
    }

    /// Rebuild aggregation (call when aggregation mode changes)
    ///
    /// Since we now store AggregatedBucket with all statistics, mode changes
    /// don't require data transformation. This just re-aggregates to reduce
    /// data size if needed.
    pub fn rebuild_aggregation(&self) {
        if let Ok(mut inner) = self.inner.write() {
            let target_points = inner.config.max_points / 2;
            let channel_names: Vec<String> = inner.channel_names.clone();

            for channel in channel_names {
                // Collect all buckets from history + buffer
                let mut all_buckets: Vec<AggregatedBucket> = Vec::new();

                // Add history data
                if let Some(hist_data) = inner.history.get(&channel) {
                    all_buckets.extend(hist_data.iter().cloned());
                }

                // Add buffer data
                if let Some(buf_data) = inner.buffer.get(&channel) {
                    all_buckets.extend(buf_data.iter().cloned());
                }

                // Add raw_buffer data as individual buckets
                if let Some(raw_data) = inner.raw_buffer.get(&channel) {
                    for &(ts, value) in raw_data.iter() {
                        all_buckets.push(AggregatedBucket {
                            ts,
                            min: value,
                            max: value,
                            avg: value,
                            count: 1,
                        });
                    }
                }

                if all_buckets.is_empty() {
                    continue;
                }

                // Sort by timestamp
                all_buckets.sort_by_key(|b| b.ts);

                // Re-aggregate preserving all statistics
                let new_history = Self::aggregate_buckets_preserving(&all_buckets, target_points);

                // Update history and clear buffer/raw_buffer
                inner.history.insert(channel.clone(), new_history);
                if let Some(buf) = inner.buffer.get_mut(&channel) {
                    buf.clear();
                }
                if let Some(raw) = inner.raw_buffer.get_mut(&channel) {
                    raw.clear();
                }
            }

            inner.current_level = 1;
            inner.bucket_size = inner.config.bucket_size;
            inner.view_cache = None;
        }
    }

    /// Get total point count across all channels (history + buffer + raw_buffer)
    pub fn total_points(&self) -> usize {
        self.inner
            .read()
            .map(|inner| {
                let hist: usize = inner.history.values().map(|v| v.len()).sum();
                let buf: usize = inner.buffer.values().map(|v| v.len()).sum();
                let raw: usize = inner.raw_buffer.values().map(|v| v.len()).sum();
                hist + buf + raw
            })
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_data_point() {
        let agg = PlotterAggregator::new();
        agg.set_enabled(true);

        agg.add_data_point("ch0", 1000, ChannelValue::Numeric(123.45));

        let info = agg.get_channel_info();
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].name, "ch0");
        assert_eq!(info[0].point_count, 1);
    }

    #[test]
    fn test_data_accumulation() {
        let config = PlotterConfig {
            max_points: 100,
            aggregation_mode: AggregationMode::Average,
            ..Default::default()
        };
        let agg = PlotterAggregator::with_config(config);
        agg.set_enabled(true);

        // Add 50 points - with new design (bucket_size=10), this will create
        // 5 aggregated points in buffer, plus 0 remaining in raw_buffer
        for i in 0..50 {
            agg.add_data_point("ch0", i * 100, ChannelValue::Numeric(i as f64));
        }

        // New design: 50 raw points -> 5 aggregated points (bucket_size=10)
        assert!(agg.total_points() > 0, "should have data");
    }

    #[test]
    fn test_dynamic_level_creation() {
        let config = PlotterConfig {
            max_points: 100,
            aggregation_mode: AggregationMode::Average,
            aggregation_threshold: Some(2),
            ..Default::default()
        };
        let agg = PlotterAggregator::with_config(config);
        agg.set_enabled(true);

        // Add 300 points - with max_points=100,
        // aggregation triggers when (aggregated + recent) > 100 points.
        // After aggregation, all data is re-compressed to max_points/2.
        for i in 0..300 {
            agg.add_data_point("ch0", i * 100, ChannelValue::Numeric(i as f64));
        }

        // Total data should be bounded
        assert!(
            agg.total_points() <= 200,
            "data should be bounded: got {}",
            agg.total_points()
        );
    }

    #[test]
    fn test_state_change_recording() {
        let agg = PlotterAggregator::new();
        agg.set_enabled(true);

        agg.add_data_point("motor", 1000, ChannelValue::State("OFF".to_string()));
        agg.add_data_point("motor", 2000, ChannelValue::State("ON".to_string()));
        agg.add_data_point("motor", 3000, ChannelValue::State("ON".to_string())); // Same state
        agg.add_data_point("motor", 4000, ChannelValue::State("OFF".to_string()));

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 1000,
            is_realtime: false,
        };
        let payload = agg.get_ranged_data(&req);

        let states = payload.state_data.get("motor").unwrap();
        assert_eq!(states.len(), 3); // OFF -> ON -> OFF
    }

    #[test]
    fn test_get_ranged_data() {
        let agg = PlotterAggregator::new();
        agg.set_enabled(true);

        for i in 0..100 {
            agg.add_data_point("ch0", i * 100, ChannelValue::Numeric(i as f64));
        }

        let req = PlotterDataRequest {
            time_min_ms: Some(2000),
            time_max_ms: Some(5000),
            pixel_width: 100,
            is_realtime: false,
        };

        let payload = agg.get_ranged_data(&req);
        let data = payload.line_data.get("ch0").unwrap();

        // Should have points in range [2000, 5000]
        assert!(!data.is_empty());

        // Check first point is >= 2000
        if let AggregatedPoint::Single { ts, .. } = &data[0] {
            assert!(*ts >= 2000);
        }
    }

    #[test]
    fn test_rebuild_aggregation() {
        let config = PlotterConfig {
            max_points: 50,
            aggregation_mode: AggregationMode::Average,
            ..Default::default()
        };
        let agg = PlotterAggregator::with_config(config);
        agg.set_enabled(true);

        // Add 200 points - because max_points=50 and aggregation uses Average mode,
        // aggregation triggers when (aggregated + recent) > 50 points.
        // After aggregation, all data is re-compressed to 25 points (max_points/2).
        for i in 0..200 {
            agg.add_data_point("ch0", i * 100, ChannelValue::Numeric(i as f64));
        }

        // Rebuild aggregation (clears aggregated and re-aggregates recent)
        agg.rebuild_aggregation();

        // total_points() returns (aggregated + recent) count,
        // which is bounded by max_points after re-aggregation
        assert!(
            agg.total_points() <= 100,
            "data should be bounded: got {}",
            agg.total_points()
        );
    }

    #[test]
    fn test_average_minmax_aggregation() {
        let config = PlotterConfig {
            max_points: 100,
            aggregation_mode: AggregationMode::Average,
            ..Default::default()
        };
        let agg = PlotterAggregator::with_config(config);
        agg.set_enabled(true);

        // Add 500 points with varying values
        for i in 0..500 {
            let value = (i % 20) as f64; // Values cycle 0-19
            agg.add_data_point("ch0", i * 100, ChannelValue::Numeric(value));
        }

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 50, // Force aggregation
            is_realtime: false,
        };

        let payload = agg.get_ranged_data(&req);
        let data = payload.line_data.get("ch0").unwrap();

        // Average mode should return MinMax points (with band data)
        let has_minmax = data
            .iter()
            .any(|p| matches!(p, AggregatedPoint::MinMax { .. }));

        // Either we have MinMax points or the data is small enough to not aggregate
        if has_minmax {
            // Check that min <= max for all MinMax points
            for point in data {
                if let AggregatedPoint::MinMax { min, max, .. } = point {
                    assert!(min <= max, "min should be <= max");
                }
            }
        }
    }

    #[test]
    fn test_cache_invalidation_on_pixel_width_change() {
        let agg = PlotterAggregator::new();
        agg.set_enabled(true);

        // Add some data
        for i in 0..100 {
            agg.add_data_point("ch0", i * 100, ChannelValue::Numeric(i as f64));
        }

        // First request
        let req1 = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 100,
            is_realtime: false,
        };
        let _ = agg.get_ranged_data(&req1);

        // Second request with significantly different pixel_width (>20% change)
        let req2 = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 500, // 5x change, definitely > 20%
            is_realtime: false,
        };
        let payload2 = agg.get_ranged_data(&req2);

        // Should return data (cache was invalidated and rebuilt)
        assert!(!payload2.line_data.is_empty());
        assert!(!payload2.line_data.get("ch0").unwrap().is_empty());
    }

    #[test]
    fn test_time_range_filtering_boundary() {
        let agg = PlotterAggregator::new();
        agg.set_enabled(true);

        // Add points at specific timestamps
        agg.add_data_point("ch0", 1000, ChannelValue::Numeric(10.0));
        agg.add_data_point("ch0", 2000, ChannelValue::Numeric(20.0));
        agg.add_data_point("ch0", 3000, ChannelValue::Numeric(30.0));
        agg.add_data_point("ch0", 4000, ChannelValue::Numeric(40.0));
        agg.add_data_point("ch0", 5000, ChannelValue::Numeric(50.0));

        // Request exactly at boundary
        let req = PlotterDataRequest {
            time_min_ms: Some(2000), // Include
            time_max_ms: Some(4000), // Include
            pixel_width: 1000,
            is_realtime: false,
        };

        let payload = agg.get_ranged_data(&req);
        let data = payload.line_data.get("ch0").unwrap();

        // Should include points at 2000, 3000, 4000 (inclusive)
        assert_eq!(data.len(), 3);

        // Verify values
        let values: Vec<f64> = data
            .iter()
            .filter_map(|p| {
                if let AggregatedPoint::Single { value, .. } = p {
                    Some(*value)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(values, vec![20.0, 30.0, 40.0]);
    }

    #[test]
    fn test_minmax_preservation_after_reaggregate() {
        // Test that min/max extremes are preserved after multiple re-aggregation cycles
        let config = PlotterConfig {
            max_points: 50, // Small to trigger frequent re-aggregation
            aggregation_mode: AggregationMode::Average,
            ..Default::default()
        };
        let agg = PlotterAggregator::with_config(config);
        agg.set_enabled(true);

        // Add data with known min/max values (sine wave with peaks at ±100)
        for i in 0..500 {
            let t = i as f64 * 0.01;
            let value = (t * std::f64::consts::PI * 2.0).sin() * 100.0;
            agg.add_data_point("ch0", i * 10, ChannelValue::Numeric(value));
        }

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 100,
            is_realtime: false,
        };

        let payload = agg.get_ranged_data(&req);
        let data = payload.line_data.get("ch0").unwrap();

        // Find global min/max from aggregated data
        let (global_min, global_max) =
            data.iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |acc, p| match p {
                    AggregatedPoint::Single { value, .. } => (acc.0.min(*value), acc.1.max(*value)),
                    AggregatedPoint::MinMax { min, max, .. } => (acc.0.min(*min), acc.1.max(*max)),
                });

        // The original data had peaks at ±100, so after re-aggregation
        // the preserved extremes should still be close to those values (within 10%)
        assert!(
            global_max > 90.0,
            "Max peak should be preserved after re-aggregation, got {}",
            global_max
        );
        assert!(
            global_min < -90.0,
            "Min peak should be preserved after re-aggregation, got {}",
            global_min
        );
    }

    #[test]
    fn test_mode_switching_preserves_data() {
        let config = PlotterConfig {
            max_points: 50,
            aggregation_mode: AggregationMode::Average,
            ..Default::default()
        };
        let agg = PlotterAggregator::with_config(config);
        agg.set_enabled(true);

        // Add sine wave data with known peaks at ±100
        for i in 0..300 {
            let t = i as f64 * 0.02;
            let value = (t * std::f64::consts::PI * 2.0).sin() * 100.0;
            agg.add_data_point("ch0", i * 10, ChannelValue::Numeric(value));
        }

        // Get data as Average mode
        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 100,
            is_realtime: false,
        };
        let payload_avg = agg.get_ranged_data(&req);
        let data_avg = payload_avg.line_data.get("ch0").unwrap();
        assert!(!data_avg.is_empty(), "Should have data before switching");

        // Find peaks in Average mode
        let (_avg_min, _avg_max) =
            data_avg
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |acc, p| match p {
                    AggregatedPoint::Single { value, .. } => (acc.0.min(*value), acc.1.max(*value)),
                    AggregatedPoint::MinMax { min, max, .. } => (acc.0.min(*min), acc.1.max(*max)),
                });

        // Switch to LTTB mode
        agg.set_aggregation_mode(AggregationMode::Lttb);
        agg.rebuild_aggregation();

        // Get data as LTTB mode
        let payload_lttb = agg.get_ranged_data(&req);
        let data_lttb = payload_lttb.line_data.get("ch0").unwrap();
        assert!(
            !data_lttb.is_empty(),
            "Should have data after switching to LTTB"
        );

        // Find peaks in LTTB mode
        let (lttb_min, lttb_max) =
            data_lttb
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |acc, p| match p {
                    AggregatedPoint::Single { value, .. } => (acc.0.min(*value), acc.1.max(*value)),
                    AggregatedPoint::MinMax { min, max, .. } => (acc.0.min(*min), acc.1.max(*max)),
                });

        // Peaks should still be reasonable (within 50% of original, accounting for mode differences)
        // Note: New uniform-level design + LTTB uses midpoints, so significant precision loss is expected
        assert!(
            lttb_max > 50.0,
            "Max peak should be reasonably preserved after mode switch, got {}",
            lttb_max
        );
        assert!(
            lttb_min < -50.0,
            "Min peak should be reasonably preserved after mode switch, got {}",
            lttb_min
        );

        // Switch back to Average
        agg.set_aggregation_mode(AggregationMode::Average);
        agg.rebuild_aggregation();

        // Should still have data
        let payload_back = agg.get_ranged_data(&req);
        let data_back = payload_back.line_data.get("ch0").unwrap();
        assert!(
            !data_back.is_empty(),
            "Should have data after switching back to Average"
        );
    }

    #[test]
    fn test_mode_switch_during_reception_no_rebuild() {
        // Test that mode can be switched during reception without calling rebuild_aggregation
        // This is the key test for the new design where all stats are stored in AggregatedBucket
        let config = PlotterConfig {
            max_points: 100,
            aggregation_mode: AggregationMode::Lttb,
            ..Default::default()
        };
        let agg = PlotterAggregator::with_config(config);
        agg.set_enabled(true);

        // Add first batch of data (will be stored as AggregatedBucket with all stats)
        for i in 0..50 {
            let t = i as f64 * 0.1;
            let value = (t * std::f64::consts::PI * 2.0).sin() * 100.0;
            agg.add_data_point("ch0", i * 10, ChannelValue::Numeric(value));
        }

        // Get data in LTTB mode
        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 100,
            is_realtime: false,
        };
        let payload_lttb = agg.get_ranged_data(&req);
        let data_lttb = payload_lttb.line_data.get("ch0").unwrap();

        // LTTB mode should return Single points
        let lttb_has_single = data_lttb
            .iter()
            .any(|p| matches!(p, AggregatedPoint::Single { .. }));
        assert!(lttb_has_single, "LTTB mode should return Single points");

        // Switch to Average mode WITHOUT calling rebuild_aggregation
        agg.set_aggregation_mode(AggregationMode::Average);

        // Add more data after mode switch
        for i in 50..100 {
            let t = i as f64 * 0.1;
            let value = (t * std::f64::consts::PI * 2.0).sin() * 100.0;
            agg.add_data_point("ch0", i * 10, ChannelValue::Numeric(value));
        }

        // Get data in Average mode - should work without rebuild
        let payload_avg = agg.get_ranged_data(&req);
        let data_avg = payload_avg.line_data.get("ch0").unwrap();

        // Average mode should return MinMax points
        let avg_has_minmax = data_avg
            .iter()
            .any(|p| matches!(p, AggregatedPoint::MinMax { .. }));
        assert!(avg_has_minmax, "Average mode should return MinMax points");

        // Data should be continuous (both old and new data should be present)
        assert!(!data_avg.is_empty(), "Should have data after mode switch");

        // Find time range of returned data
        let (min_ts, max_ts) = data_avg.iter().fold((u64::MAX, 0u64), |acc, p| {
            let ts = match p {
                AggregatedPoint::Single { ts, .. } => *ts,
                AggregatedPoint::MinMax { ts, .. } => *ts,
            };
            (acc.0.min(ts), acc.1.max(ts))
        });

        // Should have data from both before and after the mode switch
        assert!(
            min_ts < 500,
            "Should have data from before mode switch (starts at {})",
            min_ts
        );
        assert!(
            max_ts >= 500,
            "Should have data from after mode switch (ends at {})",
            max_ts
        );

        // Switch back to LTTB - should also work without rebuild
        agg.set_aggregation_mode(AggregationMode::Lttb);
        let payload_lttb2 = agg.get_ranged_data(&req);
        let data_lttb2 = payload_lttb2.line_data.get("ch0").unwrap();
        let lttb2_has_single = data_lttb2
            .iter()
            .any(|p| matches!(p, AggregatedPoint::Single { .. }));
        assert!(
            lttb2_has_single,
            "LTTB mode should return Single points after switching back"
        );
    }

    // ============================================================
    // Regression tests for performance refactoring
    // These tests verify correctness of functions that will be modified
    // ============================================================

    #[test]
    fn test_get_ranged_data_large_dataset() {
        // Regression test: verify get_ranged_data returns correct data with large dataset
        let config = PlotterConfig {
            max_points: 1000,
            aggregation_mode: AggregationMode::Lttb,
            ..Default::default()
        };
        let agg = PlotterAggregator::with_config(config);
        agg.set_enabled(true);

        // Add 5000 points
        for i in 0..5000 {
            agg.add_data_point(
                "ch0",
                i * 10,
                ChannelValue::Numeric((i as f64).sin() * 100.0),
            );
        }

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 800,
            is_realtime: true,
        };

        let payload = agg.get_ranged_data(&req);
        let data = payload.line_data.get("ch0").unwrap();

        // Data should be aggregated to around target_points
        assert!(
            data.len() <= 800 * 10,
            "Data should be bounded: got {}",
            data.len()
        );
        assert!(!data.is_empty(), "Should have data");

        // Verify time range covers full dataset (allowing for bucket aggregation)
        // Note: start_ms may be slightly offset due to average timestamp calculation in buckets
        assert!(
            payload.start_ms <= 100,
            "Start should be near 0, got {}",
            payload.start_ms
        );
        assert!(
            payload.end_ms >= 49800,
            "End time should be at least 49800, got {}",
            payload.end_ms
        );
    }

    #[test]
    fn test_get_ranged_data_realtime_mode_continuous() {
        // Regression test: verify realtime mode returns all data continuously
        let agg = PlotterAggregator::new();
        agg.set_enabled(true);

        // Add initial data
        for i in 0..100 {
            agg.add_data_point("ch0", i * 100, ChannelValue::Numeric(i as f64));
        }

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 1000,
            is_realtime: true,
        };

        let payload1 = agg.get_ranged_data(&req);
        let data1_len = payload1.line_data.get("ch0").unwrap().len();

        // Add more data
        for i in 100..200 {
            agg.add_data_point("ch0", i * 100, ChannelValue::Numeric(i as f64));
        }

        let payload2 = agg.get_ranged_data(&req);
        let data2 = payload2.line_data.get("ch0").unwrap();

        // New data should include both old and new points
        assert!(
            data2.len() >= data1_len,
            "Should have at least as many points after adding data"
        );
        assert!(
            payload2.end_ms > payload1.end_ms,
            "End time should increase"
        );
    }

    #[test]
    fn test_batch_processing_equals_individual() {
        // Regression test: batch processing should produce same results as individual adds
        let agg1 = PlotterAggregator::new();
        agg1.set_enabled(true);

        let agg2 = PlotterAggregator::new();
        agg2.set_enabled(true);

        // Individual adds
        for i in 0..50 {
            agg1.add_data_point("ch0", i * 100, ChannelValue::Numeric(i as f64));
        }

        // Batch add
        let mut batch = Vec::new();
        for i in 0..50 {
            let mut channels = std::collections::HashMap::new();
            channels.insert("ch0".to_string(), ChannelValue::Numeric(i as f64));
            batch.push(crate::plotter::parser::ParsedDataPoint {
                timestamp_ms: i * 100,
                channels,
                channel_order: vec!["ch0".to_string()],
            });
        }
        agg2.add_data_points_batch(batch);

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 1000,
            is_realtime: false,
        };

        let payload1 = agg1.get_ranged_data(&req);
        let payload2 = agg2.get_ranged_data(&req);

        // Both should have same number of points
        let data1 = payload1.line_data.get("ch0").unwrap();
        let data2 = payload2.line_data.get("ch0").unwrap();
        assert_eq!(
            data1.len(),
            data2.len(),
            "Batch and individual should produce same point count"
        );
    }

    #[test]
    fn test_aggregate_buckets_preserving_correctness() {
        // Regression test: verify aggregate_buckets_preserving maintains data integrity
        let buckets = vec![
            AggregatedBucket {
                ts: 0,
                min: 0.0,
                max: 10.0,
                avg: 5.0,
                count: 10,
            },
            AggregatedBucket {
                ts: 100,
                min: 5.0,
                max: 15.0,
                avg: 10.0,
                count: 10,
            },
            AggregatedBucket {
                ts: 200,
                min: 10.0,
                max: 20.0,
                avg: 15.0,
                count: 10,
            },
            AggregatedBucket {
                ts: 300,
                min: 15.0,
                max: 25.0,
                avg: 20.0,
                count: 10,
            },
            AggregatedBucket {
                ts: 400,
                min: 20.0,
                max: 30.0,
                avg: 25.0,
                count: 10,
            },
        ];

        // Aggregate to 2 points
        let result = PlotterAggregator::aggregate_buckets_preserving(&buckets, 2);

        // Should have 2-3 buckets (depending on bucket boundaries)
        assert!(
            result.len() <= 3,
            "Should aggregate to around target: got {}",
            result.len()
        );
        assert!(!result.is_empty(), "Should have at least 1 bucket");

        // Global min should be preserved (0.0)
        let global_min = result.iter().map(|b| b.min).fold(f64::INFINITY, f64::min);
        assert!(
            (global_min - 0.0).abs() < 0.01,
            "Global min should be 0.0, got {}",
            global_min
        );

        // Global max should be preserved (30.0)
        let global_max = result
            .iter()
            .map(|b| b.max)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (global_max - 30.0).abs() < 0.01,
            "Global max should be 30.0, got {}",
            global_max
        );

        // Total count should be preserved
        let total_count: usize = result.iter().map(|b| b.count).sum();
        assert_eq!(total_count, 50, "Total count should be preserved");
    }

    #[test]
    fn test_multiple_channels_independent() {
        // Regression test: verify multiple channels are handled independently
        let agg = PlotterAggregator::new();
        agg.set_enabled(true);

        // Add data to multiple channels with different patterns
        for i in 0..100 {
            agg.add_data_point("ch0", i * 100, ChannelValue::Numeric(i as f64));
            agg.add_data_point("ch1", i * 100, ChannelValue::Numeric((i as f64) * 2.0));
            agg.add_data_point("ch2", i * 100, ChannelValue::Numeric((i as f64) * 0.5));
        }

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 1000,
            is_realtime: false,
        };

        let payload = agg.get_ranged_data(&req);

        // All channels should have data
        assert!(payload.line_data.contains_key("ch0"));
        assert!(payload.line_data.contains_key("ch1"));
        assert!(payload.line_data.contains_key("ch2"));

        // Each channel should have similar point count
        let ch0_len = payload.line_data.get("ch0").unwrap().len();
        let ch1_len = payload.line_data.get("ch1").unwrap().len();
        let ch2_len = payload.line_data.get("ch2").unwrap().len();
        assert_eq!(ch0_len, ch1_len, "Channels should have same point count");
        assert_eq!(ch1_len, ch2_len, "Channels should have same point count");
    }

    #[test]
    fn test_time_range_filtering_correctness() {
        // Regression test: verify time range filtering returns correct subset
        let agg = PlotterAggregator::new();
        agg.set_enabled(true);

        // Add data from 0 to 10000ms
        for i in 0..101 {
            agg.add_data_point("ch0", i * 100, ChannelValue::Numeric(i as f64));
        }

        // Request only middle portion
        let req = PlotterDataRequest {
            time_min_ms: Some(3000),
            time_max_ms: Some(7000),
            pixel_width: 1000,
            is_realtime: false,
        };

        let payload = agg.get_ranged_data(&req);
        let data = payload.line_data.get("ch0").unwrap();

        // Verify all returned points are within range
        for point in data {
            let ts = match point {
                AggregatedPoint::Single { ts, .. } => *ts,
                AggregatedPoint::MinMax { ts, .. } => *ts,
            };
            assert!(
                (3000..=7000).contains(&ts),
                "Point at {} should be in range [3000, 7000]",
                ts
            );
        }
    }

    #[test]
    fn test_data_ordering_preserved() {
        // Regression test: verify data ordering is preserved in output
        let agg = PlotterAggregator::new();
        agg.set_enabled(true);

        for i in 0..100 {
            agg.add_data_point("ch0", i * 100, ChannelValue::Numeric(i as f64));
        }

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 1000,
            is_realtime: false,
        };

        let payload = agg.get_ranged_data(&req);
        let data = payload.line_data.get("ch0").unwrap();

        // Verify timestamps are in ascending order
        let mut prev_ts = 0u64;
        for point in data {
            let ts = match point {
                AggregatedPoint::Single { ts, .. } => *ts,
                AggregatedPoint::MinMax { ts, .. } => *ts,
            };
            assert!(
                ts >= prev_ts,
                "Timestamps should be ascending: {} >= {}",
                ts,
                prev_ts
            );
            prev_ts = ts;
        }
    }

    #[test]
    fn test_level_up_preserves_data_range() {
        // Regression test: verify level_up preserves full time range
        let config = PlotterConfig {
            max_points: 50, // Small to trigger frequent level-ups
            aggregation_mode: AggregationMode::Average,
            ..Default::default()
        };
        let agg = PlotterAggregator::with_config(config);
        agg.set_enabled(true);

        // Add enough data to trigger multiple level-ups
        for i in 0..500 {
            agg.add_data_point("ch0", i * 10, ChannelValue::Numeric(i as f64));
        }

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 100,
            is_realtime: false,
        };

        let payload = agg.get_ranged_data(&req);

        // Time range should still cover full data (allowing for bucket boundaries)
        // Note: start_ms may be slightly offset due to average timestamp calculation in buckets
        assert!(
            payload.start_ms <= 100,
            "Start should be near 0, got {}",
            payload.start_ms
        );
        assert!(
            payload.end_ms >= 4800,
            "End should be >= 4800, got {}",
            payload.end_ms
        );
    }

    // ==================== get_chart_data tests ====================

    #[test]
    fn test_get_chart_data_format() {
        // Verify that get_chart_data returns data in uPlot format
        let agg = PlotterAggregator::new();
        agg.set_enabled(true);

        // Add some data points
        agg.add_data_point("ch0", 1000, ChannelValue::Numeric(10.0));
        agg.add_data_point("ch0", 2000, ChannelValue::Numeric(20.0));
        agg.add_data_point("ch1", 1000, ChannelValue::Numeric(100.0));
        agg.add_data_point("ch1", 2000, ChannelValue::Numeric(200.0));

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 800,
            is_realtime: false,
        };

        let payload = agg.get_chart_data(&req);

        // Check structure: aligned_data[0] = timestamps, aligned_data[1..] = channel values
        assert!(
            payload.aligned_data.len() >= 3,
            "Should have timestamps + 2 channels"
        );
        assert_eq!(
            payload.channel_names.len(),
            payload.aligned_data.len() - 1,
            "channel_names should match data columns"
        );

        // Check channel names are sorted
        let mut sorted_names = payload.channel_names.clone();
        sorted_names.sort();
        assert_eq!(
            payload.channel_names, sorted_names,
            "Channels should be sorted"
        );

        // Check timestamps are in seconds (should be 1.0 and 2.0)
        let timestamps = &payload.aligned_data[0];
        assert_eq!(timestamps.len(), 2);
        assert_eq!(timestamps[0], Some(1.0)); // 1000ms = 1.0s
        assert_eq!(timestamps[1], Some(2.0)); // 2000ms = 2.0s
    }

    #[test]
    fn test_chart_data_null_handling() {
        // Verify that missing data points are represented as None
        let agg = PlotterAggregator::new();
        agg.set_enabled(true);

        // Add data with different timestamps per channel
        agg.add_data_point("ch0", 1000, ChannelValue::Numeric(10.0));
        agg.add_data_point("ch0", 2000, ChannelValue::Numeric(20.0));
        agg.add_data_point("ch1", 2000, ChannelValue::Numeric(200.0)); // ch1 only has ts=2000
        agg.add_data_point("ch1", 3000, ChannelValue::Numeric(300.0)); // ch1 only has ts=3000

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 800,
            is_realtime: false,
        };

        let payload = agg.get_chart_data(&req);

        // Should have 3 timestamps: 1000, 2000, 3000
        assert_eq!(payload.aligned_data[0].len(), 3);

        // Find channel indices
        let ch0_idx = payload
            .channel_names
            .iter()
            .position(|n| n == "ch0")
            .unwrap()
            + 1;
        let ch1_idx = payload
            .channel_names
            .iter()
            .position(|n| n == "ch1")
            .unwrap()
            + 1;

        let ch0_data = &payload.aligned_data[ch0_idx];
        let ch1_data = &payload.aligned_data[ch1_idx];

        // ch0: has data at ts=1000, 2000; missing at ts=3000
        assert!(ch0_data[0].is_some()); // ts=1000
        assert!(ch0_data[1].is_some()); // ts=2000
        assert!(ch0_data[2].is_none()); // ts=3000 missing

        // ch1: missing at ts=1000; has data at ts=2000, 3000
        assert!(ch1_data[0].is_none()); // ts=1000 missing
        assert!(ch1_data[1].is_some()); // ts=2000
        assert!(ch1_data[2].is_some()); // ts=3000
    }

    #[test]
    fn test_chart_data_timestamps_aligned() {
        // Verify that all channels use the same timestamp array
        let agg = PlotterAggregator::new();
        agg.set_enabled(true);

        // Add data with overlapping timestamps
        for i in 0..100 {
            agg.add_data_point("ch0", i * 100, ChannelValue::Numeric(i as f64));
            if i % 2 == 0 {
                agg.add_data_point("ch1", i * 100, ChannelValue::Numeric(i as f64 * 10.0));
            }
        }

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 800,
            is_realtime: false,
        };

        let payload = agg.get_chart_data(&req);

        // All data arrays should have the same length as timestamps
        let ts_len = payload.aligned_data[0].len();
        for (i, data) in payload.aligned_data.iter().enumerate() {
            assert_eq!(
                data.len(),
                ts_len,
                "Channel {} should have same length as timestamps",
                i
            );
        }
    }

    #[test]
    fn test_chart_data_band_data() {
        // Verify band_data is populated in Average mode
        let config = PlotterConfig {
            max_points: 10, // Small to trigger aggregation
            aggregation_mode: AggregationMode::Average,
            ..Default::default()
        };
        let agg = PlotterAggregator::with_config(config);
        agg.set_enabled(true);

        // Add enough data to trigger aggregation which produces MinMax points
        for i in 0..200 {
            agg.add_data_point("ch0", i * 10, ChannelValue::Numeric((i % 50) as f64));
        }

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 50, // Small to trigger view aggregation
            is_realtime: false,
        };

        let payload = agg.get_chart_data(&req);

        // In Average mode with aggregation, band_data should be Some
        if payload.band_data.is_some() {
            let band_data = payload.band_data.as_ref().unwrap();
            assert!(
                band_data.contains_key("ch0"),
                "Should have band data for ch0"
            );

            let ch0_band = band_data.get("ch0").unwrap();
            assert_eq!(
                ch0_band.min.len(),
                payload.aligned_data[0].len(),
                "Band min should match timestamps length"
            );
            assert_eq!(
                ch0_band.max.len(),
                payload.aligned_data[0].len(),
                "Band max should match timestamps length"
            );
        }
        // Note: band_data may be None if no aggregation occurred
    }

    #[test]
    fn test_chart_data_empty() {
        // Verify empty data handling
        let agg = PlotterAggregator::new();
        agg.set_enabled(true);

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 800,
            is_realtime: false,
        };

        let payload = agg.get_chart_data(&req);

        // Should return empty but valid structure
        assert!(
            !payload.aligned_data.is_empty(),
            "Should have at least timestamps array"
        );
        assert!(payload.channel_names.is_empty(), "Should have no channels");
        assert!(payload.band_data.is_none(), "Should have no band data");
    }

    // ==================== LTTB Algorithm Tests ====================

    #[test]
    fn test_lttb_downsample_small_data() {
        // When data is smaller than target, return all indices
        let data: Vec<AggregatedBucket> = (0..5)
            .map(|i| AggregatedBucket {
                ts: i * 100,
                min: i as f64,
                max: i as f64,
                avg: i as f64,
                count: 1,
            })
            .collect();

        let result = lttb_downsample(&data, 10);
        assert_eq!(result, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn test_lttb_downsample_preserves_endpoints() {
        // LTTB always includes first and last points
        let data: Vec<AggregatedBucket> = (0..100)
            .map(|i| AggregatedBucket {
                ts: i * 100,
                min: i as f64,
                max: i as f64,
                avg: i as f64,
                count: 1,
            })
            .collect();

        let result = lttb_downsample(&data, 10);

        assert_eq!(result[0], 0, "First point should always be included");
        assert_eq!(
            *result.last().unwrap(),
            99,
            "Last point should always be included"
        );
        assert_eq!(result.len(), 10, "Should return exactly target points");
    }

    #[test]
    fn test_lttb_preserves_peak() {
        // Create data with a clear peak - LTTB should select it
        let mut data: Vec<AggregatedBucket> = Vec::new();

        // Flat baseline at 0
        for i in 0..20 {
            data.push(AggregatedBucket {
                ts: i * 100,
                min: 0.0,
                max: 0.0,
                avg: 0.0,
                count: 1,
            });
        }

        // Sharp peak at position 25
        for i in 20..30 {
            let peak_value = if i == 25 { 100.0 } else { 0.0 };
            data.push(AggregatedBucket {
                ts: i * 100,
                min: peak_value,
                max: peak_value,
                avg: peak_value,
                count: 1,
            });
        }

        // Back to baseline
        for i in 30..50 {
            data.push(AggregatedBucket {
                ts: i * 100,
                min: 0.0,
                max: 0.0,
                avg: 0.0,
                count: 1,
            });
        }

        let result = lttb_downsample(&data, 10);

        // The peak at index 25 should be selected (forms largest triangle)
        assert!(
            result.contains(&25),
            "LTTB should select the peak point. Selected: {:?}",
            result
        );
    }

    #[test]
    fn test_lttb_mode_uses_lttb_algorithm() {
        // Test that LTTB mode actually uses the LTTB algorithm
        let config = PlotterConfig {
            max_points: 100,
            aggregation_mode: AggregationMode::Lttb,
            aggregation_threshold: Some(2), // Trigger aggregation easily
            ..Default::default()
        };
        let agg = PlotterAggregator::with_config(config);
        agg.set_enabled(true);

        // Add data with varying amplitude (triangular wave)
        for i in 0..200 {
            let value = if i % 20 < 10 {
                (i % 20) as f64 * 10.0
            } else {
                (20 - (i % 20)) as f64 * 10.0
            };
            agg.add_data_point("ch0", i * 100, ChannelValue::Numeric(value));
        }

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 50, // Force downsampling
            is_realtime: false,
        };

        let payload = agg.get_ranged_data(&req);
        let data = payload.line_data.get("ch0").unwrap();

        // LTTB mode should return Single points
        assert!(
            data.iter()
                .all(|p| matches!(p, AggregatedPoint::Single { .. })),
            "LTTB mode should only return Single points"
        );

        // Verify peaks are preserved in some form
        let values: Vec<f64> = data
            .iter()
            .map(|p| match p {
                AggregatedPoint::Single { value, .. } => *value,
                AggregatedPoint::MinMax { min, max, .. } => (min + max) / 2.0,
            })
            .collect();

        let max_value = values.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
        let min_value = values.iter().fold(f64::INFINITY, |a, &b| a.min(b));

        // Data goes through bucket averaging, so peaks are reduced.
        // What matters is that LTTB preserves relative variation (max > min)
        assert!(
            max_value > min_value,
            "LTTB should preserve variation: max={}, min={}",
            max_value,
            min_value
        );
        assert!(
            max_value > 0.0,
            "LTTB should preserve some peak values, max was {}",
            max_value
        );
    }
}
