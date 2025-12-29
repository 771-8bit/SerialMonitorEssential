import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import LineChart from './LineChart';
import StateTimeline from './StateTimeline';
import type { StateRow, StateSegment } from './StateTimeline';
import './PlotterWindow.css';

// Types matching Rust PlotterDataPayload
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

interface PlotterDataPayload {
  channels: ChannelInfo[];
  line_data: Record<string, [number, number][]>;
  state_data: Record<string, StateChange[]>;
  start_ms: number;
  end_ms: number;
}

export default function PlotterWindow() {
  const [data, setData] = useState<PlotterDataPayload | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isRunning, setIsRunning] = useState(true);
  const [threadStarted, setThreadStarted] = useState(false);
  const [timeRange, setTimeRange] = useState<{ min: number; max: number } | null>(null);

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

  // Fetch plotter data
  const fetchData = useCallback(async () => {
    try {
      const payload = await invoke<PlotterDataPayload>('get_plotter_data');
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

  // Check if we have line data to display
  const hasLineData =
    data && Object.keys(data.line_data).some((key) => data.line_data[key].length > 0);

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
            {/* Line Chart */}
            <div className="chart-area">
              <LineChart
                data={data!.line_data}
                isPaused={!isRunning}
                onTimeRangeChange={(min, max) => setTimeRange({ min, max })}
              />
            </div>

            {/* State Timeline (below chart) */}
            {(() => {
              // Build state rows from state_data (sorted alphabetically)
              const stateRows: StateRow[] = Object.entries(data!.state_data)
                .filter(([, changes]) => changes.length > 0)
                .map(([channel, changes]) => ({
                  channel,
                  segments: changes.map(c => ({
                    start_ms: c.start_ms,
                    end_ms: c.end_ms ?? Date.now(),
                    state: c.state,
                  })) as StateSegment[],
                }))
                .sort((a, b) => a.channel.localeCompare(b.channel));

              if (stateRows.length === 0) return null;

              // Use timeRange from LineChart if available, otherwise use data range
              const timeMin = timeRange?.min ?? data!.start_ms / 1000;
              const timeMax = timeRange?.max ?? data!.end_ms / 1000;

              return (
                <StateTimeline
                  rows={stateRows}
                  timeMin={timeMin}
                  timeMax={timeMax}
                />
              );
            })()}

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
                <strong>CSV Format:</strong> <code>10,20,30</code>
              </li>
              <li>
                <strong>Labeled values:</strong> <code>temp:25.5,humidity:60</code>
              </li>
              <li>
                <strong>State (CSV/Labeled):</strong> <code>RUNNING</code> or <code>state:RUNNING</code>
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
