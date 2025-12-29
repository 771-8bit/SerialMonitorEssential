// Plotter Data Store - Storage for parsed plotter data
//
// Uses ring buffers for numeric data and state change lists for state data.
// Supports multiple channels with auto-detection.

use crate::plotter::parser::ChannelValue;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};

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
    /// Largest Triangle Three Buckets
    #[default]
    Lttb,
    /// No aggregation
    None,
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
}

impl Default for PlotterConfig {
    fn default() -> Self {
        Self {
            max_points: 10000,
            channel_types: HashMap::new(),
            aggregation_mode: AggregationMode::Lttb,
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

/// Plotter data payload for frontend
#[derive(Debug, Clone, Serialize)]
pub struct PlotterDataPayload {
    /// Channels info
    pub channels: Vec<ChannelInfo>,
    /// Numeric data: channel_name -> [(timestamp_ms, value), ...]
    pub line_data: HashMap<String, Vec<(u64, f64)>>,
    /// State data: channel_name -> [StateChange, ...]
    pub state_data: HashMap<String, Vec<StateChange>>,
    /// Start timestamp of the data window
    pub start_ms: u64,
    /// End timestamp of the data window
    pub end_ms: u64,
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

    /// Get data payload for frontend
    pub fn get_data_payload(&self) -> PlotterDataPayload {
        let inner = match self.inner.read() {
            Ok(inner) => inner,
            Err(_) => {
                return PlotterDataPayload {
                    channels: Vec::new(),
                    line_data: HashMap::new(),
                    state_data: HashMap::new(),
                    start_ms: 0,
                    end_ms: 0,
                }
            }
        };

        let channels = inner
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
            .collect();

        let line_data: HashMap<String, Vec<(u64, f64)>> = inner
            .line_data
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
            .collect();

        let state_data: HashMap<String, Vec<StateChange>> = inner
            .state_data
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        // Calculate time range
        let mut start_ms = u64::MAX;
        let mut end_ms = 0u64;

        for buffer in inner.line_data.values() {
            if let Some(&(ts, _)) = buffer.front() {
                start_ms = start_ms.min(ts);
            }
            if let Some(&(ts, _)) = buffer.back() {
                end_ms = end_ms.max(ts);
            }
        }

        for states in inner.state_data.values() {
            if let Some(first) = states.first() {
                start_ms = start_ms.min(first.start_ms);
            }
            if let Some(last) = states.last() {
                end_ms = end_ms.max(last.end_ms.unwrap_or(last.start_ms));
            }
        }

        if start_ms == u64::MAX {
            start_ms = 0;
        }

        PlotterDataPayload {
            channels,
            line_data,
            state_data,
            start_ms,
            end_ms,
        }
    }

    /// Clear all data
    pub fn clear(&self) {
        if let Ok(mut inner) = self.inner.write() {
            inner.channels.clear();
            inner.channel_names.clear();
            inner.channel_types.clear();
            inner.line_data.clear();
            inner.state_data.clear();
        }
    }

    /// Update configuration
    pub fn set_config(&self, config: PlotterConfig) {
        if let Ok(mut inner) = self.inner.write() {
            inner.config = config;
        }
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

        let payload = store.get_data_payload();
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
}
