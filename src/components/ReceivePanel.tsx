import { useState, useCallback } from 'react';
import HexViewer from './viewers/HexViewer';
import AsciiViewer from './viewers/AsciiViewer';
import './ReceivePanel.css';

export type ViewMode = 'hex' | 'ascii';
export type TimestampSeparator = ' ' | ',' | '\t';

export interface ReceiveOptions {
  viewMode: ViewMode;
  lineWrap: boolean;
  showCtrl: boolean;
  showTimestamp: boolean;
  autoScroll: boolean;
  filterEnabled: boolean;
  searchQuery: string;
}

interface ReceivePanelProps {
  totalBytes: number;
  onExport: () => void;
  onClear: () => void;
  onCopy: (mode: ViewMode) => void;
  autoScroll: boolean;
  onAutoScrollChange: (enabled: boolean) => void;
}

export default function ReceivePanel({
  totalBytes,
  onExport,
  onClear,
  onCopy,
  autoScroll,
  onAutoScrollChange,
}: ReceivePanelProps) {
  // View mode: 'hex' (Binary checked) or 'ascii' (Binary unchecked)
  const [viewMode, setViewMode] = useState<ViewMode>('ascii');

  // Display options (ASCII mode only)
  const [lineWrap, setLineWrap] = useState(true);
  const [showTimestamp, setShowTimestamp] = useState(true);
  const [timestampSeparator, setTimestampSeparator] = useState<TimestampSeparator>(' ');

  // Scroll position as byte offset (preserved across mode switches)
  const [scrollOffset, setScrollOffset] = useState(0);

  // Search & Filter
  const [searchQuery, setSearchQuery] = useState('');
  const [filterEnabled, setFilterEnabled] = useState(false);

  const handleCopy = useCallback(() => {
    onCopy(viewMode);
  }, [onCopy, viewMode]);

  const handlePlotter = useCallback(() => {
    // Phase 7で実装予定
    alert('Plotter will be available in Phase 7');
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
      <div className="viewer-wrapper">
        {viewMode === 'hex' ? (
          <HexViewer
            totalBytes={totalBytes}
            autoScroll={autoScroll}
            initialOffset={scrollOffset}
            onScrollChange={setScrollOffset}
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
            title="Show timestamp (ASCII mode only)"
          >
            <input
              type="checkbox"
              checked={showTimestamp}
              onChange={(e) => setShowTimestamp(e.target.checked)}
              disabled={viewMode === 'hex'}
            />
            Timestamp
            <select
              value={timestampSeparator}
              onChange={(e) => setTimestampSeparator(e.target.value as TimestampSeparator)}
              disabled={viewMode === 'hex' || !showTimestamp}
              className="separator-select"
              title="Separator between Timestamp and Data"
              style={{
                marginLeft: '8px',
                background: '#3c3c3c',
                color: '#d4d4d4',
                border: '1px solid #555',
                borderRadius: '3px',
                padding: '1px 4px',
                fontSize: '11px',
                outline: 'none',
                cursor: 'pointer',
                opacity: (!showTimestamp || viewMode === 'hex') ? 0.5 : 1
              }}
            >
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
          <button onClick={onExport} title="Save to file">
            Save
          </button>
          <button onClick={handleCopy} title="Copy all data to clipboard">
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
