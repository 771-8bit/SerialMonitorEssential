import { useState, useCallback } from 'react';
import HexViewer from './viewers/HexViewer';
import AsciiViewer from './viewers/AsciiViewer';
import { usePersistentState, isBoolean, isOneOf } from '../utils/persist';
import './ReceivePanel.css';

export type ViewMode = 'hex' | 'ascii';
export type TimestampSeparator = ' > ' | ' ' | ',' | '\t';

export interface ReceiveOptions {
  viewMode: ViewMode;
  lineWrap: boolean;
  showCtrl: boolean;
  showTimestamp: boolean;
  autoScroll: boolean;
  filterEnabled: boolean;
  searchQuery: string;
}

/**
 * What Save / Copy produce (issue #4). With Timestamp ON, Copy gives every line (Hex:
 * every 16-byte row) with its timestamp and the chosen separator, and Save in the
 * ASCII view writes the same text. Otherwise the data as before (raw bytes for Save,
 * plain or hex text for Copy); Save in the Hex view always writes the raw bytes.
 */
export interface OutputFormat {
  mode: ViewMode;
  /** Separator after each line's timestamp, or null for no timestamps */
  timestampSeparator: TimestampSeparator | null;
}

interface ReceivePanelProps {
  captureId: number;
  totalBytes: number;
  onExport: (format: OutputFormat) => void;
  onClear: () => void;
  onCopy: (format: OutputFormat) => void;
  autoScroll: boolean;
  onAutoScrollChange: (enabled: boolean) => void;
}

