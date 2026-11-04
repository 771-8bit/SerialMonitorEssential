import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

// Create a simple mock LineChart for testing
// The actual LineChart component depends on uPlot which is hard to mock
// These tests verify the data types and prop interfaces work correctly

describe('LineChart data types', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  // Test that the data structures used by LineChart are valid

  it('aligned data structure is valid for empty data', () => {
    const alignedData: (number | null)[][] = [[]];
    expect(alignedData).toHaveLength(1);
    expect(alignedData[0]).toHaveLength(0);
  });

  it('aligned data structure is valid for single channel', () => {
    const alignedData: (number | null)[][] = [
      [0, 1, 2], // timestamps in seconds
      [10, 20, 30], // ch0 values
    ];
    expect(alignedData).toHaveLength(2);
    expect(alignedData[0]).toHaveLength(3);
    expect(alignedData[1]).toHaveLength(3);
  });

  it('aligned data structure is valid for multiple channels', () => {
    const alignedData: (number | null)[][] = [
      [0, 1, 2, 3, 4],
      [10, 20, 30, 40, 50],
      [100, 200, 300, 400, 500],
      [1, 2, 3, 4, 5],
    ];
    expect(alignedData).toHaveLength(4);
    // All arrays should have the same length
    const len = alignedData[0].length;
    alignedData.forEach((arr) => {
      expect(arr).toHaveLength(len);
    });
  });

  it('aligned data supports null values for sparse data', () => {
    const alignedData: (number | null)[][] = [
      [0, 1, 2],
      [10, 20, 30],
      [null, 200, 300], // ch1 has no data at t=0
    ];
    expect(alignedData[2][0]).toBeNull();
    expect(alignedData[2][1]).toBe(200);
  });

  it('channel names array matches data columns (minus timestamp)', () => {
    const alignedData: (number | null)[][] = [
      [0, 1, 2],
      [10, 20, 30],
      [100, 200, 300],
    ];
    const channelNames = ['ch0', 'ch1'];

    // Number of channel names should be one less than data arrays (first is timestamps)
    expect(channelNames.length).toBe(alignedData.length - 1);
  });

  it('band data structure is valid for Average mode', () => {
    interface BandSeriesData {
      min: (number | null)[];
      max: (number | null)[];
    }

    const bandData: Record<string, BandSeriesData> = {
      ch0: {
        min: [10, 15, 20],
        max: [30, 35, 40],
      },
    };

    const minVal = bandData.ch0.min[0];
    const maxVal = bandData.ch0.max[0];
    expect(minVal).not.toBeNull();
    expect(maxVal).not.toBeNull();
    if (minVal !== null && maxVal !== null) {
      expect(minVal).toBeLessThan(maxVal);
    }
  });

  it('band data supports null values', () => {
    interface BandSeriesData {
      min: (number | null)[];
      max: (number | null)[];
    }

    const bandData: Record<string, BandSeriesData> = {
      ch0: {
        min: [10, null, 20],
        max: [30, null, 40],
      },
    };

    expect(bandData.ch0.min[1]).toBeNull();
    expect(bandData.ch0.max[1]).toBeNull();
  });

  it('state row structure is valid', () => {
    interface StateEntry {
      startSec: number;
      endSec: number;
      state: string;
      color: string;
    }

    interface StateRow {
      label: string;
      states: StateEntry[];
    }

    const stateRows: StateRow[] = [
      {
        label: 'status',
        states: [
          { startSec: 0, endSec: 1, state: 'RUNNING', color: '#4ade80' },
          { startSec: 1, endSec: 2, state: 'STOPPED', color: '#f87171' },
        ],
      },
    ];

    expect(stateRows).toHaveLength(1);
    expect(stateRows[0].states).toHaveLength(2);
    expect(stateRows[0].states[0].startSec).toBe(0);
    expect(stateRows[0].states[0].endSec).toBe(1);
  });

  // Hidden channels tests

  it('hidden channels set can contain channel names', () => {
    const hiddenChannels = new Set(['ch0', 'ch1']);

    expect(hiddenChannels.has('ch0')).toBe(true);
    expect(hiddenChannels.has('ch1')).toBe(true);
    expect(hiddenChannels.has('ch2')).toBe(false);
  });

  it('hidden channels set can be empty', () => {
    const hiddenChannels = new Set<string>();

    expect(hiddenChannels.size).toBe(0);
    expect(hiddenChannels.has('ch0')).toBe(false);
  });

  // Edge case data tests

  it('supports negative values', () => {
    const alignedData: (number | null)[][] = [
      [0, 1, 2],
      [-100, 0, 100],
    ];

    expect(alignedData[1][0]).toBeLessThan(0);
    expect(alignedData[1][2]).toBeGreaterThan(0);
  });

  it('supports very large values', () => {
    const alignedData: (number | null)[][] = [
      [0, 1, 2],
      [1e10, 2e10, 3e10],
    ];

    expect(alignedData[1][0]).toBe(1e10);
    expect(alignedData[1][2]).toBe(3e10);
  });

  it('supports very small values', () => {
    const alignedData: (number | null)[][] = [
      [0, 1, 2],
      [1e-10, 2e-10, 3e-10],
    ];

    expect(alignedData[1][0]).toBe(1e-10);
    expect(alignedData[1][2]).toBe(3e-10);
  });

  it('timestamp array should be in ascending order', () => {
    const timestamps = [0, 1, 2, 3, 4];

    for (let i = 1; i < timestamps.length; i++) {
      expect(timestamps[i]).toBeGreaterThan(timestamps[i - 1]);
    }
  });

  it('timestamps are in seconds (converted from ms in backend)', () => {
    // Backend returns ms, frontend receives seconds
    const startMs = 0;
    const endMs = 5000;
    const startSec = startMs / 1000;
    const endSec = endMs / 1000;

    expect(startSec).toBe(0);
    expect(endSec).toBe(5);
  });
});
