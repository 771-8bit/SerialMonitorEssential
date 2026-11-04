// Plotter Aggregator - tiered ("pyramid") storage for plotter data (ADR-16)
//
// Each line channel keeps its samples in tiers on an ABSOLUTE time grid:
// tier k holds cells 2^k ms wide (cell start = idx << k). New samples land in
// tier 0 (1 ms cells, the chart's native resolution). As data ages, cells move
// to the next tier, where two neighbouring cells become one. Cells therefore
// only ever get coarser and never shift, and every LIVE window is drawn from
// cells no wider than its display cells, so the picture depends only on the
// samples in the window - not on when tiers were folded (UN-01, UN-03).
//
// Memory is bounded: tier k >= 1 covers ages [A * 2^(k-1), A * 2^k) with cells
// of 2^k ms, i.e. A / 2 cells whatever k is; tier 0 covers ages [0, A).

use crate::plotter::parser::ChannelValue;
use crate::plotter::spill::{SpillCell, SpillLog, LEVEL_MS};
// Note: serde traits are unused here as types are re-exported from data_store
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, RwLock};

// Re-export common types from data_store
pub use crate::plotter::data_store::{
    AggregatedPoint, AggregationMode, BandSeriesData, ChannelInfo, ChannelType,
    PlotterChartPayload, PlotterConfig, PlotterDataRequest, PlotterRangedPayload, StateChange,
};

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

/// State changes kept in memory per channel; older ones live in the disk spill
const MAX_STATE_CHANGES: usize = 10_000;

/// Most spilled state segments returned for one request
const MAX_SPILLED_STATES: usize = 50_000;

/// Highest tier index. Tier 20 holds ~1 s cells covering ages of days; beyond
/// that cells stay in tier 20 (memory then grows by A / 2 cells per tier span).
const MAX_TIER: usize = 20;

/// Internal bucket storing all statistics for mode-agnostic storage
///
/// This allows switching between Peak and Average modes at display time
/// without losing precision, since we store all necessary statistics.
#[derive(Debug, Clone)]
struct AggregatedBucket {
    /// Timestamp. Stored cells: the start of the cell on its tier's grid.
    /// Single samples: the sample time.
    ts: u64,
    /// Minimum value in bucket
    min: f64,
    /// Maximum value in bucket
    max: f64,
    /// Exact sum of the values (the average is sum / count; keeping the sum
    /// instead of a running average keeps merges exact and order-free)
    sum: f64,
    /// Number of raw points this bucket represents (for weighted averaging)
    count: usize,
    /// Time of the sample that is `min` (Peak mode draws it at its real time)
    ts_min: u64,
    /// Time of the sample that is `max`
    ts_max: u64,
}

impl AggregatedBucket {
    /// One raw sample
    fn sample(ts: u64, value: f64) -> Self {
        Self {
            ts,
            min: value,
            max: value,
            sum: value,
            count: 1,
            ts_min: ts,
            ts_max: ts,
        }
    }

    /// Count-weighted average of the values
    fn avg(&self) -> f64 {
        if self.count > 0 {
            self.sum / self.count as f64
        } else {
            0.0
        }
    }

    /// Fold another bucket's extremes into this one. The smaller (larger) value
    /// wins and, on equal values, the earlier time wins. This makes the merge
    /// associative and commutative, so the extremes do not depend on how the
    /// samples were grouped (tier folding, batch vs. one-by-one: P-5 / P-7).
    fn merge_extremes(&mut self, other: &AggregatedBucket) {
        if other.min < self.min || (other.min == self.min && other.ts_min < self.ts_min) {
            self.min = other.min;
            self.ts_min = other.ts_min;
        }
        if other.max > self.max || (other.max == self.max && other.ts_max < self.ts_max) {
            self.max = other.max;
            self.ts_max = other.ts_max;
        }
    }

    /// Fold another bucket into this one (extremes, count and sum)
    fn absorb(&mut self, other: &AggregatedBucket) {
        self.merge_extremes(other);
        self.count += other.count;
        self.sum += other.sum;
    }

    /// Average-mode point: weighted average as the line, min/max as the band
    fn to_average_point(&self) -> AggregatedPoint {
        AggregatedPoint::MinMax {
            ts: self.ts,
            min: self.min,
            max: self.max,
            avg: self.avg(),
        }
    }
}

impl From<&AggregatedBucket> for SpillCell {
    fn from(b: &AggregatedBucket) -> Self {
        SpillCell {
            ts: b.ts,
            min: b.min,
            max: b.max,
            sum: b.sum,
            count: b.count as u64,
            ts_min: b.ts_min,
            ts_max: b.ts_max,
        }
    }
}

impl From<&SpillCell> for AggregatedBucket {
    fn from(c: &SpillCell) -> Self {
        AggregatedBucket {
            ts: c.ts,
            min: c.min,
            max: c.max,
            sum: c.sum,
            count: c.count as usize,
            ts_min: c.ts_min,
            ts_max: c.ts_max,
        }
    }
}

/// Tiered storage of one line channel (see the module comment)
#[derive(Debug, Default)]
struct Pyramid {
    /// tiers[k]: cells of 2^k ms, sorted by `ts` (cell start), oldest first
    tiers: Vec<VecDeque<AggregatedBucket>>,
}

impl Pyramid {
    /// Merge `cell` into tier `k`, into the 2^k-ms cell that contains `cell.ts`
    fn merge_into(&mut self, k: usize, cell: &AggregatedBucket) {
        while self.tiers.len() <= k {
            self.tiers.push(VecDeque::new());
        }
        let start = (cell.ts >> k) << k;
        let tier = &mut self.tiers[k];
        match tier.back_mut() {
            // The common case: data arrives (and ages) in time order
            Some(back) if back.ts == start => back.absorb(cell),
            Some(back) if back.ts > start => match tier.binary_search_by(|c| c.ts.cmp(&start)) {
                Ok(i) => tier[i].absorb(cell),
                Err(i) => {
                    let mut fresh = cell.clone();
                    fresh.ts = start;
                    tier.insert(i, fresh);
                }
            },
            _ => {
                let mut fresh = cell.clone();
                fresh.ts = start;
                tier.push_back(fresh);
            }
        }
    }

    /// Move every cell that has aged out of its tier to the next one. A cell of
    /// tier k leaves once it ends at least A * 2^k before `now`. The result only
    /// depends on the final `now` (thresholds only grow and merges are
    /// associative), so it is the same however the data was batched (P-7).
    ///
    /// Cells leaving tier 0 are final 1 ms cells; they are also pushed to
    /// `departed` so they can be written to the disk spill (SYS-F-518).
    fn promote(
        &mut self,
        now: u64,
        tier0_span_ms: u64,
        mut departed: Option<&mut Vec<AggregatedBucket>>,
    ) {
        let mut k = 0;
        while k < self.tiers.len() && k < MAX_TIER {
            let limit = now.saturating_sub(tier0_span_ms.saturating_mul(1 << k));
            while let Some(front) = self.tiers[k].front() {
                if front.ts + (1u64 << k) > limit {
                    break;
                }
                let cell = self.tiers[k].pop_front().expect("front exists");
                if k == 0 {
                    if let Some(out) = departed.as_deref_mut() {
                        out.push(cell.clone());
                    }
                }
                self.merge_into(k + 1, &cell);
            }
            k += 1;
        }
    }

    /// Cells overlapping [t0, t1], oldest tier first. Every cell of tier k + 1
    /// starts no later than the first cell of tier k, so the result is sorted
    /// by `ts`.
    ///
    /// Returns the width (ms) of the coarsest cell included (0 if none).
    fn collect(&self, t0: u64, t1: u64, out: &mut Vec<AggregatedBucket>) -> u64 {
        let mut coarsest = 0;
        for (k, tier) in self.tiers.iter().enumerate().rev() {
            let width = 1u64 << k;
            let first = tier.partition_point(|c| c.ts + width <= t0);
            let before = out.len();
            out.extend(tier.iter().skip(first).take_while(|c| c.ts <= t1).cloned());
            if out.len() > before {
                coarsest = coarsest.max(width);
            }
        }
        coarsest
    }

    /// Start of the oldest stored cell
    fn first_ts(&self) -> Option<u64> {
        self.tiers
            .iter()
            .rev()
            .find_map(|tier| tier.front().map(|c| c.ts))
    }

    /// Number of samples represented
    fn sample_count(&self) -> usize {
        self.tiers.iter().flatten().map(|c| c.count).sum()
    }
}

/// Round a bucket width UP to a power of two (1, 2, 4, 8, ... ms), minimum
/// 1 ms. Powers of two nest with the storage tiers (a 2^k-ms storage cell
/// never straddles a 2^j-ms display cell when j >= k), and quantizing keeps
/// the grid itself still while the sliding window's span jitters by a few ms
/// (SYS-F-512).
fn quantize_bucket_width_pow2(raw_width_ms: u64) -> u64 {
    raw_width_ms
        .max(1)
        .checked_next_power_of_two()
        .unwrap_or(1 << 63)
}

/// Aggregate buckets into ABSOLUTE-TIME-ALIGNED cells of the given width.
///
/// Cell index = ts / width (integer division on absolute ms), cell timestamp =
/// idx*width + width/2. Because the grid is anchored to absolute time (not to
/// time_min), a cell's contents and timestamp are identical across successive
/// sliding-window requests - only the newest cell changes as data arrives.
///
/// Stats are merged with `absorb`: min of mins and max of maxs (with their
/// times), sum of sums, sum of counts.
fn aggregate_buckets_aligned(data: &[AggregatedBucket], width_ms: u64) -> Vec<AggregatedBucket> {
    let width = width_ms.max(1);

    // BTreeMap keyed by cell index => output is sorted by ts for free.
    let mut cells: BTreeMap<u64, AggregatedBucket> = BTreeMap::new();
    for bucket in data {
        let idx = bucket.ts / width;
        match cells.get_mut(&idx) {
            Some(cell) => cell.absorb(bucket),
            None => {
                cells.insert(idx, bucket.clone());
            }
        }
    }

    cells
        .into_iter()
        .map(|(idx, mut cell)| {
            cell.ts = idx * width + width / 2;
            cell
        })
        .collect()
}

/// One extreme sample collected into a Peak cell
#[derive(Clone, Copy)]
struct PeakSample {
    ts: u64,
    value: f64,
}

/// Accumulator for one Peak cell
struct PeakCell {
    min: Option<PeakSample>,
    max: Option<PeakSample>,
    weighted_sum: f64,
    count: usize,
}

