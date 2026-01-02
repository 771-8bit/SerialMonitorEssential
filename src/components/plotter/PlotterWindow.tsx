import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import LineChart from './LineChart';
import { COLORS } from './chartColors';
import { StateRow } from './stateTimelinePlugin';
import './PlotterWindow.css';

// Aggregation mode type matching Rust AggregationMode
type AggregationMode = 'Average' | 'Lttb';

// Types matching Rust PlotterRangedPayload
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

// Aggregated point for dynamic aggregation
interface AggregatedPoint {
  type: 'Single' | 'MinMax';
  ts: number;
  value?: number;
  min?: number;
  max?: number;
}

interface PlotterRangedPayload {
  channels: ChannelInfo[];
  line_data: Record<string, AggregatedPoint[]>;
  state_data: Record<string, StateChange[]>;
  start_ms: number;
  end_ms: number;
  is_aggregated: boolean;
}

interface PlotterDataRequest {
  time_min_ms: number | null;
  time_max_ms: number | null;
  pixel_width: number;
  is_realtime: boolean;
}

// MinMax band data for a channel: arrays of [timestamp, min, max]
export interface MinMaxBandData {
  timestamps: number[];
  min: number[];
  max: number[];
}

// Convert aggregated points to formats for LineChart
// Returns line data for all modes, plus band data for MinMax mode
function convertAggregatedData(data: Record<string, AggregatedPoint[]>): {
  lineData: Record<string, [number, number][]>;
  bandData: Record<string, MinMaxBandData>;
} {
  const lineData: Record<string, [number, number][]> = {};
  const bandData: Record<string, MinMaxBandData> = {};

  for (const [channel, points] of Object.entries(data)) {
    // Check if this channel has MinMax data
    const hasMinMax = points.some((p) => p.type === 'MinMax');

    if (hasMinMax) {
      // For MinMax, use average for line and store band data
      const timestamps: number[] = [];
      const mins: number[] = [];
      const maxs: number[] = [];
      const linePoints: [number, number][] = [];

      for (const p of points) {
        if (p.type === 'MinMax') {
          const min = p.min ?? 0;
          const max = p.max ?? 0;
          timestamps.push(p.ts);
          mins.push(min);
          maxs.push(max);
          // Line shows midpoint for reference
          linePoints.push([p.ts, (min + max) / 2]);
        } else {
          // Single point in MinMax mode - treat as both min and max
          const value = p.value ?? 0;
          timestamps.push(p.ts);
          mins.push(value);
          maxs.push(value);
          linePoints.push([p.ts, value]);
        }
      }

      bandData[channel] = { timestamps, min: mins, max: maxs };
      lineData[channel] = linePoints;
    } else {
      // For Single points (Average/LTTB), just use the value
      lineData[channel] = points.map((p) => [p.ts, p.value ?? 0]);
    }
  }

  return { lineData, bandData };
}

