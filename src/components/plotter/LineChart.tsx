import { useRef, useEffect, useCallback } from 'react';
import uPlot from 'uplot';
import 'uplot/dist/uPlot.min.css';
import { stateTimelinePlugin, calculateStateTimelineHeight, StateRow } from './stateTimelinePlugin';
import './LineChart.css';
import { COLORS } from './chartColors';

import type { BandSeriesData } from './PlotterWindow';

// New props interface - accepts pre-aligned data from backend
interface LineChartProps {
  // uPlot aligned data: [timestamps, ch0_values, ch1_values, ...]
  // Timestamps are in seconds, values are numbers or null
  alignedData: (number | null)[][];
  // Channel names in order (matches alignedData columns starting at index 1)
  channelNames: string[];
  // MinMax band data for channels (optional, for Average aggregation mode)
  // Uses same indices as alignedData[0] (timestamps)
  bandData?: Record<string, BandSeriesData> | null;
  // Whether the chart is paused
  isPaused?: boolean;
  // State timeline rows to display below the chart
  stateRows?: StateRow[];
  // Callback when visible time range changes (in seconds)
  onTimeRangeChange?: (min: number, max: number) => void;
  // Channels to hide from the chart (uses series.show for efficient toggling)
  hiddenChannels?: Set<string>;
}

/** Calculate Y-axis range from chart data, respecting hidden series */
function calculateYRange(
  chartData: uPlot.AlignedData,
  hiddenSeriesIndices: Set<number>
): { yMin: number; yMax: number } {
  let yMin = Infinity;
  let yMax = -Infinity;
  for (let i = 1; i < chartData.length; i++) {
    // Skip hidden series
    if (hiddenSeriesIndices.has(i)) continue;
    const yData = chartData[i] as (number | null)[];
    for (const v of yData) {
      if (v !== null) {
        yMin = Math.min(yMin, v);
        yMax = Math.max(yMax, v);
      }
    }
  }
  return { yMin, yMax };
}

