// Plotter Data Store - Storage for parsed plotter data
//
// Uses ring buffers for numeric data and state change lists for state data.
// Supports multiple channels with auto-detection.
// Includes dynamic aggregation for performance optimization.

use crate::plotter::parser::ChannelValue;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};

/// Maximum target points for aggregation (4K display cap)
const MAX_TARGET_POINTS: u32 = 4000;

/// Default pixel width threshold change percentage for cache invalidation
const PIXEL_WIDTH_THRESHOLD_PERCENT: f32 = 0.2;

/// State change record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateChange {
    /// Start timestamp (ms)
    pub start_ms: u64,
    /// End timestamp (ms), None if ongoing
    pub end_ms: Option<u64>,
    /// State value
    pub state: String,
}

/// Channel type configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum ChannelType {
    /// Line chart (numeric data)
    Line,
    /// State timeline (discrete states)
    State,
    /// Auto-detect based on data
    #[default]
    Auto,
}

/// Aggregation mode for downsampling
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum AggregationMode {
    /// Average value in bucket
    Average,
    /// Min and Max values in bucket
    MinMax,
    /// Largest Triangle Three Buckets (falls back to Average for now)
    #[default]
    Lttb,
    /// No aggregation
    None,
}

impl AggregationMode {
    /// Get default aggregation threshold for this mode
    /// Threshold = data_count > target_points * threshold triggers aggregation
    pub fn default_threshold(&self) -> usize {
        match self {
            AggregationMode::Average | AggregationMode::MinMax | AggregationMode::None => 2,
            AggregationMode::Lttb => 10,
        }
    }
}

/// Plotter configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlotterConfig {
    /// Maximum points to store per channel
    pub max_points: usize,
    /// Channel type overrides
    pub channel_types: HashMap<String, ChannelType>,
    /// Aggregation mode for downsampling
    pub aggregation_mode: AggregationMode,
    /// Aggregation trigger threshold (None = use mode's default)
    #[serde(default)]
    pub aggregation_threshold: Option<usize>,
}

impl Default for PlotterConfig {
    fn default() -> Self {
        Self {
            max_points: 10000,
            channel_types: HashMap::new(),
            aggregation_mode: AggregationMode::Lttb,
            aggregation_threshold: None,
        }
    }
}

/// Channel information for frontend
#[derive(Debug, Clone, Serialize)]
pub struct ChannelInfo {
    /// Channel name/label
    pub name: String,
    /// Detected channel type
    pub channel_type: ChannelType,
    /// Latest value (for display)
    pub latest_value: Option<String>,
    /// Point count
    pub point_count: usize,
}

// ============================================================
// New API structures for dynamic aggregation
// ============================================================

/// Request from frontend for ranged plotter data
#[derive(Debug, Clone, Deserialize)]
pub struct PlotterDataRequest {
    /// Display start time (ms), None = earliest data
    pub time_min_ms: Option<u64>,
    /// Display end time (ms), None = realtime following
    pub time_max_ms: Option<u64>,
    /// Chart width in pixels (required)
    pub pixel_width: u32,
    /// Whether in realtime following mode
    pub is_realtime: bool,
}

/// Aggregated data point
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum AggregatedPoint {
    /// Single value (Average/None mode)
    Single { ts: u64, value: f64 },
    /// Min-Max pair for waveform peaks
    MinMax { ts: u64, min: f64, max: f64 },
}

/// Ranged plotter data payload for frontend (new API)
#[derive(Debug, Clone, Serialize)]
pub struct PlotterRangedPayload {
    /// Channels info
    pub channels: Vec<ChannelInfo>,
    /// Aggregated line data: channel_name -> [AggregatedPoint, ...]
    pub line_data: HashMap<String, Vec<AggregatedPoint>>,
    /// State data: channel_name -> [StateChange, ...] (filtered by time range)
    pub state_data: HashMap<String, Vec<StateChange>>,
    /// Actual start timestamp of returned data
    pub start_ms: u64,
    /// Actual end timestamp of returned data  
    pub end_ms: u64,
    /// Whether data was aggregated
    pub is_aggregated: bool,
}

