import { useRef, useEffect, useCallback } from 'react';
import uPlot from 'uplot';
import 'uplot/dist/uPlot.min.css';
import { stateTimelinePlugin, calculateStateTimelineHeight, StateRow } from './stateTimelinePlugin';
import './LineChart.css';
import { COLORS } from './chartColors';

import type { MinMaxBandData } from './PlotterWindow';

interface LineChartProps {
  // Data: { channelName: [[timestamps], [values]] }
  data: Record<string, [number, number][]>;
  // MinMax band data for channels (optional, for MinMax aggregation mode)
  bandData?: Record<string, MinMaxBandData>;
  // Whether the chart is paused
  isPaused?: boolean;
  // State timeline rows to display below the chart
  stateRows?: StateRow[];
  // Callback when visible time range changes (in seconds)
  onTimeRangeChange?: (min: number, max: number) => void;
  // Channels to hide from the chart
  hiddenChannels?: Set<string>;
}

/** Calculate Y-axis range from chart data */
function calculateYRange(chartData: uPlot.AlignedData): { yMin: number; yMax: number } {
  let yMin = Infinity;
  let yMax = -Infinity;
  for (let i = 1; i < chartData.length; i++) {
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
  data,
  bandData = {},
  isPaused = false,
  stateRows = [],
  onTimeRangeChange,
  hiddenChannels = new Set(),
}: LineChartProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<uPlot | null>(null);
  const channelsRef = useRef<string[]>([]);
  const stateRowsRef = useRef<StateRow[]>([]);
  const isZoomedRef = useRef(false);
  const manualYScaleRef = useRef<{ min: number; max: number } | null>(null);
  const manualXRangeRef = useRef<number | null>(null);
  const onTimeRangeChangeRef = useRef(onTimeRangeChange);
  const debounceTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Debounce delay in ms for time range change notifications
  const DEBOUNCE_DELAY_MS = 200;

  // Keep ref up to date
  useEffect(() => {
    onTimeRangeChangeRef.current = onTimeRangeChange;
  }, [onTimeRangeChange]);

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
  const bandDataRef = useRef<Record<string, MinMaxBandData>>(bandData);
  useEffect(() => {
    bandDataRef.current = bandData;
  }, [bandData]);

  // MinMax band drawing plugin - draws filled areas for min/max ranges
  const minMaxBandPlugin = useCallback(
    (getChannels: () => string[], getAllChannels: () => string[]): uPlot.Plugin => {
      return {
        hooks: {
          draw: (u: uPlot) => {
            const ctx = u.ctx;
            const bands = bandDataRef.current;
            const channels = getChannels();
            const allChannels = getAllChannels();

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

            // Draw bands for each channel with band data
            for (const channel of channels) {
              const band = bands[channel];
              if (!band || band.timestamps.length === 0) continue;

              // Get channel color with transparency
              const colorIndex = allChannels.indexOf(channel);
              const color = COLORS[colorIndex % COLORS.length];
              // Convert hex to rgba with 0.2 alpha
              const r = parseInt(color.slice(1, 3), 16);
              const g = parseInt(color.slice(3, 5), 16);
              const b = parseInt(color.slice(5, 7), 16);
              ctx.fillStyle = `rgba(${r}, ${g}, ${b}, 0.25)`;

              // Draw filled area between min and max
              ctx.beginPath();

              // Forward path (min values, left to right)
              let started = false;
              for (let i = 0; i < band.timestamps.length; i++) {
                const x = u.valToPos(band.timestamps[i] / 1000, 'x', true);
                const y = u.valToPos(band.min[i], 'y', true);
                if (!isFinite(x) || !isFinite(y)) continue;
                if (!started) {
                  ctx.moveTo(x, y);
                  started = true;
                } else {
                  ctx.lineTo(x, y);
                }
              }

              // Backward path (max values, right to left)
              for (let i = band.timestamps.length - 1; i >= 0; i--) {
                const x = u.valToPos(band.timestamps[i] / 1000, 'x', true);
                const y = u.valToPos(band.max[i], 'y', true);
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

  // Build uPlot data format: [timestamps, ...values]
  const buildChartData = useCallback(() => {
    // Get all channels sorted (for consistent color indexing)
    const allChannels = Object.keys(data).sort();
    // Filter out hidden channels for display
    const visibleChannels = allChannels.filter((ch) => !hiddenChannels.has(ch));

    if (visibleChannels.length === 0) {
      return { data: [[]] as uPlot.AlignedData, channels: [], allChannels };
    }

    const timestampSet = new Set<number>();
    for (const channel of visibleChannels) {
      const points = data[channel];
      for (const [t] of points) {
        timestampSet.add(t);
      }
    }
    const timestamps = Array.from(timestampSet).sort((a, b) => a - b);

    if (timestamps.length === 0) {
      return { data: [[]] as uPlot.AlignedData, channels: [], allChannels };
    }

    const chartData: (number | null)[][] = [timestamps.map((t) => t / 1000)];

    for (const channel of visibleChannels) {
      const points = data[channel];
      const valueMap = new Map<number, number>();
      for (const [t, v] of points) {
        valueMap.set(t, v);
      }
      const values = timestamps.map((t) => valueMap.get(t) ?? null);
      chartData.push(values);
    }

    return { data: chartData as uPlot.AlignedData, channels: visibleChannels, allChannels };
  }, [data, hiddenChannels]);

  // Setup event handlers for the chart
  const setupEventHandlers = useCallback((u: uPlot) => {
    // Double-click to reset zoom
    u.over.addEventListener('dblclick', () => {
      isZoomedRef.current = false;
      manualYScaleRef.current = null;
      manualXRangeRef.current = null;
      const chartData = u.data;
      if (chartData && chartData[0] && chartData[0].length > 0) {
        const xData = chartData[0] as number[];
        const xMin = Math.min(...xData);
        const xMax = Math.max(...xData);
        u.setScale('x', { min: xMin, max: xMax });

        const { yMin, yMax } = calculateYRange(chartData);
        if (yMin !== Infinity && yMax !== -Infinity) {
          const padding = (yMax - yMin) * 0.1;
          u.setScale('y', { min: yMin - padding, max: yMax + padding });
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

  // Build chart options
  const buildChartOptions = useCallback(
    (
      channels: string[],
      allChannels: string[],
      stateRowCount: number,
      width: number,
      height: number
    ): uPlot.Options => {
      const series: uPlot.Series[] = [
        { label: 'Time' },
        ...channels.map((ch) => {
          // Use index in allChannels for consistent coloring
          const colorIndex = allChannels.indexOf(ch);
          return {
            label: ch,
            stroke: COLORS[colorIndex % COLORS.length],
            width: 2,
            spanGaps: true,
          };
        }),
      ];

      const stateTimelineHeight = calculateStateTimelineHeight(stateRowCount);

      // Build plugins array
      const plugins: uPlot.Plugin[] = [];

      // Add MinMax band plugin (draws before lines, so bands are behind)
      plugins.push(
        minMaxBandPlugin(
          () => channelsRef.current,
          () => Object.keys(data).sort()
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
    [setupEventHandlers, debouncedTimeRangeChange, minMaxBandPlugin, data]
  );

  // Update chart scales based on current data and zoom state
  const updateChartScales = useCallback((chart: uPlot, chartData: uPlot.AlignedData) => {
    const prevXScale = { min: chart.scales.x.min, max: chart.scales.x.max };
    const prevYScale = { min: chart.scales.y.min, max: chart.scales.y.max };

    chart.setData(chartData);

    if (isZoomedRef.current) {
      if (prevXScale.min !== undefined && prevXScale.max !== undefined) {
        chart.setScale('x', { min: prevXScale.min, max: prevXScale.max });
      }
      if (prevYScale.min !== undefined && prevYScale.max !== undefined) {
        chart.setScale('y', { min: prevYScale.min, max: prevYScale.max });
      }
    } else {
      if (chartData[0] && chartData[0].length > 0) {
        const xData = chartData[0] as number[];
        const xMin = Math.min(...xData);
        const xMax = Math.max(...xData);

        if (manualXRangeRef.current !== null) {
          const range = manualXRangeRef.current;
          chart.setScale('x', { min: xMax - range, max: xMax });
        } else {
          chart.setScale('x', { min: xMin, max: xMax });
        }

        const { yMin, yMax } = calculateYRange(chartData);
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
  }, []);

  // Main chart effect
  useEffect(() => {
    if (!containerRef.current) return;

    const { data: chartData, channels, allChannels } = buildChartData();

    // If no data or no channels, destroy chart
    if (channels.length === 0 || chartData[0].length === 0) {
      if (chartRef.current) {
        chartRef.current.destroy();
        chartRef.current = null;
      }
      return;
    }

    // Check if chart needs to be recreated
    const channelsChanged =
      channels.length !== channelsRef.current.length ||
      channels.some((ch, i) => ch !== channelsRef.current[i]);

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

      const opts = buildChartOptions(
        channels,
        allChannels,
        stateRows.length,
        containerRef.current.clientWidth,
        containerRef.current.clientHeight
      );

      chartRef.current = new uPlot(opts, chartData, containerRef.current);
      channelsRef.current = channels;
    } else {
      // Just update data
      stateRowsRef.current = stateRows;
      updateChartScales(chartRef.current!, chartData);
    }
  }, [buildChartData, stateRows, buildChartOptions, updateChartScales]);

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
