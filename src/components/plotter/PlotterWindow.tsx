import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import LineChart from './LineChart';
import { StateRow } from './stateTimelinePlugin';
import './PlotterWindow.css';

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

// Convert aggregated points to simple [timestamp, value] format for LineChart
function convertAggregatedToSimple(
  data: Record<string, AggregatedPoint[]>
): Record<string, [number, number][]> {
  const result: Record<string, [number, number][]> = {};
  for (const [channel, points] of Object.entries(data)) {
    result[channel] = points.map((p) => {
      if (p.type === 'MinMax') {
        // For MinMax, use average of min/max for display
        const avg = ((p.min ?? 0) + (p.max ?? 0)) / 2;
        return [p.ts, avg];
      }
      return [p.ts, p.value ?? 0];
    });
  }
  return result;
}

export default function PlotterWindow() {
  const [data, setData] = useState<PlotterRangedPayload | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isRunning, setIsRunning] = useState(true);
  const [threadStarted, setThreadStarted] = useState(false);

  // Ref for chart container to get pixel width
  const chartContainerRef = useRef<HTMLDivElement>(null);

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
  const fetchData = useCallback(async () => {
    // Get pixel width from chart container
    const pixelWidth = chartContainerRef.current?.clientWidth ?? 800;

    // Build request - always use null for realtime mode
    const request: PlotterDataRequest = {
      time_min_ms: null,
      time_max_ms: null,
      pixel_width: pixelWidth,
      is_realtime: true,
    };

    try {
      const payload = await invoke<PlotterRangedPayload>('get_plotter_data_ranged', { request });
      setData(payload);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // Poll for data updates
  useEffect(() => {
    if (!isRunning) return;

    // Use setTimeout for initial fetch to avoid synchronous setState in effect
    const timeoutId = setTimeout(fetchData, 0);
    const interval = setInterval(fetchData, 100); // 10Hz update rate

    return () => {
      clearTimeout(timeoutId);
      clearInterval(interval);
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

  // Convert aggregated line data to simple format for LineChart
  const simpleLineData = useMemo(() => {
    if (!data) return {};
    return convertAggregatedToSimple(data.line_data);
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
              <LineChart data={simpleLineData} isPaused={!isRunning} stateRows={stateRows} />
            </div>

            {/* Channel legend */}
            <div className="channel-legend">
              {data!.channels
                .filter((ch) => ch.channel_type === 'Line' || ch.channel_type === 'Auto')
                .map((channel) => (
                  <div key={channel.name} className="legend-item">
                    <span className="legend-name">{channel.name}:</span>
                    <span className="legend-value">{channel.latest_value ?? '--'}</span>
                    <span className="legend-count">{channel.point_count} pts</span>
                  </div>
                ))}
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
        <span className="version">Phase B</span>
      </div>
    </div>
  );
}