/// Cached aggregation data (for performance optimization)
#[derive(Debug, Clone)]
struct CachedAggregation {
    /// Time range of the cache (min, max) in ms
    time_range: (u64, u64),
    /// Pixel width used to generate this cache
    pixel_width: u32,
    /// Cached aggregated data per channel
    data: HashMap<String, Vec<AggregatedPoint>>,
    /// Whether this was generated in realtime mode
    was_realtime: bool,
}

/// Inner data structure (protected by RwLock)
#[derive(Debug)]
struct PlotterDataStoreInner {
    /// Channel name -> index mapping
    channels: HashMap<String, usize>,
    /// Channel index -> name mapping
    channel_names: Vec<String>,
    /// Detected channel types
    channel_types: HashMap<String, ChannelType>,
    /// Numeric data buffers (channel_name -> ring buffer of (timestamp, value))
    line_data: HashMap<String, VecDeque<(u64, f64)>>,
    /// State data (channel_name -> list of state changes)
    state_data: HashMap<String, Vec<StateChange>>,
    /// Configuration
    config: PlotterConfig,
    /// Whether plotter is enabled
    enabled: bool,
    /// Aggregation cache (optional)
    cache: Option<CachedAggregation>,
}

/// Plotter data store - thread-safe storage for plotter data
#[derive(Debug)]
pub struct PlotterDataStore {
    inner: Arc<RwLock<PlotterDataStoreInner>>,
}

impl Default for PlotterDataStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for PlotterDataStore {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl PlotterDataStore {
    /// Create a new plotter data store
    pub fn new() -> Self {
        Self::with_config(PlotterConfig::default())
    }