export default function PlotterWindow() {
  const [data, setData] = useState<PlotterRangedPayload | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isRunning, setIsRunning] = useState(true);
  const [threadStarted, setThreadStarted] = useState(false);

  // Hidden channels for legend toggle
  const [hiddenChannels, setHiddenChannels] = useState<Set<string>>(new Set());

  // Aggregation mode setting
  const [aggregationMode, setAggregationMode] = useState<AggregationMode>('Lttb');

  // Ref for chart container to get pixel width
  const chartContainerRef = useRef<HTMLDivElement>(null);

  // Refs for frame-based update loop
  const isFetchingRef = useRef(false);
  const lastEndMsRef = useRef(0);

  // Start plotter thread on mount
  useEffect(() => {
    const startThread = async () => {
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

  // Fetch plotter data using new ranged API
  // forceUpdate: if true, always update state even if end_ms hasn't changed (for mode switch)
  const fetchData = useCallback(async (forceUpdate = false): Promise<boolean> => {
    // Get pixel width from chart container
    const pixelWidth = chartContainerRef.current?.clientWidth ?? 800;

    // Build request - use realtime mode to let backend determine optimal sliding window
    const request: PlotterDataRequest = {
      time_min_ms: null,
      time_max_ms: null,
      pixel_width: pixelWidth,
      is_realtime: true,
    };

    try {
      const payload = await invoke<PlotterRangedPayload>('get_plotter_data_ranged', { request });

      // Update state if data has changed OR if forceUpdate is requested (e.g., mode switch)
      if (forceUpdate || payload.end_ms > lastEndMsRef.current) {
        lastEndMsRef.current = payload.end_ms;
        setData(payload);
      }
      setError(null);
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    }
  }, []);

  // Frame-based update loop using requestAnimationFrame
  // This ensures updates are synchronized with browser rendering and prevents
  // accumulating pending requests when data arrives faster than we can process
  useEffect(() => {
    if (!isRunning) return;

    let rafId: number;
    let isActive = true;

    const updateLoop = async () => {
      if (!isActive) return;

      // Skip if previous fetch is still in progress
      if (!isFetchingRef.current) {
        isFetchingRef.current = true;
        try {
          await fetchData();
        } finally {
          isFetchingRef.current = false;
        }
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
  }, [fetchData, isRunning]);

  // Toggle pause/resume (only affects frontend polling, backend continues collecting)
  const toggleRunning = useCallback(() => {
    setIsRunning((prev) => !prev);
    // Note: We intentionally don't call set_plotter_enabled here
    // Backend keeps collecting data so we can show it when resumed
  }, []);

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

  // Convert aggregated line data to format for LineChart (with band data for MinMax)
  const { lineData: simpleLineData, bandData } = useMemo(() => {
    if (!data) return { lineData: {}, bandData: {} };
    return convertAggregatedData(data.line_data);
  }, [data]);

  // Check if we have line data to display
  const hasLineData =
    data && Object.keys(data.line_data).some((key) => data.line_data[key].length > 0);

  // Build state rows from state_data (sorted alphabetically)
  const stateRows: StateRow[] = useMemo(() => {
    if (!data) return [];
    return Object.entries(data.state_data)
      .filter(([, changes]) => changes.length > 0)
      .map(([channel, changes]) => ({
        channel,
        segments: changes.map((c) => ({
          start_ms: c.start_ms,
          end_ms: c.end_ms ?? Date.now(),
          state: c.state,
        })),
      }))
      .sort((a, b) => a.channel.localeCompare(b.channel));
  }, [data]);

  return (
    <div className="plotter-window">
      {/* Header */}
      <div className="plotter-header">
        <h1>Serial Plotter</h1>
        <div className="plotter-controls">
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
        {hasLineData ? (
          <>
            {/* Line Chart (with integrated State Timeline) */}
            <div className="chart-area" ref={chartContainerRef}>
              <LineChart
                data={simpleLineData}
                bandData={bandData}
                isPaused={!isRunning}
                stateRows={stateRows}
                hiddenChannels={hiddenChannels}
              />
            </div>

            {/* Channel legend */}
            <div className="channel-legend">
              {(() => {
                // Get sorted channels from simpleLineData keys (exactly matches LineChart.buildChartData)
                const allChannelsSorted = Object.keys(simpleLineData).sort();

                return data!.channels
                  .filter((ch) => ch.channel_type === 'Line' || ch.channel_type === 'Auto')
                  .map((channel) => {
                    const isHidden = hiddenChannels.has(channel.name);
                    // Use index in sorted allChannels for consistent color (matches LineChart)
                    const colorIndex = allChannelsSorted.indexOf(channel.name);
                    const color = COLORS[colorIndex % COLORS.length];
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
                  });
              })()}
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
        <span className="status-text">{isRunning ? '● Recording' : '○ Paused'}</span>
        {data && (
          <span className="duration">Duration: {formatDuration(data.end_ms - data.start_ms)}</span>
        )}
        <div className="aggregation-control">
          <span className="aggregation-label">Downsample:</span>
          <select
            className="aggregation-select"
            value={aggregationMode}
            onChange={(e) => {
              const mode = e.target.value as AggregationMode;
              setAggregationMode(mode);
              invoke('set_aggregation_mode', { mode })
                .then(() => fetchData(true)) // Force update to reflect mode change immediately
                .catch(console.error);
            }}
            title="Downsampling mode"
          >
            <option value="Lttb">LTTB</option>
            <option value="Average">Average</option>
          </select>
        </div>
      </div>
    </div>
  );
}
