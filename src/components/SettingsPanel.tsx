import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { THEME_OPTIONS, type ThemeSetting } from '../utils/theme';
import './SettingsPanel.css';

/** バックエンド `BridgeActivity` に対応（直近の活動） */
interface BridgeActivity {
  kind: string;
  bytes: number;
  at_ms: number;
  /** 送信内容の先頭 64 文字（人間が AI の送信内容を確認するための表示） */
  preview: string;
}

/** `bridge_status` / `bridge_set` の戻り値 */
interface BridgeStatusInfo {
  enabled: boolean;
  port: number;
  connections: number;
  last_activity: BridgeActivity | null;
}

/** `bridge-activity` イベントのペイロード */
interface BridgeActivityEvent {
  kind: string;
  bytes: number;
  preview: string;
}

const BRIDGE_DEFAULT_PORT = 57320;
const BRIDGE_POLL_MS = 2000;
const BRIDGE_TOOLTIP = 'Local access for MCP clients and scripts (127.0.0.1 only, off by default)';

/** 活動時刻の表示（時刻のみ） */
function formatBridgeTime(atMs: number): string {
  return new Date(atMs).toLocaleTimeString();
}

/** 送信プレビューの整形: 制御文字を可視化し、表示用に切り詰める */
function formatPreview(preview: string, maxChars: number): string {
  const sanitized = Array.from(preview)
    .map((ch) => (ch < ' ' || ch.charCodeAt(0) === 0x7f ? '·' : ch))
    .join('');
  return sanitized.length > maxChars ? `${sanitized.slice(0, maxChars)}…` : sanitized;
}

export interface SerialConfig {
  baud_rate: number;
  data_bits: number;
  flow_control: 'None' | 'Software' | 'Hardware';
  parity: 'None' | 'Odd' | 'Even';
  stop_bits: number;
  dtr: boolean;
  rts: boolean;
}

interface SettingsPanelProps {
  ports: string[];
  selectedPort: string;
  onPortChange: (port: string) => void;
  /** Re-enumerate the ports; resolves once the new list is in state */
  onRefreshPorts: () => void | Promise<void>;
  config: SerialConfig;
  onConfigChange: (config: SerialConfig) => void;
  isConnected: boolean;
  onConnect: () => void;
  onDisconnect: () => void;
  /** Connect to the selected port whenever it is available (SYS-F-807) */
  autoConnect: boolean;
  onAutoConnectChange: (enabled: boolean) => void;
  /** Device path being waited for, or null when not waiting */
  reconnectWaitingFor: string | null;
  /** Why the port being waited for could not be opened (e.g. in use elsewhere) */
  reconnectError?: string | null;
  /** Time (ms) of the last automatic connect, shown until the next manual action */
  reconnectedAt: number | null;
  /** Color theme (issue #6) */
  theme: ThemeSetting;
  onThemeChange: (theme: ThemeSetting) => void;
}

const DTR_HELP =
  'DTR is off by default so that opening the port does not reset the board. ' +
  'Arduino Uno, Nano and Mega (including CH340 clones) reset whenever DTR turns on, ' +
  'as they do with the Arduino IDE. ' +
  'Boards with native USB (Leonardo, Micro, Raspberry Pi Pico, ESP32-S2/S3/C3 USB) ' +
  'may send nothing, or wait in while (!Serial), until DTR is on: tick DtrEnable for those.';

const RTS_HELP =
  'RTS is off by default. On ESP32 and ESP8266 boards with the usual auto-reset circuit, ' +
  'RTS on while DTR is off holds the chip in reset. With both on, the board runs normally. ' +
  'Arduino AVR boards ignore RTS.';

/** "?" mark that shows a note on hover (and on keyboard focus) */
function HelpMark({ text }: { text: string }) {
  return (
    <span className="help-mark" title={text} aria-label={text} tabIndex={0} role="img">
      ?
    </span>
  );
}

const THEME_LABELS: Record<ThemeSetting, string> = {
  dark: 'Dark',
  light: 'Light',
  system: 'System',
};

const AUTO_CONNECT_TOOLTIP =
  'Connect to the selected port whenever it is available: at startup, when the device is ' +
  'plugged in, and after a reset or unplug. The capture continues; nothing is cleared. ' +
  'Disconnect turns this off.';