    /// Create a new plotter data store with custom config
    pub fn with_config(config: PlotterConfig) -> Self {
        Self {
            inner: Arc::new(RwLock::new(PlotterDataStoreInner {
                channels: HashMap::new(),
                channel_names: Vec::new(),
                channel_types: HashMap::new(),
                line_data: HashMap::new(),
                state_data: HashMap::new(),
                config,
                enabled: false,
                cache: None,
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

    /// Add a data point for a channel
    pub fn add_data_point(&self, channel: &str, timestamp_ms: u64, value: ChannelValue) {
        let mut inner = match self.inner.write() {
            Ok(inner) => inner,
            Err(_) => return,
        };

        if !inner.enabled {
            return;
        }

        // Auto-create channel if not exists
        if !inner.channels.contains_key(channel) {
            let index = inner.channel_names.len();
            inner.channels.insert(channel.to_string(), index);
            inner.channel_names.push(channel.to_string());
            inner.line_data.insert(channel.to_string(), VecDeque::new());
            inner.state_data.insert(channel.to_string(), Vec::new());
        }

        // Determine channel type
        let channel_type = inner
            .config
            .channel_types
            .get(channel)
            .cloned()
            .unwrap_or(ChannelType::Auto);

        match value {
            ChannelValue::Numeric(v) => {
                // For Auto or Line type, store as numeric
                if channel_type == ChannelType::Auto || channel_type == ChannelType::Line {
                    // Capture max_points before mutable borrow to satisfy borrow checker
                    let max_points = inner.config.max_points;
                    if let Some(buffer) = inner.line_data.get_mut(channel) {
                        buffer.push_back((timestamp_ms, v));

                        // Enforce max_points limit
                        while buffer.len() > max_points {
                            buffer.pop_front();
                        }
                    }

                    // Update detected type
                    inner
                        .channel_types
                        .insert(channel.to_string(), ChannelType::Line);
                }
            }
            ChannelValue::State(s) => {
                // For Auto or State type, store as state
                if channel_type == ChannelType::Auto || channel_type == ChannelType::State {
                    if let Some(state_list) = inner.state_data.get_mut(channel) {
                        // Check if state changed
                        let state_changed = state_list
                            .last()
                            .map(|last| last.state != s)
                            .unwrap_or(true);

                        if state_changed {
                            // End previous state
                            if let Some(last) = state_list.last_mut() {
                                if last.end_ms.is_none() {
                                    last.end_ms = Some(timestamp_ms);
                                }
                            }

                            // Start new state
                            state_list.push(StateChange {
                                start_ms: timestamp_ms,
                                end_ms: None,
                                state: s,
                            });
                        }
                    }

                    // Update detected type
                    inner
                        .channel_types
                        .insert(channel.to_string(), ChannelType::State);
                }
            }
        }

        // Invalidate cache when new data is added
        inner.cache = None;
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
                        let buffer = inner.line_data.get(name);
                        let latest = buffer
                            .and_then(|b| b.back())
                            .map(|(_, v)| format!("{:.2}", v));
                        let count = buffer.map(|b| b.len()).unwrap_or(0);
                        (latest, count)
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

    /// Get numeric data for a channel
    pub fn get_line_data(&self, channel: &str) -> Vec<(u64, f64)> {
        self.inner
            .read()
            .ok()
            .and_then(|inner| inner.line_data.get(channel).cloned())
            .map(|d| d.into_iter().collect())
            .unwrap_or_default()
    }

    /// Get state data for a channel
    pub fn get_state_data(&self, channel: &str) -> Vec<StateChange> {
        self.inner
            .read()
            .ok()
            .and_then(|inner| inner.state_data.get(channel).cloned())
            .unwrap_or_default()
    }

    /// Clear all data
    pub fn clear(&self) {
        if let Ok(mut inner) = self.inner.write() {
            inner.channels.clear();
            inner.channel_names.clear();
            inner.channel_types.clear();
            inner.line_data.clear();
            inner.state_data.clear();
            inner.cache = None;
        }
    }

    /// Update configuration
    pub fn set_config(&self, config: PlotterConfig) {
        if let Ok(mut inner) = self.inner.write() {
            inner.config = config;
        }
    }

    // ============================================================
    // New API: Ranged data with dynamic aggregation
    // ============================================================

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
            // Use cached data
            let cache = inner.cache.as_ref().unwrap();
            (cache.data.clone(), true)
        } else {
            // Generate new aggregated data
            let target_points = req.pixel_width.min(MAX_TARGET_POINTS) as usize;
            let threshold = inner
                .config
                .aggregation_threshold
                .unwrap_or_else(|| inner.config.aggregation_mode.default_threshold());

            let mut aggregated_data = HashMap::new();
            let mut any_aggregated = false;

            for (channel, buffer) in &inner.line_data {
                // Filter by time range
                let filtered: Vec<(u64, f64)> = buffer
                    .iter()
                    .filter(|(ts, _)| *ts >= time_min_ms && *ts <= time_max_ms)
                    .cloned()
                    .collect();

                // Check if aggregation is needed
                let needs_aggregation = filtered.len() > target_points * threshold;

                let points = if needs_aggregation && time_max_ms > time_min_ms {
                    any_aggregated = true;
                    Self::aggregate_channel(
                        &filtered,
                        target_points,
                        &inner.config.aggregation_mode,
                    )
                } else {
                    // No aggregation, convert to Single points
                    filtered
                        .into_iter()
                        .map(|(ts, value)| AggregatedPoint::Single { ts, value })
                        .collect()
                };

                aggregated_data.insert(channel.clone(), points);
            }

            // Update cache
            inner.cache = Some(CachedAggregation {
                time_range: (time_min_ms, time_max_ms),
                pixel_width: req.pixel_width,
                data: aggregated_data.clone(),
                was_realtime: req.is_realtime,
            });

            (aggregated_data, any_aggregated)
        };

        // Filter state data by time range
        let state_data: HashMap<String, Vec<StateChange>> = inner
            .state_data
            .iter()
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
    fn calculate_data_time_range(inner: &PlotterDataStoreInner) -> (u64, u64) {
        let mut min_ms = u64::MAX;
        let mut max_ms = 0u64;

        for buffer in inner.line_data.values() {
            if let Some(&(ts, _)) = buffer.front() {
                min_ms = min_ms.min(ts);
            }
            if let Some(&(ts, _)) = buffer.back() {
                max_ms = max_ms.max(ts);
            }
        }

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
        inner: &PlotterDataStoreInner,
        time_min_ms: u64,
        time_max_ms: u64,
        req: &PlotterDataRequest,
    ) -> bool {
        let cache = match &inner.cache {
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
            // Realtime mode: check if time span is the same
            let cached_span = cache.time_range.1 - cache.time_range.0;
            let new_span = time_max_ms - time_min_ms;
            cached_span == new_span
        } else {
            // Freeze mode: check exact time range match
            cache.time_range.0 == time_min_ms && cache.time_range.1 == time_max_ms
        }
    }

    /// Aggregate a single channel's data into buckets
    fn aggregate_channel(
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

        let time_min = data.first().map(|(ts, _)| *ts).unwrap_or(0);
        let time_max = data.last().map(|(ts, _)| *ts).unwrap_or(0);
        let time_range = time_max.saturating_sub(time_min);

        if time_range == 0 {
            // All same timestamp, just return average
            let avg = data.iter().map(|(_, v)| v).sum::<f64>() / data.len() as f64;
            return vec![AggregatedPoint::Single {
                ts: time_min,
                value: avg,
            }];
        }

        let bucket_width = time_range / target_points as u64;
        let mut buckets: Vec<Vec<f64>> = vec![Vec::new(); target_points];
        let mut bucket_times: Vec<u64> = vec![0; target_points];

        for &(ts, value) in data {
            let bucket_idx =
                ((ts - time_min) / bucket_width).min(target_points as u64 - 1) as usize;
            buckets[bucket_idx].push(value);
            if bucket_times[bucket_idx] == 0 {
                bucket_times[bucket_idx] = ts;
            }
        }

        match mode {
            AggregationMode::MinMax => {
                let mut result = Vec::with_capacity(target_points);
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
            AggregationMode::Average | AggregationMode::Lttb | AggregationMode::None => {
                // LTTB falls back to Average for now
                let mut result = Vec::with_capacity(target_points);
                for (i, bucket) in buckets.iter().enumerate() {
                    if bucket.is_empty() {
                        continue;
                    }
                    let avg = bucket.iter().sum::<f64>() / bucket.len() as f64;
                    let ts = if bucket_times[i] != 0 {
                        bucket_times[i]
                    } else {
                        time_min + (i as u64 * bucket_width)
                    };
                    result.push(AggregatedPoint::Single { ts, value: avg });
                }
                result
            }
        }
    }

    /// Build channel info from inner state
    fn build_channel_info(inner: &PlotterDataStoreInner) -> Vec<ChannelInfo> {
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
                        let buffer = inner.line_data.get(name);
                        let latest = buffer
                            .and_then(|b| b.back())
                            .map(|(_, v)| format!("{:.2}", v));
                        let count = buffer.map(|b| b.len()).unwrap_or(0);
                        (latest, count)
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_data_point() {
        let store = PlotterDataStore::new();
        store.set_enabled(true);

        store.add_data_point("ch0", 1000, ChannelValue::Numeric(123.45));

        let data = store.get_line_data("ch0");
        assert_eq!(data.len(), 1);
        assert_eq!(data[0], (1000, 123.45));
    }

    #[test]
    fn test_ring_buffer_overflow() {
        let config = PlotterConfig {
            max_points: 3,
            ..Default::default()
        };
        let store = PlotterDataStore::with_config(config);
        store.set_enabled(true);

        // Add 5 points
        for i in 0..5 {
            store.add_data_point("ch0", i * 1000, ChannelValue::Numeric(i as f64));
        }

        let data = store.get_line_data("ch0");
        assert_eq!(data.len(), 3);
        // Should have last 3 points (2, 3, 4)
        assert_eq!(data[0], (2000, 2.0));
        assert_eq!(data[1], (3000, 3.0));
        assert_eq!(data[2], (4000, 4.0));
    }

    #[test]
    fn test_channel_auto_create() {
        let store = PlotterDataStore::new();
        store.set_enabled(true);

        assert!(store.get_channels().is_empty());

        store.add_data_point("temp", 1000, ChannelValue::Numeric(25.5));
        store.add_data_point("humidity", 1000, ChannelValue::Numeric(60.0));

        let channels = store.get_channels();
        assert_eq!(channels.len(), 2);
        assert!(channels.contains(&"temp".to_string()));
        assert!(channels.contains(&"humidity".to_string()));
    }

    #[test]
    fn test_state_change_recording() {
        let store = PlotterDataStore::new();
        store.set_enabled(true);

        store.add_data_point("motor", 1000, ChannelValue::State("OFF".to_string()));
        store.add_data_point("motor", 2000, ChannelValue::State("ON".to_string()));
        store.add_data_point("motor", 3000, ChannelValue::State("ON".to_string())); // Same state
        store.add_data_point("motor", 4000, ChannelValue::State("OFF".to_string()));

        let states = store.get_state_data("motor");
        assert_eq!(states.len(), 3); // OFF -> ON -> OFF

        assert_eq!(states[0].state, "OFF");
        assert_eq!(states[0].start_ms, 1000);
        assert_eq!(states[0].end_ms, Some(2000));

        assert_eq!(states[1].state, "ON");
        assert_eq!(states[1].start_ms, 2000);
        assert_eq!(states[1].end_ms, Some(4000));

        assert_eq!(states[2].state, "OFF");
        assert_eq!(states[2].start_ms, 4000);
        assert_eq!(states[2].end_ms, None); // Ongoing
    }

    #[test]
    fn test_get_data_range() {
        let store = PlotterDataStore::new();
        store.set_enabled(true);

        for i in 0..10 {
            store.add_data_point("ch0", i * 1000, ChannelValue::Numeric(i as f64));
        }

        let request = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 800,
            is_realtime: true,
        };
        let payload = store.get_ranged_data(&request);
        assert_eq!(payload.start_ms, 0);
        assert_eq!(payload.end_ms, 9000);
    }

    #[test]
    fn test_disabled_store() {
        let store = PlotterDataStore::new();
        // Not enabled

        store.add_data_point("ch0", 1000, ChannelValue::Numeric(123.45));

        let data = store.get_line_data("ch0");
        assert!(data.is_empty());
    }

    #[test]
    fn test_clear() {
        let store = PlotterDataStore::new();
        store.set_enabled(true);

        store.add_data_point("ch0", 1000, ChannelValue::Numeric(123.45));
        assert_eq!(store.get_channels().len(), 1);

        store.clear();
        assert!(store.get_channels().is_empty());
    }

    #[test]
    fn test_channel_info() {
        let store = PlotterDataStore::new();
        store.set_enabled(true);

        store.add_data_point("temp", 1000, ChannelValue::Numeric(25.5));
        store.add_data_point("temp", 2000, ChannelValue::Numeric(26.0));
        store.add_data_point("state", 1000, ChannelValue::State("ON".to_string()));

        let info = store.get_channel_info();
        assert_eq!(info.len(), 2);

        let temp_info = info.iter().find(|i| i.name == "temp").unwrap();
        assert_eq!(temp_info.channel_type, ChannelType::Line);
        assert_eq!(temp_info.latest_value, Some("26.00".to_string()));
        assert_eq!(temp_info.point_count, 2);

        let state_info = info.iter().find(|i| i.name == "state").unwrap();
        assert_eq!(state_info.channel_type, ChannelType::State);
        assert_eq!(state_info.latest_value, Some("ON".to_string()));
        assert_eq!(state_info.point_count, 1);
    }

    // ============================================================
    // Tests for new ranged data API
    // ============================================================

    #[test]
    fn test_get_ranged_data_no_aggregation() {
        let store = PlotterDataStore::new();
        store.set_enabled(true);

        // Add 10 points - below aggregation threshold
        for i in 0..10 {
            store.add_data_point("ch0", i * 1000, ChannelValue::Numeric(i as f64));
        }

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 800,
            is_realtime: true,
        };

        let payload = store.get_ranged_data(&req);

        assert_eq!(payload.start_ms, 0);
        assert_eq!(payload.end_ms, 9000);
        assert!(!payload.is_aggregated);
        assert_eq!(payload.line_data.get("ch0").unwrap().len(), 10);
    }

    #[test]
    fn test_get_ranged_data_with_aggregation() {
        let config = PlotterConfig {
            max_points: 10000,
            aggregation_mode: AggregationMode::Average,
            aggregation_threshold: Some(2), // aggregate when data > target * 2
            ..Default::default()
        };
        let store = PlotterDataStore::with_config(config);
        store.set_enabled(true);

        // Add 1000 points - will trigger aggregation for small pixel_width
        for i in 0..1000 {
            store.add_data_point("ch0", i * 10, ChannelValue::Numeric((i as f64).sin()));
        }

        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 100, // Small width forces aggregation: 1000 > 100 * 2
            is_realtime: true,
        };

        let payload = store.get_ranged_data(&req);

        assert!(payload.is_aggregated);
        // Should be aggregated to ~100 points
        let points = payload.line_data.get("ch0").unwrap();
        assert!(points.len() <= 100);
        assert!(!points.is_empty());
    }

    #[test]
    fn test_get_ranged_data_time_filter() {
        let store = PlotterDataStore::new();
        store.set_enabled(true);

        // Add points from 0 to 9000ms
        for i in 0..10 {
            store.add_data_point("ch0", i * 1000, ChannelValue::Numeric(i as f64));
        }

        // Request only middle range
        let req = PlotterDataRequest {
            time_min_ms: Some(3000),
            time_max_ms: Some(7000),
            pixel_width: 800,
            is_realtime: false,
        };

        let payload = store.get_ranged_data(&req);

        assert_eq!(payload.start_ms, 3000);
        assert_eq!(payload.end_ms, 7000);
        // Should have points at 3000, 4000, 5000, 6000, 7000 = 5 points
        let points = payload.line_data.get("ch0").unwrap();
        assert_eq!(points.len(), 5);
    }

    #[test]
    fn test_aggregate_channel_minmax() {
        let data: Vec<(u64, f64)> = (0..100)
            .map(|i| (i * 100, (i as f64).sin() * 10.0))
            .collect();

        let result = PlotterDataStore::aggregate_channel(&data, 10, &AggregationMode::MinMax);

        assert_eq!(result.len(), 10);
        for point in &result {
            match point {
                AggregatedPoint::MinMax { min, max, .. } => {
                    assert!(min <= max);
                }
                _ => panic!("Expected MinMax point"),
            }
        }
    }

    #[test]
    fn test_aggregate_channel_average() {
        let data: Vec<(u64, f64)> = (0..100).map(|i| (i * 100, i as f64)).collect();

        let result = PlotterDataStore::aggregate_channel(&data, 10, &AggregationMode::Average);

        assert_eq!(result.len(), 10);
        for point in &result {
            match point {
                AggregatedPoint::Single { .. } => {}
                _ => panic!("Expected Single point"),
            }
        }
    }

    #[test]
    fn test_state_data_time_filter() {
        let store = PlotterDataStore::new();
        store.set_enabled(true);

        store.add_data_point("motor", 1000, ChannelValue::State("OFF".to_string()));
        store.add_data_point("motor", 3000, ChannelValue::State("ON".to_string()));
        store.add_data_point("motor", 6000, ChannelValue::State("OFF".to_string()));

        // Add some line data too
        for i in 0..10 {
            store.add_data_point("temp", i * 1000, ChannelValue::Numeric(20.0 + i as f64));
        }

        // Request range 2000-5000
        let req = PlotterDataRequest {
            time_min_ms: Some(2000),
            time_max_ms: Some(5000),
            pixel_width: 800,
            is_realtime: false,
        };

        let payload = store.get_ranged_data(&req);

        // State "OFF" (1000-3000) overlaps with range (ends in range)
        // State "ON" (3000-6000) overlaps with range (starts in range)
        let states = payload.state_data.get("motor").unwrap();
        assert_eq!(states.len(), 2);
    }
}
