import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import './SettingsPanel.css';

/** バックエンド `BridgeActivity` に対応（直近の活動） */
interface BridgeActivity {
  kind: string;
  bytes: number;
  at_ms: number;
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
const BRIDGE_TOOLTIP = 'ローカルのAIエージェント用ブリッジ（127.0.0.1のみ・既定OFF）';

/** 活動時刻の表示（時刻のみ） */
function formatBridgeTime(atMs: number): string {
  return new Date(atMs).toLocaleTimeString();
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
  onRefreshPorts: () => void;
  config: SerialConfig;
  onConfigChange: (config: SerialConfig) => void;
  isConnected: boolean;
  onConnect: () => void;
  onDisconnect: () => void;
}

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
}: SettingsPanelProps) {
  const [baudRateEditing, setBaudRateEditing] = useState(false);
  const [baudDropdownOpen, setBaudDropdownOpen] = useState(false);
  // Draft text while editing the baud rate; committed (with validation) on
  // blur/Enter so invalid values (empty, 0, negative, decimal) never reach
  // the config / backend.
  const [baudDraft, setBaudDraft] = useState('');

  // --- AI Bridge -----------------------------------------------------------
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
        <select
          value={selectedPort}
          onChange={(e) => onPortChange(e.target.value)}
          onClick={onRefreshPorts}
          disabled={isConnected}
          className="port-select"
        >
          {ports.map((port) => (
            <option key={port} value={port}>
              {port}
            </option>
          ))}
          {ports.length === 0 && <option value="">No ports found</option>}
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
          {isConnected ? 'Disconnect' : 'Connect'}
        </button>

        <label className="checkbox-label">
          <input type="checkbox" disabled />
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
          <label>
            <input
              type="checkbox"
              checked={config.rts}
              onChange={(e) => handleChange('rts', e.target.checked)}
            />{' '}
            RtsEnable
          </label>
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
          AI Bridge
        </label>

        {bridgeEnabled && (
          <>
            <span className="bridge-endpoint">127.0.0.1:{bridgePort}</span>
            <span className="bridge-hint">
              {bridgeConnections > 0 ? `接続 ${bridgeConnections}` : '待機中'}
              {bridgeActivity &&
                ` / 送信 ${bridgeActivity.bytes} bytes ${formatBridgeTime(bridgeActivity.at_ms)}`}
            </span>
          </>
        )}
      </div>
    </div>
  );
}
