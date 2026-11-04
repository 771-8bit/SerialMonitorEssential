import { describe, it, expect, vi } from 'vitest';
import type uPlot from 'uplot';

// uPlot reads window.matchMedia at import time, which jsdom does not provide.
// calculateYRange is pure and never touches uPlot at runtime (the import in
// LineChart.tsx is only needed for the AlignedData type), so a stub is enough.
vi.mock('uplot', () => ({ default: class UPlotStub {} }));

import { calculateYRange } from '../components/plotter/LineChart';
import type { BandSeriesData } from '../components/plotter/PlotterWindow';

// GAP-04 / SYS-F-525:
// In Average aggregation mode the min/max band shares the y scale with the
// series values, so it has to take part in the auto-range. Otherwise the band
// (which carries the raw peaks of each bucket) is clipped and a spike
// disappears - UN-03 says a spike must never disappear.

// [timestamps, ch0, ch1]
const alignedData = [
  [0, 1, 2],
  [10, 20, 30],
  [15, 25, 35],
] as unknown as uPlot.AlignedData;

const channelNames = ['ch0', 'ch1'];

describe('calculateYRange', () => {
  it('uses only the series values when no band data is given', () => {
    expect(calculateYRange(alignedData, new Set())).toEqual({ yMin: 10, yMax: 35 });
  });

  it('is unchanged when bandData is null (behaviour identical to no band)', () => {
    const withoutBand = calculateYRange(alignedData, new Set());
    const withNullBand = calculateYRange(
      alignedData,
      new Set(),
      undefined,
      undefined,
      null,
      channelNames
    );
    expect(withNullBand).toEqual(withoutBand);
  });

  it('is unchanged when channelNames is missing (cannot map band to series)', () => {
    const bandData: Record<string, BandSeriesData> = {
      ch0: { min: [-100, -100, -100], max: [100, 100, 100] },
    };
    expect(calculateYRange(alignedData, new Set(), undefined, undefined, bandData, null)).toEqual({
      yMin: 10,
      yMax: 35,
    });
  });

  it('extends the range when the band is wider than the values', () => {
    const bandData: Record<string, BandSeriesData> = {
      ch0: { min: [5, 5, 5], max: [50, 50, 50] },
    };
    expect(
      calculateYRange(alignedData, new Set(), undefined, undefined, bandData, channelNames)
    ).toEqual({ yMin: 5, yMax: 50 });
  });

  it('keeps the value range when the band is narrower', () => {
    const bandData: Record<string, BandSeriesData> = {
      ch0: { min: [12, 12, 12], max: [18, 18, 18] },
      ch1: { min: [16, 16, 16], max: [34, 34, 34] },
    };
    expect(
      calculateYRange(alignedData, new Set(), undefined, undefined, bandData, channelNames)
    ).toEqual({ yMin: 10, yMax: 35 });
  });

  it('ignores the band of a hidden channel', () => {
    const bandData: Record<string, BandSeriesData> = {
      // ch0 is series index 1 -> hidden below, its wide band must not count
      ch0: { min: [-1000, -1000, -1000], max: [1000, 1000, 1000] },
      ch1: { min: [5, 5, 5], max: [50, 50, 50] },
    };
    const hidden = new Set([1]); // ch0
    expect(
      calculateYRange(alignedData, hidden, undefined, undefined, bandData, channelNames)
    ).toEqual({ yMin: 5, yMax: 50 });
  });

  it('ignores null entries inside the band arrays', () => {
    const bandData: Record<string, BandSeriesData> = {
      ch0: { min: [null, 5, null], max: [null, null, 50] },
    };
    expect(
      calculateYRange(alignedData, new Set(), undefined, undefined, bandData, channelNames)
    ).toEqual({ yMin: 5, yMax: 50 });
  });

  it('only folds in band samples inside the x window', () => {
    const bandData: Record<string, BandSeriesData> = {
      // the extreme band values sit at t=2, which is outside the [0, 1] window
      ch0: { min: [8, 8, -999], max: [40, 40, 999] },
    };
    expect(calculateYRange(alignedData, new Set(), 0, 1, bandData, channelNames)).toEqual({
      yMin: 8,
      yMax: 40,
    });
  });

  it('returns the empty sentinel range when nothing is visible', () => {
    const bandData: Record<string, BandSeriesData> = {
      ch0: { min: [1, 1, 1], max: [2, 2, 2] },
    };
    const hidden = new Set([1, 2]);
    expect(
      calculateYRange(alignedData, hidden, undefined, undefined, bandData, channelNames)
    ).toEqual({ yMin: Infinity, yMax: -Infinity });
  });

  it('handles a band for a channel that is not in the data', () => {
    const bandData: Record<string, BandSeriesData> = {
      ghost: { min: [-999, -999, -999], max: [999, 999, 999] },
    };
    expect(
      calculateYRange(alignedData, new Set(), undefined, undefined, bandData, channelNames)
    ).toEqual({ yMin: 10, yMax: 35 });
  });
});
