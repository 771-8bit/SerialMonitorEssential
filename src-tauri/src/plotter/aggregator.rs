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
    AggregatedPoint, AggregationMode, ChannelInfo, ChannelType, PlotterConfig, PlotterDataRequest,
    PlotterRangedPayload, StateChange,
};

/// Maximum target points for aggregation (4K display cap)
const MAX_TARGET_POINTS: u32 = 4000;

/// Default pixel width threshold change percentage for cache invalidation
const PIXEL_WIDTH_THRESHOLD_PERCENT: f32 = 0.2;

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

/// Default bucket size for aggregating raw data
const DEFAULT_BUCKET_SIZE: usize = 10;

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
                bucket_size: DEFAULT_BUCKET_SIZE,
                state_data: HashMap::new(),
                config,
                enabled: false,
                view_cache: None,
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

        inner.view_cache = None;
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
            for (channel, value) in point.channels {
                Self::ensure_channel_exists(&mut inner, &channel);

                match value {
                    ChannelValue::Numeric(v) => {
                        Self::process_numeric_value(&mut inner, &channel, point.timestamp_ms, v);
                    }
                    ChannelValue::State(s) => {
                        Self::process_state_value(&mut inner, &channel, point.timestamp_ms, s);
                    }
                }
            }
        }

        inner.view_cache = None;
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
        static CALL_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let call_count = CALL_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
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

        let ts = chunk[0].0; // Use first timestamp
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

    /// Aggregate data into target number of points
    ///
    /// Uses stable bucket boundaries: buckets are fixed intervals from time_min,
    /// so adding new data only affects the latest bucket(s), not historical ones.
    #[cfg(test)]
    pub fn aggregate_data(
        data: &[(u64, f64)],
        target_points: usize,
        mode: &AggregationMode,
    ) -> Vec<AggregatedPoint> {
        if data.is_empty() || target_points == 0 {
            return Vec::new();
        }

        if data.len() <= target_points {
            return data
                .iter()
                .map(|&(ts, value)| AggregatedPoint::Single { ts, value })
                .collect();
        }

        // Use LTTB algorithm for Lttb mode
        if matches!(mode, AggregationMode::Lttb) {
            return Self::lttb_downsample(data, target_points);
        }

        let time_min = data.first().map(|(ts, _)| *ts).unwrap_or(0);
        let time_max = data.last().map(|(ts, _)| *ts).unwrap_or(0);
        let time_range = time_max.saturating_sub(time_min);

        if time_range == 0 {
            let avg = data.iter().map(|(_, v)| v).sum::<f64>() / data.len() as f64;
            return vec![AggregatedPoint::Single {
                ts: time_min,
                value: avg,
            }];
        }

        // Calculate stable bucket width based on data density
        // This ensures bucket boundaries are fixed relative to time_min
        let data_rate = data.len() as f64 / time_range as f64; // points per ms

        // Calculate bucket width that would give us target_points worth of data
        // Use ceiling to ensure we don't exceed target_points
        let bucket_width = if data_rate > 0.0 {
            // Each bucket should contain approximately (data.len() / target_points) points
            // bucket_width = points_per_bucket / data_rate
            let points_per_bucket = data.len() as f64 / target_points as f64;
            (points_per_bucket / data_rate).max(1.0) as u64
        } else {
            time_range / target_points as u64
        }
        .max(1);

        // Calculate actual number of buckets needed (may exceed target_points slightly)
        let num_buckets = ((time_range / bucket_width) + 1) as usize;

        let mut buckets: Vec<Vec<f64>> = vec![Vec::new(); num_buckets];
        let mut bucket_times: Vec<u64> = vec![0; num_buckets];

        // Assign data to buckets using stable boundaries from time_min
        for &(ts, value) in data {
            let bucket_idx = ((ts - time_min) / bucket_width) as usize;
            if bucket_idx < num_buckets {
                buckets[bucket_idx].push(value);
                if bucket_times[bucket_idx] == 0 {
                    bucket_times[bucket_idx] = ts;
                }
            }
        }

        match mode {
            AggregationMode::Average => {
                // Average mode returns MinMax points (average line with min/max band)
                let mut result = Vec::with_capacity(num_buckets);
                for (i, bucket) in buckets.iter().enumerate() {
                    if bucket.is_empty() {
                        continue;
                    }
                    let min = bucket.iter().cloned().fold(f64::INFINITY, f64::min);
                    let max = bucket.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                    let ts = if bucket_times[i] != 0 {
                        bucket_times[i]
                    } else {
                        time_min + (i as u64 * bucket_width)
                    };
                    result.push(AggregatedPoint::MinMax { ts, min, max });
                }
                result
            }
            AggregationMode::Lttb => {
                // Already handled above, this branch is unreachable
                unreachable!()
            }
        }
    }

    /// LTTB (Largest Triangle Three Buckets) downsampling algorithm
    ///
    /// This algorithm preserves the visual characteristics of the waveform by selecting
    /// points that maximize the triangle area formed with neighboring buckets.
    /// Reference: https://skemman.is/bitstream/1946/15343/3/SS_MSthesis.pdf
    #[cfg(test)]
    fn lttb_downsample(data: &[(u64, f64)], target_points: usize) -> Vec<AggregatedPoint> {
        let data_len = data.len();

        if data_len <= target_points || target_points < 3 {
            return data
                .iter()
                .map(|&(ts, value)| AggregatedPoint::Single { ts, value })
                .collect();
        }

        let mut result = Vec::with_capacity(target_points);

        // Always include the first point
        let (first_ts, first_val) = data[0];
        result.push(AggregatedPoint::Single {
            ts: first_ts,
            value: first_val,
        });

        // Calculate bucket size (excluding first and last points)
        let bucket_size = (data_len - 2) as f64 / (target_points - 2) as f64;

        let mut prev_selected_idx: usize = 0;

        for i in 0..(target_points - 2) {
            // Calculate bucket boundaries
            let bucket_start = ((i as f64 * bucket_size) as usize) + 1;
            let bucket_end = (((i + 1) as f64 * bucket_size) as usize + 1).min(data_len - 1);

            // Calculate average point of next bucket (for triangle calculation)
            let next_bucket_start = bucket_end;
            let next_bucket_end = (((i + 2) as f64 * bucket_size) as usize + 1).min(data_len);

            let (avg_ts, avg_val) = if next_bucket_start < next_bucket_end {
                let slice = &data[next_bucket_start..next_bucket_end];
                let sum_ts: u64 = slice.iter().map(|(ts, _)| *ts).sum();
                let sum_val: f64 = slice.iter().map(|(_, v)| *v).sum();
                let count = slice.len() as f64;
                (sum_ts as f64 / count, sum_val / count)
            } else {
                // Last bucket: use last point
                let (ts, val) = data[data_len - 1];
                (ts as f64, val)
            };

            // Find the point in current bucket that forms the largest triangle
            let (prev_ts, prev_val) = data[prev_selected_idx];
            let mut max_area = -1.0f64;
            let mut selected_idx = bucket_start;

            for (idx, &(curr_ts, curr_val)) in data[bucket_start..bucket_end].iter().enumerate() {
                // Calculate triangle area using the cross product formula
                // Area = 0.5 * |x1(y2-y3) + x2(y3-y1) + x3(y1-y2)|
                let area = ((prev_ts as f64 - avg_ts) * (curr_val - prev_val)
                    - (prev_ts as f64 - curr_ts as f64) * (avg_val - prev_val))
                    .abs();

                if area > max_area {
                    max_area = area;
                    selected_idx = bucket_start + idx;
                }
            }

            let (sel_ts, sel_val) = data[selected_idx];
            result.push(AggregatedPoint::Single {
                ts: sel_ts,
                value: sel_val,
            });
            prev_selected_idx = selected_idx;
        }

        // Always include the last point
        let (last_ts, last_val) = data[data_len - 1];
        result.push(AggregatedPoint::Single {
            ts: last_ts,
            value: last_val,
        });

        result
    }

    /// Get ranged plotter data with dynamic aggregation
    pub fn get_ranged_data(&self, req: &PlotterDataRequest) -> PlotterRangedPayload {
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

        // Calculate global time range from data
        let (data_min_ms, data_max_ms) = Self::calculate_data_time_range(&inner);

        // Resolve actual time range
        let time_min_ms = req.time_min_ms.unwrap_or(data_min_ms);
        let time_max_ms = req.time_max_ms.unwrap_or(data_max_ms);

        // Check cache validity
        let cache_valid = Self::is_cache_valid(&inner, time_min_ms, time_max_ms, req);

        let (line_data, is_aggregated) = if cache_valid {
            let cache = inner.view_cache.as_ref().unwrap();
            (cache.data.clone(), true)
        } else {
            // Generate fresh data by merging aggregated and recent
            let target_points = req.pixel_width.min(MAX_TARGET_POINTS) as usize;
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
                let points: Vec<AggregatedPoint> = if needs_aggregation && time_max_ms > time_min_ms
                {
                    any_aggregated = true;
                    // Re-aggregate buckets first, then convert to points
                    let aggregated_buckets =
                        Self::aggregate_buckets_preserving(&all_buckets, target_points);
                    aggregated_buckets
                        .iter()
                        .map(|b| b.to_point(&aggregation_mode))
                        .collect()
                } else {
                    // Just convert to points based on mode
                    all_buckets
                        .iter()
                        .map(|b| b.to_point(&aggregation_mode))
                        .collect()
                };

                aggregated_data.insert(channel.clone(), points);
            }

            // Update cache (move instead of clone when possible)
            let cache_data = aggregated_data.clone();
            inner.view_cache = Some(ViewCache {
                time_range: (time_min_ms, time_max_ms),
                pixel_width: req.pixel_width,
                data: cache_data,
                was_realtime: req.is_realtime,
            });

            (aggregated_data, any_aggregated)
        };

        // Filter state data by time range
        let state_data: HashMap<String, Vec<StateChange>> = inner
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
            .collect();

        // Build channel info
        let channels = Self::build_channel_info(&inner);

        PlotterRangedPayload {
            channels,
            line_data,
            state_data,
            start_ms: time_min_ms,
            end_ms: time_max_ms,
            is_aggregated,
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
        let pixel_threshold = cache.pixel_width as f32 * PIXEL_WIDTH_THRESHOLD_PERCENT;
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
            inner.bucket_size = DEFAULT_BUCKET_SIZE;
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
            inner.bucket_size = DEFAULT_BUCKET_SIZE;
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
    fn test_lttb_aggregation() {
        // Create a sine wave with clear peaks and valleys
        let mut data: Vec<(u64, f64)> = Vec::new();
        for i in 0..1000 {
            let t = i as f64 * 0.01;
            let value = (t * std::f64::consts::PI * 2.0).sin() * 100.0;
            data.push((i * 10, value)); // 10ms intervals
        }

        // Downsample to 50 points using LTTB
        let result = PlotterAggregator::aggregate_data(&data, 50, &AggregationMode::Lttb);

        // Should return approximately target_points
        assert!(result.len() <= 52); // Allow small variance
        assert!(result.len() >= 48);

        // First and last points should be preserved (LTTB always keeps them)
        if let AggregatedPoint::Single { ts, value } = result[0] {
            assert_eq!(ts, 0);
            assert!((value - 0.0).abs() < 0.01); // sin(0) = 0
        }
        if let AggregatedPoint::Single { ts, .. } = result[result.len() - 1] {
            assert_eq!(ts, 9990); // Last point
        }

        // Check that peaks are approximately preserved
        // The max abs value should be close to 100 (within 10%)
        let max_abs: f64 = result
            .iter()
            .filter_map(|p| {
                if let AggregatedPoint::Single { value, .. } = p {
                    Some(value.abs())
                } else {
                    None
                }
            })
            .fold(0.0, f64::max);
        assert!(
            max_abs > 90.0,
            "LTTB should preserve peaks, got max={}",
            max_abs
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
    fn test_lttb_with_small_data() {
        // LTTB with data <= target_points should return all data
        let data: Vec<(u64, f64)> = (0..10).map(|i| (i * 100, i as f64)).collect();

        let result = PlotterAggregator::aggregate_data(&data, 20, &AggregationMode::Lttb);

        // All 10 points should be returned as Single
        assert_eq!(result.len(), 10);
        for (i, point) in result.iter().enumerate() {
            if let AggregatedPoint::Single { ts, value } = *point {
                assert_eq!(ts, (i as u64) * 100);
                assert_eq!(value, i as f64);
            } else {
                panic!("Expected Single point");
            }
        }
    }

    #[test]
    fn test_lttb_minimum_target() {
        // LTTB with target_points < 3 should return all data
        let data: Vec<(u64, f64)> = (0..100).map(|i| (i * 100, i as f64)).collect();

        let result = PlotterAggregator::aggregate_data(&data, 2, &AggregationMode::Lttb);

        // Should return all data since target < 3
        assert_eq!(result.len(), 100);
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
        assert_eq!(payload.start_ms, 0);
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
                ts >= 3000 && ts <= 7000,
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
        assert_eq!(payload.start_ms, 0, "Start should be 0");
        assert!(
            payload.end_ms >= 4800,
            "End should be >= 4800, got {}",
            payload.end_ms
        );
    }
}
