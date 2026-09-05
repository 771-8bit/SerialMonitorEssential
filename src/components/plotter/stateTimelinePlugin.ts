/**
 * State Timeline uPlot Plugin
 *
 * Draws state segments as colored bars below the line chart.
 * Uses uPlot's time axis for zoom/pan synchronization.
 */

import type uPlot from 'uplot';

// Types for state data
export interface StateSegment {
  start_ms: number;
  end_ms: number;
  state: string;
}

export interface StateRow {
  channel: string;
  segments: StateSegment[];
}

export interface StateTimelinePluginOpts {
  /** Function to get current state rows */
  getRows: () => StateRow[];
  /** Height of each state row in pixels */
  rowHeight?: number;
  /** Gap between rows in pixels */
  rowGap?: number;
  /** Custom state-to-color mapping */
  stateColors?: Record<string, string>;
  /** Whether to show state labels inside bars */
  showLabel?: boolean;
  /** Label font */
  labelFont?: string;
}

// Default colors for common states
const DEFAULT_STATE_COLORS: Record<string, string> = {
  ON: '#22c55e',
  OFF: '#6b7280',
  RUNNING: '#22c55e',
  STOPPED: '#ef4444',
  IDLE: '#f59e0b',
  ERROR: '#dc2626',
  WARNING: '#f59e0b',
  OK: '#22c55e',
  TRUE: '#22c55e',
  FALSE: '#6b7280',
  '1': '#22c55e',
  '0': '#6b7280',
};

// Generate a consistent color from a string hash
function hashColor(str: string): string {
  let hash = 0;
  for (let i = 0; i < str.length; i++) {
    hash = str.charCodeAt(i) + ((hash << 5) - hash);
    hash = hash & hash;
  }

  // Generate HSL with fixed saturation and lightness for readability
  const hue = Math.abs(hash) % 360;
  return `hsl(${hue}, 65%, 50%)`;
}

// Color cache for session consistency
const colorCache = new Map<string, string>();

function getStateColor(state: string, customColors?: Record<string, string>): string {
  // Check custom colors first
  if (customColors?.[state]) {
    return customColors[state];
  }

  // Check default colors (case-insensitive)
  const upperState = state.toUpperCase();
  if (DEFAULT_STATE_COLORS[upperState]) {
    return DEFAULT_STATE_COLORS[upperState];
  }

  // Check cache
  if (colorCache.has(state)) {
    return colorCache.get(state)!;
  }

  // Generate and cache color
  const color = hashColor(state);
  colorCache.set(state, color);
  return color;
}

/** Scale the px size in a CSS font shorthand (e.g. "11px sans-serif") by dpr */
function scaleFontPx(font: string, dpr: number): string {
  return font.replace(/(\d+(?:\.\d+)?)px/, (_, size) => `${Math.round(Number(size) * dpr)}px`);
}

/**
 * Creates a uPlot plugin for rendering state timelines
 */
