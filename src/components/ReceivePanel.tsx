import HexViewer from './HexViewer';

interface ReceivePanelProps {
  totalBytes: number;
  onExport: () => void;
  onClear: () => void;
  onCopy: () => void;
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
  return (
    <div className="receive-panel">
      <div className="panel-header">Recieve</div>
      <div className="user-note">
        <input type="text" placeholder="User's Note" />
      </div>

      <div className="hex-viewer-wrapper">
        <HexViewer totalBytes={totalBytes} autoScroll={autoScroll} />
      </div>

      <div className="receive-footer">
        <div className="footer-left">
          <label>
            <input type="checkbox" defaultChecked /> Line Wrap
          </label>
          <label>
            <input type="checkbox" /> Binary (hex)
          </label>
          <label>
            <input type="checkbox" defaultChecked /> Show [CRLF]
          </label>
          <label>
            <input type="checkbox" defaultChecked /> Show [NUL]
          </label>
          <label>
            <input
              type="checkbox"
              checked={autoScroll}
              onChange={(e) => onAutoScrollChange(e.target.checked)}
            />{' '}
            Auto Scroll
          </label>
          <label>
            <input type="checkbox" defaultChecked /> Show Timestamp
          </label>
        </div>
        <div className="footer-right">
          <select disabled>
            <option>{'>'}</option>
          </select>
          <button onClick={onExport}>Save</button>
          <button onClick={onCopy}>Copy</button>
          <button onClick={onClear}>CLEAR</button>
        </div>
      </div>
    </div>
  );
}