/// Peak-detect envelope (SYS-F-515 / UN-03).
///
/// Every stored bucket contributes its two extreme samples, (ts_min, min) and
/// (ts_max, max). Each sample goes into the absolute-time-aligned cell of ITS
/// OWN time - not the bucket's `ts` - so the output stays in time order even
/// when a bucket straddles a cell boundary (SYS-NF-602). Each cell then emits
/// its minimum and its maximum sample in the order they occurred. The values
/// are real samples, never averages, so the highest and the lowest sample in
/// the window always appear (why this replaced a triangle-area selection that
/// could drop a spike: ADR-15).
///
/// Cells depend only on absolute time, so a finished cell is identical on every
/// later request (SYS-F-511); only the newest cell and the cells cut by the
/// window edges can change (SYS-F-504).
///
/// The chart aligns all channels on shared millisecond timestamps and cannot
/// draw two values at one instant. When a cell's minimum and maximum fall in the
/// same millisecond, the one farther from the cell average (the spike) is kept.
fn peak_envelope(
    data: &[AggregatedBucket],
    width_ms: u64,
    time_min: u64,
    time_max: u64,
) -> Vec<AggregatedPoint> {
    let width = width_ms.max(1);
    let mut cells: BTreeMap<u64, PeakCell> = BTreeMap::new();

    for bucket in data {
        let extremes = [
            PeakSample {
                ts: bucket.ts_min,
                value: bucket.min,
            },
            PeakSample {
                ts: bucket.ts_max,
                value: bucket.max,
            },
        ];
        for sample in extremes {
            if sample.ts < time_min || sample.ts > time_max {
                continue;
            }
            let cell = cells.entry(sample.ts / width).or_insert(PeakCell {
                min: None,
                max: None,
                weighted_sum: 0.0,
                count: 0,
            });
            let lower = cell.min.is_none_or(|m| {
                sample.value < m.value || (sample.value == m.value && sample.ts < m.ts)
            });
            if lower {
                cell.min = Some(sample);
            }
            let higher = cell.max.is_none_or(|m| {
                sample.value > m.value || (sample.value == m.value && sample.ts < m.ts)
            });
            if higher {
                cell.max = Some(sample);
            }
        }
        // The average only breaks same-millisecond ties; attribute it to the
        // cell of the bucket's own timestamp.
        if let Some(cell) = cells.get_mut(&(bucket.ts / width)) {
            cell.weighted_sum += bucket.sum;
            cell.count += bucket.count;
        }
    }

    let mut points = Vec::with_capacity(cells.len() * 2);
    for cell in cells.into_values() {
        let (Some(min), Some(max)) = (cell.min, cell.max) else {
            continue;
        };
        if min.ts == max.ts {
            let keep = if min.value == max.value || cell.count == 0 {
                max
            } else {
                let avg = cell.weighted_sum / cell.count as f64;
                if max.value - avg >= avg - min.value {
                    max
                } else {
                    min
                }
            };
            points.push(AggregatedPoint::Single {
                ts: keep.ts,
                value: keep.value,
            });
        } else {
            let (first, second) = if min.ts < max.ts {
                (min, max)
            } else {
                (max, min)
            };
            points.push(AggregatedPoint::Single {
                ts: first.ts,
                value: first.value,
            });
            points.push(AggregatedPoint::Single {
                ts: second.ts,
                value: second.value,
            });
        }
    }
    points
}

/// Inner data structure (protected by RwLock). Line channels are stored in
/// per-channel `Pyramid`s (see the module comment); state channels as lists of
/// changes.
#[derive(Debug)]
struct PlotterAggregatorInner {
    /// Channel name -> index mapping
    channels: HashMap<String, usize>,
    /// Channel index -> name mapping
    channel_names: Vec<String>,
    /// Detected channel types
    channel_types: HashMap<String, ChannelType>,

    /// Tiered storage per line channel
    pyramids: HashMap<String, Pyramid>,

    /// Latest numeric value per channel (legend)
    latest: HashMap<String, f64>,

    /// Full-resolution disk spill of this session (SYS-F-518), if enabled
    spill: Option<Arc<SpillLog>>,

    /// State data (channel_name -> list of state changes)
    state_data: HashMap<String, Vec<StateChange>>,

    /// Configuration
    config: PlotterConfig,

    /// Whether plotter is enabled
    enabled: bool,

    /// View cache for current display
    view_cache: Option<ViewCache>,

    /// Data version counter (incremented on every data change)
    data_version: u64,

    /// Timestamp of the most recently received data point (any channel,
    /// including unchanged-state repeats that don't append a StateChange)
    last_data_ts: u64,
}

