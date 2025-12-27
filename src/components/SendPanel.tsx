import { useState, KeyboardEvent } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
  parseHexString,
  appendLineEnding,
  addToHistory,
  type LineEnding,
} from '../utils/sendUtils';

interface SendPanelProps {
  connected: boolean;
  onSend?: (bytes: number) => void;
}

type InputMode = 'TEXT' | 'HEX';

export default function SendPanel({ connected, onSend }: SendPanelProps) {
  const [inputMode, setInputMode] = useState<InputMode>('TEXT');
  const [inputText, setInputText] = useState('');
  const [lineEnding, setLineEnding] = useState<LineEnding>('LF');
  const [sendOnEnter, setSendOnEnter] = useState(false);
  const [sendBinary, setSendBinary] = useState(false);

  // History
  const [history, setHistory] = useState<string[]>([]);
  const [historyIndex, setHistoryIndex] = useState(-1);

  const handleSend = async () => {
    if (!connected || !inputText) return;

    let dataToSend: number[] = [];

    try {
      if (inputMode === 'HEX' || sendBinary) {
        // sendBinary flag or HEX mode
        const parsed = parseHexString(inputText);
        if (parsed === null) {
          alert('Invalid Hex: Odd number of characters or invalid hex');
          return;
        }
        dataToSend = parsed;
      } else {
        // Convert string to bytes (UTF-8)
        const encoder = new TextEncoder();
        dataToSend = Array.from(encoder.encode(inputText));

        // Append line ending
        dataToSend = appendLineEnding(dataToSend, lineEnding);
      }

      await invoke('write_data', { data: dataToSend });

      // Update history
      setHistory((prev) => addToHistory(prev, inputText));
      setHistoryIndex(-1);
      setInputText('');

      if (onSend) onSend(dataToSend.length);
    } catch (e) {
      console.error(e);
      alert('Failed to send: ' + e);
    }
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLInputElement | HTMLTextAreaElement>) => {
    if (e.key === 'Enter') {
      if (sendOnEnter && !e.shiftKey) {
        e.preventDefault();
        handleSend();
      }
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (history.length > 0) {
        const nextIndex = Math.min(historyIndex + 1, history.length - 1);
        setHistoryIndex(nextIndex);
        setInputText(history[nextIndex]);
      }
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (historyIndex > 0) {
        const nextIndex = historyIndex - 1;
        setHistoryIndex(nextIndex);
        setInputText(history[nextIndex]);
      } else if (historyIndex === 0) {
        setHistoryIndex(-1);
        setInputText('');
      }
    }
  };

  return (
    <div className="send-panel">
      <div className="panel-header">Send</div>
      <div className="send-body">
        <textarea
          className="send-input"
          value={inputText}
          onChange={(e) => setInputText(e.target.value)}
          onKeyDown={handleKeyDown}
          disabled={!connected}
          placeholder={inputMode === 'HEX' || sendBinary ? 'e.g. 48 65 6C 6C 6F' : ''}
        />
        <div className="send-controls">
          <button onClick={handleSend} disabled={!connected || !inputText} className="send-button">
            Send
          </button>
          <div className="send-options">
            <label>
              <input
                type="checkbox"
                checked={sendOnEnter}
                onChange={(e) => setSendOnEnter(e.target.checked)}
              />{' '}
              Send with Enter
            </label>
            <label>
              <input
                type="checkbox"
                checked={sendBinary}
                onChange={(e) => {
                  setSendBinary(e.target.checked);
                  if (e.target.checked) setInputMode('HEX');
                  else setInputMode('TEXT');
                }}
              />{' '}
              Send Binary
            </label>
          </div>
          <select
            value={lineEnding}
            onChange={(e) => setLineEnding(e.target.value as LineEnding)}
            className="line-ending-select"
            disabled={sendBinary}
          >
            <option value="NONE">None</option>
            <option value="LF">LF (\n)</option>
            <option value="CR">CR (\r)</option>
            <option value="CRLF">CRLF (\r\n)</option>
          </select>
        </div>
      </div>
    </div>
  );
}
