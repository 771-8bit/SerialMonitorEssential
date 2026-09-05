import React from 'react';
import { renderHook, act } from '@testing-library/react';
import { describe, it, expect, vi } from 'vitest';
import { useByteScroll } from './useByteScroll';
import { MAX_SCROLL_HEIGHT, ROW_HEIGHT } from './viewerConstants';

describe('useByteScroll', () => {
  const defaultProps = {
    totalBytes: 1000,
    autoScroll: false,
    initialOffset: 0,
    onScrollChange: vi.fn(),
  };

  it('initializes with correct defaults', () => {
    const { result } = renderHook(() => useByteScroll(defaultProps));

    expect(result.current.scrollTop).toBe(0);
    // 1000 bytes / 16 bytes/row = 62.5 rows -> 63 rows. 63 * 20px = 1260px
    expect(result.current.scrollHeight).toBe(63 * ROW_HEIGHT);
  });

  it('updates scrollHeight when totalBytes changes', () => {
    const { result, rerender } = renderHook((props) => useByteScroll(props), {
      initialProps: defaultProps,
    });

    rerender({ ...defaultProps, totalBytes: 2000 });

    // 2000 / 16 = 125 rows * 20 = 2500px
    expect(result.current.scrollHeight).toBe(125 * ROW_HEIGHT);
  });

  it('handles auto-scroll correctly', () => {
    const { result, rerender } = renderHook((props) => useByteScroll(props), {
      initialProps: { ...defaultProps, autoScroll: true },
    });

    // Mock container
    const mockContainer = document.createElement('div');
    Object.defineProperty(mockContainer, 'clientHeight', { value: 500 });
    Object.defineProperty(mockContainer, 'scrollTop', { value: 0, writable: true });

    // We need to manually assign the ref since we aren't rendering JSX
    // But renderHook doesn't attach refs to real DOM.
    // We can simulate the effect by manually setting the ref current value
    // However, the hook uses the ref in useEffects.
    // A better way is to pass the ref or rely on internal logic that checks ref.
    // The hook creates its own ref. We can modify it.
    (result.current.containerRef as React.MutableRefObject<HTMLDivElement | null>).current =
      mockContainer;

    // Simulate new data arrival
    rerender({ ...defaultProps, totalBytes: 2000, autoScroll: true });

    // Should have scrolled to bottom
    // Total height 2500px, viewport 500px -> max scrollTop 2000
    // The hook calculates targetScrollTop.
    expect(mockContainer.scrollTop).toBeGreaterThan(0);
    expect(result.current.scrollTop).toBe(mockContainer.scrollTop);
  });

  it('anchors position when data grows and not auto-scrolling', () => {
    const { result, rerender } = renderHook((props) => useByteScroll(props), {
      initialProps: { ...defaultProps, totalBytes: 16000 }, // 1000 rows, 20000px
    });

    const mockContainer = document.createElement('div');
    Object.defineProperty(mockContainer, 'clientHeight', { value: 500 });
    Object.defineProperty(mockContainer, 'scrollTop', { value: 10000, writable: true }); // Middle
    (result.current.containerRef as React.MutableRefObject<HTMLDivElement | null>).current =
      mockContainer;

    // Trigger scroll event to update anchor
    act(() => {
      // We need to manually call handleScroll because we are not in a browser env triggering events
      result.current.handleScroll({
        currentTarget: mockContainer,
      } as unknown as React.UIEvent<HTMLDivElement>);
    });

    // Initial check
    expect(result.current.scrollTop).toBe(10000);

    // Double the data
    rerender({ ...defaultProps, totalBytes: 32000, autoScroll: false }); // 2000 rows, 40000px

    // Should stay at same PIXEL position (10000) because data is appended at end,
    // so the byte at offset 8000 is still at 10000px.
    expect(mockContainer.scrollTop).toBeCloseTo(10000, -2);
    expect(result.current.scrollTop).toBeCloseTo(10000, -2);
  });

  it('scrollTo updates position', () => {
    const { result } = renderHook(() => useByteScroll(defaultProps));
    const mockContainer = document.createElement('div');
    (result.current.containerRef as React.MutableRefObject<HTMLDivElement | null>).current =
      mockContainer;

    act(() => {
      result.current.scrollTo(500);
    });

    expect(mockContainer.scrollTop).toBe(500);
    expect(result.current.scrollTop).toBe(500);
  });

  it('caps scrollHeight at MAX_SCROLL_HEIGHT', () => {
    const hugeBytes = 100_000_000 * 16; // 100M rows
    const { result } = renderHook(() => useByteScroll({ ...defaultProps, totalBytes: hugeBytes }));

    expect(result.current.scrollHeight).toBe(MAX_SCROLL_HEIGHT);
  });

  it('uses totalRows for scroll calculation when provided', () => {
    const { result } = renderHook(() =>
      useByteScroll({
        ...defaultProps,
        totalBytes: 1000,
        totalRows: 10, // Explicitly small row count
      })
    );

    // 10 rows * 20px = 200px (ignores 1000 bytes => ~63 rows)
    expect(result.current.scrollHeight).toBe(10 * ROW_HEIGHT);
  });
});