export default function LineChart({
  alignedData,
  channelNames,
  bandData = null,
  isPaused = false,
  stateRows = [],
  onTimeRangeChange,
  hiddenChannels = new Set(),
}: LineChartProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<uPlot | null>(null);
  const channelsRef = useRef<string[]>([]);
  const stateRowsRef = useRef<StateRow[]>([]);
  const hiddenChannelsRef = useRef<Set<string>>(hiddenChannels);
  const isZoomedRef = useRef(false);
  const manualYScaleRef = useRef<{ min: number; max: number } | null>(null);
  const manualXRangeRef = useRef<number | null>(null);
  const onTimeRangeChangeRef = useRef(onTimeRangeChange);
  const debounceTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Debounce delay in ms for time range change notifications
  const DEBOUNCE_DELAY_MS = 200;

  // Keep refs up to date
  useEffect(() => {
    onTimeRangeChangeRef.current = onTimeRangeChange;
  }, [onTimeRangeChange]);

  useEffect(() => {
    hiddenChannelsRef.current = hiddenChannels;
  }, [hiddenChannels]);

  // Debounced time range change handler
  const debouncedTimeRangeChange = useCallback((min: number, max: number) => {
    if (debounceTimerRef.current) {
      clearTimeout(debounceTimerRef.current);
    }
    debounceTimerRef.current = setTimeout(() => {
      if (onTimeRangeChangeRef.current) {
        onTimeRangeChangeRef.current(min, max);
      }
    }, DEBOUNCE_DELAY_MS);
  }, []);

  // Ref for band data (updated each render, used by plugin)
  const bandDataRef = useRef<Record<string, BandSeriesData> | null>(bandData);
  useEffect(() => {
    bandDataRef.current = bandData;
  }, [bandData]);

  // MinMax band drawing plugin - draws filled areas for min/max ranges
  // Updated to use new BandSeriesData format (same indices as timestamps)
  const minMaxBandPlugin = useCallback(
    (getChannelNames: () => string[], getTimestamps: () => (number | null)[]): uPlot.Plugin => {
      return {
        hooks: {
          draw: (u: uPlot) => {
            const ctx = u.ctx;
            const bands = bandDataRef.current;
            const channelNamesList = getChannelNames();
            const timestamps = getTimestamps();

            if (!bands || Object.keys(bands).length === 0) return;

            ctx.save();

            // Clip to plot area
            const plotLeft = u.bbox.left;
            const plotTop = u.bbox.top;
            const plotWidth = u.bbox.width;
            const plotHeight = u.bbox.height;
            ctx.beginPath();
            ctx.rect(plotLeft, plotTop, plotWidth, plotHeight);
            ctx.clip();

            // Draw bands for each channel with band data (if not hidden)
            for (const channel of channelNamesList) {
              // Skip hidden channels
              if (hiddenChannelsRef.current.has(channel)) continue;

              const band = bands[channel];
              if (!band || band.min.length === 0) continue;

              // Get channel color with transparency
              const colorIndex = channelNamesList.indexOf(channel);
              const color = COLORS[colorIndex % COLORS.length];
              // Convert hex to rgba with 0.25 alpha
              const r = parseInt(color.slice(1, 3), 16);
              const g = parseInt(color.slice(3, 5), 16);
              const b = parseInt(color.slice(5, 7), 16);
              ctx.fillStyle = `rgba(${r}, ${g}, ${b}, 0.25)`;

              // Draw filled area between min and max
              ctx.beginPath();

              // Forward path (min values, left to right)
              let started = false;
              for (let i = 0; i < timestamps.length; i++) {
                const ts = timestamps[i];
                const minVal = band.min[i];
                if (ts === null || minVal === null) continue;
                const x = u.valToPos(ts, 'x', true);
                const y = u.valToPos(minVal, 'y', true);
                if (!isFinite(x) || !isFinite(y)) continue;
                if (!started) {
                  ctx.moveTo(x, y);
                  started = true;
                } else {
                  ctx.lineTo(x, y);
                }
              }

              // Backward path (max values, right to left)
              for (let i = timestamps.length - 1; i >= 0; i--) {
                const ts = timestamps[i];
                const maxVal = band.max[i];
                if (ts === null || maxVal === null) continue;
                const x = u.valToPos(ts, 'x', true);
                const y = u.valToPos(maxVal, 'y', true);
                if (!isFinite(x) || !isFinite(y)) continue;
                ctx.lineTo(x, y);
              }

              ctx.closePath();
              ctx.fill();
            }

            ctx.restore();
          },
        },
      };
    },
    []
  );

  // Setup event handlers for the chart
  const setupEventHandlers = useCallback((u: uPlot) => {
    // Double-click to reset zoom
    u.over.addEventListener('dblclick', () => {
      isZoomedRef.current = false;
      manualYScaleRef.current = null;
      manualXRangeRef.current = null;
      const chartData = u.data;
      if (chartData && chartData[0] && chartData[0].length > 0) {
        const xData = chartData[0].filter((v): v is number => v !== null);
        if (xData.length > 0) {
          const xMin = Math.min(...xData);
          const xMax = Math.max(...xData);
          u.setScale('x', { min: xMin, max: xMax });

          // Calculate Y range excluding hidden series
          const hiddenIndices = new Set<number>();
          for (let i = 0; i < channelsRef.current.length; i++) {
            if (hiddenChannelsRef.current.has(channelsRef.current[i])) {
              hiddenIndices.add(i + 1); // +1 because index 0 is timestamps
            }
          }
          const { yMin, yMax } = calculateYRange(chartData, hiddenIndices);
          if (yMin !== Infinity && yMax !== -Infinity) {
            const padding = (yMax - yMin) * 0.1;
            u.setScale('y', { min: yMin - padding, max: yMax + padding });
          }
        }
      }
    });

    // Wheel zoom
    u.over.addEventListener('wheel', (e) => {
      e.preventDefault();

      const factor = e.deltaY > 0 ? 1.1 : 0.9;
      const rect = u.over.getBoundingClientRect();
      const xPos = e.clientX - rect.left;
      const yPos = e.clientY - rect.top;

      const xScale = u.scales.x;
      const yScale = u.scales.y;

      if (
        xScale.min === undefined ||
        xScale.max === undefined ||
        yScale.min === undefined ||
        yScale.max === undefined
      )
        return;

      const xVal = u.posToVal(xPos, 'x');
      const yVal = u.posToVal(yPos, 'y');

      if (e.shiftKey) {
        const xRange = xScale.max - xScale.min;
        const newRange = xRange * factor;
        const ratio = (xVal - xScale.min) / xRange;
        const newMin = xVal - newRange * ratio;
        const newMax = xVal + newRange * (1 - ratio);
        u.setScale('x', { min: newMin, max: newMax });
        manualXRangeRef.current = newRange;
      } else {
        const yRange = yScale.max - yScale.min;
        const newRange = yRange * factor;
        const ratio = (yVal - yScale.min) / yRange;
        const newMin = yVal - newRange * ratio;
        const newMax = yVal + newRange * (1 - ratio);
        u.setScale('y', { min: newMin, max: newMax });
        manualYScaleRef.current = { min: newMin, max: newMax };
      }
    });
  }, []);

  // Build chart options with series.show for hidden channels
  const buildChartOptions = useCallback(
    (
      channels: string[],
      stateRowCount: number,
      width: number,
      height: number,
      currentHiddenChannels: Set<string>
    ): uPlot.Options => {
      const series: uPlot.Series[] = [
        { label: 'Time' },
        ...channels.map((ch, i) => ({
          label: ch,
          stroke: COLORS[i % COLORS.length],
          width: 2,
          spanGaps: true,
          show: !currentHiddenChannels.has(ch), // Use series.show for hiding
        })),
      ];

      const stateTimelineHeight = calculateStateTimelineHeight(stateRowCount);

      // Build plugins array
      const plugins: uPlot.Plugin[] = [];

      // Add MinMax band plugin (draws before lines, so bands are behind)
      plugins.push(
        minMaxBandPlugin(
          () => channelsRef.current,
          () => (chartRef.current?.data[0] as (number | null)[]) ?? []
        )
      );

      // Add state timeline plugin if needed
      if (stateRowCount > 0) {
        plugins.push(
          stateTimelinePlugin({
            getRows: () => stateRowsRef.current,
            rowHeight: 24,
            rowGap: 2,
            showLabel: true,
          })
        );
      }

      return {
        width,
        height,
        series,
        padding: [null, null, stateTimelineHeight, null],
        scales: {
          x: { time: false, auto: false },
          y: { auto: false },
        },
        axes: [
          { stroke: '#888', grid: { stroke: '#333' }, ticks: { stroke: '#444' } },
          { stroke: '#888', grid: { stroke: '#333' }, ticks: { stroke: '#444' } },
        ],
        legend: { show: true },
        cursor: {
          show: true,
          points: { show: true },
          drag: { x: true, y: true, setScale: true },
        },
        hooks: {
          setScale: [
            (u, key) => {
              if (key === 'x') {
                const xMin = u.scales.x.min;
                const xMax = u.scales.x.max;
                if (xMin !== undefined && xMax !== undefined) {
                  debouncedTimeRangeChange(xMin, xMax);
                }
              }
            },
          ],
          setSelect: [
            () => {
              isZoomedRef.current = true;
            },
          ],
          init: [(u) => setupEventHandlers(u)],
        },
        plugins,
      };
    },
    [setupEventHandlers, debouncedTimeRangeChange, minMaxBandPlugin]
  );

  // Update chart scales based on current data and zoom state
  const updateChartScales = useCallback(
    (chart: uPlot, chartData: uPlot.AlignedData, currentHiddenChannels: Set<string>) => {
      const prevXScale = { min: chart.scales.x.min, max: chart.scales.x.max };
      const prevYScale = { min: chart.scales.y.min, max: chart.scales.y.max };

      chart.setData(chartData);

      // Calculate hidden series indices
      const hiddenIndices = new Set<number>();
      for (let i = 0; i < channelsRef.current.length; i++) {
        if (currentHiddenChannels.has(channelsRef.current[i])) {
          hiddenIndices.add(i + 1); // +1 because index 0 is timestamps
        }
      }

      if (isZoomedRef.current) {
        if (prevXScale.min !== undefined && prevXScale.max !== undefined) {
          chart.setScale('x', { min: prevXScale.min, max: prevXScale.max });
        }
        if (prevYScale.min !== undefined && prevYScale.max !== undefined) {
          chart.setScale('y', { min: prevYScale.min, max: prevYScale.max });
        }
      } else {
        if (chartData[0] && chartData[0].length > 0) {
          const xData = chartData[0].filter((v): v is number => v !== null);
          if (xData.length > 0) {
            const xMin = Math.min(...xData);
            const xMax = Math.max(...xData);

            if (manualXRangeRef.current !== null) {
              const range = manualXRangeRef.current;
              chart.setScale('x', { min: xMax - range, max: xMax });
            } else {
              chart.setScale('x', { min: xMin, max: xMax });
            }

            const { yMin, yMax } = calculateYRange(chartData, hiddenIndices);
            if (yMin !== Infinity && yMax !== -Infinity) {
              if (manualYScaleRef.current) {
                chart.setScale('y', manualYScaleRef.current);
              } else {
                const padding = (yMax - yMin) * 0.1 || 1;
                chart.setScale('y', { min: yMin - padding, max: yMax + padding });
              }
            }
          }
        }
      }
    },
    []
  );

  // Update series visibility when hiddenChannels changes
  useEffect(() => {
    if (!chartRef.current) return;
    const chart = chartRef.current;

    // Update each series' show property
    for (let i = 0; i < channelsRef.current.length; i++) {
      const channel = channelsRef.current[i];
      const seriesIdx = i + 1; // +1 because index 0 is time
      const shouldShow = !hiddenChannels.has(channel);
      if (chart.series[seriesIdx]) {
        chart.series[seriesIdx].show = shouldShow;
      }
    }

    // Redraw to apply visibility changes
    chart.redraw();
  }, [hiddenChannels]);

  // Main chart effect
  useEffect(() => {
    if (!containerRef.current) return;

    // Use aligned data directly (no transformation needed!)
    const chartData = alignedData as uPlot.AlignedData;

    // If no data or no channels, destroy chart
    if (channelNames.length === 0 || chartData[0].length === 0) {
      if (chartRef.current) {
        chartRef.current.destroy();
        chartRef.current = null;
      }
      return;
    }

    // Check if chart needs to be recreated
    const channelsChanged =
      channelNames.length !== channelsRef.current.length ||
      channelNames.some((ch, i) => ch !== channelsRef.current[i]);

    const stateRowsChanged =
      stateRows.length !== stateRowsRef.current.length ||
      stateRows.some((row, i) => row.channel !== stateRowsRef.current[i]?.channel);

    const needsRecreate = channelsChanged || stateRowsChanged || !chartRef.current;

    if (needsRecreate) {
      // Need to recreate chart
      if (chartRef.current) {
        chartRef.current.destroy();
      }

      stateRowsRef.current = stateRows;
      channelsRef.current = channelNames;

      const opts = buildChartOptions(
        channelNames,
        stateRows.length,
        containerRef.current.clientWidth,
        containerRef.current.clientHeight,
        hiddenChannels
      );

      chartRef.current = new uPlot(opts, chartData, containerRef.current);
    } else {
      // Just update data
      stateRowsRef.current = stateRows;
      updateChartScales(chartRef.current!, chartData, hiddenChannels);
    }
  }, [alignedData, channelNames, stateRows, buildChartOptions, updateChartScales, hiddenChannels]);

  // Handle resize
  useEffect(() => {
    if (!containerRef.current) return;

    const resizeObserver = new ResizeObserver((entries) => {
      if (chartRef.current && entries[0]) {
        const { width, height } = entries[0].contentRect;
        chartRef.current.setSize({ width, height });
      }
    });

    resizeObserver.observe(containerRef.current);

    return () => {
      resizeObserver.disconnect();
    };
  }, []);

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      if (chartRef.current) {
        chartRef.current.destroy();
        chartRef.current = null;
      }
      // Clear debounce timer
      if (debounceTimerRef.current) {
        clearTimeout(debounceTimerRef.current);
      }
    };
  }, []);

  return <div ref={containerRef} className={`line-chart-container ${isPaused ? 'paused' : ''}`} />;
}
