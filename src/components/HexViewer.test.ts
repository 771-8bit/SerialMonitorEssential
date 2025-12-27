import { describe, it, expect } from 'vitest';
import {
  calculateScrollHeight,
  calculateScale,
} from './viewers/scrollUtils';
import { BYTES_PER_ROW, ROW_HEIGHT, MAX_SCROLL_HEIGHT } from './viewers/viewerConstants';

/**
 * HexViewer utility function tests
 * Testing the actual scrollUtils functions used by HexViewer
 */

describe('HexViewer - Scale Calculation (via scrollUtils)', () => {
  it('returns scale 1 for small data', () => {
    const totalBytes = 100 * BYTES_PER_ROW; // 100 rows
    const scale = calculateScale(totalBytes);
    const scrollHeight = calculateScrollHeight(totalBytes);
    expect(scale).toBe(1);
    expect(scrollHeight).toBe(100 * ROW_HEIGHT);
  });

  it('returns scale 1 at boundary', () => {
    // MAX_SCROLL_HEIGHT / ROW_HEIGHT rows * BYTES_PER_ROW bytes
    const maxBytesAtScale1 = (MAX_SCROLL_HEIGHT / ROW_HEIGHT) * BYTES_PER_ROW;
    const scale = calculateScale(maxBytesAtScale1);
    expect(scale).toBe(1);
  });

  it('returns scale < 1 when exceeding max height', () => {
    // 1M rows = 16M bytes -> 20M px > 10M px max
    const totalBytes = 1_000_000 * BYTES_PER_ROW;
    const scale = calculateScale(totalBytes);
    const scrollHeight = calculateScrollHeight(totalBytes);
    expect(scale).toBeLessThan(1);
    expect(scrollHeight).toBe(MAX_SCROLL_HEIGHT);
  });

  it('calculates correct scale for 32MB data', () => {
    // 32MB = 33,554,432 bytes
    // 33,554,432 / 16 bytes per row = 2,097,152 rows
    // 2,097,152 * 20 px = 41,943,040 px natural height
    // scale = 10,000,000 / 41,943,040 ≈ 0.238
    const totalBytes = 32 * 1024 * 1024;
    const scale = calculateScale(totalBytes);
    const scrollHeight = calculateScrollHeight(totalBytes);
    expect(scale).toBeCloseTo(0.238, 2);
    expect(scrollHeight).toBe(MAX_SCROLL_HEIGHT);
  });

  it('handles zero bytes', () => {
    const scale = calculateScale(0);
    const scrollHeight = calculateScrollHeight(0);
    expect(scale).toBe(1);
    expect(scrollHeight).toBe(0);
  });
});

describe('HexViewer - Row Calculation', () => {
  // Helper function to calculate total rows (matching HexViewer logic)
  const calculateTotalRows = (totalBytes: number) => Math.ceil(totalBytes / BYTES_PER_ROW);

  // Helper function to calculate start row (matching HexViewer logic)
  const calculateStartRow = (scrollTop: number, scrollHeight: number, totalBytes: number, bufferRows: number = 5) => {
    const totalRows = calculateTotalRows(totalBytes);
    const scrollRatio = scrollHeight > 0 ? scrollTop / scrollHeight : 0;
    const targetRow = Math.floor(scrollRatio * totalRows);
    return Math.max(0, targetRow - bufferRows);
  };

  it('calculates correct total rows from bytes', () => {
    expect(calculateTotalRows(16)).toBe(1);
    expect(calculateTotalRows(17)).toBe(2);
    expect(calculateTotalRows(32)).toBe(2);
    expect(calculateTotalRows(0)).toBe(0);
  });

  it('calculates start row at scale 1', () => {
    const totalBytes = 1000 * BYTES_PER_ROW; // 1000 rows
    const scrollHeight = 1000 * ROW_HEIGHT;  // No scaling

    // scrollTop = 100 -> scrollRatio = 100/20000 = 0.005 -> targetRow = 5, with buffer -> 0
    expect(calculateStartRow(100, scrollHeight, totalBytes, 5)).toBe(0);

    // scrollTop = 200 -> scrollRatio = 200/20000 = 0.01 -> targetRow = 10, with buffer -> 5
    expect(calculateStartRow(200, scrollHeight, totalBytes, 5)).toBe(5);
  });

  it('calculates start row at scale < 1', () => {
    // 2M rows = scaled down
    const totalBytes = 2_000_000 * BYTES_PER_ROW;
    const scrollHeight = MAX_SCROLL_HEIGHT; // Scaled to max

    // scrollTop at middle -> should be near middle row
    const scrollTop = scrollHeight / 2;
    const startRow = calculateStartRow(scrollTop, scrollHeight, totalBytes, 5);
    expect(startRow).toBeCloseTo(1_000_000 - 5, -2); // Approximately 999,995
  });

  it('never returns negative start row', () => {
    const totalBytes = 100 * BYTES_PER_ROW;
    const scrollHeight = 100 * ROW_HEIGHT;
    expect(calculateStartRow(0, scrollHeight, totalBytes, 5)).toBe(0);
    expect(calculateStartRow(10, scrollHeight, totalBytes, 5)).toBe(0);
  });

  it('handles edge cases with small buffer', () => {
    const totalBytes = 1000 * BYTES_PER_ROW;
    const scrollHeight = 1000 * ROW_HEIGHT;

    // scrollTop = 200 -> targetRow = 10
    expect(calculateStartRow(200, scrollHeight, totalBytes, 0)).toBe(10);
    expect(calculateStartRow(200, scrollHeight, totalBytes, 2)).toBe(8);
  });
});

describe('HexViewer - Large Data Scenarios', () => {
  const calculateTotalRows = (totalBytes: number) => Math.ceil(totalBytes / BYTES_PER_ROW);

  it('handles 1GB of data', () => {
    const bytes = 1024 * 1024 * 1024; // 1GB
    const rows = calculateTotalRows(bytes);
    const scale = calculateScale(bytes);
    const scrollHeight = calculateScrollHeight(bytes);

    expect(rows).toBe(67108864); // 1GB / 16
    expect(scale).toBeLessThan(0.01); // Should be heavily scaled
    expect(scrollHeight).toBe(MAX_SCROLL_HEIGHT);
  });

  it('scrolling at bottom of large data', () => {
    const totalRows = 2_000_000;
    const scrollHeight = MAX_SCROLL_HEIGHT;

    // Scroll to bottom
    const scrollTop = scrollHeight - 400; // viewport height
    const scrollRatio = scrollTop / scrollHeight;
    const targetRow = Math.floor(scrollRatio * totalRows);
    const startRow = Math.max(0, targetRow - 5);

    // Should be near the end
    expect(startRow).toBeGreaterThan(totalRows - 100);
  });
});
