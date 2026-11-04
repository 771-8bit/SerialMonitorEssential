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

// ---- State colors (docs/27 PF-4) -----------------------------------------
//
// Colors carry meaning: abnormal states are always red/orange, normal running
// states green, stopped / off / idle states gray, warnings amber. Every other
// state name gets the next color of a categorical palette that avoids those
// four hues, in order of first appearance, so two different states never get
// nearly the same color (the old string-hash hue gave E01/E02/E03 hues one
// degree apart and painted FAULT_OVERTEMP_LATCHED the same green as ON).

export type StateKind = 'abnormal' | 'warning' | 'normal' | 'neutral' | 'other';

const ABNORMAL =
  /(FAULT|ERR|ALARM|FAIL|TRIP|CRIT|PANIC|ABORT|EMERG|OVERTEMP|OVERCURRENT|OVERVOLT|OVERLOAD)/;
const WARNING = /WARN/;
const NORMAL = new Set([
  'ON',
  'RUNNING',
  'RUN',
  'OK',
  'TRUE',
  '1',
  'READY',
  'ENABLED',
  'ACTIVE',
  'GOOD',
]);
const NEUTRAL = new Set([
  'OFF',
  'STOPPED',
  'STOP',
  'IDLE',
  'FALSE',
  '0',
  'NONE',
  'DISABLED',
  'INACTIVE',
  'STANDBY',
]);

/** Meaning of a state name (case-insensitive) */
export function classifyState(state: string): StateKind {
  const upper = state.trim().toUpperCase();
  if (ABNORMAL.test(upper)) return 'abnormal';
  if (WARNING.test(upper)) return 'warning';
  if (NORMAL.has(upper)) return 'normal';
  if (NEUTRAL.has(upper)) return 'neutral';
  return 'other';
}

const KIND_COLORS: Record<Exclude<StateKind, 'other' | 'abnormal'>, string> = {
  warning: '#f59e0b',
  normal: '#22c55e',
  neutral: '#6b7280',
};

/** Abnormal states: reds and oranges, distinct per state name */
const ABNORMAL_PALETTE = ['#dc2626', '#f97316', '#be123c', '#ea580c', '#ef4444', '#c2410c'];

/** Other states: no red, green, gray or amber, so meaning is never implied */
const CATEGORY_PALETTE = [
  '#3b82f6', // blue
  '#a855f7', // purple
  '#06b6d4', // cyan
  '#ec4899', // pink
  '#6366f1', // indigo
  '#ca8a04', // ochre
  '#93c5fd', // light blue
  '#d946ef', // orchid
];

// Session-stable assignment (first appearance wins)
const assigned = new Map<string, string>();
const nextIndex = { abnormal: 0, other: 0 };

/** Forget assigned colors (tests) */
export function resetStateColors(): void {
  assigned.clear();
  nextIndex.abnormal = 0;
  nextIndex.other = 0;
}

export function getStateColor(state: string, customColors?: Record<string, string>): string {
  if (customColors?.[state]) {
    return customColors[state];
  }
  const kind = classifyState(state);
  if (kind === 'warning' || kind === 'normal' || kind === 'neutral') {
    return KIND_COLORS[kind];
  }
  const cached = assigned.get(state);
  if (cached) return cached;
  const palette = kind === 'abnormal' ? ABNORMAL_PALETTE : CATEGORY_PALETTE;
  const color = palette[nextIndex[kind] % palette.length];
  nextIndex[kind] += 1;
  assigned.set(state, color);
  return color;
}

/** Abnormal segments are drawn at least this wide (CSS px), so a short, rare
 * fault never disappears between pixels in a long window */
const ABNORMAL_MIN_WIDTH = 4;

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

          // Draw each segment. Abnormal ones go in a second pass so their
          // minimum width is not painted over by the following segment.
          for (const abnormalPass of [false, true])
            for (const seg of row.segments) {
              if ((classifyState(seg.state) === 'abnormal') !== abnormalPass) continue;
              // Skip segments outside visible range
              if (seg.end_ms < xMinMs || seg.start_ms > xMaxMs) continue;

              // Clamp segment to visible range
              const visStart = Math.max(seg.start_ms, xMinMs);
              const visEnd = Math.min(seg.end_ms, xMaxMs);

              // Convert to pixel coordinates (using seconds)
              const x0 = u.valToPos(visStart / 1000, 'x', true);
              const x1 = u.valToPos(visEnd / 1000, 'x', true);
              const barWidth = Math.max(abnormalPass ? ABNORMAL_MIN_WIDTH * dpr : 1, x1 - x0);

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
