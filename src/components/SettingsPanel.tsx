import { useState } from 'react';

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

  const handleChange = <K extends keyof SerialConfig>(key: K, value: SerialConfig[K]) => {
    onConfigChange({ ...config, [key]: value });
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

        <div className="baud-custom-container" style={{ position: 'relative', width: '130px' }}>
          {baudRateEditing ? (
            <input
              type="number"
              value={config.baud_rate}
              onChange={(e) => handleChange('baud_rate', Number(e.target.value))}
              onBlur={() => setBaudRateEditing(false)}
              onKeyDown={(e) => e.key === 'Enter' && setBaudRateEditing(false)}
              disabled={isConnected}
              autoFocus
              className="baud-input"
              style={{ width: '100%' }}
            />
          ) : (
            <div
              className="baud-trigger"
              onClick={() => {
                if (isConnected) return;
                setBaudDropdownOpen(!baudDropdownOpen);
              }}
              style={{
                border: '1px solid #444',
                borderRadius: '3px',
                padding: '4px 8px',
                fontSize: '13px',
                backgroundColor: isConnected ? '#2a2a2a' : '#2a2a2a',
                color: isConnected ? '#888' : '#eee',
                cursor: isConnected ? 'not-allowed' : 'pointer',
                height: '28px',
                boxSizing: 'border-box',
                display: 'flex',
                alignItems: 'center',
                justifyContent: 'space-between',
              }}
            >
              <span>{config.baud_rate} bps</span>
              <span style={{ fontSize: '10px', marginLeft: '4px' }}>▼</span>
            </div>
          )}

          {baudDropdownOpen && !baudRateEditing && !isConnected && (
            <>
              <div
                style={{ position: 'fixed', top: 0, left: 0, right: 0, bottom: 0, zIndex: 999 }}
                onClick={() => setBaudDropdownOpen(false)}
              />
              <ul
                style={{
                  position: 'absolute',
                  top: '100%',
                  left: 0,
                  width: '100%',
                  border: '1px solid #444',
                  backgroundColor: '#2a2a2a',
                  listStyle: 'none',
                  padding: 0,
                  margin: 0,
                  zIndex: 1000,
                  boxShadow: '0 2px 4px rgba(0,0,0,0.5)',
                }}
              >
                {[
                  9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600, 1000000, 2000000,
                  3000000, 12000000,
                ].map((rate) => (
                  <li
                    key={rate}
                    onClick={() => {
                      handleChange('baud_rate', rate);
                      setBaudDropdownOpen(false);
                    }}
                    style={{
                      padding: '4px 8px',
                      cursor: 'pointer',
                      fontSize: '13px',
                      color: '#eee',
                      borderBottom: '1px solid #333',
                    }}
                    onMouseEnter={(e) => (e.currentTarget.style.backgroundColor = '#3a3a3a')}
                    onMouseLeave={(e) => (e.currentTarget.style.backgroundColor = 'transparent')}
                  >
                    {rate}
                  </li>
                ))}
                <li
                  onClick={() => {
                    setBaudRateEditing(true);
                    setBaudDropdownOpen(false);
                  }}
                  style={{
                    padding: '4px 8px',
                    cursor: 'pointer',
                    fontSize: '13px',
                    color: '#8cf',
                    fontStyle: 'italic',
                  }}
                  onMouseEnter={(e) => (e.currentTarget.style.backgroundColor = '#3a3a3a')}
                  onMouseLeave={(e) => (e.currentTarget.style.backgroundColor = 'transparent')}
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
    </div>
  );
}
