/**
 * Byte-based scroll utility functions.
 * All scroll calculations use byte offset for consistency between Hex/ASCII modes.
 */

import { MAX_SCROLL_HEIGHT, BYTES_PER_ROW, ROW_HEIGHT } from './viewerConstants';

/**
 * Calculate scroll height based on total bytes.
 * Converts bytes to rows, then rows to pixels.
 * Applies scaling if height exceeds MAX_SCROLL_HEIGHT.
 */
/**
 * Calculate scroll height based on total bytes or explicit total rows.
 * Converts bytes to rows (if totalRows not provided), then rows to pixels.
 * Applies scaling if height exceeds MAX_SCROLL_HEIGHT.
 */
export function calculateScrollHeight(totalBytes: number, totalRows?: number): number {
  const rows = totalRows ?? Math.ceil(totalBytes / BYTES_PER_ROW);
  const naturalHeight = rows * ROW_HEIGHT;
  return Math.min(naturalHeight, MAX_SCROLL_HEIGHT);
}

/**
 * Calculate scale factor for large data sets.
 * scale = 1 when no compression, < 1 when compressed.
 */
export function calculateScale(totalBytes: number, totalRows?: number): number {
  const rows = totalRows ?? Math.ceil(totalBytes / BYTES_PER_ROW);
  const naturalHeight = rows * ROW_HEIGHT;
  if (naturalHeight <= MAX_SCROLL_HEIGHT) {
    return 1;
  }
  return MAX_SCROLL_HEIGHT / naturalHeight;
}

/**
 * Convert scroll position to byte offset.
 * now accepts optional totalRows for more accurate row-based calculation in ASCII mode
 */
export function scrollTopToByteOffset(
  scrollTop: number,
  scrollHeight: number,
  totalBytes: number
): number {
  if (scrollHeight <= 0 || totalBytes <= 0) return 0;

  // Simple linear mapping: (scrollTop / scrollHeight) * totalBytes
  // This assumes uniform distribution, which is an approximation for ASCII
  // but matches the visual scrollbar position.
  return Math.floor((scrollTop / scrollHeight) * totalBytes);
}

/**
 * Convert byte offset to scroll position.
 */
export function byteOffsetToScrollTop(
  byteOffset: number,
  scrollHeight: number,
  totalBytes: number
): number {
  if (totalBytes <= 0) return 0;
  return (byteOffset / totalBytes) * scrollHeight;
}

/**
 * Calculate byte offset for bottom of data (auto-scroll).
 * Returns the byte offset that would show the last data.
 */
export function calculateBottomByteOffset(
  totalBytes: number,
  viewportHeight: number,
  scrollHeight: number
): number {
  if (totalBytes <= 0 || scrollHeight <= 0) return 0;
  // How many bytes fit in viewport
  const viewportBytes = (viewportHeight / scrollHeight) * totalBytes;
  return Math.max(0, totalBytes - viewportBytes);
}

/**
 * Clamp byte offset to valid range.
 */
export function clampByteOffset(byteOffset: number, totalBytes: number): number {
  return Math.max(0, Math.min(byteOffset, totalBytes));
}

/** Start line that asks the backend for the last lines (clamped there atomically) */
export const TAIL_START_LINE = Number.MAX_SAFE_INTEGER;

/**
 * First visible row for a scroll position, for fixed-height rows.
 *
 * Maps the full scroll range onto the full row range: the top shows row 0 and the
 * bottom shows the last page. With unscaled heights this is simply
 * scrollTop / ROW_HEIGHT. When the scroll height is capped (huge captures) the plain
 * ratio scrollTop / scrollHeight never reaches the last rows, because the maximum
 * scrollTop is scrollHeight - viewport, which in rows is far less than a page.
 */
export function rowAtScrollTop(
  scrollTop: number,
  scrollHeight: number,
  viewportHeight: number,
  totalRows: number
): number {
  const maxScroll = scrollHeight - viewportHeight;
  const lastTopRow = Math.max(0, totalRows - viewportHeight / ROW_HEIGHT);
  if (maxScroll <= 0 || viewportHeight <= 0) {
    // Not measured yet / everything fits: the plain ratio
    return scrollHeight > 0 ? Math.floor((scrollTop / scrollHeight) * totalRows) : 0;
  }
  return Math.floor(Math.min(1, Math.max(0, scrollTop / maxScroll)) * lastTopRow);
}