export default function ReceivePanel({
  captureId,
  totalBytes,
  onExport,
  onClear,
  onCopy,
  autoScroll,
  onAutoScrollChange,
}: ReceivePanelProps) {
  // View mode: 'hex' (Binary checked) or 'ascii' (Binary unchecked)
  // (view options are remembered across restarts - issue #7)
  const [viewMode, setViewMode] = usePersistentState<ViewMode>(
    'receive.viewMode',
    'ascii',
    isOneOf<ViewMode>(['ascii', 'hex'])
  );

  // Display options (ASCII mode only)
  const [lineWrap, setLineWrap] = usePersistentState('receive.lineWrap', false, isBoolean);
  // C# version: "Show [CRLF]" / "Show [NUL]", both off by default
  const [showCrLf, setShowCrLf] = usePersistentState('receive.showCrLf', false, isBoolean);
  const [showNul, setShowNul] = usePersistentState('receive.showNul', false, isBoolean);
  const [showTimestamp, setShowTimestamp] = usePersistentState(
    'receive.showTimestamp',
    true,
    isBoolean
  );
  const [timestampSeparator, setTimestampSeparator] = usePersistentState<TimestampSeparator>(
    'receive.timestampSeparator',
    ' > ',
    isOneOf<TimestampSeparator>([' > ', ' ', ',', '\t'])
  );

  // Scroll position as byte offset (preserved across mode switches)
  const [scrollPosition, setScrollPosition] = useState({ captureId, offset: 0 });
  const scrollOffset = scrollPosition.captureId === captureId ? scrollPosition.offset : 0;
  const setScrollOffset = useCallback(
    (offset: number) => setScrollPosition({ captureId, offset }),
    [captureId]
  );

  // Search & Filter
  const [searchQuery, setSearchQuery] = useState('');
  const [filterEnabled, setFilterEnabled] = useState(false);

  const outputFormat: OutputFormat = {
    mode: viewMode,
    timestampSeparator: showTimestamp ? timestampSeparator : null,
  };

  const handleCopy = () => onCopy(outputFormat);
  const handleExport = () => onExport(outputFormat);

  const handlePlotter = useCallback(async () => {
    try {
      const { invoke } = await import('@tauri-apps/api/core');
      await invoke('open_plotter_window');
    } catch (e) {
      console.error('Failed to open plotter:', e);
      alert('Failed to open plotter: ' + e);
    }
  }, []);

  return (
    <div className="receive-panel">
      <div className="panel-header">Receive</div>

      {/* Search Bar */}
      <div className="search-bar">
        <input
          type="text"
          placeholder={viewMode === 'ascii' ? 'Search (regex supported)...' : 'Search...'}
          value={searchQuery}
          onChange={(e) => setSearchQuery(e.target.value)}
          className="search-input"
        />
        <button
          className={`filter-toggle ${filterEnabled ? 'active' : ''}`}
          onClick={() => setFilterEnabled(!filterEnabled)}
          title="Filter: Show only matching lines"
        >
          {filterEnabled ? '🔍 Filter ON' : '🔍 Filter'}
        </button>
      </div>

      {/* Log Viewer Area - Switch between HexViewer and AsciiViewer */}
      <div className="viewer-wrapper" key={captureId}>
        {viewMode === 'hex' ? (
          <HexViewer
            totalBytes={totalBytes}
            autoScroll={autoScroll}
            initialOffset={scrollOffset}
            onScrollChange={setScrollOffset}
            showTimestamp={showTimestamp}
          />
        ) : (
          <AsciiViewer
            totalBytes={totalBytes}
            autoScroll={autoScroll}
            showTimestamp={showTimestamp}
            lineWrap={lineWrap}
            initialOffset={scrollOffset}
            onScrollChange={setScrollOffset}
            timestampSeparator={timestampSeparator}
            showCrLf={showCrLf}
            showNul={showNul}
          />
        )}
      </div>

      {/* Footer Controls */}
      <div className="receive-footer">
        <div className="footer-left">
          {/* Hex/ASCII Toggle Switch */}
          <div className="mode-switch" title="Toggle between Hex and ASCII view">
            <span className={`mode-label ${viewMode === 'hex' ? 'active hex' : ''}`}>Hex</span>
            <label className="switch">
              <input
                type="checkbox"
                checked={viewMode === 'ascii'}
                onChange={(e) => setViewMode(e.target.checked ? 'ascii' : 'hex')}
              />
              <span className="slider"></span>
            </label>
            <span className={`mode-label ${viewMode === 'ascii' ? 'active ascii' : ''}`}>
              ASCII
            </span>
          </div>

          <div className="control-separator"></div>

          <label
            className={`control-label ${viewMode === 'hex' ? 'disabled' : ''}`}
            title="Wrap long lines (ASCII mode only)"
          >
            <input
              type="checkbox"
              checked={lineWrap}
              onChange={(e) => setLineWrap(e.target.checked)}
              disabled={viewMode === 'hex'}
            />
            Line Wrap
          </label>

          <label
            className={`control-label ${viewMode === 'hex' ? 'disabled' : ''}`}
            title="Show CR, LF and TAB as symbols (ASCII mode only)"
          >
            <input
              type="checkbox"
              checked={showCrLf}
              onChange={(e) => setShowCrLf(e.target.checked)}
              disabled={viewMode === 'hex'}
            />
            Show CR/LF
          </label>

          <label
            className={`control-label ${viewMode === 'hex' ? 'disabled' : ''}`}
            title="Show NUL bytes as symbols instead of hiding them (ASCII mode only)"
          >
            <input
              type="checkbox"
              checked={showNul}
              onChange={(e) => setShowNul(e.target.checked)}
              disabled={viewMode === 'hex'}
            />
            Show NUL
          </label>

          <label
            className="control-label"
            title="Show the reception time of each line (Hex: of each row's first byte)"
          >
            <input
              type="checkbox"
              checked={showTimestamp}
              onChange={(e) => setShowTimestamp(e.target.checked)}
            />
            Timestamp
            <select
              value={timestampSeparator}
              onChange={(e) => setTimestampSeparator(e.target.value as TimestampSeparator)}
              disabled={!showTimestamp}
              className="separator-select"
              title="Separator between Timestamp and Data"
              style={{ opacity: !showTimestamp ? 0.5 : 1 }}
            >
              <option value=" > ">&gt;</option>
              <option value=" ">Space</option>
              <option value=",">Comma</option>
              <option value="	">Tab</option>
            </select>
          </label>

          <div className="control-separator"></div>

          <label className="control-label" title="Auto scroll to bottom on new data">
            <input
              type="checkbox"
              checked={autoScroll}
              onChange={(e) => onAutoScrollChange(e.target.checked)}
            />
            Auto Scroll
          </label>
        </div>

        <div className="footer-right">
          <button onClick={handlePlotter} className="plotter-button" title="Open Plotter (Phase 7)">
            Plotter
          </button>
          <button
            onClick={handleExport}
            title={
              outputFormat.timestampSeparator !== null && viewMode === 'ascii'
                ? 'Save as text with timestamps'
                : 'Save the received bytes to a file'
            }
          >
            Save
          </button>
          <button
            onClick={handleCopy}
            title={
              outputFormat.timestampSeparator !== null
                ? 'Copy all data with timestamps to the clipboard'
                : 'Copy all data to the clipboard'
            }
          >
            Copy
          </button>
          <button onClick={onClear} title="Clear all data (including disk)">
            Clear
          </button>
        </div>
      </div>
    </div>
  );
}
