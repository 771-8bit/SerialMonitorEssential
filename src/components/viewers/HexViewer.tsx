import { useEffect, useCallback, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ROW_HEIGHT, BUFFER_ROWS, BYTES_PER_ROW } from './viewerConstants';
import { useByteScroll } from './useByteScroll';
import { renderAsciiColumn } from '../../utils/hexUtils';
import './HexViewer.css';

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
  initialOffset?: number;
  onScrollChange?: (byteOffset: number) => void;
}

export default function HexViewer({
  totalBytes,
  autoScroll,
  initialOffset = 0,
  onScrollChange = () => {},
}: HexViewerProps) {
  // Data state
  const [rows, setRows] = useState<DisplayRow[]>([]);
  const [currentStartRow, setCurrentStartRow] = useState(0);

  // Fetching guard
  const fetchingRef = useRef(false);
  const lastFetchRowRef = useRef(-Infinity);
  const lastTotalBytesRef = useRef(0);

  // Byte-based scroll
  const { containerRef, scrollTop, scrollHeight, visibleRows, handleScroll, getByteOffset } =
    useByteScroll({
      totalBytes,
      autoScroll,
      initialOffset,
      onScrollChange,
    });

  // Fetch rows from backend
  const fetchRows = useCallback(
    async (startRow: number, force: boolean = false) => {
      if (fetchingRef.current) return;
      if (!force && Math.abs(startRow - lastFetchRowRef.current) < BUFFER_ROWS / 2) return;

      fetchingRef.current = true;
      lastFetchRowRef.current = startRow;

      try {
        const fetchCount = visibleRows + BUFFER_ROWS * 2;
        const payload = await invoke<DisplayRowsPayload>('get_display_rows', {
          startRow: startRow,
          rowCount: fetchCount,
        });
        setRows(payload.rows);
        setCurrentStartRow(startRow);
      } catch (err) {
        console.error('Failed to fetch rows:', err);
      } finally {
        fetchingRef.current = false;
      }
    },
    [visibleRows]
  );

  // Effect: fetch data when scroll position changes
  useEffect(() => {
    if (totalBytes === 0) {
      setRows([]);
      return;
    }

    // Calculate start row from scroll ratio (works for both normal and scaled modes)
    const totalRows = Math.ceil(totalBytes / BYTES_PER_ROW);
    const scrollRatio = scrollHeight > 0 ? scrollTop / scrollHeight : 0;
    const targetRow = Math.floor(scrollRatio * totalRows);
    const startRow = Math.max(0, targetRow - BUFFER_ROWS);

    // Force refetch when totalBytes changes (new data arrived)
    const forceRefetch = totalBytes !== lastTotalBytesRef.current;
    lastTotalBytesRef.current = totalBytes;

    fetchRows(startRow, forceRefetch);
  }, [totalBytes, scrollTop, scrollHeight, fetchRows]);

  // Debug info and display position calculation
  const totalRows = Math.ceil(totalBytes / BYTES_PER_ROW);
  const naturalHeight = totalRows * ROW_HEIGHT;
  const scale = scrollHeight < naturalHeight ? scrollHeight / naturalHeight : 1;
  const byteOffset = getByteOffset();

  // Always position rows at scrollTop - this ensures rows are visible in viewport
  // regardless of scale mode (rows are always placed where the viewport is looking)
  const displayTop = scrollTop;

  return (
    <div className="hex-viewer">
      <div className="hex-scroll-container" ref={containerRef} onScroll={handleScroll}>
        <div style={{ height: scrollHeight, position: 'relative' }}>
          <div style={{ position: 'absolute', top: displayTop, width: '100%' }}>
            {rows.map((row) => (
              <div key={row.offset} className="hex-row" style={{ height: ROW_HEIGHT }}>
                <span className="hex-offset">
                  {row.offset.toString(16).padStart(8, '0').toUpperCase()}
                </span>
                <span className="hex-bytes">{row.hex}</span>
                <span className="hex-ascii">{renderAsciiColumn(row.ascii)}</span>
              </div>
            ))}
          </div>
        </div>
      </div>
      <div className="hex-footer">
        <span>Total: {totalBytes.toLocaleString()} bytes</span>
        <span className="hex-debug">
          scrollTop={scrollTop.toFixed(0)} | scrollHeight={scrollHeight.toLocaleString()} |
          byteOffset={byteOffset.toLocaleString()} | displayTop={displayTop.toLocaleString()} |
          startRow={currentStartRow.toLocaleString()} | isScaled={scale < 1 ? 'YES' : 'no'} | scale=
          {scale.toFixed(4)}
        </span>
      </div>
    </div>
  );
}
