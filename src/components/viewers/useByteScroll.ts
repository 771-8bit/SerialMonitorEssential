/**
 * Byte-based scroll hook for HexViewer and AsciiViewer.
 * Manages scroll state using byte offset for consistency across modes.
 */

import { useState, useRef, useCallback, useEffect } from 'react';
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
}

export function useByteScroll({
    totalBytes,
    autoScroll,
    initialOffset,
    onScrollChange,
}: UseByteScrollProps): UseByteScrollReturn {
    const containerRef = useRef<HTMLDivElement>(null);
    const [scrollTop, setScrollTop] = useState(0);
    const [visibleRows, setVisibleRows] = useState(25);

    // Refs for preventing re-render loops
    const lastUpdateRef = useRef(0);
    const isAutoScrollingRef = useRef(false);
    const initialOffsetAppliedRef = useRef(false);

    // Calculate scroll height based on total bytes
    const scrollHeight = calculateScrollHeight(totalBytes);

    // Update visible rows based on container height
    useEffect(() => {
        if (containerRef.current) {
            const height = containerRef.current.clientHeight;
            if (height > 0) {
                setVisibleRows(Math.ceil(height / ROW_HEIGHT));
            }
        }
    }, []);

    // Handle initial offset on mount (manual mode only)
    useEffect(() => {
        if (!autoScroll && initialOffset > 0 && totalBytes > 0 && !initialOffsetAppliedRef.current) {
            initialOffsetAppliedRef.current = true;
            const targetScrollTop = byteOffsetToScrollTop(initialOffset, scrollHeight, totalBytes);
            if (containerRef.current) {
                containerRef.current.scrollTop = targetScrollTop;
                setScrollTop(targetScrollTop);
            }
        }
    }, [autoScroll, initialOffset, totalBytes, scrollHeight]);

    // Auto-scroll: keep at bottom when new data arrives
    useEffect(() => {
        if (autoScroll && totalBytes > 0 && containerRef.current) {
            isAutoScrollingRef.current = true;
            // Use byte-based bottom calculation for consistency in scaling mode
            const viewportHeight = containerRef.current.clientHeight;
            const bottomByteOffset = calculateBottomByteOffset(totalBytes, viewportHeight, scrollHeight);
            const targetScrollTop = byteOffsetToScrollTop(bottomByteOffset, scrollHeight, totalBytes);
            containerRef.current.scrollTop = targetScrollTop;
            setScrollTop(targetScrollTop);

            // Reset flag after animation frame
            requestAnimationFrame(() => {
                isAutoScrollingRef.current = false;
            });
        }
    }, [autoScroll, totalBytes, scrollHeight]);

    // Handle user scroll
    const handleScroll = useCallback(
        (e: React.UIEvent<HTMLDivElement>) => {
            const newScrollTop = e.currentTarget.scrollTop;
            setScrollTop(newScrollTop);

            // Skip reporting during auto-scroll
            if (isAutoScrollingRef.current) return;

            // Throttle scroll change reporting
            const now = Date.now();
            if (now - lastUpdateRef.current < THROTTLE_MS) return;
            lastUpdateRef.current = now;

            // Report byte offset to parent
            const byteOffset = scrollTopToByteOffset(newScrollTop, scrollHeight, totalBytes);
            onScrollChange(clampByteOffset(byteOffset, totalBytes));
        },
        [scrollHeight, totalBytes, onScrollChange]
    );

    // Get current byte offset
    const getByteOffset = useCallback(() => {
        return scrollTopToByteOffset(scrollTop, scrollHeight, totalBytes);
    }, [scrollTop, scrollHeight, totalBytes]);

    return {
        containerRef,
        scrollTop,
        scrollHeight,
        visibleRows,
        handleScroll,
        getByteOffset,
    };
}