export default function SettingsPanel({
  ports,
  selectedPort,
  onPortChange,
  onRefreshPorts,
  config,
  onConfigChange,
  isConnected,
  onConnect,
  onDisconnect,
  autoConnect,
  onAutoConnectChange,
  reconnectWaitingFor,
  reconnectError = null,
  reconnectedAt,
  theme,
  onThemeChange,
}: SettingsPanelProps) {
  const [baudRateEditing, setBaudRateEditing] = useState(false);
  const [baudDropdownOpen, setBaudDropdownOpen] = useState(false);
  // Draft text while editing the baud rate; committed (with validation) on
  // blur/Enter so invalid values (empty, 0, negative, decimal) never reach
  // the config / backend.
  const [baudDraft, setBaudDraft] = useState('');

  // --- MCP Server (bridge) -----------------------------------------------------------
  // 状態は SettingsPanel 内で完結させる（App.tsx には触れない）。
  const [bridgeEnabled, setBridgeEnabled] = useState(false);
  const [bridgePort, setBridgePort] = useState(BRIDGE_DEFAULT_PORT);
  const [bridgeConnections, setBridgeConnections] = useState(0);
  const [bridgeActivity, setBridgeActivity] = useState<BridgeActivity | null>(null);

  const applyBridgeStatus = useCallback((info: BridgeStatusInfo) => {
    setBridgeEnabled(info.enabled);
    setBridgePort(info.port);
    setBridgeConnections(info.connections);
    // イベントで先に受け取った活動の方が新しい場合は上書きしない
    setBridgeActivity((prev) => {
      const next = info.last_activity;
      if (!next) return prev;
      return !prev || next.at_ms >= prev.at_ms ? next : prev;
    });
  }, []);

  const handleBridgeToggle = async (enabled: boolean) => {
    setBridgeEnabled(enabled); // 楽観的更新（失敗時に戻す）
    try {
      const info = await invoke<BridgeStatusInfo>('bridge_set', { enabled, port: null });
      if (info) applyBridgeStatus(info);
    } catch (e) {
      console.error(e);
      setBridgeEnabled(false);
      setBridgeConnections(0);
    }
  };

  // 有効な間だけ 2 秒ごとに状態をポーリングする
  useEffect(() => {
    if (!bridgeEnabled) return;
    let cancelled = false;
    const timer = setInterval(() => {
      invoke<BridgeStatusInfo>('bridge_status')
        .then((info) => {
          if (!cancelled && info) applyBridgeStatus(info);
        })
        .catch((e) => console.error(e));
    }, BRIDGE_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [bridgeEnabled, applyBridgeStatus]);

  // 送信は即座に見えてほしいのでイベントも購読する
  // （unmount 前に listen() が解決しなかった場合に備えた cancelled パターン）
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    listen<BridgeActivityEvent>('bridge-activity', (event) => {
      setBridgeActivity({
        kind: event.payload.kind,
        bytes: event.payload.bytes,
        at_ms: Date.now(),
        preview: event.payload.preview,
      });
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });

    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  // --- Port list freshness (issue #5) ------------------------------------------------
  // The list is polled every 2 s, but a native <select> popup shows the options it
  // had at the moment it opened, so a device plugged in just before the click was
  // missing. Where the WebView supports showPicker() (Chromium/WebView2), the click
  // re-enumerates first and then opens the popup with the current list. Elsewhere
  // the popup opens as usual and the refresh still updates the list.
  const portSelectRef = useRef<HTMLSelectElement>(null);
  const openingRef = useRef(false);

  const refreshThenOpen = async () => {
    const el = portSelectRef.current;
    if (!el || openingRef.current) return;
    openingRef.current = true;
    try {
      await onRefreshPorts();
      // Let React commit the new <option>s before the popup reads them
      await new Promise((resolve) => setTimeout(resolve, 0));
      el.focus();
      el.showPicker();
    } catch (e) {
      console.error(e);
    } finally {
      openingRef.current = false;
    }
  };

  const canShowPicker = () =>
    typeof HTMLSelectElement !== 'undefined' &&
    typeof HTMLSelectElement.prototype.showPicker === 'function';

  const handlePortMouseDown = (e: React.MouseEvent<HTMLSelectElement>) => {
    if (e.button !== 0) return;
    if (canShowPicker()) {
      e.preventDefault(); // open it ourselves, after the refresh
      void refreshThenOpen();
    } else {
      void onRefreshPorts();
    }
  };

  const handlePortKeyDown = (e: React.KeyboardEvent<HTMLSelectElement>) => {
    const opens = e.key === 'F4' || (e.altKey && (e.key === 'ArrowDown' || e.key === 'ArrowUp'));
    if (opens && canShowPicker()) {
      e.preventDefault();
      void refreshThenOpen();
    }
  };

  const handleChange = <K extends keyof SerialConfig>(key: K, value: SerialConfig[K]) => {
    onConfigChange({ ...config, [key]: value });
  };

  const commitBaudDraft = () => {
    const n = Math.floor(Number(baudDraft));
    if (Number.isFinite(n) && n >= 1 && n <= 12_000_000) {
      handleChange('baud_rate', n);
    }
    // Invalid input: keep the previous valid baud rate
    setBaudRateEditing(false);
  };

  return (
    <div className="settings-panel">
      <div className="panel-header">Settings</div>
      <div className="settings-row">
        <button type="button" onClick={() => void onRefreshPorts()} disabled={isConnected}>
          Reload
        </button>
        <select
          ref={portSelectRef}
          value={selectedPort}
          onChange={(e) => onPortChange(e.target.value)}
          onMouseDown={handlePortMouseDown}
          onKeyDown={handlePortKeyDown}
          disabled={isConnected}
          className="port-select"
          aria-label="Port"
        >
          {/* Keep showing the selected port even when its device is unplugged. */}
          {selectedPort && !ports.includes(selectedPort) && (
            <option value={selectedPort}>{selectedPort} (not present)</option>
          )}
          {ports.map((port) => (
            <option key={port} value={port}>
              {port}
            </option>
          ))}
          {ports.length === 0 && !selectedPort && <option value="">No ports found</option>}
        </select>

        <div className="baud-custom-container">
          {baudRateEditing ? (
            <input
              type="number"
              value={baudDraft}
              onChange={(e) => setBaudDraft(e.target.value)}
              onBlur={commitBaudDraft}
              onKeyDown={(e) => {
                if (e.key === 'Enter') commitBaudDraft();
                if (e.key === 'Escape') setBaudRateEditing(false);
              }}
              disabled={isConnected}
              autoFocus
              className="baud-input"
            />
          ) : (
            <div
              className={`baud-trigger ${isConnected ? 'disabled' : ''}`}
              onClick={() => {
                if (isConnected) return;
                setBaudDropdownOpen(!baudDropdownOpen);
              }}
            >
              <span>{config.baud_rate} bps</span>
              <span className="baud-trigger-arrow">▼</span>
            </div>
          )}

          {baudDropdownOpen && !baudRateEditing && !isConnected && (
            <>
              <div className="baud-dropdown-overlay" onClick={() => setBaudDropdownOpen(false)} />
              <ul className="baud-dropdown-list">
                {[
                  9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600, 1000000, 2000000,
                  3000000, 12000000,
                ].map((rate) => (
                  <li
                    key={rate}
                    className="baud-dropdown-item"
                    onClick={() => {
                      handleChange('baud_rate', rate);
                      setBaudDropdownOpen(false);
                    }}
                  >
                    {rate}
                  </li>
                ))}
                <li
                  className="baud-dropdown-edit"
                  onClick={() => {
                    setBaudDraft(String(config.baud_rate));
                    setBaudRateEditing(true);
                    setBaudDropdownOpen(false);
                  }}
                >
                  [edit]
                </li>
              </ul>
            </>
          )}
        </div>

        <button
          onClick={isConnected ? onDisconnect : onConnect}
          className={`connect-button ${isConnected ? 'connected' : ''}`}
        >
          {reconnectWaitingFor !== null ? 'Cancel' : isConnected ? 'Disconnect' : 'Connect'}
        </button>

        <label className="checkbox-label" title={AUTO_CONNECT_TOOLTIP}>
          <input
            type="checkbox"
            checked={autoConnect}
            onChange={(e) => onAutoConnectChange(e.target.checked)}
          />
          Auto Connect
        </label>
      </div>

      <div className="settings-advanced">
        <div className="advanced-group">
          <span className="label">Data Bits</span>
          <select
            value={config.data_bits}
            onChange={(e) => handleChange('data_bits', Number(e.target.value))}
            disabled={isConnected}
          >
            {[5, 6, 7, 8].map((b) => (
              <option key={b} value={b}>
                {b}
              </option>
            ))}
          </select>
        </div>

        <div className="advanced-group">
          <span className="label">Parity</span>
          <select
            value={config.parity}
            onChange={(e) => handleChange('parity', e.target.value as SerialConfig['parity'])}
            disabled={isConnected}
          >
            {['None', 'Odd', 'Even'].map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
        </div>

        <div className="advanced-group checkbox-group">
          <label>
            <input
              type="checkbox"
              checked={config.dtr}
              onChange={(e) => handleChange('dtr', e.target.checked)}
            />{' '}
            DtrEnable
          </label>
          <HelpMark text={DTR_HELP} />
          <label>
            <input
              type="checkbox"
              checked={config.rts}
              onChange={(e) => handleChange('rts', e.target.checked)}
            />{' '}
            RtsEnable
          </label>
          <HelpMark text={RTS_HELP} />
        </div>

        <div className="advanced-group">
          <span className="label">Stop Bits</span>
          <select
            value={config.stop_bits}
            onChange={(e) => handleChange('stop_bits', Number(e.target.value))}
            disabled={isConnected}
          >
            <option value={1}>One</option>
            <option value={2}>Two</option>
          </select>
        </div>

        <div className="advanced-group">
          <span className="label">Flow Control</span>
          <select
            value={config.flow_control}
            onChange={(e) =>
              handleChange('flow_control', e.target.value as SerialConfig['flow_control'])
            }
            disabled={isConnected}
          >
            {['None', 'Software', 'Hardware'].map((f) => (
              <option key={f} value={f}>
                {f}
              </option>
            ))}
          </select>
        </div>
      </div>

      <div className="settings-advanced">
        <label className="checkbox-label" title={BRIDGE_TOOLTIP}>
          <input
            type="checkbox"
            checked={bridgeEnabled}
            onChange={(e) => {
              void handleBridgeToggle(e.target.checked);
            }}
          />
          MCP Server
        </label>

        <button
          type="button"
          className="bridge-guide-button"
          title="How to connect an AI agent (MCP setup guide)"
          onClick={() => {
            invoke('open_ai_guide_window').catch((e) => console.error(e));
          }}
        >
          Setup Guide
        </button>

        {bridgeEnabled && (
          <>
            <span className="bridge-endpoint">127.0.0.1:{bridgePort}</span>
            <span
              className="bridge-hint"
              title={bridgeActivity?.preview ? `Sent: ${bridgeActivity.preview}` : undefined}
            >
              {bridgeConnections > 0 ? `Clients: ${bridgeConnections}` : 'Waiting'}
              {bridgeActivity &&
                ` / Sent ${bridgeActivity.bytes} bytes ${formatBridgeTime(bridgeActivity.at_ms)}`}
              {bridgeActivity?.preview && (
                <span className="bridge-preview">
                  {' '}
                  “{formatPreview(bridgeActivity.preview, 24)}”
                </span>
              )}
            </span>
          </>
        )}

        {/* Auto Connect status. Kept out of the first row: its text contains the
            device path, and nothing there may change the width of the port list */}
        {reconnectWaitingFor !== null ? (
          <span
            className="reconnect-status waiting"
            role="status"
            title={
              `Waiting for ${reconnectWaitingFor}…` +
              (reconnectError !== null ? ` (${reconnectError})` : '')
            }
          >
            Waiting for {reconnectWaitingFor}…{reconnectError !== null && ' (cannot open the port)'}
          </span>
        ) : (
          reconnectedAt !== null && (
            <span className="reconnect-status" role="status">
              Connected automatically {new Date(reconnectedAt).toLocaleTimeString()}
            </span>
          )
        )}

        <label className="theme-control" title="Color theme (System follows the OS setting)">
          Theme
          <select
            value={theme}
            onChange={(e) => onThemeChange(e.target.value as ThemeSetting)}
            aria-label="Theme"
          >
            {THEME_OPTIONS.map((t) => (
              <option key={t} value={t}>
                {THEME_LABELS[t]}
              </option>
            ))}
          </select>
        </label>
      </div>
    </div>
  );
}
