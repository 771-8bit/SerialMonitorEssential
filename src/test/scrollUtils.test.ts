import { describe, it, expect } from 'vitest';
import {
  calculateScrollHeight,
  calculateScale,
  scrollTopToByteOffset,
  byteOffsetToScrollTop,
  calculateBottomByteOffset,
  clampByteOffset,
} from '../components/viewers/scrollUtils';

describe('scrollUtils', () => {
  describe('calculateScrollHeight', () => {
    it('returns natural height when under limit', () => {
      // 500,000 bytes / 16 bytes per row = 31,250 rows
      // 31,250 rows * 20px = 625,000px (under MAX_SCROLL_HEIGHT of 10,000,000)
      expect(calculateScrollHeight(500_000)).toBe(625_000);
    });

    it('caps at MAX_SCROLL_HEIGHT for large data', () => {
      // 100MB / 16 = 6,250,000 rows * 20px = 125,000,000 > 10M
      expect(calculateScrollHeight(100_000_000)).toBe(10_000_000);
    });

    it('handles zero', () => {
      expect(calculateScrollHeight(0)).toBe(0);
    });

    it('uses explicit totalRows when provided', () => {
      // 100 rows * 20px = 2000px, regardless of bytes
      expect(calculateScrollHeight(500, 100)).toBe(2000);
    });
  });

  describe('calculateScale', () => {
    it('returns 1 when natural height is under limit', () => {
      // 500,000 bytes / 16 = 31,250 rows * 20px = 625,000 < 10M
      expect(calculateScale(500_000)).toBe(1);
    });

    it('returns scale < 1 when natural height exceeds limit', () => {
      // 100,000,000 bytes / 16 = 6,250,000 rows * 20px = 125,000,000px
      // scale = 10M / 125,000,000 = 0.08
      expect(calculateScale(100_000_000)).toBe(0.08);
    });

    it('handles zero', () => {
      expect(calculateScale(0)).toBe(1);
    });
  });

  describe('scrollTopToByteOffset', () => {
    it('converts scroll position to byte offset', () => {
      // scrollTop 500, scrollHeight 1000, totalBytes 2000 -> byte 1000
      expect(scrollTopToByteOffset(500, 1000, 2000)).toBe(1000);
    });

    it('handles start position', () => {
      expect(scrollTopToByteOffset(0, 1000, 2000)).toBe(0);
    });

    it('handles end position', () => {
      expect(scrollTopToByteOffset(1000, 1000, 2000)).toBe(2000);
    });

    it('handles zero scrollHeight', () => {
      expect(scrollTopToByteOffset(500, 0, 2000)).toBe(0);
    });
  });

  describe('byteOffsetToScrollTop', () => {
    it('converts byte offset to scroll position', () => {
      // byteOffset 1000, scrollHeight 1000, totalBytes 2000 -> scrollTop 500
      expect(byteOffsetToScrollTop(1000, 1000, 2000)).toBe(500);
    });

    it('handles zero totalBytes', () => {
      expect(byteOffsetToScrollTop(1000, 1000, 0)).toBe(0);
    });
  });

  describe('calculateBottomByteOffset', () => {
    it('calculates byte offset for bottom', () => {
      // totalBytes 1000, viewport shows 100px of 1000px height = 100 bytes
      // bottom offset = 1000 - 100 = 900
      expect(calculateBottomByteOffset(1000, 100, 1000)).toBe(900);
    });

    it('handles zero', () => {
      expect(calculateBottomByteOffset(0, 100, 0)).toBe(0);
    });
  });

  describe('clampByteOffset', () => {
    it('clamps to valid range', () => {
      expect(clampByteOffset(-10, 1000)).toBe(0);
      expect(clampByteOffset(500, 1000)).toBe(500);
      expect(clampByteOffset(1500, 1000)).toBe(1000);
    });
  });
});
