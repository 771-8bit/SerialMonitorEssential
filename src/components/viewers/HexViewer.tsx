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
  const [totalRows, setTotalRows] = useState(0);

  // Fetching guard and queue
  const isFetchingRef = useRef(false);
  const pendingStartRowRef = useRef<number | null>(null);
  const lastFetchRowRef = useRef(-Infinity);
  const totalRowsRef = useRef(0);
  const lastTotalBytesRef = useRef(0);

  // Calculate effective total rows for scroll height
  // Use backend confirmed rows if available, otherwise estimate
  const effectiveTotalRows =
    totalRowsRef.current > 0
      ? totalRowsRef.current
      : Math.max(1, Math.ceil(totalBytes / BYTES_PER_ROW));

  // Byte-based scroll
  const { containerRef, scrollTop, scrollHeight, visibleRows, handleScroll, getByteOffset } =
    useByteScroll({
      totalBytes,
      totalRows: effectiveTotalRows, // Use row-based anchoring like AsciiViewer
      autoScroll,
      initialOffset,
      onScrollChange,
    });

  // Fetch rows from backend
  const fetchRows = useCallback(
    async (startRow: number, force: boolean = false) => {
      // Always update the pending request
      pendingStartRowRef.current = startRow;

      // If already fetching, the loop will pick up this new pendingStartRowRef
      if (isFetchingRef.current) return;

      isFetchingRef.current = true;

      try {
        // Process requests until we catch up
        while (pendingStartRowRef.current !== null) {
          const targetStartRow = pendingStartRowRef.current;

          // Optimization: If we already fetched this row (or close enough) and not forced, skip
          // Check needs to be rigorous: if we are "close enough" but new data arrived (force), we must fetch.
          // The 'force' argument is only for the *initial* call. Subsequent loop iterations might lose 'force' context.
          // But 'force' is driven by totalBytes change.
          // Let's simplify: if targetStartRow is different from lastFetchRowRef, fetch it.
          // We can apply the buffer logic: if abs(target - last) < BUFFER/2, we can maybe skip?
          // BUT: if we are in this loop, it means we entered because of a request.
          // Unless pendingStartRow remained same as lastFetchRow?
          if (!force && Math.abs(targetStartRow - lastFetchRowRef.current) < BUFFER_ROWS / 2) {
            // Close enough, no need to fetch again.
            // Clear pending and break IF it's the latest.
            // Only clear if pending === target (it hasn't changed since we started checks)
            if (pendingStartRowRef.current === targetStartRow) {
              pendingStartRowRef.current = null;
            }
            continue; // Check loop again (or break if null)
          }

          // We are committing to fetch 'targetStartRow'.
          // Clear pending so we can detect *new* requests that come in during await.
          // If pending changes during await, we loop again.
          if (pendingStartRowRef.current === targetStartRow) {
            pendingStartRowRef.current = null;
          }

          lastFetchRowRef.current = targetStartRow;

          try {
            const fetchCount = visibleRows + BUFFER_ROWS * 2;
            const payload = await invoke<DisplayRowsPayload>('get_display_rows', {
              startRow: targetStartRow,
              rowCount: fetchCount,
            });

            // Only update state if this is still relevant?
            // Ideally yes. Even if old, it's better than blank.
            // But we want the *latest*.
            // If pendingStartRowRef is non-null, it means a newer request arrived.
            // We still show this intermediate result to prevent perceived lag?
            // Yes, showing *something* is better.
            setRows(payload.rows);
            setTotalRows(payload.total_rows);
            totalRowsRef.current = payload.total_rows;
            setCurrentStartRow(targetStartRow);
          } catch (err) {
            console.error('Failed to fetch rows:', err);
          }
        }
      } finally {
        isFetchingRef.current = false;
      }
    },
    [visibleRows]
  );

  // Effect: fetch data when scroll position changes
  useEffect(() => {
    if (totalBytes === 0) {
      setRows([]);
      setTotalRows(0);
      totalRowsRef.current = 0;
      return;
    }

    // Calculate start row from scroll ratio (works for both normal and scaled modes)
    // Use effectiveTotalRows to stay in sync with scroll bar size
    const scrollRatio = scrollHeight > 0 ? scrollTop / scrollHeight : 0;
    const targetRow = Math.floor(scrollRatio * effectiveTotalRows);
    const startRow = Math.max(0, targetRow - BUFFER_ROWS);

    // Force refetch when totalBytes changes (new data arrived)
    const forceRefetch = totalBytes !== lastTotalBytesRef.current;
    lastTotalBytesRef.current = totalBytes;

    fetchRows(startRow, forceRefetch);
  }, [totalBytes, scrollTop, scrollHeight, fetchRows, effectiveTotalRows]);

  // Debug info and display position calculation
  const naturalHeight = effectiveTotalRows * ROW_HEIGHT;
  const scale = scrollHeight < naturalHeight ? scrollHeight / naturalHeight : 1;
  const byteOffset = getByteOffset();

  // Always position rows at scrollTop - CORRECTED to account for buffer offset
  // We fetch rows starting at 'currentStartRow' (which is usually targetRow - BUFFER).
  // We need to shift the display top UP by the buffer amount so the target row aligns with scrollTop.

  // Use scrollRatio to determine the "Visual Target Row" consistent with scroll position
  const scrollRatio = scrollHeight > 0 ? scrollTop / scrollHeight : 0;
  const targetRow = Math.floor(scrollRatio * effectiveTotalRows);

  // Calculate the theoretical offset between target and fetched data
  // If positive, it means Target (Screen) is ahead of Start (Data) -> Data is "above".
  const rowDiff = targetRow - currentStartRow;

  // Visual correction:
  // If the data is lagging significantly (rowDiff is large), standard calculation would push data off-screen (top).
  // We clamp the diff to ensure that if we are lagging, we 'drag' the old data along with the scrollbar
  // so it remains visible until the new data arrives.
  // We allow normal positioning within the buffer range.
  const visualRowDiff = Math.min(rowDiff, BUFFER_ROWS);

  const displayTop = scrollTop - visualRowDiff * ROW_HEIGHT;

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
        <span>
          Total: {totalBytes.toLocaleString()} bytes ({totalRows.toLocaleString()} rows)
        </span>
        <span className="hex-debug">
          scrollTop={scrollTop.toFixed(0)} | scrollHeight={scrollHeight.toLocaleString()} |
          byteOffset={byteOffset.toLocaleString()} | displayTop={displayTop.toLocaleString()} |
          startRow={currentStartRow.toLocaleString()} | targetRow={targetRow.toLocaleString()} |
          fetched={rows.length} | isScaled={scale < 1 ? 'YES' : 'no'} | scale=
          {scale.toFixed(4)}
        </span>
      </div>
    </div>
  );
}
