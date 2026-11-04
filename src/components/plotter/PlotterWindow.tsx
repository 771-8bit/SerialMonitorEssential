import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import LineChart, { type LineChartHandle } from './LineChart';
import { COLORS } from './chartColors';
import { StateRow } from './stateTimelinePlugin';
import './PlotterWindow.css';
import { usePersistentState, isOneOf } from '../../utils/persist';

// Aggregation mode type matching Rust AggregationMode
type AggregationMode = 'Average' | 'Peak';

// Types matching Rust PlotterChartPayload (new uPlot-ready format)
interface ChannelInfo {
  name: string;
  channel_type: 'Line' | 'State' | 'Auto';
  latest_value: string | null;
  point_count: number;
}

interface StateChange {
  start_ms: number;
  end_ms: number | null;
  state: string;
}

// Band series data for min/max bands (same indices as aligned_data[0])
export interface BandSeriesData {
  min: (number | null)[];
  max: (number | null)[];
}

// New payload format - uPlot ready, no transformation needed
interface PlotterChartPayload {
  // uPlot aligned data: [timestamps, ch0_values, ch1_values, ...]
  aligned_data: (number | null)[][];
  // Channel names in order (matches aligned_data columns starting at index 1)
  channel_names: string[];
  // MinMax band data (Average mode only)
  band_data: Record<string, BandSeriesData> | null;
  // State timeline data
  state_data: Record<string, StateChange[]>;
  // Channel metadata for legend
  channels: ChannelInfo[];
  // Time range
  start_ms: number;
  end_ms: number;
  // Session generation: changes on a new session (Clear / manual Connect),
  // where the backend's time axis restarts at 0
  generation?: number;
}

interface PlotterDataRequest {
  time_min_ms: number | null;
  time_max_ms: number | null;
  pixel_width: number;
  is_realtime: boolean;
}

// Version info for lightweight polling (matches Rust PlotterVersionInfo)
interface PlotterVersionInfo {
  version: number;
  has_data: boolean;
  generation?: number;
}

/** Selectable widths (seconds) of the sliding live window */
export const WINDOW_OPTIONS = [1, 2, 5, 10, 30, 60, 120, 300] as const;
const DEFAULT_WINDOW_SEC = 10;

