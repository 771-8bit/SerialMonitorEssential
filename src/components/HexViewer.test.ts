import { describe, it, expect } from 'vitest';

/**
 * HexViewer utility function tests
 * These test the core logic without rendering the component
 */

const ROW_HEIGHT = 20;
const MAX_SCROLL_HEIGHT = 10_000_000;

// Extract scale calculation logic from HexViewer for testing
function getScaleInfo(rowCount: number): { scale: number; scrollHeight: number } {
  const naturalHeight = rowCount * ROW_HEIGHT;
  if (naturalHeight <= MAX_SCROLL_HEIGHT) {
    return { scale: 1, scrollHeight: naturalHeight };
  }
  const scale = MAX_SCROLL_HEIGHT / naturalHeight;
  return { scale, scrollHeight: MAX_SCROLL_HEIGHT };
}

// Calculate start row from scroll position
function calculateStartRow(scrollTop: number, scale: number, bufferRows: number = 5): number {
  let startRow: number;
  if (scale === 1) {
    startRow = Math.floor(scrollTop / ROW_HEIGHT);
  } else {
    startRow = Math.floor(scrollTop / scale / ROW_HEIGHT);
  }
  return Math.max(0, startRow - bufferRows);
}

// Calculate total rows from total bytes
function calculateTotalRows(totalBytes: number): number {
  return Math.ceil(totalBytes / 16); // 16 bytes per row
}

describe('HexViewer - Scale Calculation', () => {
  it('returns scale 1 for small data', () => {
    const { scale, scrollHeight } = getScaleInfo(100);
    expect(scale).toBe(1);
    expect(scrollHeight).toBe(100 * ROW_HEIGHT);
  });

  it('returns scale 1 at boundary', () => {
    const maxRowsAtScale1 = MAX_SCROLL_HEIGHT / ROW_HEIGHT;
    const { scale } = getScaleInfo(maxRowsAtScale1);
    expect(scale).toBe(1);
  });

  it('returns scale < 1 when exceeding max height', () => {
    const rowCount = 1_000_000; // 1M rows = 20M px > 10M px max
    const { scale, scrollHeight } = getScaleInfo(rowCount);
    expect(scale).toBeLessThan(1);
    expect(scrollHeight).toBe(MAX_SCROLL_HEIGHT);
  });

  it('calculates correct scale for 2M rows (32MB data)', () => {
    // 32MB = 33,554,432 bytes
    // 33,554,432 / 16 bytes per row = 2,097,152 rows
    // 2,097,152 * 20 px = 41,943,040 px natural height
    // scale = 10,000,000 / 41,943,040 ≈ 0.238
    const rowCount = Math.ceil((32 * 1024 * 1024) / 16);
    const { scale, scrollHeight } = getScaleInfo(rowCount);
    expect(scale).toBeCloseTo(0.238, 2);
    expect(scrollHeight).toBe(MAX_SCROLL_HEIGHT);
  });

  it('handles zero rows', () => {
    const { scale, scrollHeight } = getScaleInfo(0);
    expect(scale).toBe(1);
    expect(scrollHeight).toBe(0);
  });
});

describe('HexViewer - Row Calculation', () => {
  it('calculates correct total rows from bytes', () => {
    expect(calculateTotalRows(16)).toBe(1);
    expect(calculateTotalRows(17)).toBe(2);
    expect(calculateTotalRows(32)).toBe(2);
    expect(calculateTotalRows(0)).toBe(0);
  });

  it('calculates start row at scale 1', () => {
    // scrollTop = 100, ROW_HEIGHT = 20 -> row 5, with buffer -> row 0
    expect(calculateStartRow(100, 1, 5)).toBe(0);
    // scrollTop = 200 -> row 10, with buffer -> row 5
    expect(calculateStartRow(200, 1, 5)).toBe(5);
  });

  it('calculates start row at scale < 1', () => {
    const scale = 0.5;
    // scrollTop = 100, scale 0.5 -> actual row = 100 / 0.5 / 20 = 10, with buffer -> 5
    expect(calculateStartRow(100, scale, 5)).toBe(5);
  });

  it('never returns negative start row', () => {
    expect(calculateStartRow(0, 1, 5)).toBe(0);
    expect(calculateStartRow(10, 1, 5)).toBe(0);
  });

  it('handles edge cases with small buffer', () => {
    expect(calculateStartRow(100, 1, 0)).toBe(5);
    expect(calculateStartRow(100, 1, 2)).toBe(3);
  });
});

describe('HexViewer - Large Data Scenarios', () => {
  it('handles 1GB of data', () => {
    const bytes = 1024 * 1024 * 1024; // 1GB
    const rows = calculateTotalRows(bytes);
    const { scale, scrollHeight } = getScaleInfo(rows);

    expect(rows).toBe(67108864); // 1GB / 16
    expect(scale).toBeLessThan(0.01); // Should be heavily scaled
    expect(scrollHeight).toBe(MAX_SCROLL_HEIGHT);
  });

  it('scrolling at bottom of large data', () => {
    const totalRows = 2_000_000;
    const { scale, scrollHeight } = getScaleInfo(totalRows);

    // Scroll to bottom
    const scrollTop = scrollHeight - 400; // viewport height
    const startRow = calculateStartRow(scrollTop, scale, 5);

    // Should be near the end
    expect(startRow).toBeGreaterThan(totalRows - 100);
  });
});
