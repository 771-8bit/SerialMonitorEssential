import { useEffect, useCallback, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ROW_HEIGHT, BUFFER_ROWS } from './viewerConstants';
import { useByteScroll } from './useByteScroll';
import './AsciiViewer.css';

interface AsciiLine {
  offset: number;
  text: string;
  timestamp: string | null;
}

interface AsciiLinesPayload {
  lines: AsciiLine[];
  total_lines: number;
}

interface AsciiViewerProps {
  totalBytes: number;
  autoScroll: boolean;
  showTimestamp?: boolean;
  lineWrap?: boolean;
  initialOffset?: number;
  onScrollChange?: (byteOffset: number) => void;
}

export default function AsciiViewer({
  totalBytes,
  autoScroll,
  showTimestamp = false,
  lineWrap = false,
  initialOffset = 0,
  onScrollChange = () => {},
}: AsciiViewerProps) {
  // Data state
  const [lines, setLines] = useState<AsciiLine[]>([]);
  const [totalLines, setTotalLines] = useState(0);
  const [currentStartLine, setCurrentStartLine] = useState(0);

  // Fetching guard
  const fetchingRef = useRef(false);
  const lastFetchLineRef = useRef(-Infinity);
  const lastTotalBytesRef = useRef(0);

  // Ref for timestamp column to sync scroll
  const timestampColumnRef = useRef<HTMLDivElement>(null);
  const isSyncingScrollRef = useRef(false);

  // Track total lines from backend for proper scroll calculation
  const totalLinesRef = useRef(0);

  // Track initialization to prevent double-setting
  const [initialLineSet, setInitialLineSet] = useState(false);

  // Determine effective total lines for scroll calculation
  const effectiveTotalLines =
    totalLinesRef.current > 0 ? totalLinesRef.current : Math.max(1, Math.ceil(totalBytes / 80));

  // Byte-based scroll (Text column is main scroll)
  const {
    containerRef,
    scrollTop,
    scrollHeight,
    visibleRows,
    handleScroll: baseHandleScroll,
    getByteOffset,
    scrollTo,
  } = useByteScroll({
    totalBytes,
    totalRows: effectiveTotalLines,
    autoScroll,
    initialOffset,
    onScrollChange,
  });

  // Calculate ASCII-specific scroll height based on actual line count
  // NOW HANDLED BY HOOK via totalRows
  const asciiScrollHeight = scrollHeight;

  // Initialize scroll position using exact line mapping from backend
  useEffect(() => {
    // Only run if we have an initial offset (e.g. switched from Hex), haven't set it yet, and data exists.
    // CRITICAL FIX: We MUST wait until effectiveTotalLines > 1 (meaning we have actual line count or at least a valid estimation based on data)
    // to avoid clamping the scroll position to an incorrectly small scrollHeight.
    // If totalLinesRef.current is 0, effectiveTotalLines is a raw estimation.
    // Ideally we wait for totalLinesRef.current > 0 if totalBytes is large.
    const hasValidLineCount = totalLinesRef.current > 0 || totalBytes < 1000;

    if (!initialLineSet && totalBytes > 0 && hasValidLineCount) {
      if (initialOffset > 0) {
        invoke<{ line_index: number }>('get_line_index', { offset: initialOffset })
          .then((res) => {
            // Calculate exact target scrollTop for this line
            const targetScrollTop = res.line_index * ROW_HEIGHT;

            // Force scroll to this position and update anchor to match
            // Passing initialOffset ensures the "Byte Anchor" remains exact (e.g. 5000),
            // even though visual position is now at Line Start (e.g. 4980 bytes).
            scrollTo(targetScrollTop, initialOffset);

            setInitialLineSet(true);
          })
          .catch((err) => {
            console.error('Failed to get line index:', err);
            setInitialLineSet(true); // Fallback to default behavior
          });
      } else {
        // No offset, just mark as set
        setInitialLineSet(true);
      }
    }
  }, [initialOffset, totalBytes, scrollTo, initialLineSet, effectiveTotalLines]);

  // Fetch lines from backend
  const fetchLines = useCallback(
    async (startLine: number, force: boolean = false) => {
      if (fetchingRef.current) return;
      if (!force && Math.abs(startLine - lastFetchLineRef.current) < BUFFER_ROWS / 2) return;

      fetchingRef.current = true;
      lastFetchLineRef.current = startLine;

      try {
        const fetchCount = visibleRows + BUFFER_ROWS * 2;
        const payload = await invoke<AsciiLinesPayload>('get_ascii_lines', {
          startLine: startLine,
          lineCount: fetchCount,
          showCtrl: false,
          showTimestamp: showTimestamp,
        });
        setLines(payload.lines);
        setTotalLines(payload.total_lines);
        totalLinesRef.current = payload.total_lines;
        setCurrentStartLine(startLine);
      } catch (err) {
        console.error('Failed to fetch lines:', err);
      } finally {
        fetchingRef.current = false;
      }
    },
    [visibleRows, showTimestamp]
  );

  // Effect: fetch data when scroll position changes or totalBytes changes
  useEffect(() => {
    if (totalBytes === 0) {
      setLines([]);
      setTotalLines(0);
      totalLinesRef.current = 0;
      return;
    }

    // Use byte offset for consistent Hex/ASCII scroll position
    const byteOffset = getByteOffset();
    // Use totalLines from backend if available, otherwise estimate
    // const effectiveTotalLines =
    //   totalLinesRef.current > 0 ? totalLinesRef.current : Math.max(1, Math.ceil(totalBytes / 80));

    // Calculate start line from byte ratio (same approach as HexViewer)
    const scrollRatio = totalBytes > 0 ? byteOffset / totalBytes : 0;
    const startLine = Math.max(0, Math.floor(scrollRatio * effectiveTotalLines) - BUFFER_ROWS);

    // Force refetch when totalBytes changes (new data arrived)
    const forceRefetch = totalBytes !== lastTotalBytesRef.current;
    lastTotalBytesRef.current = totalBytes;

    fetchLines(startLine, forceRefetch);
  }, [totalBytes, scrollTop, getByteOffset, fetchLines, effectiveTotalLines]);

  // Handle scroll from Text column (main scroll) - sync to timestamp
  const handleScroll = useCallback(
    (e: React.UIEvent<HTMLDivElement>) => {
      baseHandleScroll(e);

      // Sync timestamp column
      if (!isSyncingScrollRef.current && showTimestamp && timestampColumnRef.current) {
        isSyncingScrollRef.current = true;
        timestampColumnRef.current.scrollTop = e.currentTarget.scrollTop;
        isSyncingScrollRef.current = false;
      }
    },
    [baseHandleScroll, showTimestamp]
  );

  // Handle scroll from Timestamp column - sync back to text
  const handleTimestampScroll = useCallback(
    (e: React.UIEvent<HTMLDivElement>) => {
      if (!isSyncingScrollRef.current && containerRef.current) {
        isSyncingScrollRef.current = true;
        containerRef.current.scrollTop = e.currentTarget.scrollTop;
        isSyncingScrollRef.current = false;
      }
    },
    [containerRef]
  );

  // Calculate display position
  // Adjust top position to account for the buffer offset (we fetched starting at 'startLine' which is < 'targetLine')
  // We want the line corresponding to 'startLine' to appear at 'startLine * Height', not 'scrollTop'.
  // displayTop = scrollTop - (targetLine - startLine) * ROW_HEIGHT
  const scrollRatio = scrollHeight > 0 ? scrollTop / scrollHeight : 0;
  const targetLine = Math.floor(scrollRatio * effectiveTotalLines);
  const displayTop = scrollTop - (targetLine - currentStartLine) * ROW_HEIGHT;

  const naturalLineHeight = totalLines * ROW_HEIGHT;
  const isScaled = asciiScrollHeight < naturalLineHeight;

  return (
    <div className={`ascii-viewer ${lineWrap ? 'line-wrap' : ''}`}>
      <div className="ascii-content-wrapper">
        {/* Timestamp column (synced, not main scroll) */}
        {showTimestamp && (
          <div
            className="ascii-timestamp-column"
            ref={timestampColumnRef}
            onScroll={handleTimestampScroll}
          >
            <div style={{ height: asciiScrollHeight, position: 'relative' }}>
              <div style={{ position: 'absolute', top: displayTop, width: '100%' }}>
                {lines.map((line) => (
                  <div
                    key={`ts-${line.offset}`}
                    className="ascii-timestamp-row"
                    style={{ height: ROW_HEIGHT }}
                  >
                    {line.timestamp || ''}
                  </div>
                ))}
              </div>
            </div>
          </div>
        )}

        {/* Text column (main scroll) */}
        <div className="ascii-text-column" ref={containerRef} onScroll={handleScroll}>
          <div style={{ height: asciiScrollHeight, position: 'relative' }}>
            <div style={{ position: 'absolute', top: displayTop }}>
              {lines.map((line) => (
                <div
                  key={line.offset}
                  className="ascii-text-row"
                  style={{ height: lineWrap ? 'auto' : ROW_HEIGHT, minHeight: ROW_HEIGHT }}
                >
                  {line.text}
                </div>
              ))}
            </div>
          </div>
        </div>
      </div>

      <div className="ascii-footer">
        <span>
          Total: {totalBytes.toLocaleString()} bytes ({totalLines.toLocaleString()} lines)
        </span>
        <span className="ascii-debug">
          scrollTop={scrollTop.toFixed(0)} | scrollHeight={asciiScrollHeight.toLocaleString()} |
          byteOffset={getByteOffset().toLocaleString()} | displayTop={displayTop.toLocaleString()} |
          startLine={currentStartLine.toLocaleString()} | isScaled={isScaled ? 'YES' : 'no'} |
          scale={(asciiScrollHeight / naturalLineHeight || 1).toFixed(4)}
        </span>
      </div>
    </div>
  );
}
