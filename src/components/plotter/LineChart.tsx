import { useRef, useEffect, useCallback, forwardRef, useImperativeHandle } from 'react';
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
  // Callback when visible time range changes (in seconds).
  // Only fired while NOT in follow mode - in follow mode the parent owns the
  // x scale (via setXWindow) and re-notifying it would create a fetch loop.
  onTimeRangeChange?: (min: number, max: number) => void;
  // Channels to hide from the chart (uses series.show for efficient toggling)
  hiddenChannels?: Set<string>;
  // LIVE follow mode: the parent drives the x window imperatively through
  // setXWindow(), so data updates must not touch the scales.
  followMode?: boolean;
  // Fired when the user zooms (wheel) or drag-selects - parent enters Inspect
  onUserInteraction?: () => void;
  // Fired on double click - parent returns to LIVE follow mode
  onLiveRequest?: () => void;
}

/** Imperative API used by PlotterWindow's rAF loop (no React state per frame) */
export interface LineChartHandle {
  /** Set the visible x window (seconds) and re-evaluate the y auto-range */
  setXWindow: (minSec: number, maxSec: number) => void;
  /** Drop all zoom/manual-scale state (returning to LIVE) */
  resetView: () => void;
  /** Drop only the y auto-range hysteresis state (e.g. window width changed) */
  resetYRange: () => void;
}

// Y auto-range hysteresis:
// - expand immediately when data leaves the current range (never clip a spike)
// - shrink only after the data has occupied < 60% of the span for 3 seconds
const Y_SHRINK_OCCUPANCY = 0.6;
const Y_SHRINK_DELAY_MS = 3000;

/** Round away float noise introduced by the nice-range snapping */
function cleanFloat(v: number): number {
  return Number.isFinite(v) ? parseFloat(v.toPrecision(12)) : v;
}

/** Nearest step from the 1-2-5 series at or above `raw` */
function niceStep(raw: number): number {
  if (!Number.isFinite(raw) || raw <= 0) return 1;
  const exp = Math.floor(Math.log10(raw));
  const pow = Math.pow(10, exp);
  const frac = raw / pow;
  const mult = frac <= 1 ? 1 : frac <= 2 ? 2 : frac <= 5 ? 5 : 10;
  return mult * pow;
}

/**
 * "Nice" y range: pad the extents by 10%, then round min down / max up to a
 * 1-2-5 step of about 1/8 of the span. Keeps y changes rare and discrete
 * instead of jittering on every frame.
 */
function niceYRange(min: number, max: number): { min: number; max: number } {
  if (!Number.isFinite(min) || !Number.isFinite(max)) return { min: 0, max: 1 };
  let span = max - min;
  if (span <= 0) span = Math.abs(max) || 1;
  const pad = span * 0.1;
  let lo = min - pad;
  let hi = max + pad;
  const step = niceStep((hi - lo) / 8);
  lo = Math.floor(lo / step) * step;
  hi = Math.ceil(hi / step) * step;
  if (hi <= lo) hi = lo + step;
  return { min: cleanFloat(lo), max: cleanFloat(hi) };
}

/**
 * Calculate Y-axis range from chart data, respecting hidden series.
 * When xMin/xMax are given, only samples inside that x window are considered.
 */
function calculateYRange(
  chartData: uPlot.AlignedData,
  hiddenSeriesIndices: Set<number>,
  xMin?: number,
  xMax?: number
): { yMin: number; yMax: number } {
  let yMin = Infinity;
  let yMax = -Infinity;
  const windowed = xMin !== undefined && xMax !== undefined;
  const xData = chartData[0] as (number | null)[] | undefined;
  for (let i = 1; i < chartData.length; i++) {
    // Skip hidden series
    if (hiddenSeriesIndices.has(i)) continue;
    const yData = chartData[i] as (number | null)[];
    for (let j = 0; j < yData.length; j++) {
      if (windowed) {
        const x = xData?.[j];
        if (x == null || x < xMin! || x > xMax!) continue;
      }
      const v = yData[j];
      if (v !== null && v !== undefined && Number.isFinite(v)) {
        if (v < yMin) yMin = v;
        if (v > yMax) yMax = v;
      }
    }
  }
  return { yMin, yMax };
}

