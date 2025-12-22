import { useEffect, useRef, useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import './HexViewer.css';

const ROW_HEIGHT = 20;
const VISIBLE_ROWS = 25;
const BUFFER_ROWS = 5;
const FETCH_ROW_COUNT = VISIBLE_ROWS + BUFFER_ROWS * 2;
const MAX_SCROLL_HEIGHT = 10_000_000;
const THROTTLE_MS = 100;
const VIEWPORT_HEIGHT = 400;

interface DisplayRow {
  offset: number;
  hex: string;
  ascii: string;
}

interface DisplayRowsPayload {
  rows: DisplayRow[];
  total_rows: number;
}

interface HexViewerProps {
  totalBytes: number;
  autoScroll: boolean;
}

export default function HexViewer({ totalBytes, autoScroll }: HexViewerProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [rows, setRows] = useState<DisplayRow[]>([]);
  const [totalRows, setTotalRows] = useState(0);
  const [currentStartRow, setCurrentStartRow] = useState(0);
  const [scrollTop, setScrollTop] = useState(0);
  const fetchingRef = useRef(false);
  const ignoreScrollRef = useRef(false);
  const prevScaleRef = useRef(1);
  const lastUpdateRef = useRef(0);

  // Calculate scale factor
  const getScaleInfo = useCallback((rowCount: number) => {
    const naturalHeight = rowCount * ROW_HEIGHT;
    if (naturalHeight <= MAX_SCROLL_HEIGHT) {
      return { scale: 1, scrollHeight: naturalHeight };
    }
    const scale = MAX_SCROLL_HEIGHT / naturalHeight;
    return { scale, scrollHeight: MAX_SCROLL_HEIGHT };
  }, []);

  // Fetch rows from backend
  const fetchRows = useCallback(async (newStartRow: number, newTotalRows?: number) => {
    if (fetchingRef.current) return;
    fetchingRef.current = true;

    try {
      const clampedStartRow = Math.max(0, newStartRow);
      const result = await invoke<DisplayRowsPayload>('get_display_rows', {
        startRow: clampedStartRow,
        rowCount: FETCH_ROW_COUNT,
      });
      setRows(result.rows);
      if (newTotalRows !== undefined) {
        setTotalRows(newTotalRows);
      } else {
        setTotalRows(result.total_rows);
      }
      setCurrentStartRow(clampedStartRow);
    } catch (e) {
      console.error('Failed to fetch display rows:', e);
    } finally {
      fetchingRef.current = false;
    }
  }, []);

  // Handle totalBytes changes
  useEffect(() => {
    if (totalBytes === 0) return;

    const newTotalRows = Math.ceil(totalBytes / 16);
    const { scale: newScale, scrollHeight } = getScaleInfo(newTotalRows);

    if (autoScroll) {
      // Auto-scroll: fetch bottom rows (throttled)
      const now = Date.now();
      if (now - lastUpdateRef.current < THROTTLE_MS / 2) return;
      lastUpdateRef.current = now;

      const newStartRow = Math.max(0, newTotalRows - VISIBLE_ROWS);
      fetchRows(newStartRow, newTotalRows);

      requestAnimationFrame(() => {
        if (containerRef.current) {
          ignoreScrollRef.current = true;
          const targetScrollTop = Math.max(0, scrollHeight - VIEWPORT_HEIGHT);
          containerRef.current.scrollTop = targetScrollTop;
          setScrollTop(targetScrollTop);
          requestAnimationFrame(() => {
            ignoreScrollRef.current = false;
          });
        }
      });
    } else {
      // Manual mode: throttle updates
      const now = Date.now();
      const prevScale = prevScaleRef.current;
      const scaleChanged = Math.abs(newScale - prevScale) > 0.0001;

      if (now - lastUpdateRef.current < THROTTLE_MS && !scaleChanged) {
        return;
      }
      lastUpdateRef.current = now;

      setTotalRows(newTotalRows);

      // If scale changed, sync scrollTop to keep rows visible
      if (containerRef.current && scaleChanged) {
        ignoreScrollRef.current = true;
        const targetScrollTop = currentStartRow * ROW_HEIGHT * newScale;
        containerRef.current.scrollTop = targetScrollTop;
        setScrollTop(targetScrollTop);
        requestAnimationFrame(() => {
          ignoreScrollRef.current = false;
        });
      }
    }

    prevScaleRef.current = newScale;
  }, [totalBytes, autoScroll, fetchRows, getScaleInfo, currentStartRow]);

  // Handle scroll
  const handleScroll = useCallback(
    (e: React.UIEvent<HTMLDivElement>) => {
      const newScrollTop = e.currentTarget.scrollTop;
      setScrollTop(newScrollTop);

      // Ignore programmatic scroll events (e.g., from autoScroll)
      if (ignoreScrollRef.current) return;

      const { scale } = getScaleInfo(totalRows);

      // Calculate row from scroll position
      let newStartRow: number;
      if (scale === 1) {
        newStartRow = Math.floor(newScrollTop / ROW_HEIGHT);
      } else {
        newStartRow = Math.floor(newScrollTop / scale / ROW_HEIGHT);
      }
      newStartRow = Math.max(0, newStartRow - BUFFER_ROWS);

      // Fetch if needed
      if (Math.abs(newStartRow - currentStartRow) > BUFFER_ROWS / 2) {
        fetchRows(newStartRow);
      }
    },
    [currentStartRow, fetchRows, getScaleInfo, totalRows]
  );

  const { scale, scrollHeight } = getScaleInfo(totalRows);

  // Calculate displayTop based on scroll position for accurate positioning
  // In scaled mode, we need to position rows relative to the current scroll position
  let displayTop: number;
  if (scale === 1) {
    displayTop = currentStartRow * ROW_HEIGHT;
  } else {
    // Position rows so they appear in the viewport correctly
    // scrollTop shows the top of the viewport in the scaled space
    // We want the rows to start at the scrollTop position
    displayTop = scrollTop;
  }

  if (totalRows === 0 && totalBytes === 0) {
    return (
      <div className="hex-viewer hex-viewer-empty">
        <div className="hex-placeholder">No data received...</div>
      </div>
    );
  }

  return (
    <div className="hex-viewer">
      <div className="hex-header">
        <span className="hex-offset">Offset</span>
        <span className="hex-bytes">
          {Array.from({ length: 16 }, (_, i) => i.toString(16).toUpperCase().padStart(2, '0')).join(
            ' '
          )}
        </span>
        <span className="hex-ascii">ASCII</span>
      </div>
      <div
        ref={containerRef}
        className="hex-scroll-container"
        style={{ height: VIEWPORT_HEIGHT, overflow: 'auto' }}
        onScroll={handleScroll}
      >
        <div style={{ height: scrollHeight, position: 'relative' }}>
          <div style={{ position: 'absolute', top: displayTop, width: '100%' }}>
            {rows.map((row) => (
              <div key={row.offset} className="hex-row" style={{ height: ROW_HEIGHT }}>
                <span className="hex-offset">
                  {row.offset.toString(16).toUpperCase().padStart(8, '0')}
                </span>
                <span className="hex-bytes">{row.hex}</span>
                <span className="hex-ascii">{row.ascii}</span>
              </div>
            ))}
          </div>
        </div>
      </div>
      <div className="hex-footer">
        <span>
          Total: {totalBytes.toLocaleString()} bytes ({totalRows.toLocaleString()} rows)
        </span>
        <span style={{ marginLeft: 10, color: '#888' }}>
          Row: {currentStartRow.toLocaleString()}
        </span>
        {scale < 1 && (
          <span style={{ marginLeft: 10, color: '#f80' }}>
            (scale: {(scale * 100).toFixed(1)}%)
          </span>
        )}
      </div>
    </div>
  );
}