export function stateTimelinePlugin(opts: StateTimelinePluginOpts): uPlot.Plugin {
  const rowHeight = opts.rowHeight ?? 24;
  const rowGap = opts.rowGap ?? 2;
  const showLabel = opts.showLabel ?? true;
  const labelFont = opts.labelFont ?? '11px sans-serif';

  return {
    hooks: {
      draw: (u: uPlot) => {
        const ctx = u.ctx;
        const rows = opts.getRows();

        if (rows.length === 0) return;

        // Canvas coordinates (u.bbox, valToPos(..., true)) are in DEVICE
        // pixels, while option values (rowHeight, axis size, fonts) are CSS
        // pixels. Scale all CSS-px quantities by the device pixel ratio so
        // rows land below the x-axis at 125%/150% Windows display scaling.
        const dpr = window.devicePixelRatio || 1;
        const rh = rowHeight * dpr;
        const rg = rowGap * dpr;

        // Get plot area dimensions
        const plotLeft = u.bbox.left;
        const plotWidth = u.bbox.width;
        const plotBottom = u.bbox.top + u.bbox.height;

        // Get current x scale range
        const xMin = u.scales.x.min;
        const xMax = u.scales.x.max;

        if (xMin === undefined || xMax === undefined) return;

        // Calculate X-axis height (axis labels, ticks, gap) in device px
        const xAxis = u.axes[0];
        const axisSize = ((xAxis as { _size?: number })._size ?? 30) * dpr;
        const axisGap = (xAxis.gap ?? 5) * dpr;
        const xAxisHeight = axisSize + axisGap;

        // Start drawing below the X-axis
        const stateAreaTop = plotBottom + xAxisHeight;

        // Convert ms to seconds for uPlot scale
        const xMinMs = xMin * 1000;
        const xMaxMs = xMax * 1000;

        ctx.save();

        // Clip to state timeline area (below X-axis)
        const totalStateHeight = rows.length * (rh + rg);
        ctx.beginPath();
        ctx.rect(plotLeft, stateAreaTop, plotWidth, totalStateHeight);
        ctx.clip();

        // Draw each state row (bars only, clipped to plot area)
        for (let r = 0; r < rows.length; r++) {
          const row = rows[r];
          const y0 = stateAreaTop + r * (rh + rg) + rg / 2;

          // Draw channel label background
          ctx.fillStyle = '#252526';
          ctx.fillRect(plotLeft, y0, plotWidth, rh);

          // Draw each segment
          for (const seg of row.segments) {
            // Skip segments outside visible range
            if (seg.end_ms < xMinMs || seg.start_ms > xMaxMs) continue;

            // Clamp segment to visible range
            const visStart = Math.max(seg.start_ms, xMinMs);
            const visEnd = Math.min(seg.end_ms, xMaxMs);

            // Convert to pixel coordinates (using seconds)
            const x0 = u.valToPos(visStart / 1000, 'x', true);
            const x1 = u.valToPos(visEnd / 1000, 'x', true);
            const barWidth = Math.max(1, x1 - x0);

            // Draw state bar
            const color = getStateColor(seg.state, opts.stateColors);
            ctx.fillStyle = color;
            ctx.fillRect(x0, y0, barWidth, rh);

            // Draw label if bar is wide enough
            if (showLabel && barWidth > 20 * dpr) {
              ctx.fillStyle = '#fff';
              ctx.font = scaleFontPx(labelFont, dpr);
              ctx.textBaseline = 'middle';
              ctx.textAlign = 'left';

              // Truncate text if needed
              const maxTextWidth = barWidth - 8 * dpr;
              let text = seg.state;
              const measured = ctx.measureText(text);
              if (measured.width > maxTextWidth) {
                // Truncate with ellipsis
                while (text.length > 1 && ctx.measureText(text + '…').width > maxTextWidth) {
                  text = text.slice(0, -1);
                }
                text = text + '…';
              }

              ctx.fillText(text, x0 + 4 * dpr, y0 + rh / 2);
            }
          }
        }

        ctx.restore();

        // Draw channel names on the left (OUTSIDE the clip region)
        ctx.save();
        for (let r = 0; r < rows.length; r++) {
          const row = rows[r];
          const y0 = stateAreaTop + r * (rh + rg) + rg / 2;

          ctx.fillStyle = '#9cdcfe';
          ctx.font = scaleFontPx('11px sans-serif', dpr);
          ctx.textBaseline = 'middle';
          ctx.textAlign = 'right';
          ctx.fillText(row.channel + ':', plotLeft - 8 * dpr, y0 + rh / 2);
        }
        ctx.restore();
      },
    },
  };
}

/**
 * Calculate the total height needed for state timeline rows
 */
export function calculateStateTimelineHeight(rowCount: number, rowHeight = 24, rowGap = 2): number {
  if (rowCount === 0) return 0;
  return rowCount * (rowHeight + rowGap);
}
