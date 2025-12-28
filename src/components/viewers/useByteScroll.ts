/**
 * Byte-based scroll hook for HexViewer and AsciiViewer.
 * Manages scroll state using byte offset for consistency across modes.
 */

import { useState, useRef, useCallback, useEffect, useLayoutEffect } from 'react';
import { ROW_HEIGHT, THROTTLE_MS } from './viewerConstants';
import {
  calculateScrollHeight,
  scrollTopToByteOffset,
  byteOffsetToScrollTop,
  calculateBottomByteOffset,
  clampByteOffset,
} from './scrollUtils';

interface UseByteScrollProps {
  totalBytes: number;
  totalRows?: number;
  autoScroll: boolean;
  initialOffset: number;
  onScrollChange: (byteOffset: number) => void;
}

interface UseByteScrollReturn {
  // Refs
  containerRef: React.RefObject<HTMLDivElement | null>;

  // State
  scrollTop: number;
  scrollHeight: number;
  visibleRows: number;

  // Handlers
  handleScroll: (e: React.UIEvent<HTMLDivElement>) => void;

  // Utilities
  getByteOffset: () => number;
  scrollTo: (scrollTop: number, byteOffset?: number) => void;
}

export function useByteScroll({
  totalBytes,
  totalRows,
  autoScroll,
  initialOffset,
  onScrollChange,
}: UseByteScrollProps): UseByteScrollReturn {
  const containerRef = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [visibleRows, setVisibleRows] = useState(25);

  // Refs for preventing re-render loops and managing scroll anchoring
  const lastUpdateRef = useRef(0);
  const isAutoScrollingRef = useRef(false);
  const initialOffsetAppliedRef = useRef(false);
  const lastTotalBytesRef = useRef(0); // Added missing ref
  // Anchor tracking
  // We track the "Logical Position" (Row Index for ASCII, Byte Offset for Hex)
  const anchorPositionRef = useRef(0);
  const prevScrollHeightRef = useRef(0);
  const prevTotalBytesRef = useRef(0);

  const lastProgrammaticScrollTopRef = useRef<number | null>(null);

  // Calculate scroll height based on total bytes OR explicit total rows
  const scrollHeight = calculateScrollHeight(totalBytes, totalRows);

  // Helper: Calculate logical anchor position from scroll top
  const getLogicalPosition = useCallback((currentScrollTop: number) => {
    if (scrollHeight <= 0) return 0;
    const ratio = currentScrollTop / scrollHeight;
    if (totalRows !== undefined) {
      // Anchor on Row Index
      return ratio * totalRows;
    } else {
      // Anchor on Byte Offset
      return ratio * totalBytes;
    }
  }, [scrollHeight, totalRows, totalBytes]);

  // Helper: Calculate scroll top from logical anchor position
  const getScrollTopFromAnchor = useCallback((anchor: number, currentTotalRows?: number, currentTotalBytes?: number, currentScrollHeight?: number) => {
    const tRows = currentTotalRows ?? totalRows;
    const tBytes = currentTotalBytes ?? totalBytes;
    const sHeight = currentScrollHeight ?? scrollHeight;

    if (tRows !== undefined && tRows > 0) {
      return (anchor / tRows) * sHeight;
    }
    if (tBytes !== undefined && tBytes > 0) {
      return (anchor / tBytes) * sHeight;
    }
    return 0;
  }, [totalRows, totalBytes, scrollHeight]);

  // Update visible rows based on container height
  useEffect(() => {
    if (containerRef.current) {
      const height = containerRef.current.clientHeight;
      if (height > 0) {
        setVisibleRows(Math.ceil(height / ROW_HEIGHT));
      }
    }
  }, []);

  // Anchor Scroll Logic: Maintain viewing position when data grows
  useLayoutEffect(() => {
    // If not auto-scrolling...
    if (!autoScroll && containerRef.current) {
      // Check if geometry changed significantly (Bytes grew OR Height changed due to new TotalLines)
      const bytesChanged = totalBytes > prevTotalBytesRef.current;
      const heightChanged = Math.abs(scrollHeight - prevScrollHeightRef.current) > 1;

      // Only re-anchor if we have a valid previous state and something changed
      if (prevTotalBytesRef.current > 0 && (bytesChanged || heightChanged)) {
        // Recalculate scrollTop for the SAME logical position in the NEW coordinate system
        const newScrollTop = getScrollTopFromAnchor(anchorPositionRef.current, totalRows, totalBytes, scrollHeight);

        // If the position needs adjustment
        if (Math.abs(containerRef.current.scrollTop - newScrollTop) > 1) {
          containerRef.current.scrollTop = newScrollTop;
          // eslint-disable-next-line react-hooks/set-state-in-effect
          setScrollTop(newScrollTop);

          // Mark as programmatic to ignore header feedback
          lastProgrammaticScrollTopRef.current = newScrollTop;
        }
      }
    }

    // Update refs for next render
    prevTotalBytesRef.current = totalBytes;
    prevScrollHeightRef.current = scrollHeight;
    // NOTE: We do NOT update anchorPositionRef here. We trust our previous anchor.
    // Only USER interaction should update the anchor.
  }, [totalBytes, scrollHeight, autoScroll, totalRows, getScrollTopFromAnchor]);

  // Handle initial offset on mount (manual mode only)
  useLayoutEffect(() => {
    if (!autoScroll && initialOffset > 0 && totalBytes > 0 && !initialOffsetAppliedRef.current) {
      initialOffsetAppliedRef.current = true;

      // Calculate initial scroll top
      const targetScrollTop = byteOffsetToScrollTop(initialOffset, scrollHeight, totalBytes);

      // Initialize anchor based on that position (normalized to Row/Byte as needed)
      // Note: initialOffset is always BYTES. If in Row mode, we map Byte->Row approximate?
      // Actually, if we start in Row mode, we might want initialRow?
      // But props is initialOffset (bytes).
      // We will perform a best-effort conversion to our Anchor Unit.
      if (totalRows !== undefined) {
        // Convert Byte Offset to Row Index (approx)
        const ratio = initialOffset / totalBytes;
        anchorPositionRef.current = ratio * totalRows;
      } else {
        anchorPositionRef.current = initialOffset;
      }

      if (containerRef.current) {
        containerRef.current.scrollTop = targetScrollTop;
        // eslint-disable-next-line react-hooks/set-state-in-effect
        setScrollTop(targetScrollTop);
        lastProgrammaticScrollTopRef.current = targetScrollTop;
      }
    }
  }, [autoScroll, initialOffset, totalBytes, scrollHeight, totalRows]);

  // Auto-scroll: keep at bottom when new data arrives
  useEffect(() => {
    // Only auto-scroll if enabled AND data GREW
    const hasNewData = totalBytes > lastTotalBytesRef.current;

    if (autoScroll && totalBytes > 0 && containerRef.current && hasNewData) {
      lastTotalBytesRef.current = totalBytes;

      isAutoScrollingRef.current = true;

      const viewportHeight = containerRef.current.clientHeight;
      const bottomByteOffset = calculateBottomByteOffset(totalBytes, viewportHeight, scrollHeight);
      const targetScrollTop = byteOffsetToScrollTop(bottomByteOffset, scrollHeight, totalBytes);

      containerRef.current.scrollTop = targetScrollTop;
      setScrollTop(targetScrollTop);
      lastProgrammaticScrollTopRef.current = targetScrollTop;

      // Update anchor to bottom
      anchorPositionRef.current = getLogicalPosition(targetScrollTop);

      requestAnimationFrame(() => {
        isAutoScrollingRef.current = false;
      });
    }
    // Update tracker if not autoscrolling
    if (!autoScroll) {
      lastTotalBytesRef.current = totalBytes;
    }
  }, [autoScroll, totalBytes, scrollHeight, getLogicalPosition]);

  // Hook to capture anchor when AutoScroll stops (Manual Stop or Toggle Off)
  useEffect(() => {
    if (!autoScroll && containerRef.current) {
      // User just stopped auto-scroll. Lock to current position.
      anchorPositionRef.current = getLogicalPosition(containerRef.current.scrollTop);
    }
  }, [autoScroll, getLogicalPosition]);

  // Handle user scroll
  const handleScroll = useCallback(
    (e: React.UIEvent<HTMLDivElement>) => {
      const newScrollTop = e.currentTarget.scrollTop;
      setScrollTop(newScrollTop);

      // Check for programmatic scroll echo
      if (lastProgrammaticScrollTopRef.current !== null) {
        if (Math.abs(newScrollTop - lastProgrammaticScrollTopRef.current) < 2) {
          return;
        }
        lastProgrammaticScrollTopRef.current = null;
      }

      if (isAutoScrollingRef.current) return;

      const now = Date.now();
      if (now - lastUpdateRef.current < THROTTLE_MS) return;
      lastUpdateRef.current = now;

      // Update Anchor using our Logical Unit (Row or Byte)
      anchorPositionRef.current = getLogicalPosition(newScrollTop);

      // Report byte offset to parent (Always Bytes for sync)
      const byteOffset = scrollTopToByteOffset(newScrollTop, scrollHeight, totalBytes);
      onScrollChange(clampByteOffset(byteOffset, totalBytes));
    },
    [scrollHeight, totalBytes, onScrollChange, getLogicalPosition]
  );

  // Get current byte offset
  const getByteOffset = useCallback(() => {
    return scrollTopToByteOffset(scrollTop, scrollHeight, totalBytes);
  }, [scrollTop, scrollHeight, totalBytes]);

  // Programmatic scroll helper
  const scrollTo = useCallback((newScrollTop: number, newByteOffset?: number) => {
    if (containerRef.current) {
      containerRef.current.scrollTop = newScrollTop;
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setScrollTop(newScrollTop);
      lastProgrammaticScrollTopRef.current = newScrollTop;

      if (newByteOffset !== undefined) {
        // Forced anchor update (e.g. exact line mapping)
        // If we are in Row Mode (Ascii), anchor is usually Row Index.
        // If we are passed a Byte Offset, we might want to store that?
        // But our anchor logic expects "Logical Position".
        // If caller passes Byte Offset, and we are in Row Mode, we should probably calculate Row Index?
        // Or does the caller pass the Anchor Value directly?
        // Let's assume caller passes the BYTE OFFSET (source of truth).
        // We need to convert it to Logical Anchor if needed.
        // But wait, if we are setting scrollTop to match a line, 
        // we probably already know the line index.
        // Let's genericize: pass anchorValue?
        // No, let's keep it simple. The conflict "visual vs byte" is exactly what we are solving.
        // If we set scrollTop to Line 50, and byteOffset to 5000.
        // We want next scroll to start from 5000.
        //getLogicalPosition(newScrollTop) -> Row 50.
        // So updating anchor to getLogicalPosition(newScrollTop) is correct for continuity.
        // The passed 'newByteOffset' is for the PARENT update.
        // But we can just use the prop 'initialOffset' logic for that?

        // Actually, simply updating anchorPositionRef to getLogicalPosition(newScrollTop)
        // is enough to keep the VIEW stable at that new position.
        // If we want to correct the "Drift" on switch back, we need to ensure 'onScrollChange' sends the exact byte.
        // But 'onScrollChange' is driven by handleScroll.
        // If we suppress handleScroll, onScrollChange isn't called.
        // So we should call onScrollChange(newByteOffset) here!
        anchorPositionRef.current = getLogicalPosition(newScrollTop);
        onScrollChange(clampByteOffset(newByteOffset, totalBytes));
        // Also update prevRef to this exact value?
        // No, prevRef is for diffing.
      }
    }
  }, [onScrollChange, totalBytes, getLogicalPosition]);

  return {
    containerRef,
    scrollTop,
    scrollHeight,
    visibleRows,
    handleScroll,
    getByteOffset,
    scrollTo,
  };
}