export default function PlotterWindow() {
  const [data, setData] = useState<PlotterChartPayload | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isRunning, setIsRunning] = useState(true);
  const [threadStarted, setThreadStarted] = useState(false);

  // Hidden channels for legend toggle (controls series.show in LineChart)
  const [hiddenChannels, setHiddenChannels] = useState<Set<string>>(new Set());

  // Aggregation mode setting
  // Remembered across restarts (issue #7); the backend is told on mount below
  const [aggregationMode, setAggregationMode] = usePersistentState<AggregationMode>(
    'plotter.aggregationMode',
    'Peak',
    isOneOf<AggregationMode>(['Peak', 'Average'])
  );

  // Width of the sliding live window (seconds)
  const [windowSec, setWindowSec] = usePersistentState<number>(
    'plotter.windowSec',
    DEFAULT_WINDOW_SEC,
    isOneOf<number>(WINDOW_OPTIONS)
  );
  const windowSecRef = useRef(windowSec);

  // View state: LIVE (following) vs Inspect (frozen on a chosen range).
  // Paused is a third state, driven by isRunning.
  const [isFollowing, setIsFollowing] = useState(true);
  const followRef = useRef(true);
  // Ref mirror of isRunning for use inside stable callbacks
  const runningRef = useRef(true);
  useEffect(() => {
    runningRef.current = isRunning;
  }, [isRunning]);

  // Ref for chart container to get pixel width
  const chartContainerRef = useRef<HTMLDivElement>(null);
  const chartHandleRef = useRef<LineChartHandle | null>(null);

  // Refs for frame-based update loop
  const isFetchingRef = useRef(false);
  const lastEndMsRef = useRef(0);
  // Track last known version for smart polling
  const lastVersionRef = useRef(0);
  // Last session generation seen (null before the first payload)
  const lastGenerationRef = useRef<number | null>(null);
  // Right edge of the live window ("now"), in ms since plotter start.
  // null until the first payload tells us where the data ends.
  const rightEdgeRef = useRef<number | null>(null);
  // Whether a chart is currently on screen (mirrors hasChartContent)
  const hasDataRef = useRef(false);
  // Range currently inspected (seconds), used to re-request on pan/zoom
  const inspectRangeRef = useRef<{ minSec: number; maxSec: number } | null>(null);

  const aggregationModeAtMount = useRef(aggregationMode);

  // Start plotter thread on mount
  useEffect(() => {
    const startThread = async () => {
      // The mode lives in the backend for the whole process; send the remembered one
      // so the selector and the drawing always agree (also after reopening this window)
      invoke('set_aggregation_mode', { mode: aggregationModeAtMount.current }).catch(console.error);
      try {
        await invoke('start_plotter_thread');
        setThreadStarted(true);
        console.log('Plotter thread started');
      } catch (e) {
        // May fail if serial port not open, that's OK
        console.log('Could not start plotter thread (port may not be open):', e);
      }
    };
    startThread();

    // Stop thread on unmount
    return () => {
      invoke('stop_plotter_thread').catch(console.error);
    };
  }, []);

  // Build the backend request for the current view state.
  // LIVE: explicit sliding window [rightEdge - W, rightEdge] (is_realtime).
  // LIVE before the first payload: nulls (bootstrap - backend picks the range).
  // Inspect: the user-selected range (not realtime, so it can be cached).
  const buildRequest = useCallback((): PlotterDataRequest => {
    const measured = chartContainerRef.current?.clientWidth ?? 0;
    const pixelWidth = measured > 0 ? measured : 800;

    if (followRef.current) {
      const rightEdge = rightEdgeRef.current;
      if (rightEdge === null) {
        return {
          time_min_ms: null,
          time_max_ms: null,
          pixel_width: pixelWidth,
          is_realtime: true,
        };
      }
      return {
        time_min_ms: Math.max(0, Math.floor(rightEdge - windowSecRef.current * 1000)),
        time_max_ms: Math.ceil(rightEdge),
        pixel_width: pixelWidth,
        is_realtime: true,
      };
    }

    const range = inspectRangeRef.current;
    if (!range) {
      return { time_min_ms: null, time_max_ms: null, pixel_width: pixelWidth, is_realtime: false };
    }
    return {
      time_min_ms: Math.max(0, Math.floor(range.minSec * 1000)),
      time_max_ms: Math.max(0, Math.ceil(range.maxSec * 1000)),
      pixel_width: pixelWidth,
      is_realtime: false,
    };
  }, []);

  // Apply a payload to the chart, handling reset- and empty-window cases.
  // hasData comes from check_plotter_version when available.
  const applyPayload = useCallback((payload: PlotterChartPayload, hasData = true) => {
    const hasPoints = (payload.aligned_data[0]?.length ?? 0) > 0;
    const hasStates = Object.values(payload.state_data).some((changes) => changes.length > 0);

    // A new session: the generation changed (Clear / manual Connect restart the
    // backend's time axis at 0, PF-5) - the data may already be flowing again,
    // so has_data alone would miss it.
    const generationChanged =
      payload.generation !== undefined &&
      lastGenerationRef.current !== null &&
      payload.generation !== lastGenerationRef.current;
    if (payload.generation !== undefined) lastGenerationRef.current = payload.generation;

    // Backend clear(): has_data went false, the generation changed, or the data
    // range moved backwards. (The last only while following - in Inspect we
    // request older ranges on purpose, so a lower end_ms is expected there.)
    const isReset =
      !hasData || generationChanged || (followRef.current && payload.end_ms < lastEndMsRef.current);
    if (isReset) {
      lastEndMsRef.current = payload.end_ms;
      rightEdgeRef.current = null;
      inspectRangeRef.current = null;
      chartHandleRef.current?.resetView();
      // The range being inspected belonged to the old session: follow the new one
      if (!followRef.current) {
        followRef.current = true;
        setIsFollowing(true);
      }
      hasDataRef.current = hasPoints || hasStates;
      setData(payload);
      return;
    }

    // The live window has scrolled past the last sample (dead stream). Keep the
    // old data object: the window itself empties as it scrolls, and swapping in
    // an empty payload would flip us to the "No data yet" placeholder.
    if (!hasPoints && !hasStates && hasDataRef.current) return;

    lastEndMsRef.current = Math.max(lastEndMsRef.current, payload.end_ms);
    if (followRef.current && rightEdgeRef.current === null) {
      // Bootstrap answer (open-ended request): place the right edge at the
      // newest data. After that the edge advances by wall time only. Explicit
      // requests echo time_max_ms (a ceil() of the edge) as end_ms, so snapping
      // to it made the edge creep ahead of real time on every fetch.
      rightEdgeRef.current = payload.end_ms;
    }
    hasDataRef.current = hasPoints || hasStates;
    setData(payload);
  }, []);

  // One-shot fetch for the current view state (mode switch, window change,
  // resume, return to LIVE). The rAF loop fetches on version changes instead.
  const fetchData = useCallback(async (): Promise<boolean> => {
    try {
      const payload = await invoke<PlotterChartPayload>('get_plotter_chart_data', {
        request: buildRequest(),
      });
      applyPayload(payload);
      setError(null);
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    }
  }, [buildRequest, applyPayload]);

  // Frame-based update loop using requestAnimationFrame.
  // Two jobs per frame:
  //  1. advance the live window by the elapsed wall time (imperative - a 60Hz
  //     setState here previously caused a re-render storm / memory leak)
  //  2. lightweight version check, fetching data only when it changed
  // Runs only while LIVE and not paused; Inspect freezes the view entirely.
  useEffect(() => {
    if (!isRunning || !isFollowing) return;

    let rafId: number;
    let isActive = true;
    let lastFrameMs = performance.now();

    const updateLoop = async () => {
      if (!isActive) return;
      // The user may have left follow mode between frames (wheel / drag select).
      // The effect teardown only lands a render later, so stop right here -
      // otherwise this frame would overwrite the range they just picked.
      if (!followRef.current) return;

      // Step 1: scroll the window (local only, no IPC).
      // performance.now() deltas stay correct even if frames are throttled.
      const now = performance.now();
      const deltaMs = Math.max(0, now - lastFrameMs);
      lastFrameMs = now;
      if (rightEdgeRef.current !== null) {
        rightEdgeRef.current += deltaMs;
        // Until a full window of data exists, show [0, W] and fill it from the
        // left instead of drawing negative times (PF-5)
        const maxSec = Math.max(rightEdgeRef.current / 1000, windowSecRef.current);
        chartHandleRef.current?.setXWindow(maxSec - windowSecRef.current, maxSec);
      }

      // Step 2: Lightweight version check (8 bytes payload)
      try {
        const versionInfo = await invoke<PlotterVersionInfo>('check_plotter_version');

        // New session: the old right edge belongs to the old time axis, so the
        // next request must be a bootstrap (an explicit window in the old range
        // would come back empty and the chart would freeze)
        if (
          versionInfo.generation !== undefined &&
          lastGenerationRef.current !== null &&
          versionInfo.generation !== lastGenerationRef.current
        ) {
          rightEdgeRef.current = null;
        }

        // Fetch full data whenever the version changed.
        // Note: fetch even when has_data is false, so a backend clear()
        // replaces the stale chart with the empty payload.
        if (versionInfo.version !== lastVersionRef.current) {
          // Skip if previous fetch is still in progress
          if (!isFetchingRef.current) {
            isFetchingRef.current = true;
            try {
              const payload = await invoke<PlotterChartPayload>('get_plotter_chart_data', {
                request: buildRequest(),
              });
              lastVersionRef.current = versionInfo.version;
              applyPayload(payload, versionInfo.has_data);
              setError(null);
            } finally {
              isFetchingRef.current = false;
            }
          }
        }
      } catch (e) {
        setError(String(e));
      }

      // Schedule next frame
      if (isActive) {
        rafId = requestAnimationFrame(updateLoop);
      }
    };

    // Start the loop
    rafId = requestAnimationFrame(updateLoop);

    return () => {
      isActive = false;
      cancelAnimationFrame(rafId);
    };
  }, [isRunning, isFollowing, buildRequest, applyPayload]);

  // Toggle pause/resume (only affects frontend polling, backend continues collecting)
  const toggleRunning = useCallback(() => {
    if (!isRunning && followRef.current) {
      // Resuming: the window stood still while paused, so re-anchor it to
      // the newest data (bootstrap request) instead of resuming in the past.
      rightEdgeRef.current = null;
      void fetchData();
    }
    setIsRunning((prev) => !prev);
    // Note: We intentionally don't call set_plotter_enabled here
    // Backend keeps collecting data so we can show it when resumed
  }, [isRunning, fetchData]);

  // The user zoomed or drag-selected: stop following, freeze on that range
  const handleUserInteraction = useCallback(() => {
    if (!followRef.current) return;
    followRef.current = false;
    setIsFollowing(false);
  }, []);

  // Return to LIVE follow mode (LIVE button or double click on the chart)
  const goLive = useCallback(() => {
    followRef.current = true;
    inspectRangeRef.current = null;
    // Re-anchor to the newest data with a bootstrap request
    rightEdgeRef.current = null;
    chartHandleRef.current?.resetView();
    setIsFollowing(true);
    void fetchData();
  }, [fetchData]);

  // Inspect-mode scrollback: the visible range changed (debounced by LineChart),
  // so request exactly that range from the backend.
  const handleTimeRangeChange = useCallback(
    (minSec: number, maxSec: number) => {
      // In LIVE mode the window is driven locally - never fetch from here.
      if (followRef.current) return;
      if (!Number.isFinite(minSec) || !Number.isFinite(maxSec) || maxSec <= minSec) return;

      const prev = inspectRangeRef.current;
      if (prev && Math.abs(prev.minSec - minSec) < 1e-6 && Math.abs(prev.maxSec - maxSec) < 1e-6) {
        return;
      }
      inspectRangeRef.current = { minSec, maxSec };

      if (isFetchingRef.current) return;
      isFetchingRef.current = true;
      void (async () => {
        try {
          const payload = await invoke<PlotterChartPayload>('get_plotter_chart_data', {
            request: buildRequest(),
          });
          applyPayload(payload);
          setError(null);
        } catch (e) {
          setError(String(e));
        } finally {
          isFetchingRef.current = false;
        }
      })();
    },
    [buildRequest, applyPayload]
  );

  // Window width change: refetch immediately and drop the y hysteresis state.
  // While Paused, only record the new width - no request is issued (SYS-F-606:
  // Paused stops all frontend polling); the width takes effect on resume.
  const handleWindowChange = useCallback(
    (nextWindowSec: number) => {
      windowSecRef.current = nextWindowSec;
      setWindowSec(nextWindowSec);
      chartHandleRef.current?.resetYRange();
      if (followRef.current && runningRef.current) {
        void fetchData();
      }
    },
    [fetchData, setWindowSec]
  );

  // Format time duration
  const formatDuration = (ms: number): string => {
    const seconds = Math.floor(ms / 1000);
    const minutes = Math.floor(seconds / 60);
    const hours = Math.floor(minutes / 60);

    if (hours > 0) {
      return `${hours}h ${minutes % 60}m ${seconds % 60}s`;
    } else if (minutes > 0) {
      return `${minutes}m ${seconds % 60}s`;
    } else {
      return `${seconds}.${Math.floor((ms % 1000) / 100)}s`;
    }
  };

  // Build state rows from state_data (sorted alphabetically)
  const stateRows: StateRow[] = useMemo(() => {
    if (!data) return [];
    return Object.entries(data.state_data)
      .filter(([, changes]) => changes.length > 0)
      .map(([channel, changes]) => ({
        channel,
        segments: changes.map((c) => ({
          start_ms: c.start_ms,
          // Open-ended segment extends to the end of the plotted data range.
          // (Timestamps are relative to plotter start, NOT Unix time, so
          // Date.now() would be the wrong time base here.)
          end_ms: c.end_ms ?? data.end_ms,
          state: c.state,
        })),
      }))
      .sort((a, b) => a.channel.localeCompare(b.channel));
  }, [data]);

  // Check if we have anything to display: line data, or state-only data
  // (state-only payloads have no line series but carry an x-range + states)
  const hasChartContent =
    !!data &&
    (data.aligned_data[0]?.length ?? 0) > 0 &&
    (data.aligned_data.length > 1 || stateRows.length > 0);

  const statusText = !isRunning ? '⏸ Paused' : isFollowing ? '● LIVE' : '🔍 Inspect';
  const statusClass = !isRunning ? 'paused' : isFollowing ? 'live' : 'inspect';

  return (
    <div className="plotter-window">
      {/* Header */}
      <div className="plotter-header">
        <h1>Serial Plotter</h1>
        <div className="plotter-controls">
          {!isFollowing && (
            <button
              className="control-button live"
              onClick={goLive}
              title="Return to the live view"
            >
              ▶ LIVE
            </button>
          )}
          <button
            className={`control-button ${isRunning ? 'pause' : 'play'}`}
            onClick={toggleRunning}
            title={isRunning ? 'Pause' : 'Resume'}
          >
            {isRunning ? '⏸ Pause' : '▶ Resume'}
          </button>
        </div>
      </div>

      {/* Error display */}
      {error && <div className="plotter-error">{error}</div>}

      {/* Main content */}
      <div className="plotter-content">
        {hasChartContent ? (
          <>
            {/* Line Chart (with integrated State Timeline) */}
            <div className="chart-area" ref={chartContainerRef}>
              <LineChart
                ref={chartHandleRef}
                alignedData={data!.aligned_data}
                channelNames={data!.channel_names}
                bandData={data!.band_data}
                isPaused={!isRunning}
                stateRows={stateRows}
                hiddenChannels={hiddenChannels}
                followMode={isFollowing}
                onUserInteraction={handleUserInteraction}
                onLiveRequest={goLive}
                onTimeRangeChange={handleTimeRangeChange}
              />
            </div>

            {/* Channel legend */}
            <div className="channel-legend">
              {data!.channels
                .filter((ch) => ch.channel_type === 'Line' || ch.channel_type === 'Auto')
                .map((channel) => {
                  const isHidden = hiddenChannels.has(channel.name);
                  // Use index in sorted channel_names for consistent color (matches LineChart)
                  const colorIndex = data!.channel_names.indexOf(channel.name);
                  const color = colorIndex >= 0 ? COLORS[colorIndex % COLORS.length] : COLORS[0];
                  return (
                    <div
                      key={channel.name}
                      className={`legend-item ${isHidden ? 'hidden' : ''}`}
                      onClick={() => {
                        setHiddenChannels((prev) => {
                          const next = new Set(prev);
                          if (next.has(channel.name)) {
                            next.delete(channel.name);
                          } else {
                            next.add(channel.name);
                          }
                          return next;
                        });
                      }}
                      title={isHidden ? 'Click to show' : 'Click to hide'}
                    >
                      <span className="legend-name" style={{ color }}>
                        {channel.name}:
                      </span>
                      <span className="legend-value" style={{ color }}>
                        {channel.latest_value ?? '--'}
                      </span>
                      <span className="legend-count">{channel.point_count} pts</span>
                    </div>
                  );
                })}
            </div>
          </>
        ) : (
          <div className="no-data">
            <p>No data yet.</p>
            <p className="hint">
              {threadStarted
                ? 'Send data in one of these formats:'
                : 'Connect to a serial port first, then send data:'}
            </p>
            <ul className="format-list">
              <li>
                <strong>CSV Format:</strong> <code>25.5,60,RUNNING</code>
              </li>
              <li>
                <strong>Labeled values:</strong> <code>temp:25.5,humidity:60,state:RUNNING</code>
              </li>
            </ul>
          </div>
        )}
      </div>

      {/* Footer */}
      <div className="plotter-footer">
        <span className={`status-text ${statusClass}`}>{statusText}</span>
        {data && <span className="duration">Duration: {formatDuration(data.end_ms)}</span>}
        <div className="window-control">
          <span className="window-label">Window:</span>
          <select
            className="window-select"
            aria-label="Time window"
            value={windowSec}
            onChange={(e) => handleWindowChange(Number(e.target.value))}
            title="Visible time window"
          >
            {WINDOW_OPTIONS.map((sec) => (
              <option key={sec} value={sec}>
                {sec}s
              </option>
            ))}
          </select>
        </div>
        <div className="aggregation-control">
          <span className="aggregation-label">Downsample:</span>
          <select
            className="aggregation-select"
            aria-label="Downsample mode"
            value={aggregationMode}
            onChange={(e) => {
              const mode = e.target.value as AggregationMode;
              setAggregationMode(mode);
              invoke('set_aggregation_mode', { mode })
                .then(() => fetchData()) // Refetch so the mode change shows immediately
                .catch(console.error);
            }}
            title="Downsampling mode"
          >
            <option
              value="Peak"
              title="Draws the minimum and maximum of every pixel column at the times they occurred, so a single-sample spike is never lost"
            >
              Peak
            </option>
            <option
              value="Average"
              title="Draws the average of every pixel column, with the minimum-to-maximum range as a shaded band"
            >
              Average
            </option>
          </select>
        </div>
      </div>
    </div>
  );
}