/// Version info for lightweight polling
#[derive(Debug, Clone, serde::Serialize)]
pub struct PlotterVersionInfo {
    pub version: u64,
    pub has_data: bool,
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
        let spill = Self::new_spill(&config);
        Self {
            inner: Arc::new(RwLock::new(PlotterAggregatorInner {
                channels: HashMap::new(),
                channel_names: Vec::new(),
                channel_types: HashMap::new(),
                pyramids: HashMap::new(),
                latest: HashMap::new(),
                spill,
                state_data: HashMap::new(),
                config,
                enabled: false,
                view_cache: None,
                data_version: 0,
                last_data_ts: 0,
            })),
        }
    }

    /// A fresh spill generation if the config asks for one. A spill that cannot
    /// be created (no temp space) only costs zoom detail, so log and go on.
    fn new_spill(config: &PlotterConfig) -> Option<Arc<SpillLog>> {
        if !config.spill_to_disk {
            return None;
        }
        match SpillLog::create_default() {
            Ok(spill) => Some(Arc::new(spill)),
            Err(e) => {
                log::warn!("[Aggregator] Plotter disk spill disabled: {e}");
                None
            }
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

    /// Lightweight check: returns version and whether data exists
    /// This is O(1) and doesn't require any data processing
    pub fn check_version(&self) -> PlotterVersionInfo {
        match self.inner.read() {
            Ok(inner) => PlotterVersionInfo {
                version: inner.data_version,
                has_data: !inner.channel_names.is_empty(),
            },
            // Poisoned lock: report "no data" instead of panicking at 60Hz
            Err(_) => PlotterVersionInfo {
                version: 0,
                has_data: false,
            },
        }
    }

    /// Ensure channel exists, creating it if needed
    fn ensure_channel_exists(inner: &mut PlotterAggregatorInner, channel: &str) {
        if !inner.channels.contains_key(channel) {
            let index = inner.channel_names.len();
            inner.channels.insert(channel.to_string(), index);
            inner.channel_names.push(channel.to_string());
            inner
                .pyramids
                .insert(channel.to_string(), Pyramid::default());
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
            if let Some(pyramid) = inner.pyramids.get_mut(channel) {
                pyramid.merge_into(0, &AggregatedBucket::sample(timestamp_ms, value));
            }
            inner.latest.insert(channel.to_string(), value);
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
            let mut dropped = Vec::new();
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

                    // Bound memory: drop the oldest changes once the list grows
                    // past the cap (long sessions with fast-toggling states
                    // would otherwise grow without limit). The dropped,
                    // closed segments go to the disk spill so a zoom into
                    // the past still shows them (SYS-F-712 / SYS-F-518).
                    if state_list.len() > MAX_STATE_CHANGES {
                        let overflow = state_list.len() - MAX_STATE_CHANGES;
                        dropped = state_list
                            .drain(..overflow)
                            .map(|s| (s.start_ms, s.end_ms.unwrap_or(s.start_ms), s.state))
                            .collect();
                    }
                }
            }
            if let (false, Some(spill), Some(&index)) = (
                dropped.is_empty(),
                inner.spill.as_ref(),
                inner.channels.get(channel),
            ) {
                spill.append_states(index, dropped);
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

        // Track the latest received timestamp even when the value doesn't
        // append anything (e.g. an unchanged state repeat).
        inner.last_data_ts = inner.last_data_ts.max(timestamp_ms);

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
            // Track the latest received timestamp even when a point doesn't
            // append anything (e.g. an unchanged state repeat).
            inner.last_data_ts = inner.last_data_ts.max(point.timestamp_ms);

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

    /// Age the stored cells (move them up the tiers) and bump the version.
    /// Ages are measured against the newest data time, not the wall clock, so
    /// the result is deterministic and independent of polling.
    fn maybe_aggregate(inner: &mut PlotterAggregatorInner) {
        let now = inner.last_data_ts;
        let span = inner.config.tier0_span_ms;
        let PlotterAggregatorInner {
            pyramids,
            channels,
            spill,
            ..
        } = &mut *inner;
        let mut departed = Vec::new();
        for (name, pyramid) in pyramids.iter_mut() {
            match spill.as_ref() {
                Some(spill) => {
                    departed.clear();
                    pyramid.promote(now, span, Some(&mut departed));
                    if let (false, Some(&index)) = (departed.is_empty(), channels.get(name)) {
                        spill.append(index, departed.iter().map(SpillCell::from).collect());
                    }
                }
                None => pyramid.promote(now, span, None),
            }
        }

        // Increment version on any data change
        inner.data_version = inner.data_version.wrapping_add(1);
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
                let state_data =
                    Self::filter_state_data(&inner, time_min_ms, time_max_ms, req.is_realtime);
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
            let state_data =
                Self::filter_state_data(&inner, time_min_ms, time_max_ms, req.is_realtime);
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

            // Display cell width for this request (see below)
            let span = time_max_ms.saturating_sub(time_min_ms);
            let display_cells = if aggregation_mode == AggregationMode::Peak {
                (target_points / 2).saturating_sub(1).max(1) as u64
            } else {
                target_points.saturating_sub(1).max(1) as u64
            };
            let display_width = quantize_bucket_width_pow2(span.div_ceil(display_cells));

            // Cells overlapping the range, from every tier, in time order
            let mut all_buckets: Vec<AggregatedBucket> = Vec::new();
            let coarsest = match inner.pyramids.get(channel) {
                Some(pyramid) => pyramid.collect(time_min_ms, time_max_ms, &mut all_buckets),
                None => 0,
            };

            // A zoomed static range over old data needs finer cells than memory
            // keeps: read them from the disk spill (SYS-F-518). LIVE windows never
            // need this (ADR-16) and never touch the disk.
            if !req.is_realtime && coarsest > display_width {
                if let (Some(spill), Some(&index), Some(pyramid)) = (
                    inner.spill.as_ref(),
                    inner.channels.get(channel),
                    inner.pyramids.get(channel),
                ) {
                    all_buckets = Self::collect_full_resolution(
                        spill,
                        index,
                        pyramid,
                        time_min_ms,
                        time_max_ms,
                        display_width,
                    );
                }
            }

            // Convert buckets to points on an ABSOLUTE-TIME-ALIGNED grid with a
            // power-of-two width (SYS-F-511 / 512): a cell's boundaries depend
            // only on absolute time, so once formed it is identical on every
            // later fetch; only the newest, still-filling cell changes. Storage
            // cells are powers of two too, so in a LIVE window (whose oldest data
            // is young enough, ADR-16) every storage cell lies inside one display
            // cell and the picture is a function of the samples alone.
            //
            // Peak (SYS-F-515): each cell emits its min and max sample, so about
            // target / 2 cells keeps the point count within the pixel width
            // (SYS-F-516). A span of N cell widths can touch N + 1 cells (partial
            // cells at both edges), hence target / 2 - 1.
            //
            // Average: one point per cell (the weighted average, with the
            // min/max band); sparse data is passed through at its own times.
            let points: Vec<AggregatedPoint> = if aggregation_mode == AggregationMode::Peak {
                let envelope = peak_envelope(&all_buckets, display_width, time_min_ms, time_max_ms);
                let samples: usize = all_buckets.iter().map(|b| b.count).sum();
                if envelope.len() < samples {
                    any_aggregated = true;
                }
                envelope
            } else if span > 0 && all_buckets.len() > target_points {
                any_aggregated = true;
                aggregate_buckets_aligned(&all_buckets, display_width)
                    .into_iter()
                    .map(|mut cell| {
                        // The cells at the window edges are cut by the window:
                        // keep their centre inside the requested range
                        cell.ts = cell.ts.clamp(time_min_ms, time_max_ms);
                        cell.to_average_point()
                    })
                    .collect()
            } else {
                all_buckets
                    .iter()
                    .map(AggregatedBucket::to_average_point)
                    .collect()
            };

            aggregated_data.insert(channel.clone(), points);
        }

        // Update cache.
        // Skip in realtime mode: the next poll always advances time_max_ms and
        // invalidates the cache, so storing it would only add a full deep copy
        // of the multi-channel result on every fetch for zero hits.
        if !req.is_realtime {
            let cache_data = aggregated_data.clone();
            inner.view_cache = Some(ViewCache {
                time_range: (time_min_ms, time_max_ms),
                pixel_width: req.pixel_width,
                data: cache_data,
                was_realtime: req.is_realtime,
            });
        }

        // Filter state data by time range
        let state_data = Self::filter_state_data(&inner, time_min_ms, time_max_ms, req.is_realtime);

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

    /// Cells of [t0, t1] at least as fine as `width`, from the disk spill plus
    /// the part of tier 0 not yet spilled (SYS-F-518). The spill level is the
    /// coarsest one not wider than `width`, so a display cell reads at most ~64
    /// records; the tail of that level that is still being accumulated is read
    /// from level 0. Everything newer than the spill is still in tier 0.
    fn collect_full_resolution(
        spill: &SpillLog,
        index: usize,
        pyramid: &Pyramid,
        t0: u64,
        t1: u64,
        width: u64,
    ) -> Vec<AggregatedBucket> {
        if !spill.sync() {
            log::warn!("[Aggregator] Spill sync timed out; drawing from memory");
        }
        let level = LEVEL_MS.iter().rposition(|&w| w <= width).unwrap_or(0);
        let end = t1.saturating_add(1);
        let (cells, mut covered) = spill.read(index, level, t0, end);
        let mut out: Vec<AggregatedBucket> = cells.iter().map(AggregatedBucket::from).collect();
        if level > 0 {
            let (fine, fine_covered) = spill.read(index, 0, t0.max(covered), end);
            out.extend(fine.iter().map(AggregatedBucket::from));
            covered = covered.max(fine_covered);
        }
        // Memory: only what the spill does not hold (tier 0, or every tier past
        // the point where the spill hit its size cap)
        let mut memory = Vec::new();
        pyramid.collect(t0.max(covered), t1, &mut memory);
        out.extend(memory.into_iter().filter(|c| c.ts >= covered));
        out
    }

    /// Filter state data by time range (helper for get_ranged_data)
    ///
    /// Static requests reaching back before the oldest change kept in memory
    /// also get the spilled segments (SYS-F-712 / SYS-F-518).
    fn filter_state_data(
        inner: &PlotterAggregatorInner,
        time_min_ms: u64,
        time_max_ms: u64,
        is_realtime: bool,
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
                let mut filtered: Vec<StateChange> = Vec::new();
                let oldest_in_memory = states.first().map_or(u64::MAX, |s| s.start_ms);
                if !is_realtime && time_min_ms < oldest_in_memory {
                    if let (Some(spill), Some(&index)) =
                        (inner.spill.as_ref(), inner.channels.get(channel))
                    {
                        spill.sync();
                        filtered.extend(
                            spill
                                .read_states(index, time_min_ms, time_max_ms, MAX_SPILLED_STATES)
                                .into_iter()
                                .map(|(start, end, state)| StateChange {
                                    start_ms: start,
                                    end_ms: Some(end),
                                    state,
                                }),
                        );
                    }
                }
                filtered.extend(
                    states
                        .iter()
                        .filter(|s| {
                            let end = s.end_ms.unwrap_or(time_max_ms);
                            s.start_ms <= time_max_ms && end >= time_min_ms
                        })
                        .cloned(),
                );
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

        // If no line data, return payload without line series.
        // State-only streams still get an x-range (as two timestamps) so the
        // frontend can establish a time scale for the state timeline.
        if channel_names.is_empty() {
            let has_states = ranged.state_data.values().any(|v| !v.is_empty());
            let aligned_data = if has_states && ranged.end_ms >= ranged.start_ms {
                // A zero-span range (single state change, nothing since) would
                // give uPlot a degenerate x scale; widen it by 1s instead.
                let end_ms = if ranged.end_ms == ranged.start_ms {
                    ranged.start_ms + 1000
                } else {
                    ranged.end_ms
                };
                vec![vec![
                    Some(ranged.start_ms as f64 / 1000.0),
                    Some(end_ms as f64 / 1000.0),
                ]]
            } else {
                vec![vec![]] // Empty timestamps array
            };
            return PlotterChartPayload {
                aligned_data,
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
                    Some(AggregatedPoint::MinMax { min, max, avg, .. }) => {
                        // Use the true weighted average as the center line
                        values.push(Some(*avg));
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

        // Line channels: oldest stored cell (the latest is last_data_ts below)
        for pyramid in inner.pyramids.values() {
            if let Some(first) = pyramid.first_ts() {
                min_ms = min_ms.min(first);
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

        // Extend to the latest line actually received. Without this, a stream
        // whose only open state segment has end_ms=None would report a range
        // ending at that segment's start, rendering it zero-width.
        let max_ms = max_ms.max(inner.last_data_ts);

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
                        let latest = inner.latest.get(name).map(|v| format!("{:.2}", v));
                        // Samples received (the legend's "pts")
                        let count = inner
                            .pyramids
                            .get(name)
                            .map(Pyramid::sample_count)
                            .unwrap_or(0);
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
            inner.pyramids.clear();
            inner.latest.clear();
            // New session, new spill generation; the old one is deleted once
            // the last reader lets go of it
            inner.spill = Self::new_spill(&inner.config);
            inner.state_data.clear();
            inner.view_cache = None;
            inner.last_data_ts = 0;
            // Increment version to notify frontend of data change
            inner.data_version = inner.data_version.wrapping_add(1);
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

    /// Number of line samples held across all channels
    pub fn total_points(&self) -> usize {
        self.inner
            .read()
            .map(|inner| inner.pyramids.values().map(Pyramid::sample_count).sum())
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
            tier0_span_ms: 100,
            aggregation_mode: AggregationMode::Average,
            ..Default::default()
        };
        let agg = PlotterAggregator::with_config(config);
        agg.set_enabled(true);

        // Add 50 points: each lands in its own 1 ms cell of tier 0
        // count-1 bucket (no averaging below the tier_span cap)
        for i in 0..50 {
            agg.add_data_point("ch0", i * 100, ChannelValue::Numeric(i as f64));
        }

        assert_eq!(agg.total_points(), 50, "every sample is kept below the cap");
    }

    /// Memory stays bounded however long the session runs: tier k >= 1 holds
    /// about A / 2 cells, tier 0 at most A (ADR-16), and no sample is lost
    #[test]
    fn test_tier_memory_is_bounded() {
        let span: u64 = 128;
        let agg = PlotterAggregator::with_config(PlotterConfig {
            tier0_span_ms: span,
            ..Default::default()
        });
        agg.set_enabled(true);

        let n = 200_000u64; // 2000 s at 100 Hz
        for i in 0..n {
            agg.add_data_point("ch0", i * 10, ChannelValue::Numeric((i % 97) as f64));
        }

        let inner = agg.inner.read().unwrap();
        let pyramid = inner.pyramids.get("ch0").unwrap();
        assert!(pyramid.tiers.len() > 10, "the stream must reach deep tiers");
        assert!(pyramid.tiers[0].len() as u64 <= span + 1);
        for (k, tier) in pyramid.tiers.iter().enumerate().skip(1) {
            assert!(
                tier.len() as u64 <= span / 2 + 2,
                "tier {k} holds {} cells",
                tier.len()
            );
        }
        assert_eq!(pyramid.sample_count(), n as usize, "samples lost");
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
    fn test_average_minmax_aggregation() {
        let config = PlotterConfig {
            tier0_span_ms: 100,
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
            tier0_span_ms: 50, // Small to trigger frequent re-aggregation
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
            tier0_span_ms: 50,
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
        let (avg_min, avg_max) =
            data_avg
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |acc, p| match p {
                    AggregatedPoint::Single { value, .. } => (acc.0.min(*value), acc.1.max(*value)),
                    AggregatedPoint::MinMax { min, max, .. } => (acc.0.min(*min), acc.1.max(*max)),
                });

        // Switch to Peak mode
        agg.set_aggregation_mode(AggregationMode::Peak);

        // Get data as Peak mode
        let payload_peak = agg.get_ranged_data(&req);
        let data_peak = payload_peak.line_data.get("ch0").unwrap();
        assert!(
            !data_peak.is_empty(),
            "Should have data after switching to Peak"
        );

        // Find peaks in Peak mode
        let (peak_min, peak_max) =
            data_peak
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |acc, p| match p {
                    AggregatedPoint::Single { value, .. } => (acc.0.min(*value), acc.1.max(*value)),
                    AggregatedPoint::MinMax { min, max, .. } => (acc.0.min(*min), acc.1.max(*max)),
                });

        // Peak draws real extreme samples, so it shows exactly the extremes
        // that the Average mode's band shows
        assert_eq!(peak_max, avg_max, "max changed across the mode switch");
        assert_eq!(peak_min, avg_min, "min changed across the mode switch");

        // Switch back to Average
        agg.set_aggregation_mode(AggregationMode::Average);

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
            tier0_span_ms: 100,
            aggregation_mode: AggregationMode::Peak,
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

        // Get data in Peak mode
        let req = PlotterDataRequest {
            time_min_ms: None,
            time_max_ms: None,
            pixel_width: 100,
            is_realtime: false,
        };
        let payload_peak = agg.get_ranged_data(&req);
        let data_peak = payload_peak.line_data.get("ch0").unwrap();

        // Peak mode should return Single points
        let peak_has_single = data_peak
            .iter()
            .any(|p| matches!(p, AggregatedPoint::Single { .. }));
        assert!(peak_has_single, "Peak mode should return Single points");

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

        // Switch back to Peak - should also work without rebuild
        agg.set_aggregation_mode(AggregationMode::Peak);
        let payload_peak2 = agg.get_ranged_data(&req);
        let data_peak2 = payload_peak2.line_data.get("ch0").unwrap();
        let peak2_has_single = data_peak2
            .iter()
            .any(|p| matches!(p, AggregatedPoint::Single { .. }));
        assert!(
            peak2_has_single,
            "Peak mode should return Single points after switching back"
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
            tier0_span_ms: 1000,
            aggregation_mode: AggregationMode::Peak,
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
    fn test_tier_folding_preserves_data_range() {
        // Regression test: folding cells up the tiers preserves the full time range
        let config = PlotterConfig {
            tier0_span_ms: 50, // small: most data is folded into coarser tiers
            aggregation_mode: AggregationMode::Average,
            ..Default::default()
        };
        let agg = PlotterAggregator::with_config(config);
        agg.set_enabled(true);

        // Add enough data to fold into several tiers
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

    /// P-4 (docs/24 3.3) - count conservation, worked example.
    ///
    /// Every raw point stays represented exactly once, in the `count` of one
    /// stored cell. This must survive folding cells up the tiers, and so must
    /// the min/max envelope, bit for bit.
    #[test]
    fn test_count_conservation_across_tier_folding() {
        const N: usize = 2000;

        // Baseline: with a tier-0 span longer than the stream nothing is folded
        let flat = PlotterAggregator::with_config(PlotterConfig {
            tier0_span_ms: 1 << 20,
            ..Default::default()
        });
        flat.set_enabled(true);
        for i in 0..N {
            flat.add_data_point("ch0", i as u64 * 10, ChannelValue::Numeric(i as f64));
            flat.add_data_point("ch1", i as u64 * 10, ChannelValue::Numeric(-(i as f64)));
        }
        assert_eq!(tier_depth(&flat), 1, "nothing should be folded");
        assert_eq!(represented_count(&flat, "ch0"), N);
        assert_eq!(represented_count(&flat, "ch1"), N);
        assert_eq!(stored_extremes(&flat, "ch0"), (0.0, (N - 1) as f64));
        assert_eq!(stored_extremes(&flat, "ch1"), (-((N - 1) as f64), 0.0));

        // Same stream through a tiny tier-0 span, folding into several tiers
        let levelled = PlotterAggregator::with_config(PlotterConfig {
            tier0_span_ms: 20,
            aggregation_mode: AggregationMode::Average,
            ..Default::default()
        });
        levelled.set_enabled(true);
        for i in 0..N {
            levelled.add_data_point("ch0", i as u64 * 10, ChannelValue::Numeric(i as f64));
            levelled.add_data_point("ch1", i as u64 * 10, ChannelValue::Numeric(-(i as f64)));
        }

        assert!(
            tier_depth(&levelled) >= 4,
            "expected several tiers, got {}",
            tier_depth(&levelled)
        );
        assert_eq!(
            represented_count(&levelled, "ch0"),
            N,
            "folding must not lose or duplicate raw points"
        );
        assert_eq!(represented_count(&levelled, "ch1"), N);

        // Extremes are stored exactly (min of mins / max of maxs), so no
        // epsilon is allowed here.
        assert_eq!(stored_extremes(&levelled, "ch0"), (0.0, (N - 1) as f64));
        assert_eq!(stored_extremes(&levelled, "ch1"), (-((N - 1) as f64), 0.0));
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
            tier0_span_ms: 10, // Small to trigger aggregation
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
        if let Some(band_data) = payload.band_data.as_ref() {
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

    // ==================== Peak envelope tests (SYS-F-515 / UN-03) ====================

    fn sample_buckets(values: &[(u64, f64)]) -> Vec<AggregatedBucket> {
        values
            .iter()
            .map(|&(ts, v)| AggregatedBucket::sample(ts, v))
            .collect()
    }

    fn single_values(points: &[AggregatedPoint]) -> Vec<(u64, f64)> {
        points
            .iter()
            .map(|p| match p {
                AggregatedPoint::Single { ts, value } => (*ts, *value),
                AggregatedPoint::MinMax { .. } => panic!("Peak mode emits Single points only"),
            })
            .collect()
    }

    /// A single-sample spike survives any amount of downsampling at its real
    /// value (the mode it replaced drew the bucket average: 400 became about 41)
    #[test]
    fn test_peak_envelope_keeps_single_sample_spike() {
        let mut data: Vec<(u64, f64)> = (0..1000).map(|i| (i * 10, 1.2)).collect();
        data[437].1 = 400.0;
        let points = single_values(&peak_envelope(&sample_buckets(&data), 1000, 0, 9990));
        assert!(points.contains(&(4370, 400.0)), "{points:?}");
        assert!(
            points.len() <= 20,
            "two points per 1 s cell at most: {}",
            points.len()
        );
    }

    /// Each cell emits its min and max in the order they occurred, at their real times
    #[test]
    fn test_peak_envelope_orders_extremes_by_time() {
        // cell 0: max first, then min; cell 1: min first, then max
        let data = [
            (0, 0.0),
            (10, 9.0),
            (20, -3.0),
            (100, 5.0),
            (110, -1.0),
            (120, 7.0),
        ];
        let points = single_values(&peak_envelope(&sample_buckets(&data), 100, 0, 200));
        assert_eq!(points, vec![(10, 9.0), (20, -3.0), (110, -1.0), (120, 7.0)]);
    }

    /// Sparse data (fewer samples than cells) is drawn sample for sample
    #[test]
    fn test_peak_envelope_is_lossless_for_sparse_data() {
        let data = [(0, 1.0), (1000, 3.0), (2000, 2.0)];
        let points = single_values(&peak_envelope(&sample_buckets(&data), 100, 0, 2000));
        assert_eq!(points, data.to_vec());
    }

    /// Samples outside the requested window are not emitted
    #[test]
    fn test_peak_envelope_respects_window() {
        let data = [(50, 100.0), (150, 1.0), (250, -100.0)];
        let points = single_values(&peak_envelope(&sample_buckets(&data), 10, 100, 200));
        assert_eq!(points, vec![(150, 1.0)]);
    }

    /// A merged bucket whose extremes straddle a cell boundary still yields
    /// points in time order (each extreme goes to the cell of its own time)
    #[test]
    fn test_peak_envelope_uses_extreme_times_for_cells() {
        let mut straddling = AggregatedBucket::sample(95, 0.0);
        straddling.merge_extremes(&AggregatedBucket::sample(105, 9.0));
        let data = vec![straddling, AggregatedBucket::sample(103, -2.0)];
        let points = single_values(&peak_envelope(&data, 100, 0, 200));
        let times: Vec<u64> = points.iter().map(|p| p.0).collect();
        assert!(times.windows(2).all(|w| w[0] < w[1]), "{points:?}");
        assert!(points.contains(&(105, 9.0)) && points.contains(&(103, -2.0)));
    }

    /// Same-millisecond min and max cannot both be drawn: the spike is kept
    #[test]
    fn test_peak_envelope_same_millisecond_keeps_the_spike() {
        let data = [(0, 1.0), (1, 1.0), (5, 50.0), (5, 0.9), (9, 1.0)];
        let points = single_values(&peak_envelope(&sample_buckets(&data), 10, 0, 9));
        // min 0.9 and max 50.0 are both at 5 ms; 50.0 is farther from the average
        assert_eq!(points, vec![(5, 50.0)]);
    }

    /// The merge of extremes does not depend on grouping (associative tie rule)
    #[test]
    fn test_merge_extremes_is_grouping_independent() {
        let samples = [(5, 2.0), (1, 7.0), (3, 2.0), (4, 7.0), (2, -1.0), (6, -1.0)];
        let fold = |order: &[usize]| {
            let mut acc = AggregatedBucket::sample(samples[order[0]].0, samples[order[0]].1);
            for &i in &order[1..] {
                acc.merge_extremes(&AggregatedBucket::sample(samples[i].0, samples[i].1));
            }
            (acc.min, acc.ts_min, acc.max, acc.ts_max)
        };
        let expected = (-1.0, 2, 7.0, 1);
        assert_eq!(fold(&[0, 1, 2, 3, 4, 5]), expected);
        assert_eq!(fold(&[5, 4, 3, 2, 1, 0]), expected);
        assert_eq!(fold(&[3, 0, 5, 1, 4, 2]), expected);
    }

    #[test]
    fn test_peak_mode_returns_real_extremes() {
        // Peak mode returns only Single points, and they are real samples:
        // the triangle wave's peaks (100) and troughs (0) survive downsampling
        let config = PlotterConfig {
            tier0_span_ms: 100,
            aggregation_mode: AggregationMode::Peak,
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

        // Peak mode should return Single points
        assert!(
            data.iter()
                .all(|p| matches!(p, AggregatedPoint::Single { .. })),
            "Peak mode should only return Single points"
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

        assert_eq!(max_value, 100.0, "peak averaged away");
        assert_eq!(min_value, 0.0, "trough averaged away");
    }

    #[test]
    fn test_version_increments_on_data_add() {
        let agg = PlotterAggregator::new();
        agg.set_enabled(true);

        let v1 = agg.check_version();
        assert_eq!(v1.version, 0);
        assert!(!v1.has_data);

        agg.add_data_point("ch0", 1000, ChannelValue::Numeric(10.0));

        let v2 = agg.check_version();
        assert!(
            v2.version > v1.version,
            "Version should increment on data add"
        );
        assert!(v2.has_data, "has_data should be true after adding data");
    }

    #[test]
    fn test_version_increments_on_clear() {
        let agg = PlotterAggregator::new();
        agg.set_enabled(true);

        agg.add_data_point("ch0", 1000, ChannelValue::Numeric(10.0));
        let v1 = agg.check_version();
        assert!(v1.has_data);

        agg.clear();
        let v2 = agg.check_version();

        assert!(v2.version > v1.version, "Version should increment on clear");
        assert!(!v2.has_data, "has_data should be false after clear");
    }

    #[test]
    fn test_check_version_is_lightweight() {
        use std::time::Instant;

        let agg = PlotterAggregator::new();
        agg.set_enabled(true);

        // Add 10K points
        for i in 0..10_000 {
            agg.add_data_point("ch0", i, ChannelValue::Numeric(i as f64));
        }

        // check_version should be O(1) - verify by timing many calls
        let start = Instant::now();
        for _ in 0..10_000 {
            let _ = agg.check_version();
        }
        let elapsed = start.elapsed();

        // 10K calls should complete in well under 100ms (generous bound)
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "check_version too slow: {:?}",
            elapsed
        );
    }

    // ==================== Realtime aligned-grid tests ====================

    /// Extract (ts, min, max, avg) from either point variant.
    fn point_fields(point: &AggregatedPoint) -> (u64, f64, f64, f64) {
        match point {
            AggregatedPoint::Single { ts, value } => (*ts, *value, *value, *value),
            AggregatedPoint::MinMax { ts, min, max, avg } => (*ts, *min, *max, *avg),
        }
    }

    /// Map of ts -> (min, max, avg) for every point except the first and last.
    ///
    /// The first and last cells of a window are the only ones that can be
    /// clipped by the window boundary, so they are excluded when comparing
    /// two overlapping windows.
    fn interior_cells(points: &[AggregatedPoint]) -> BTreeMap<u64, (f64, f64, f64)> {
        if points.len() <= 2 {
            return BTreeMap::new();
        }
        points[1..points.len() - 1]
            .iter()
            .map(|p| {
                let (ts, min, max, avg) = point_fields(p);
                (ts, (min, max, avg))
            })
            .collect()
    }

    #[test]
    fn test_quantize_bucket_width_pow2() {
        let cases: &[(u64, u64)] = &[
            (0, 1),
            (1, 1),
            (2, 2),
            (3, 4),
            (4, 4),
            (5, 8),
            (100, 128),
            (128, 128),
            (129, 256),
            (123_456, 131_072),
        ];
        for &(input, expected) in cases {
            assert_eq!(
                quantize_bucket_width_pow2(input),
                expected,
                "quantize_bucket_width_pow2({input})"
            );
        }
    }

    #[test]
    fn test_aligned_buckets_are_stable_across_sliding_windows() {
        // THE key property: in realtime mode, a cell that is fully inside two
        // overlapping sliding windows must be byte-for-byte identical in both.
        let config = PlotterConfig {
            aggregation_mode: AggregationMode::Average,
            ..Default::default()
        };
        let agg = PlotterAggregator::with_config(config);
        agg.set_enabled(true);

        const T0: u64 = 1_000_000;
        // ~2000 points at one point / 7 ms
        for i in 0..2000u64 {
            let ts = T0 + i * 7;
            let value = (i as f64 * 0.13).sin() * 50.0 + (i as f64) * 0.01;
            agg.add_data_point("ch0", ts, ChannelValue::Numeric(value));
        }

        // Window A and window B: same span (10 s), B slid by 143 ms.
        let req_a = PlotterDataRequest {
            time_min_ms: Some(T0),
            time_max_ms: Some(T0 + 10_000),
            pixel_width: 100,
            is_realtime: true,
        };
        let req_b = PlotterDataRequest {
            time_min_ms: Some(T0 + 143),
            time_max_ms: Some(T0 + 10_143),
            pixel_width: 100,
            is_realtime: true,
        };

        let payload_a = agg.get_ranged_data(&req_a);
        let payload_b = agg.get_ranged_data(&req_b);

        assert!(
            payload_a.is_aggregated,
            "window A should have triggered aggregation"
        );
        assert!(
            payload_b.is_aggregated,
            "window B should have triggered aggregation"
        );

        let points_a = payload_a.line_data.get("ch0").unwrap();
        let points_b = payload_b.line_data.get("ch0").unwrap();
        assert!(points_a.len() > 2, "A should have several cells");
        assert!(points_b.len() > 2, "B should have several cells");

        let cells_a = interior_cells(points_a);
        let cells_b = interior_cells(points_b);

        let mut shared = 0usize;
        for (ts, stats_a) in &cells_a {
            if let Some(stats_b) = cells_b.get(ts) {
                shared += 1;
                assert_eq!(
                    stats_a, stats_b,
                    "cell at ts={} changed between sliding windows (A={:?}, B={:?})",
                    ts, stats_a, stats_b
                );
            }
        }

        assert!(
            shared > 50,
            "expected substantial overlap between the two windows, got {} shared cells",
            shared
        );
    }

    #[test]
    fn test_aligned_cells_on_absolute_grid() {
        // Average mode draws one point per cell at the cell centre. Peak mode
        // draws real sample times instead (test_peak_realtime_*).
        let agg = PlotterAggregator::with_config(PlotterConfig {
            aggregation_mode: AggregationMode::Average,
            ..Default::default()
        });
        agg.set_enabled(true);

        const T0: u64 = 1_000_000;
        for i in 0..2000u64 {
            let ts = T0 + i * 7;
            agg.add_data_point("ch0", ts, ChannelValue::Numeric(i as f64));
        }

        let pixel_width: u32 = 100;
        let span: u64 = 10_000;
        let req = PlotterDataRequest {
            time_min_ms: Some(T0),
            time_max_ms: Some(T0 + span),
            pixel_width,
            is_realtime: true,
        };

        let payload = agg.get_ranged_data(&req);
        assert!(payload.is_aggregated, "should have been aggregated");

        // Replicate the width computation of the Average path (one point per
        // cell, cells = target - 1)
        let cells = pixel_width as u64 - 1;
        let width = quantize_bucket_width_pow2(span.div_ceil(cells));
        assert_eq!(width, 128, "expected a 128 ms quantized width");

        let points = payload.line_data.get("ch0").unwrap();
        assert!(!points.is_empty(), "should have points");

        for point in points {
            let (ts, _, _, _) = point_fields(point);
            assert!(ts >= width / 2, "ts {} is below the first cell center", ts);
            assert_eq!(
                (ts - width / 2) % width,
                0,
                "cell center {} is not on the absolute {} ms grid",
                ts,
                width
            );
        }
    }

    /// Peak mode, realtime: a single-sample spike in a long, heavily
    /// downsampled window is drawn at its real value and time; the output stays
    /// within the pixel budget and in time order (PF-1, docs/27)
    #[test]
    fn test_peak_realtime_keeps_spike_through_tier_folding() {
        let agg = PlotterAggregator::with_config(PlotterConfig {
            tier0_span_ms: 1000, // fold into many tiers
            ..Default::default()
        });
        agg.set_enabled(true);

        const T0: u64 = 1_000_000;
        for i in 0..30_000u64 {
            let value = if i == 12_345 { 400.0 } else { 1.2 };
            agg.add_data_point("ch0", T0 + i * 10, ChannelValue::Numeric(value));
        }
        assert!(
            tier_depth(&agg) > 1,
            "the scenario must exercise tier folding"
        );

        let pixel_width = 800;
        let payload = agg.get_ranged_data(&PlotterDataRequest {
            time_min_ms: Some(T0),
            time_max_ms: Some(T0 + 300_000),
            pixel_width,
            is_realtime: true,
        });
        let points = single_values(payload.line_data.get("ch0").unwrap());
        assert!(points.contains(&(T0 + 123_450, 400.0)), "spike lost");
        assert!(
            points.len() <= pixel_width as usize,
            "{} points",
            points.len()
        );
        assert!(
            points.windows(2).all(|w| w[0].0 < w[1].0),
            "not in time order"
        );
    }

    // ==================== Full-resolution zoom (SYS-F-518) ====================

    fn spill_aggregator(tier_span: u64) -> PlotterAggregator {
        let agg = PlotterAggregator::with_config(PlotterConfig {
            tier0_span_ms: tier_span,
            spill_to_disk: true,
            ..Default::default()
        });
        agg.set_enabled(true);
        agg
    }

    fn spill_of(agg: &PlotterAggregator) -> Arc<SpillLog> {
        agg.inner
            .read()
            .unwrap()
            .spill
            .clone()
            .expect("spill enabled")
    }

    fn wave(i: u64) -> f64 {
        ((i * 7919) % 1000) as f64 / 10.0
    }

    /// After an hour at 100 Hz, zooming into a range 50 minutes back shows
    /// every sample - memory alone only has ~1 s cells there
    #[test]
    fn test_static_zoom_is_full_resolution_after_an_hour() {
        let n = 360_000u64; // 1 h at 100 Hz
        let feed = |agg: &PlotterAggregator| {
            for i in 0..n {
                agg.add_data_point("ch0", i * 10, ChannelValue::Numeric(wave(i)));
            }
        };
        let req = PlotterDataRequest {
            time_min_ms: Some(600_000), // 10 min into the session
            time_max_ms: Some(600_400),
            pixel_width: 1000,
            is_realtime: false,
        };
        let expected: Vec<(u64, f64)> = (60_000..=60_040u64).map(|i| (i * 10, wave(i))).collect();

        let with_spill = spill_aggregator(PlotterConfig::default().tier0_span_ms);
        feed(&with_spill);
        let got = single_values(
            with_spill
                .get_ranged_data(&req)
                .line_data
                .get("ch0")
                .unwrap(),
        );
        assert_eq!(
            got, expected,
            "every sample of the zoomed range must be drawn"
        );

        // Without the spill, memory only keeps coarse cells that old
        let memory_only = aggregator_with(
            AggregationMode::Peak,
            PlotterConfig::default().tier0_span_ms,
        );
        feed(&memory_only);
        let coarse = single_values(
            memory_only
                .get_ranged_data(&req)
                .line_data
                .get("ch0")
                .unwrap(),
        );
        assert!(
            coarse.len() < expected.len() / 4,
            "memory alone was already fine: {}",
            coarse.len()
        );
    }

    /// State changes beyond the in-memory cap are not lost: zooming into the
    /// start of a long session shows them from the spill (SYS-F-712 / 518)
    #[test]
    fn test_state_changes_beyond_memory_cap_come_back_from_spill() {
        let feed = |agg: &PlotterAggregator| {
            for i in 0..25_000u64 {
                let state = if i % 2 == 0 { "ON" } else { "OFF" };
                agg.add_data_point("valve", i * 10, ChannelValue::State(state.to_string()));
            }
        };
        let req = PlotterDataRequest {
            time_min_ms: Some(0),
            time_max_ms: Some(95),
            pixel_width: 800,
            is_realtime: false,
        };

        let with_spill = spill_aggregator(PlotterConfig::default().tier0_span_ms);
        feed(&with_spill);
        let states = with_spill
            .get_ranged_data(&req)
            .state_data
            .remove("valve")
            .unwrap();
        let got: Vec<(u64, Option<u64>, String)> = states
            .into_iter()
            .map(|s| (s.start_ms, s.end_ms, s.state))
            .collect();
        let expected: Vec<(u64, Option<u64>, String)> = (0..10u64)
            .map(|i| {
                let state = if i % 2 == 0 { "ON" } else { "OFF" };
                (i * 10, Some(i * 10 + 10), state.to_string())
            })
            .collect();
        assert_eq!(got, expected);

        // LIVE requests stay in memory
        let live = PlotterDataRequest {
            is_realtime: true,
            ..req
        };
        with_spill.get_ranged_data(&live);

        let memory_only = aggregator_with(
            AggregationMode::Peak,
            PlotterConfig::default().tier0_span_ms,
        );
        feed(&memory_only);
        let lost = memory_only
            .get_ranged_data(&req)
            .state_data
            .remove("valve")
            .unwrap();
        assert!(
            lost.is_empty(),
            "memory alone keeps only the last 10,000 changes"
        );
    }

    /// LIVE windows are drawn from memory only (ADR-16); the disk is never read
    #[test]
    fn test_live_window_never_reads_the_spill() {
        let agg = spill_aggregator(PlotterConfig::default().tier0_span_ms);
        for i in 0..60_000u64 {
            agg.add_data_point("ch0", i * 10, ChannelValue::Numeric(wave(i)));
        }
        let now = 599_990;
        for window_s in [1u64, 10, 300] {
            for pixel_width in [800u32, 4000] {
                agg.get_ranged_data(&PlotterDataRequest {
                    time_min_ms: Some(now - window_s * 1000),
                    time_max_ms: Some(now),
                    pixel_width,
                    is_realtime: true,
                });
            }
        }
        assert_eq!(spill_of(&agg).reads(), 0);
    }

    /// Clear starts a new spill generation; the old directory is deleted once
    /// nobody holds it, and the new one works
    #[test]
    fn test_clear_switches_spill_generation() {
        let agg = spill_aggregator(16);
        for i in 0..2_000u64 {
            agg.add_data_point("ch0", i * 10, ChannelValue::Numeric(wave(i)));
        }
        let old = spill_of(&agg);
        assert!(old.sync());
        let old_dir = old.dir().to_path_buf();
        drop(old);
        agg.clear();
        assert!(!old_dir.exists(), "old generation left behind");
        let new = spill_of(&agg);
        assert_ne!(new.dir(), old_dir.as_path());
    }

    /// Slow data below the storage cap is drawn sample for sample, and an
    /// already-drawn point never changes when later samples arrive (the old
    /// fixed 10-sample grouping averaged 1 Hz data into 10 s and rewrote it)
    #[test]
    fn test_slow_data_is_not_averaged_and_does_not_change() {
        let agg = PlotterAggregator::new();
        agg.set_enabled(true);
        let req = PlotterDataRequest {
            time_min_ms: Some(0),
            time_max_ms: Some(60_000),
            pixel_width: 1000,
            is_realtime: true,
        };

        let mut drawn: Vec<Vec<(u64, f64)>> = Vec::new();
        for i in 0..30u64 {
            agg.add_data_point(
                "ch0",
                i * 1000,
                ChannelValue::Numeric((i as f64 * 0.7).sin()),
            );
            drawn.push(single_values(
                agg.get_ranged_data(&req).line_data.get("ch0").unwrap(),
            ));
        }
        let expected: Vec<(u64, f64)> = (0..30u64)
            .map(|i| (i * 1000, (i as f64 * 0.7).sin()))
            .collect();
        assert_eq!(drawn.last().unwrap(), &expected);
        for (n, frame) in drawn.iter().enumerate() {
            assert_eq!(
                frame.as_slice(),
                &expected[..=n],
                "frame {n} rewrote history"
            );
        }
    }

    #[test]
    fn test_realtime_raw_points_not_lost() {
        // Below the aggregation threshold, realtime must return every raw point
        // individually (no count-chunking, no downsampling).
        let agg = PlotterAggregator::new();
        agg.set_enabled(true);

        let raw: Vec<(u64, f64)> = vec![
            (1000, 10.0),
            (1500, 20.0),
            (2000, 30.0),
            (2500, 40.0),
            (3000, 50.0),
        ];
        for &(ts, v) in &raw {
            agg.add_data_point("ch0", ts, ChannelValue::Numeric(v));
        }

        let req = PlotterDataRequest {
            time_min_ms: Some(1000),
            time_max_ms: Some(3000),
            pixel_width: 1000, // way above the point count -> no aggregation
            is_realtime: true,
        };

        let payload = agg.get_ranged_data(&req);
        assert!(
            !payload.is_aggregated,
            "should not aggregate below the threshold"
        );

        let points = payload.line_data.get("ch0").unwrap();
        assert_eq!(points.len(), raw.len(), "every raw point should survive");

        for (point, &(ts, v)) in points.iter().zip(raw.iter()) {
            match point {
                AggregatedPoint::Single { ts: p_ts, value } => {
                    assert_eq!(*p_ts, ts, "timestamp should be preserved");
                    assert_eq!(*value, v, "value should be preserved");
                }
                other => panic!("expected Single point, got {:?}", other),
            }
        }
    }

    // ================================================================
    // Property-based tests (proptest)
    // ================================================================

    use proptest::prelude::*;

    /// Deterministic 64-bit mixer (splitmix64 finalizer).
    ///
    /// The generated streams are derived from a handful of scalar seeds rather
    /// than from thousands of individually generated f64s. That keeps proptest
    /// generation AND shrinking cheap (shrinking a 9000-element float vector is
    /// pathologically slow) while still covering the input space: shrinking
    /// reduces point count / channel count / start time, which is what actually
    /// helps when minimizing a counterexample.
    fn mix64(x: u64) -> u64 {
        let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Description of a generated input stream shared by the aggregator
    /// properties: monotonically NON-decreasing timestamps (increments
    /// 0..=50 ms) starting at 0..10_000, 1-3 channels, 100..=3000 points, and
    /// values mixing a smooth sine baseline with occasional +/-1e3 spikes.
    #[derive(Debug, Clone)]
    struct StreamSpec {
        start_ms: u64,
        n: usize,
        channels: usize,
        ts_seed: u64,
        val_seed: u64,
    }

    impl StreamSpec {
        fn channel_name(ch: usize) -> String {
            format!("ch{}", ch)
        }

        fn timestamps(&self) -> Vec<u64> {
            let mut out = Vec::with_capacity(self.n);
            let mut t = self.start_ms;
            for i in 0..self.n {
                if i > 0 {
                    t += mix64(self.ts_seed ^ (i as u64)) % 51;
                }
                out.push(t);
            }
            out
        }

        fn value(&self, i: usize, ch: usize) -> f64 {
            let t = i as f64 * 0.07 + ch as f64 * 1.3;
            let base = t.sin() * 50.0 + (t * 0.31).cos() * 20.0;
            let h = mix64(self.val_seed ^ ((i as u64) << 3) ^ (ch as u64 + 1));
            if h.is_multiple_of(41) {
                if h & 0x100 == 0 {
                    base + 1.0e3
                } else {
                    base - 1.0e3
                }
            } else {
                base
            }
        }

        /// Feed the whole stream through `add_data_point`, returning every
        /// value that was fed, per channel.
        fn feed(&self, agg: &PlotterAggregator) -> Vec<Vec<f64>> {
            let ts = self.timestamps();
            let mut fed: Vec<Vec<f64>> = vec![Vec::with_capacity(self.n); self.channels];
            for (i, &t) in ts.iter().enumerate() {
                for (ch, values) in fed.iter_mut().enumerate() {
                    let v = self.value(i, ch);
                    values.push(v);
                    agg.add_data_point(&Self::channel_name(ch), t, ChannelValue::Numeric(v));
                }
            }
            fed
        }

        /// The very same stream `feed` sends point-by-point, packaged as one
        /// batch of `ParsedDataPoint`s (one point per timestamp, every channel
        /// listed in `channel_order`).
        fn as_batch(&self) -> Vec<crate::plotter::parser::ParsedDataPoint> {
            self.timestamps()
                .into_iter()
                .enumerate()
                .map(|(i, t)| {
                    let mut channels = HashMap::new();
                    let mut channel_order = Vec::with_capacity(self.channels);
                    for ch in 0..self.channels {
                        let name = Self::channel_name(ch);
                        channels.insert(name.clone(), ChannelValue::Numeric(self.value(i, ch)));
                        channel_order.push(name);
                    }
                    crate::plotter::parser::ParsedDataPoint {
                        timestamp_ms: t,
                        channels,
                        channel_order,
                    }
                })
                .collect()
        }
    }

    fn stream_spec() -> impl Strategy<Value = StreamSpec> {
        (
            0u64..10_000,
            100usize..=3000,
            1usize..=3,
            any::<u64>(),
            any::<u64>(),
        )
            .prop_map(|(start_ms, n, channels, ts_seed, val_seed)| StreamSpec {
                start_ms,
                n,
                channels,
                ts_seed,
                val_seed,
            })
    }

    fn aggregator_with(mode: AggregationMode, tier_span: u64) -> PlotterAggregator {
        let agg = PlotterAggregator::with_config(PlotterConfig {
            aggregation_mode: mode,
            tier0_span_ms: tier_span,
            ..Default::default()
        });
        agg.set_enabled(true);
        agg
    }

    fn average_aggregator() -> PlotterAggregator {
        aggregator_with(
            AggregationMode::Average,
            PlotterConfig::default().tier0_span_ms,
        )
    }

    // ---- internal-state probes (P-4 / P-7) ----------------------------
    //
    // `count` is deliberately not exposed on `AggregatedPoint`, and the tiers
    // are an implementation detail, so these properties read the private
    // inner state directly (docs/24 3.3, P-4).

    /// Number of RAW points a channel currently represents (the `count` of
    /// every stored cell in every tier)
    fn represented_count(agg: &PlotterAggregator, channel: &str) -> usize {
        let inner = agg.inner.read().expect("inner lock");
        inner
            .pyramids
            .get(channel)
            .map(Pyramid::sample_count)
            .unwrap_or(0)
    }

    /// Exact (min, max) over everything a channel stores
    fn stored_extremes(agg: &PlotterAggregator, channel: &str) -> (f64, f64) {
        let inner = agg.inner.read().expect("inner lock");
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        if let Some(pyramid) = inner.pyramids.get(channel) {
            for cell in pyramid.tiers.iter().flatten() {
                min = min.min(cell.min);
                max = max.max(cell.max);
            }
        }
        (min, max)
    }

    /// Number of tiers in use by the deepest channel (1 = everything is still
    /// in tier 0, i.e. no cell has been folded yet)
    fn tier_depth(agg: &PlotterAggregator) -> usize {
        let inner = agg.inner.read().expect("inner lock");
        inner
            .pyramids
            .values()
            .map(|p| {
                p.tiers
                    .iter()
                    .rposition(|t| !t.is_empty())
                    .map_or(0, |k| k + 1)
            })
            .max()
            .unwrap_or(0)
    }

    /// Comparable projection of `band_data` (`BandSeriesData` has no
    /// `PartialEq`, and `HashMap` iteration order is not stable).
    #[allow(clippy::type_complexity)]
    fn band_shape(
        payload: &PlotterChartPayload,
    ) -> Option<BTreeMap<String, (Vec<Option<f64>>, Vec<Option<f64>>)>> {
        payload.band_data.as_ref().map(|bands| {
            bands
                .iter()
                .map(|(name, band)| (name.clone(), (band.min.clone(), band.max.clone())))
                .collect()
        })
    }

    /// Tier-0 age span in ms. Small spans push most of a stream into the
    /// coarser tiers - the path where an extreme is most likely to be lost -
    /// while the default keeps a short stream in tier 0.
    fn tier_span_strategy() -> impl Strategy<Value = u64> {
        prop_oneof![
            2 => Just(16u64),
            2 => Just(128u64),
            1 => Just(1_024u64),
            1 => Just(4_096u64),
        ]
    }

    /// Chart width in pixels, weighted toward narrow charts.
    ///
    /// Display aggregation only kicks in when `bucket_count > pixel_width *
    /// threshold`, so a uniform 50..=2000 draw would almost always land on the
    /// pass-through branch and leave the interesting code untested.
    fn pixel_width_strategy() -> impl Strategy<Value = u32> {
        prop_oneof![
            3 => 50u32..=150,
            1 => 150u32..=2_000,
        ]
    }

    /// Ops for the version-monotonicity property.
    #[derive(Debug, Clone)]
    enum AggOp {
        AddPoint {
            channel: usize,
            ts: u64,
            value: f64,
        },
        AddBatch {
            channel: usize,
            base_ts: u64,
            count: usize,
        },
        Clear,
        SetAverageMode(bool),
        GetChart {
            pixel_width: u32,
        },
    }

    fn agg_op() -> impl Strategy<Value = AggOp> {
        prop_oneof![
            6 => (0usize..3, 0u64..100_000, -1.0e3f64..1.0e3).prop_map(
                |(channel, ts, value)| AggOp::AddPoint { channel, ts, value }),
            3 => (0usize..3, 0u64..100_000, 1usize..40).prop_map(
                |(channel, base_ts, count)| AggOp::AddBatch { channel, base_ts, count }),
            1 => Just(AggOp::Clear),
            2 => any::<bool>().prop_map(AggOp::SetAverageMode),
            2 => (50u32..2000).prop_map(|pixel_width| AggOp::GetChart { pixel_width }),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

        /// P-16 (SYS-F-518) - a zoomed static range is drawn at full resolution
        /// whatever the age of the data: equal to the envelope of the raw samples
        /// in the range (the display cells cut by either range edge excepted:
        /// a stored cell straddling an edge keeps only its extremes). Small tier
        /// spans push nearly everything into coarse memory tiers, so the detail
        /// must come from the disk spill.
        #[test]
        fn prop_static_zoom_is_drawn_from_raw_samples(
            spec in stream_spec(),
            tier_span in prop::sample::select(vec![16u64, 64, 256]),
            from_pct in 0u64..90,
            len_pct in 1u64..=10,
            pixel_width in 50u32..=2000,
        ) {
            let agg = spill_aggregator(tier_span);
            let fed = spec.feed(&agg);
            let ts = spec.timestamps();
            let (first, last) = (ts[0], *ts.last().unwrap());
            let total = last - first;
            let t0 = first + total * from_pct / 100;
            let t1 = (t0 + (total * len_pct / 100).max(1)).min(last);
            let payload = agg.get_ranged_data(&PlotterDataRequest {
                time_min_ms: Some(t0),
                time_max_ms: Some(t1),
                pixel_width,
                is_realtime: false,
            });

            let target = pixel_width.min(PlotterConfig::default().max_target_points) as u64;
            let cells = (target / 2).saturating_sub(1).max(1);
            let width = quantize_bucket_width_pow2((t1 - t0).div_ceil(cells));
            let edge = |t: u64| t / width == t0 / width || t / width == t1 / width;

            for (ch, values) in fed.iter().enumerate() {
                let name = StreamSpec::channel_name(ch);
                let raw: Vec<AggregatedBucket> = ts
                    .iter()
                    .zip(values)
                    .filter(|(&t, _)| t >= t0 && t <= t1)
                    .map(|(&t, &v)| AggregatedBucket::sample(t, v))
                    .collect();
                let expected: Vec<(u64, f64)> = single_values(&peak_envelope(&raw, width, t0, t1))
                    .into_iter()
                    .filter(|p| !edge(p.0))
                    .collect();
                let got: Vec<(u64, f64)> = single_values(payload.line_data.get(&name).unwrap())
                    .into_iter()
                    .filter(|p| !edge(p.0))
                    .collect();
                prop_assert_eq!(got, expected, "{} [{}, {}] px={} span={}", name, t0, t1, pixel_width, tier_span);
            }
        }

        /// A1b - Peak mode never averages and never loses an extreme (UN-03 / SYS-F-515).
        ///
        /// For static and realtime requests over the whole stream: (1) every
        /// drawn point is a real input sample (same time, same value), and (2)
        /// the channel's highest and lowest sample are drawn. The only allowed
        /// exception is the documented one: the chart cannot place two values
        /// at one millisecond, so an extreme may give way to another sample of
        /// the same millisecond.
        #[test]
        fn prop_peak_mode_keeps_extremes_and_real_samples(
            spec in stream_spec(),
            pixel_width in pixel_width_strategy(),
            tier_span in tier_span_strategy(),
            realtime in any::<bool>(),
        ) {
            let agg = aggregator_with(AggregationMode::Peak, tier_span);
            let fed = spec.feed(&agg);
            let ts = spec.timestamps();
            let req = PlotterDataRequest {
                time_min_ms: Some(ts[0]),
                time_max_ms: Some(*ts.last().unwrap()),
                pixel_width,
                is_realtime: realtime,
            };
            let payload = agg.get_ranged_data(&req);

            for (ch, values) in fed.iter().enumerate() {
                let name = StreamSpec::channel_name(ch);
                let points = single_values(payload.line_data.get(&name).unwrap());
                let samples: Vec<(u64, f64)> = ts.iter().copied().zip(values.iter().copied()).collect();

                for p in &points {
                    prop_assert!(samples.contains(p), "{} drew a non-sample point {:?}", name, p);
                }
                prop_assert!(points.len() <= pixel_width as usize,
                    "{} drew {} points for {} px", name, points.len(), pixel_width);

                // An extreme may only be missing if another value shares its millisecond
                let shares_ms = |t: u64, v: f64| samples.iter().any(|&(st, sv)| st == t && sv != v);
                let want_max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let want_min = values.iter().copied().fold(f64::INFINITY, f64::min);
                for (want, label) in [(want_max, "maximum"), (want_min, "minimum")] {
                    let drawn = points.iter().any(|&(_, v)| v == want);
                    let excused = samples.iter().any(|&(t, v)| v == want && shares_ms(t, v));
                    prop_assert!(drawn || excused,
                        "{} lost its {} {} (n={}, px={}, tier_span={}, realtime={})",
                        name, label, want, spec.n, pixel_width, tier_span, realtime);
                }
            }
        }

        /// A1 - the min/max envelope is never lost.
        ///
        /// Guards: "a spike can never disappear". Over a full-range Average-mode
        /// request, the extremes reported across all returned points must be
        /// EXACTLY the extremes of the raw input for that channel - aggregation
        /// stores exact min/max, so no epsilon is allowed here.
        #[test]
        fn prop_minmax_envelope_preserved(
            spec in stream_spec(),
            pixel_width in pixel_width_strategy(),
            tier_span in tier_span_strategy(),
        ) {
            let agg = aggregator_with(AggregationMode::Average, tier_span);
            let fed = spec.feed(&agg);

            let req = PlotterDataRequest {
                time_min_ms: None,
                time_max_ms: None,
                pixel_width,
                is_realtime: false,
            };
            let payload = agg.get_ranged_data(&req);

            for (ch, values) in fed.iter().enumerate() {
                let name = StreamSpec::channel_name(ch);
                let points = payload
                    .line_data
                    .get(&name)
                    .unwrap_or_else(|| panic!("channel {} missing from payload", name));
                prop_assert!(!points.is_empty(), "channel {} returned no points", name);

                let (got_min, got_max) = points.iter().fold(
                    (f64::INFINITY, f64::NEG_INFINITY),
                    |acc, p| {
                        let (_, min, max, _) = point_fields(p);
                        (acc.0.min(min), acc.1.max(max))
                    },
                );
                let want_min = values.iter().copied().fold(f64::INFINITY, f64::min);
                let want_max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);

                prop_assert_eq!(
                    got_max, want_max,
                    "channel {} lost its maximum (n={}, pixel_width={}, tier_span={})",
                    name, spec.n, pixel_width, tier_span
                );
                prop_assert_eq!(
                    got_min, want_min,
                    "channel {} lost its minimum (n={}, pixel_width={}, tier_span={})",
                    name, spec.n, pixel_width, tier_span
                );
            }
        }

        /// A2 - per-point statistical sanity.
        ///
        /// Guards: every MinMax point satisfies min <= avg <= max (a tiny
        /// relative epsilon absorbs the float error of the count-weighted
        /// average accumulation), and every returned timestamp lies inside the
        /// range the payload claims to cover.
        #[test]
        fn prop_invariant_min_le_avg_le_max(
            spec in stream_spec(),
            pixel_width in pixel_width_strategy(),
            tier_span in tier_span_strategy(),
        ) {
            let agg = aggregator_with(AggregationMode::Average, tier_span);
            spec.feed(&agg);

            let req = PlotterDataRequest {
                time_min_ms: None,
                time_max_ms: None,
                pixel_width,
                is_realtime: false,
            };
            let payload = agg.get_ranged_data(&req);

            for (name, points) in &payload.line_data {
                for point in points {
                    let (ts, min, max, avg) = point_fields(point);
                    prop_assert!(
                        ts >= payload.start_ms && ts <= payload.end_ms,
                        "channel {}: ts {} outside payload range [{}, {}]",
                        name, ts, payload.start_ms, payload.end_ms
                    );

                    if let AggregatedPoint::MinMax { .. } = point {
                        prop_assert!(min <= max, "channel {}: min {} > max {}", name, min, max);
                        let eps = 1.0e-9 * min.abs().max(max.abs()).max(1.0);
                        prop_assert!(
                            avg >= min - eps && avg <= max + eps,
                            "channel {}: avg {} outside [{}, {}]",
                            name, avg, min, max
                        );
                    }
                }
            }
        }

        /// A3 - ordering and alignment of the chart payload.
        ///
        /// Guards: per-channel timestamps are non-decreasing, `aligned_data[0]`
        /// is STRICTLY increasing, and every channel column has exactly the
        /// same length as the timestamp column (a length mismatch would
        /// silently mis-associate values with timestamps in uPlot).
        #[test]
        fn prop_timestamps_sorted(
            spec in stream_spec(),
            pixel_width in pixel_width_strategy(),
            tier_span in tier_span_strategy(),
            use_average in any::<bool>(),
        ) {
            let mode = if use_average {
                AggregationMode::Average
            } else {
                AggregationMode::Peak
            };
            let agg = aggregator_with(mode, tier_span);
            spec.feed(&agg);

            let req = PlotterDataRequest {
                time_min_ms: None,
                time_max_ms: None,
                pixel_width,
                is_realtime: false,
            };

            let payload = agg.get_ranged_data(&req);
            for (name, points) in &payload.line_data {
                let mut prev = 0u64;
                for point in points {
                    let (ts, _, _, _) = point_fields(point);
                    prop_assert!(
                        ts >= prev,
                        "channel {}: timestamps not sorted ({} after {})",
                        name, ts, prev
                    );
                    prev = ts;
                }
            }

            let chart = agg.get_chart_data(&req);
            prop_assert!(!chart.aligned_data.is_empty(), "aligned_data must have an x column");
            prop_assert_eq!(
                chart.aligned_data.len(),
                chart.channel_names.len() + 1,
                "aligned_data must be [timestamps, ...one column per channel]"
            );

            let ts_col = &chart.aligned_data[0];
            for w in ts_col.windows(2) {
                let (a, b) = (w[0], w[1]);
                prop_assert!(a.is_some() && b.is_some(), "timestamp column must not contain nulls");
                prop_assert!(
                    a.unwrap() < b.unwrap(),
                    "aligned_data[0] not strictly increasing: {:?} then {:?}",
                    a, b
                );
            }
            for (i, col) in chart.aligned_data.iter().enumerate().skip(1) {
                prop_assert_eq!(
                    col.len(),
                    ts_col.len(),
                    "channel column {} has {} rows but there are {} timestamps",
                    i, col.len(), ts_col.len()
                );
            }
        }

        /// A4 - the data version counter never goes backwards.
        ///
        /// Guards: the frontend polls `check_version()` and re-fetches only when
        /// the number changes; a decrease (or a reset on `clear`) would strand
        /// it on stale data.
        #[test]
        fn prop_version_monotonic(ops in prop::collection::vec(agg_op(), 1..80)) {
            let agg = PlotterAggregator::new();
            agg.set_enabled(true);

            let mut last = agg.check_version().version;
            for op in &ops {
                match op {
                    AggOp::AddPoint { channel, ts, value } => {
                        agg.add_data_point(
                            &StreamSpec::channel_name(*channel),
                            *ts,
                            ChannelValue::Numeric(*value),
                        );
                    }
                    AggOp::AddBatch { channel, base_ts, count } => {
                        let name = StreamSpec::channel_name(*channel);
                        let batch: Vec<crate::plotter::parser::ParsedDataPoint> = (0..*count)
                            .map(|i| {
                                let mut channels = HashMap::new();
                                channels.insert(
                                    name.clone(),
                                    ChannelValue::Numeric(i as f64),
                                );
                                crate::plotter::parser::ParsedDataPoint {
                                    timestamp_ms: base_ts + i as u64 * 10,
                                    channels,
                                    channel_order: vec![name.clone()],
                                }
                            })
                            .collect();
                        agg.add_data_points_batch(batch);
                    }
                    AggOp::Clear => agg.clear(),
                    AggOp::SetAverageMode(average) => {
                        agg.set_aggregation_mode(if *average {
                            AggregationMode::Average
                        } else {
                            AggregationMode::Peak
                        });
                    }
                    AggOp::GetChart { pixel_width } => {
                        let _ = agg.get_chart_data(&PlotterDataRequest {
                            time_min_ms: None,
                            time_max_ms: None,
                            pixel_width: *pixel_width,
                            is_realtime: false,
                        });
                    }
                }

                let now = agg.check_version().version;
                prop_assert!(
                    now >= last,
                    "version decreased ({} -> {}) after {:?}",
                    last, now, op
                );
                last = now;
            }
        }

        /// P-4 (docs/24 3.3) - count conservation, generated streams.
        ///
        /// Guards: aggregation never invents or loses raw points. The `count`
        /// carried by the stored cells of every tier must equal the number of
        /// points fed - at any `tier_span`, i.e. however much was folded. The
        /// min/max envelope must be preserved exactly at the same time (folding
        /// merges min-of-mins / max-of-maxs, so no epsilon is allowed).
        #[test]
        fn prop_count_conservation(
            spec in stream_spec(),
            tier_span in tier_span_strategy(),
        ) {
            let agg = aggregator_with(AggregationMode::Average, tier_span);
            let fed = spec.feed(&agg);

            for (ch, values) in fed.iter().enumerate() {
                let name = StreamSpec::channel_name(ch);
                prop_assert_eq!(
                    represented_count(&agg, &name),
                    values.len(),
                    "channel {} lost points (n={}, tier_span={}, level={})",
                    name, spec.n, tier_span, tier_depth(&agg)
                );

                let (got_min, got_max) = stored_extremes(&agg, &name);
                let want_min = values.iter().copied().fold(f64::INFINITY, f64::min);
                let want_max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                prop_assert_eq!(
                    got_min, want_min,
                    "channel {} lost its stored minimum (tier_span={})", name, tier_span
                );
                prop_assert_eq!(
                    got_max, want_max,
                    "channel {} lost its stored maximum (tier_span={})", name, tier_span
                );
            }
        }

        /// P-7 (docs/24 3.3) - batch equivalence, strong form in every regime.
        ///
        /// Guards: `add_data_points_batch` is only a performance shortcut for
        /// N x `add_data_point`, so the chart the frontend receives must not
        /// depend on which one the reader thread used. With the tiered storage
        /// (ADR-16) where a cell sits depends only on the newest data time, and
        /// merges are associative, so the two routes store the same cells
        /// however often they folded. Peak payloads are therefore identical;
        /// Average bands are identical and the centre line (sum / count) may
        /// differ only by floating-point summation order.
        #[test]
        fn prop_batch_equivalence(
            spec in stream_spec(),
            pixel_width in pixel_width_strategy(),
            tier_span in tier_span_strategy(),
            use_average in any::<bool>(),
        ) {
            let mode = if use_average {
                AggregationMode::Average
            } else {
                AggregationMode::Peak
            };

            let one_by_one = aggregator_with(mode.clone(), tier_span);
            let fed = spec.feed(&one_by_one);

            let batched = aggregator_with(mode, tier_span);
            batched.add_data_points_batch(spec.as_batch());

            for (ch, values) in fed.iter().enumerate() {
                let name = StreamSpec::channel_name(ch);
                prop_assert_eq!(represented_count(&one_by_one, &name), values.len());
                prop_assert_eq!(represented_count(&batched, &name), values.len());
            }

            let req = PlotterDataRequest {
                time_min_ms: None,
                time_max_ms: None,
                pixel_width,
                is_realtime: false,
            };
            let chart_a = one_by_one.get_chart_data(&req);
            let chart_b = batched.get_chart_data(&req);

            prop_assert_eq!(&chart_a.channel_names, &chart_b.channel_names);
            prop_assert_eq!(chart_a.start_ms, chart_b.start_ms);
            prop_assert_eq!(chart_a.end_ms, chart_b.end_ms);
            prop_assert!(
                !chart_a.aligned_data[0].is_empty(),
                "nothing was plotted, the comparison would be vacuous"
            );
            prop_assert_eq!(band_shape(&chart_a), band_shape(&chart_b));
            if use_average {
                prop_assert_eq!(chart_a.aligned_data.len(), chart_b.aligned_data.len());
                prop_assert_eq!(&chart_a.aligned_data[0], &chart_b.aligned_data[0]);
                for (a, b) in chart_a.aligned_data.iter().zip(&chart_b.aligned_data).skip(1) {
                    prop_assert_eq!(a.len(), b.len());
                    for (x, y) in a.iter().zip(b) {
                        match (x, y) {
                            (Some(x), Some(y)) => prop_assert!(
                                (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0),
                                "average centre line diverged: {} vs {}", x, y
                            ),
                            _ => prop_assert_eq!(x, y),
                        }
                    }
                }
            } else {
                prop_assert_eq!(
                    &chart_a.aligned_data,
                    &chart_b.aligned_data,
                    "batch vs point-by-point diverged (n={}, channels={},                      pixel_width={}, tier_span={})",
                    spec.n, spec.channels, pixel_width, tier_span
                );
            }
        }

        /// P-14 (ADR-16) - a LIVE window is drawn from the samples in it alone.
        ///
        /// With the default tier span, every stored cell inside a LIVE window
        /// [now - W, now] is no wider than the window's display cells and lies
        /// inside one of them, so the Peak payload equals the envelope computed
        /// directly from the raw samples in the window - whatever the tiers
        /// folded and when. Only the display cell cut by the left window edge
        /// may differ (a stored cell straddling the edge keeps only its extremes).
        #[test]
        fn prop_live_window_is_drawn_from_raw_samples(
            spec in stream_spec(),
            window_s in prop::sample::select(vec![1u64, 2, 5, 10, 30, 60, 120, 300]),
            pixel_width in 50u32..=4000,
        ) {
            let agg = aggregator_with(AggregationMode::Peak, PlotterConfig::default().tier0_span_ms);
            let fed = spec.feed(&agg);
            let ts = spec.timestamps();
            let now = *ts.last().unwrap();
            let t0 = now.saturating_sub(window_s * 1000);
            let req = PlotterDataRequest {
                time_min_ms: Some(t0),
                time_max_ms: Some(now),
                pixel_width,
                is_realtime: true,
            };
            let payload = agg.get_ranged_data(&req);

            let target = pixel_width.min(PlotterConfig::default().max_target_points) as u64;
            let cells = (target / 2).saturating_sub(1).max(1);
            let width = quantize_bucket_width_pow2((now - t0).div_ceil(cells));
            let edge_cell = t0 / width;

            for (ch, values) in fed.iter().enumerate() {
                let name = StreamSpec::channel_name(ch);
                let raw: Vec<AggregatedBucket> = ts
                    .iter()
                    .zip(values)
                    .filter(|(&t, _)| t >= t0 && t <= now)
                    .map(|(&t, &v)| AggregatedBucket::sample(t, v))
                    .collect();
                let expected: Vec<(u64, f64)> = single_values(&peak_envelope(&raw, width, t0, now))
                    .into_iter()
                    .filter(|p| p.0 / width != edge_cell)
                    .collect();
                let got: Vec<(u64, f64)> = single_values(payload.line_data.get(&name).unwrap())
                    .into_iter()
                    .filter(|p| p.0 / width != edge_cell)
                    .collect();
                prop_assert_eq!(got, expected, "{} (W={} s, px={})", name, window_s, pixel_width);
            }
        }

        /// A5 - the realtime aligned grid is stable while the window slides.
        ///
        /// Generalizes `test_aligned_buckets_are_stable_across_sliding_windows`:
        /// for two realtime requests of the SAME span whose starts differ by an
        /// arbitrary slide, every cell that is interior to both results must be
        /// byte-for-byte identical (ts, min, max, avg). Interior = not the first
        /// or last returned cell, which are the only ones a window boundary can
        /// clip.
        ///
        /// GENERATOR CONSTRAINT: if one window lands above the aggregation
        /// threshold and the other below it, the two results are in different
        /// representations by design (aligned grid cells vs. pass-through
        /// buckets), so that pairing is excluded rather than compared.
        #[test]
        fn prop_aligned_realtime_stability(
            cadence in 1u64..=20,
            val_seed in any::<u64>(),
            span in 2_000u64..=20_000,
            pixel_width in 50u32..=400,
            slide in 1u64..=500,
            start_offset in 0u64..1_000,
        ) {
            const T0: u64 = 1_000_000;
            const N: usize = 2500;

            let agg = average_aggregator();
            for i in 0..N {
                let ts = T0 + i as u64 * cadence;
                let h = mix64(val_seed ^ (i as u64));
                let base = (i as f64 * 0.11).sin() * 50.0;
                let v = if h.is_multiple_of(53) { base + 1.0e3 } else { base };
                agg.add_data_point("ch0", ts, ChannelValue::Numeric(v));
            }

            let a_min = T0 + start_offset;
            let req_a = PlotterDataRequest {
                time_min_ms: Some(a_min),
                time_max_ms: Some(a_min + span),
                pixel_width,
                is_realtime: true,
            };
            let req_b = PlotterDataRequest {
                time_min_ms: Some(a_min + slide),
                time_max_ms: Some(a_min + slide + span),
                pixel_width,
                is_realtime: true,
            };

            let payload_a = agg.get_ranged_data(&req_a);
            let payload_b = agg.get_ranged_data(&req_b);

            // Excluded pairing (see doc comment): mixed representations.
            if payload_a.is_aggregated == payload_b.is_aggregated {
                let empty: Vec<AggregatedPoint> = Vec::new();
                let points_a = payload_a.line_data.get("ch0").unwrap_or(&empty);
                let points_b = payload_b.line_data.get("ch0").unwrap_or(&empty);

                let cells_a = interior_cells(points_a);
                let cells_b = interior_cells(points_b);

                for (ts, stats_a) in &cells_a {
                    if let Some(stats_b) = cells_b.get(ts) {
                        prop_assert_eq!(
                            stats_a, stats_b,
                            "cell at ts={} changed when the window slid by {} ms \
                             (span={}, pixel_width={}, cadence={})",
                            ts, slide, span, pixel_width, cadence
                        );
                    }
                }
            }
        }
    }
}
