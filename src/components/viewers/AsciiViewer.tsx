import { useEffect, useCallback, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ROW_HEIGHT, BUFFER_ROWS } from './viewerConstants';
import { useByteScroll } from './useByteScroll';
import { TAIL_START_LINE } from './scrollUtils';
import './AsciiViewer.css';
import { asciiDisplaySegments } from './asciiDisplay';

interface AsciiLine {
  offset: number;
  text: string;
  timestamp: string | null;
}

interface AsciiLinesPayload {
  lines: AsciiLine[];
  total_lines: number;
  /** Line index of lines[0]; a request past the end is clamped to the last lines */
  start_line: number;
  /** Whether the served lines run to the end of the data */
  reaches_end: boolean;
}

interface AsciiViewerProps {
  totalBytes: number;
  autoScroll: boolean;
  showTimestamp?: boolean;
  lineWrap?: boolean;
  initialOffset?: number;
  onScrollChange?: (byteOffset: number) => void;
  timestampSeparator?: string;
  /** Show CR/LF as symbols (otherwise hidden) and TAB as a symbol (otherwise a tab) */
  showCrLf?: boolean;
  /** Show NUL as a symbol (otherwise removed) */
  showNul?: boolean;
}

export default function AsciiViewer({
  totalBytes,
  autoScroll,
  showTimestamp = false,
  lineWrap = false,
  initialOffset = 0,
  onScrollChange = () => {},
  timestampSeparator = ' ',
  showCrLf = false,
  showNul = false,
}: AsciiViewerProps) {
  // Data state
  const [lines, setLines] = useState<AsciiLine[]>([]);
  const [totalLines, setTotalLines] = useState(0);
  const [currentStartLine, setCurrentStartLine] = useState(0);
  // Whether the fetched block contains the last line (issue #3). The block is then
  // laid out bottom-anchored, so the newest line is at the very bottom even when
  // Line Wrap makes rows taller than ROW_HEIGHT (the scroll model assumes fixed
  // rows) or the scroll height is scaled down for huge captures. Decided by the
  // data, not the scroll position, so entering/leaving the mode cannot oscillate.
  const [reachesEnd, setReachesEnd] = useState(false);
  const isTail = reachesEnd && lines.length > 0;

  // Fetching guard
  const fetchingRef = useRef(false);
  const lastFetchLineRef = useRef(-Infinity);
  const lastTotalBytesRef = useRef(0);

  // Track total lines from backend for proper scroll calculation
  const totalLinesRef = useRef(0);

  // Track initialization to prevent double-setting
  const [initialLineSet, setInitialLineSet] = useState(false);

  // Determine effective total lines for scroll calculation
  const effectiveTotalLines =
    totalLinesRef.current > 0 ? totalLinesRef.current : Math.max(1, Math.ceil(totalBytes / 80));

  // Byte-based scroll
  const {
    containerRef,
    scrollTop,
    scrollHeight,
    visibleRows,
    handleScroll,
    getByteOffset,
    scrollTo,
  } = useByteScroll({
    totalBytes,
    totalRows: effectiveTotalLines,
    autoScroll,
    initialOffset,
    onScrollChange,
  });

  // Track visible-row count as of the last fetch, to detect viewport growth
  // (e.g. window resize) that should trigger a refetch of newly exposed rows.
  const prevVisibleRowsRef = useRef(visibleRows);
  // Track the timestamp toggle: switching it must force a refetch, otherwise
  // the proximity skip keeps showing rows fetched with the old setting.
  const prevShowTimestampRef = useRef(showTimestamp);

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

  // Pending request queue: a request arriving while a fetch is in flight is
  // processed afterwards instead of dropped (a dropped forced refetch would
  // leave the last received data permanently un-displayed).
  const pendingFetchRef = useRef<{ startLine: number; force: boolean } | null>(null);

  // Fetch lines from backend
  const fetchLines = useCallback(
    async (startLine: number, force: boolean = false) => {
      // Always record the latest request, OR-ing the force flag so a forced
      // refetch queued during an in-flight fetch is not lost.
      pendingFetchRef.current = {
        startLine,
        force: force || (pendingFetchRef.current?.force ?? false),
      };
      if (fetchingRef.current) return;

      fetchingRef.current = true;
      try {
        while (pendingFetchRef.current) {
          const { startLine: target, force: effectiveForce } = pendingFetchRef.current;
          pendingFetchRef.current = null;

          // Optimization: Skip if we are close to the last fetched line.
          // EXCEPTION: If target is 0, we MUST ensure we have the very first line.
          if (
            !effectiveForce &&
            target !== 0 &&
            Math.abs(target - lastFetchLineRef.current) < BUFFER_ROWS / 2
          )
            continue;
          if (!effectiveForce && target === 0 && lastFetchLineRef.current === 0) continue;

          lastFetchLineRef.current = target;

          try {
            const fetchCount = visibleRows + BUFFER_ROWS * 2;
            const payload = await invoke<AsciiLinesPayload>('get_ascii_lines', {
              startLine: target,
              lineCount: fetchCount,
              showCtrl: false,
              showTimestamp: showTimestamp,
            });
            setLines(payload.lines);
            setTotalLines(payload.total_lines);
            totalLinesRef.current = payload.total_lines;
            setCurrentStartLine(payload.start_line ?? target);
            setReachesEnd(payload.reaches_end ?? false);
          } catch (err) {
            console.error('Failed to fetch lines:', err);
          }
        }
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
    // Auto Scroll always shows the end: ask for the tail rather than a position
    // derived from the (fixed-row, possibly scaled) scroll model.
    const startLine = autoScroll
      ? TAIL_START_LINE
      : Math.max(0, Math.floor(scrollRatio * effectiveTotalLines) - BUFFER_ROWS);

    // Force refetch when totalBytes changes (new data arrived)
    const forceRefetch = totalBytes !== lastTotalBytesRef.current;
    lastTotalBytesRef.current = totalBytes;

    // Refetch when the viewport grew (more rows visible than last fetch covered)
    const viewportGrew = visibleRows > prevVisibleRowsRef.current;
    prevVisibleRowsRef.current = visibleRows;

    // Refetch when the timestamp display setting changed
    const timestampToggled = showTimestamp !== prevShowTimestampRef.current;
    prevShowTimestampRef.current = showTimestamp;

    fetchLines(startLine, forceRefetch || viewportGrew || timestampToggled);
  }, [
    autoScroll,
    totalBytes,
    scrollTop,
    getByteOffset,
    fetchLines,
    effectiveTotalLines,
    visibleRows,
    showTimestamp,
  ]);

  // Auto Scroll: pin to the real bottom of the rendered rows. The shared scroll hook
  // scrolls to the bottom of the fixed-row model, which is short of the real bottom
  // when wrapped rows are taller. Declared after the hook so this runs after its
  // auto-scroll effect in the same commit.
  useEffect(() => {
    const el = containerRef.current;
    if (!autoScroll || !isTail || !el) return;
    const bottom = el.scrollHeight - el.clientHeight;
    if (Math.abs(el.scrollTop - bottom) > 1) el.scrollTop = bottom;
  }, [autoScroll, isTail, lines, totalBytes, asciiScrollHeight, containerRef]);

  // Handle Ctrl+A to warn user about partial selection.
  // Skip when focus is in an editable element (Send textarea, search input,
  // etc.) so their native select-all keeps working.
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === 'a') {
        const target = e.target as HTMLElement | null;
        if (target?.closest('input, textarea, select, [contenteditable]')) return;
        e.preventDefault();
        alert(
          'To copy all data, please use the export function or copy button in the toolbar.\n(Standard selection only copies visible data due to performance optimizations)'
        );
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, []);

  // Handle Copy to restore Real ASCII control codes
  const handleCopy = useCallback(
    (e: React.ClipboardEvent<HTMLDivElement>) => {
      const selection = window.getSelection();
      if (!selection) return;
      let text = selection.toString();

      if (text) {
        e.preventDefault();

        // 1. Remove browser-inserted newlines (display artifacts)
        text = text.replace(/[\r\n]+/g, '');

        // 2. Replace Unicode Control Pictures back to ASCII
        // 0x2401-0x241F -> 0x01-0x1F (Skip 0x2400 NULL to prevent truncation)
        // 0x2421 -> 0x7F
        let restoredText = text.replace(/[\u2401-\u241F\u2421]/g, (char) => {
          const code = char.charCodeAt(0);
          if (code === 0x2421) return '\x7F'; // DEL
          return String.fromCharCode(code - 0x2400); // 0x01-0x1F
        });

        // 3. Insert Separator between Timestamp and Data if separators are enabled
        // Timestamp format: [HH:MM:SS.mmm]
        // We look for the closing bracket ']' followed directly by the next character,
        // and insert the separator.
        // Note: If text implies valid newlines, this global replace works per occurrence.
        if (showTimestamp && timestampSeparator) {
          // Regex: Matches timestamp pattern HH:MM:SS.d (1 digit ms from backend)
          // Also handling --:--:--.0 fallback
          // The backend formats millis as (ms % 1000) / 100, so it's always 1 digit.
          // We match this pattern and insert the separator.
          restoredText = restoredText.replace(
            /(\d{2}:\d{2}:\d{2}\.\d|--:--:--\.0)/g,
            `$1${timestampSeparator}`
          );
        }

        // Use Clipboard API with Blob to bypass OS/Browser CRLF normalization
        // Standard e.clipboardData.setData('text/plain') forces \r\n on Windows.
        if (
          navigator.clipboard &&
          navigator.clipboard.write &&
          typeof ClipboardItem !== 'undefined'
        ) {
          const blob = new Blob([restoredText], { type: 'text/plain' });
          const item = new ClipboardItem({ 'text/plain': blob });
          navigator.clipboard.write([item]).catch((err) => {
            console.error('Clipboard write failed:', err);
            // Fallback if async write fails (though preventDefault already called)
            // We can't really fallback here effectively because preventDefault is done.
            // But failure here is rare in a focused window context.
          });
        } else {
          // Fallback for environments without ClipboardItem support
          e.clipboardData.setData('text/plain', restoredText);
        }
      }
    },
    [timestampSeparator, showTimestamp]
  );

  // Calculate display position.
  // Clamp the visual offset so stale rows stay attached to the viewport during
  // fast scrolling instead of rendering thousands of pixels away (blank view).
  // The clamp is symmetric (+/- maxDiff) so it only kicks in once the stale
  // block would actually leave the viewport - a plain BUFFER_ROWS cap would
  // freeze normal downward scrolling whenever the diff legitimately exceeds
  // the buffer before the refetch lands.
  const scrollRatio = scrollHeight > 0 ? scrollTop / scrollHeight : 0;
  const targetLine = Math.floor(scrollRatio * effectiveTotalLines);
  const maxDiff = Math.max(BUFFER_ROWS, lines.length - visibleRows);
  const visualLineDiff = Math.max(Math.min(targetLine - currentStartLine, maxDiff), -maxDiff);
  const displayTop = scrollTop - visualLineDiff * ROW_HEIGHT;
  const naturalLineHeight = totalLines * ROW_HEIGHT;
  const isScaled = asciiScrollHeight < naturalLineHeight;

  // Render combined rows for consistent selection
  return (
    <div
      className={`ascii-viewer ${lineWrap ? 'line-wrap' : ''} ${showTimestamp ? 'with-timestamp' : ''}`}
      onCopy={handleCopy}
    >
      <div className="ascii-scroll-container" ref={containerRef} onScroll={handleScroll}>
        <div
          style={
            isTail
              ? {
                  // Bottom-anchored: grows (instead of overflowing) when wrapped rows
                  // are taller than the model, so the last line is always reachable
                  minHeight: asciiScrollHeight,
                  display: 'flex',
                  flexDirection: 'column',
                  justifyContent: 'flex-end',
                }
              : { height: asciiScrollHeight, position: 'relative' }
          }
        >
          <div
            style={
              isTail ? { width: '100%' } : { position: 'absolute', top: displayTop, width: '100%' }
            }
          >
            {lines.map((line) => (
              <div
                key={line.offset}
                className="ascii-row"
                style={{ height: lineWrap ? 'auto' : ROW_HEIGHT, minHeight: ROW_HEIGHT }}
              >
                {showTimestamp && <span className="ascii-timestamp">{line.timestamp || ''}</span>}
                <span className="ascii-text">
                  {asciiDisplaySegments(line.text, { showCrLf, showNul }).map((segment, i) =>
                    segment.hidden ? (
                      <span key={i} className="ascii-hidden-ctrl">
                        {segment.text}
                      </span>
                    ) : (
                      segment.text
                    )
                  )}
                </span>
              </div>
            ))}
          </div>
        </div>
      </div>

      <div className="ascii-footer">
        <span>
          Total: {totalBytes.toLocaleString()} bytes ({totalLines.toLocaleString()} lines)
        </span>
        {/* Internal scroll diagnostics - dev builds only, never shipped */}
        {import.meta.env.DEV && (
          <span className="ascii-debug">
            scrollTop={scrollTop.toFixed(0)} | scrollHeight={asciiScrollHeight.toLocaleString()} |
            byteOffset={getByteOffset().toLocaleString()} | displayTop=
            {displayTop.toLocaleString()} | startLine={currentStartLine.toLocaleString()} | tail=
            {isTail ? 'YES' : 'no'} | isScaled={isScaled ? 'YES' : 'no'} | scale=
            {(asciiScrollHeight / naturalLineHeight || 1).toFixed(4)}
          </span>
        )}
      </div>
    </div>
  );
}
