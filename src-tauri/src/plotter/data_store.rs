// Plotter Data Types - Shared type definitions for plotter
//
// Contains type definitions used by PlotterAggregator and frontend API.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    /// Average value in bucket with min/max band
    Average,
    /// Largest Triangle Three Buckets (feature-preserving)
    #[default]
    Lttb,
}

impl AggregationMode {
    /// Get default aggregation threshold for this mode
    /// Threshold = data_count > target_points * threshold triggers aggregation
    pub fn default_threshold(&self) -> usize {
        match self {
            AggregationMode::Average => 2,
            AggregationMode::Lttb => 10,
        }
    }
}

/// Plotter configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlotterConfig {
    /// Maximum points to store per channel (for pending buffer)
    pub max_points: usize,
    /// Channel type overrides
    pub channel_types: HashMap<String, ChannelType>,
    /// Aggregation mode for downsampling
    pub aggregation_mode: AggregationMode,
    /// Aggregation trigger threshold (None = use mode's default)
    #[serde(default)]
    pub aggregation_threshold: Option<usize>,
    /// Maximum target points for display aggregation (4K display cap)
    /// This limits how many points are sent to the frontend for rendering.
    #[serde(default = "default_max_target_points")]
    pub max_target_points: u32,
    /// Pixel width threshold change percentage for cache invalidation (0.0 - 1.0)
    /// Cache is invalidated when pixel width changes by more than this percentage.
    #[serde(default = "default_pixel_width_threshold")]
    pub pixel_width_threshold_percent: f32,
    /// Bucket size for aggregating raw data into buffer
    /// Higher values mean more compression but less granularity.
    #[serde(default = "default_bucket_size")]
    pub bucket_size: usize,
}

fn default_max_target_points() -> u32 {
    4000
}

fn default_pixel_width_threshold() -> f32 {
    0.2
}

fn default_bucket_size() -> usize {
    10
}

impl Default for PlotterConfig {
    fn default() -> Self {
        Self {
            max_points: 10000,
            channel_types: HashMap::new(),
            aggregation_mode: AggregationMode::Lttb,
            aggregation_threshold: None,
            max_target_points: default_max_target_points(),
            pixel_width_threshold_percent: default_pixel_width_threshold(),
            bucket_size: default_bucket_size(),
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
    /// Single value (LTTB mode)
    Single { ts: u64, value: f64 },
    /// Min-Max pair for waveform peaks
    MinMax { ts: u64, min: f64, max: f64 },
}

/// Ranged plotter data payload for frontend
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

/// MinMax band series data for a channel
///
/// Used for Average mode to show min/max range bands.
/// Uses the same timestamp indices as `aligned_data[0]` in PlotterChartPayload.
#[derive(Debug, Clone, Serialize)]
pub struct BandSeriesData {
    /// Minimum values (same length as timestamps)
    pub min: Vec<Option<f64>>,
    /// Maximum values (same length as timestamps)
    pub max: Vec<Option<f64>>,
}

/// Chart data payload in uPlot-ready format
///
/// Pre-aligned data format that can be passed directly to uPlot without
/// any per-frame transformation in the frontend. This eliminates the
/// memory leak caused by repeated object creation.
#[derive(Debug, Clone, Serialize)]
pub struct PlotterChartPayload {
    /// uPlot aligned data: [timestamps, ch0_values, ch1_values, ...]
    ///
    /// - `aligned_data[0]`: timestamps in seconds (f64)
    /// - `aligned_data[1..]`: channel values in `channel_names` order
    ///
    /// All channels are included; frontend uses `series.show` for hiding.
    /// `Option<f64>` serializes to JSON `null` for uPlot compatibility.
    pub aligned_data: Vec<Vec<Option<f64>>>,

    /// Channel names in order (matches aligned_data columns starting at index 1)
    pub channel_names: Vec<String>,

    /// MinMax band data (Average mode only): channel_name -> BandSeriesData
    ///
    /// Uses the same timestamp indices as `aligned_data[0]`.
    /// `None` for LTTB mode (no bands).
    pub band_data: Option<HashMap<String, BandSeriesData>>,

    /// State timeline data (unchanged from current format)
    pub state_data: HashMap<String, Vec<StateChange>>,

    /// Channel metadata for frontend legend
    pub channels: Vec<ChannelInfo>,

    /// Actual start timestamp of returned data (ms)
    pub start_ms: u64,

    /// Actual end timestamp of returned data (ms)
    pub end_ms: u64,
}
