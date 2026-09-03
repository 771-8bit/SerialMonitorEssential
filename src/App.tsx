import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { save, ask } from '@tauri-apps/plugin-dialog';
import SettingsPanel, { SerialConfig } from './components/SettingsPanel';
import SendPanel from './components/SendPanel';
import ReceivePanel, { ViewMode } from './components/ReceivePanel';
import './App.css';

interface DataUpdatePayload {
  total_bytes: number;
}

function App() {
  const [ports, setPorts] = useState<string[]>([]);
  const [selectedPort, setSelectedPort] = useState<string>('');
  const [isConnected, setIsConnected] = useState(false);
  const [totalBytes, setTotalBytes] = useState<number>(0);
  const [autoScroll, setAutoScroll] = useState(true);

  const [config, setConfig] = useState<SerialConfig>({
    baud_rate: 115200,
    data_bits: 8,
    flow_control: 'None',
    parity: 'None',
    stop_bits: 1,
    dtr: true,
    rts: true,
  });

  const updatePorts = async () => {
    try {
      const p = await invoke<string[]>('list_ports');
      setPorts(p);
      // Use the functional form: this callback may run after the user changed
      // the selection, so decide based on the CURRENT value, not a stale one.
      setSelectedPort((current) => {
        if (current && !p.includes(current)) {
          return p.length > 0 ? p[0] : '';
        }
        if (!current && p.length > 0) {
          return p[0];
        }
        return current;
      });
    } catch (e) {
      console.error(e);
    }
  };

  // updatePorts is async: setPorts fires after the invoke resolves, not
  // synchronously within the effect (set-state-in-effect false positive).
  /* eslint-disable react-hooks/set-state-in-effect */
  useEffect(() => {
    updatePorts();
  }, []);
  /* eslint-enable react-hooks/set-state-in-effect */

  // Listen for serial-status events (disconnection)
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    listen<{ connected: boolean; error: string | null }>('serial-status', (event) => {
      if (!event.payload.connected) {
        setIsConnected(false);
        if (event.payload.error) {
          alert('Serial disconnected: ' + event.payload.error);
        }
      }
    }).then((fn) => {
      // If unmounted before listen() resolved, release immediately
      // (otherwise the listener would leak and duplicate on remount)
      if (cancelled) fn();
      else unlisten = fn;
    });

    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    listen<DataUpdatePayload>('data-update', (event) => {
      setTotalBytes(event.payload.total_bytes);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });

    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  // Update DTR when config changes while connected
  useEffect(() => {
    if (isConnected) {
      invoke('write_dtr', { level: config.dtr }).catch(console.error);
    }
  }, [config.dtr, isConnected]);

  // Update RTS when config changes while connected
  useEffect(() => {
    if (isConnected) {
      invoke('write_rts', { level: config.rts }).catch(console.error);
    }
  }, [config.rts, isConnected]);

  async function handleToggleConnection() {
    if (isConnected) {
      try {
        await invoke('close_port');
        setIsConnected(false);
      } catch (e) {
        console.error(e);
        alert('Failed to close port: ' + e);
      }
    } else {
      if (!selectedPort) return;
      try {
        await invoke('open_port', { portName: selectedPort, config: config });
        setIsConnected(true);
        setTotalBytes(0);
      } catch (e) {
        console.error(e);
        alert('Failed to open port: ' + e);
      }
    }
  }

  async function handleExport() {
    if (totalBytes === 0) {
      alert('No data to export');
      return;
    }

    try {
      const filePath = await save({
        filters: [
          { name: 'Binary', extensions: ['bin'] },
          { name: 'All Files', extensions: ['*'] },
        ],
        defaultPath: 'serial_log.bin',
      });

      if (filePath) {
        const bytesExported = await invoke<number>('export_log', { path: filePath });
        console.log(`Exported ${bytesExported.toLocaleString()} bytes to ${filePath}`);
      }
    } catch (e) {
      console.error(e);
      alert('Failed to export: ' + e);
    }
  }

  async function handleClear() {
    try {
      await invoke('clear_data');
      setTotalBytes(0);
    } catch (e) {
      console.error(e);
      alert('Failed to clear data: ' + e);
    }
  }

  async function handleCopy(mode: ViewMode) {
    if (totalBytes === 0) return;

    if (totalBytes > 10 * 1024 * 1024) {
      // 10MB
      const confirmed = await ask(
        'The data is quite large (>10MB). Copying to clipboard might freeze the application momentarily. Continue?',
        {
          title: 'Large Data Warning',
          kind: 'warning',
        }
      );
      if (!confirmed) return;
    }

    try {
      const text = await invoke<string>('get_clipboard_text', { mode });
      if (text) {
        await navigator.clipboard.writeText(text);
      }
    } catch (e) {
      console.error(e);
      alert('Failed to copy: ' + e);
    }
  }

  return (
    <div className="app-container">
      <div className="header-section">
        <SettingsPanel
          ports={ports}
          selectedPort={selectedPort}
          onPortChange={setSelectedPort}
          onRefreshPorts={updatePorts}
          config={config}
          onConfigChange={setConfig}
          isConnected={isConnected}
          onConnect={handleToggleConnection}
          onDisconnect={handleToggleConnection}
        />
      </div>

      <div className="middle-section">
        <SendPanel connected={isConnected} />
      </div>

      <div className="bottom-section">
        <ReceivePanel
          totalBytes={totalBytes}
          onExport={handleExport}
          onClear={handleClear}
          onCopy={handleCopy}
          autoScroll={autoScroll}
          onAutoScrollChange={setAutoScroll}
        />
      </div>
    </div>
  );
}

export default App;