/** min/max of an array of possibly-null numbers (no spread - arrays can be large) */
function extentOf(values: (number | null)[]): { min: number; max: number } | null {
  let min = Infinity;
  let max = -Infinity;
  for (const v of values) {
    if (v === null || v === undefined || !Number.isFinite(v)) continue;
    if (v < min) min = v;
    if (v > max) max = v;
  }
  return min === Infinity ? null : { min, max };
}

const LineChart = forwardRef<LineChartHandle, LineChartProps>(function LineChart(
  {
    alignedData,
    channelNames,
    bandData = null,
    isPaused = false,
    stateRows = [],
    onTimeRangeChange,
    hiddenChannels = new Set(),
    followMode = false,
    onUserInteraction,
    onLiveRequest,
  },
  ref
) {
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
  const followModeRef = useRef(followMode);
  const onUserInteractionRef = useRef(onUserInteraction);
  const onLiveRequestRef = useRef(onLiveRequest);
  // Currently applied auto y range (null = needs (re)computation)
  const yRangeRef = useRef<{ min: number; max: number } | null>(null);
  // Timestamp (ms) since when the data has been "too small" for the y range
  const shrinkSinceRef = useRef<number | null>(null);

  // Debounce delay in ms for time range change notifications
  const DEBOUNCE_DELAY_MS = 200;

  // Keep refs up to date (declared before the chart effects so the refs are
  // already current when those run in the same commit)
  useEffect(() => {
    onTimeRangeChangeRef.current = onTimeRangeChange;
  }, [onTimeRangeChange]);

  useEffect(() => {
    onUserInteractionRef.current = onUserInteraction;
  }, [onUserInteraction]);

  useEffect(() => {
    onLiveRequestRef.current = onLiveRequest;
  }, [onLiveRequest]);

  useEffect(() => {
    followModeRef.current = followMode;
  }, [followMode]);

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

  // Series indices (1-based, index 0 is the timestamp column) that are hidden
  const getHiddenIndices = useCallback((currentHiddenChannels?: Set<string>) => {
    const hidden = currentHiddenChannels ?? hiddenChannelsRef.current;
    const indices = new Set<number>();
    for (let i = 0; i < channelsRef.current.length; i++) {
      if (hidden.has(channelsRef.current[i])) {
        indices.add(i + 1); // +1 because index 0 is timestamps
      }
    }
    return indices;
  }, []);

  // Apply a y range, skipping redundant uPlot commits
  const applyYRange = useCallback((chart: uPlot, range: { min: number; max: number }) => {
    const prev = yRangeRef.current;
    yRangeRef.current = range;
    if (prev && prev.min === range.min && prev.max === range.max) return;
    chart.setScale('y', range);
  }, []);

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
  const setupEventHandlers = useCallback(
    (u: uPlot) => {
      // Double-click: hand control back to the parent (return to LIVE).
      // Only when no parent handler is wired do we do the legacy local reset.
      u.over.addEventListener('dblclick', () => {
        if (onLiveRequestRef.current) {
          onLiveRequestRef.current();
          return;
        }
        isZoomedRef.current = false;
        manualYScaleRef.current = null;
        manualXRangeRef.current = null;
        yRangeRef.current = null;
        shrinkSinceRef.current = null;
        const chartData = u.data;
        if (chartData && chartData[0] && chartData[0].length > 0) {
          const xExtent = extentOf(chartData[0] as (number | null)[]);
          if (xExtent) {
            u.setScale('x', { min: xExtent.min, max: xExtent.max });

            // Calculate Y range excluding hidden series
            const { yMin, yMax } = calculateYRange(chartData, getHiddenIndices());
            if (yMin !== Infinity && yMax !== -Infinity) {
              const padding = (yMax - yMin) * 0.1;
              u.setScale('y', { min: yMin - padding, max: yMax + padding });
            }
          }
        }
      });

      // Wheel zoom - always takes the view out of follow mode
      u.over.addEventListener('wheel', (e) => {
        e.preventDefault();

        const factor = e.deltaY > 0 ? 1.1 : 0.9;
        const rect = u.over.getBoundingClientRect();
        const xPos = e.clientX - rect.left;
        const yPos = e.clientY - rect.top;

        const xScale = u.scales.x;
        const yScale = u.scales.y;

        if (xScale.min == null || xScale.max == null || yScale.min == null || yScale.max == null)
          return;

        const xVal = u.posToVal(xPos, 'x');
        const yVal = u.posToVal(yPos, 'y');

        // Freeze the view on the chosen range and notify the parent (Inspect)
        isZoomedRef.current = true;
        onUserInteractionRef.current?.();

        if (e.shiftKey) {
          const xRange = xScale.max - xScale.min;
          const newRange = xRange * factor;
          const ratio = (xVal - xScale.min) / xRange;
          const newMin = xVal - newRange * ratio;
          const newMax = xVal + newRange * (1 - ratio);
          if (!Number.isFinite(newMin) || !Number.isFinite(newMax)) return;
          u.setScale('x', { min: newMin, max: newMax });
          manualXRangeRef.current = newRange;
        } else {
          const yRange = yScale.max - yScale.min;
          const newRange = yRange * factor;
          const ratio = (yVal - yScale.min) / yRange;
          const newMin = yVal - newRange * ratio;
          const newMax = yVal + newRange * (1 - ratio);
          if (!Number.isFinite(newMin) || !Number.isFinite(newMax)) return;
          u.setScale('y', { min: newMin, max: newMax });
          manualYScaleRef.current = { min: newMin, max: newMax };
        }
      });
    },
    [getHiddenIndices]
  );

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
        // When there are no line channels (state-only chart) the y-axis
        // collapses to zero width, which would push the state timeline's
        // right-aligned channel-name labels (drawn at plotLeft - 8px) off
        // the left edge of the canvas - reserve left padding for them.
        padding: [
          null,
          null,
          stateTimelineHeight,
          channels.length === 0 && stateRowCount > 0 ? 60 : null,
        ],
        scales: {
          x: { time: false, auto: false },
          y: { auto: false },
        },
        axes: [
          { stroke: '#888', grid: { stroke: '#333' }, ticks: { stroke: '#444' } },
          { stroke: '#888', grid: { stroke: '#333' }, ticks: { stroke: '#444' } },
        ],
        // Built-in legend disabled: its interactive toggles bypass the
        // hiddenChannels state and desync from the custom legend panel
        legend: { show: false },
        cursor: {
          show: true,
          points: { show: true },
          drag: { x: true, y: true, setScale: true },
        },
        hooks: {
          setScale: [
            (u, key) => {
              if (key !== 'x') return;
              // In follow mode the parent drives the x scale every frame -
              // notifying it back would spam fetches at 60Hz.
              if (followModeRef.current) return;
              const xMin = u.scales.x.min;
              const xMax = u.scales.x.max;
              if (xMin !== undefined && xMax !== undefined) {
                debouncedTimeRangeChange(xMin, xMax);
              }
            },
          ],
          setSelect: [
            () => {
              isZoomedRef.current = true;
              onUserInteractionRef.current?.();
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
      const hiddenIndices = getHiddenIndices(currentHiddenChannels);

      if (followModeRef.current) {
        // Follow mode: setXWindow() owns both scales. uPlot re-applies the
        // current (non-auto) x/y scales on setData, so the window is kept.
        chart.setData(chartData);

        // First data for this chart instance: establish scales right away so
        // uPlot's [-1000, 1000] default is never shown before the next frame.
        if (yRangeRef.current === null) {
          const xExtent = extentOf(chartData[0] as (number | null)[]);
          if (xExtent) {
            chart.setScale('x', {
              min: xExtent.min,
              max: xExtent.max > xExtent.min ? xExtent.max : xExtent.min + 1,
            });
          }
          const { yMin, yMax } = calculateYRange(chartData, hiddenIndices);
          if (yMin !== Infinity && yMax !== -Infinity) {
            applyYRange(chart, manualYScaleRef.current ?? niceYRange(yMin, yMax));
          }
        }
        return;
      }

      const prevXScale = { min: chart.scales.x.min, max: chart.scales.x.max };
      const prevYScale = { min: chart.scales.y.min, max: chart.scales.y.max };

      chart.setData(chartData);

      if (isZoomedRef.current) {
        if (prevXScale.min != null && prevXScale.max != null) {
          chart.setScale('x', { min: prevXScale.min, max: prevXScale.max });
        }
        if (prevYScale.min != null && prevYScale.max != null) {
          chart.setScale('y', { min: prevYScale.min, max: prevYScale.max });
        }
      } else {
        if (chartData[0] && chartData[0].length > 0) {
          const xExtent = extentOf(chartData[0] as (number | null)[]);
          if (xExtent) {
            if (manualXRangeRef.current !== null) {
              const range = manualXRangeRef.current;
              chart.setScale('x', { min: xExtent.max - range, max: xExtent.max });
            } else {
              chart.setScale('x', { min: xExtent.min, max: xExtent.max });
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
    [applyYRange, getHiddenIndices]
  );

  // Imperative API for the parent's rAF loop - deliberately not React state:
  // a 60Hz setState here caused a re-render storm / memory leak (see docs/07).
  useImperativeHandle(
    ref,
    (): LineChartHandle => ({
      setXWindow(minSec: number, maxSec: number) {
        const chart = chartRef.current;
        if (!chart) return;
        if (!Number.isFinite(minSec) || !Number.isFinite(maxSec) || maxSec <= minSec) return;

        chart.setScale('x', { min: minSec, max: maxSec });

        // Manual wheel zoom on y wins until the view is reset to LIVE
        if (manualYScaleRef.current) {
          chart.setScale('y', manualYScaleRef.current);
          return;
        }

        const chartData = chart.data as uPlot.AlignedData;
        const { yMin, yMax } = calculateYRange(chartData, getHiddenIndices(), minSec, maxSec);
        // Nothing visible (e.g. the stream died and the data slid out of the
        // window) - keep the current y range rather than collapsing it.
        if (yMin === Infinity || yMax === -Infinity) return;

        const current = yRangeRef.current;

        // Expand immediately: never clip a spike
        if (current === null || yMin < current.min || yMax > current.max) {
          const nice = niceYRange(yMin, yMax);
          applyYRange(
            chart,
            current === null
              ? nice
              : { min: Math.min(nice.min, current.min), max: Math.max(nice.max, current.max) }
          );
          shrinkSinceRef.current = null;
          return;
        }

        // Shrink only after the data stayed small for Y_SHRINK_DELAY_MS
        const span = current.max - current.min;
        const occupancy = span > 0 ? (yMax - yMin) / span : 1;
        if (occupancy < Y_SHRINK_OCCUPANCY) {
          const now = performance.now();
          if (shrinkSinceRef.current === null) {
            shrinkSinceRef.current = now;
          } else if (now - shrinkSinceRef.current >= Y_SHRINK_DELAY_MS) {
            applyYRange(chart, niceYRange(yMin, yMax));
            shrinkSinceRef.current = null;
          }
        } else {
          shrinkSinceRef.current = null;
        }
      },
      resetView() {
        isZoomedRef.current = false;
        manualYScaleRef.current = null;
        manualXRangeRef.current = null;
        yRangeRef.current = null;
        shrinkSinceRef.current = null;
      },
      resetYRange() {
        yRangeRef.current = null;
        shrinkSinceRef.current = null;
      },
    }),
    [applyYRange, getHiddenIndices]
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

    // The visible extents changed - let the y range be recomputed at once
    yRangeRef.current = null;
    shrinkSinceRef.current = null;

    // Redraw to apply visibility changes
    chart.redraw();
  }, [hiddenChannels]);

  // Main chart effect
  useEffect(() => {
    if (!containerRef.current) return;

    // Use aligned data directly (no transformation needed!)
    const chartData = alignedData as uPlot.AlignedData;

    // If no data at all, destroy chart.
    // (State-only payloads have channelNames.length === 0 but non-empty
    // timestamps plus stateRows - those still need the chart for the
    // state timeline.)
    if (chartData[0].length === 0 || (channelNames.length === 0 && stateRows.length === 0)) {
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

      // The old chart's zoom/manual-scale context no longer applies
      isZoomedRef.current = false;
      manualYScaleRef.current = null;
      manualXRangeRef.current = null;
      yRangeRef.current = null;
      shrinkSinceRef.current = null;

      const opts = buildChartOptions(
        channelNames,
        stateRows.length,
        containerRef.current.clientWidth,
        containerRef.current.clientHeight,
        hiddenChannels
      );

      chartRef.current = new uPlot(opts, chartData, containerRef.current);

      // Establish scales immediately: with auto:false and no min/max, uPlot
      // defaults the y scale to [-1000, 1000] until the next data update.
      updateChartScales(chartRef.current, chartData, hiddenChannels);
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
});

export default LineChart;
