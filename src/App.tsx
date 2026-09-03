import { useState, useEffect, useCallback, useRef } from 'react';
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

// Active hotplug detection (GAP-07 / SYS-F-107).
// There is no cross-platform OS-level device notification we can use without
// native code (Windows WM_DEVICECHANGE / Linux udev), so the port list is
// polled instead. 2s is fast enough for "plug in a board and see it appear"
// while list_ports stays cheap.
const PORT_POLL_INTERVAL_MS = 2000;

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

  // Read in the poll callback without making it a dependency (the interval
  // must not be torn down and restarted on every connect/disconnect).
  const isConnectedRef = useRef(isConnected);
  useEffect(() => {
    isConnectedRef.current = isConnected;
  }, [isConnected]);

  // Shared by the manual refresh (SettingsPanel) and the hotplug poll below.
  const updatePorts = useCallback(async () => {
    try {
      const p = await invoke<string[]>('list_ports');
      // Skip the state update when the list is unchanged: the poll runs every
      // PORT_POLL_INTERVAL_MS and a new array identity would re-render the
      // whole tree (and the settings dropdown) on every tick for nothing.
      setPorts((current) =>
        current.length === p.length && current.every((name, i) => name === p[i]) ? current : p
      );
      // Use the functional form: this callback may run after the user changed
      // the selection, so decide based on the CURRENT value, not a stale one.
      setSelectedPort((current) => {
        // While connected the port <select> is disabled, so silently moving
        // the selection off the connected device would be invisible and would
        // make a later reconnect open the wrong port. A true unplug is handled
        // within milliseconds by the read-error path (serial-status), which is
        // what actually drops the connection - the poll only keeps the list
        // fresh (GAP-07).
        if (isConnectedRef.current) return current;
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
  }, []);

  // updatePorts is async: setPorts fires after the invoke resolves, not
  // synchronously within the effect (set-state-in-effect false positive).
  /* eslint-disable react-hooks/set-state-in-effect */
  useEffect(() => {
    updatePorts();
    const timer = setInterval(updatePorts, PORT_POLL_INTERVAL_MS);
    return () => clearInterval(timer);
  }, [updatePorts]);
  /* eslint-enable react-hooks/set-state-in-effect */

  // Listen for serial-status events (disconnection)
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    listen<{ connected: boolean; error: string | null }>('serial-status', (event) => {
      if (!event.payload.connected) {
        setIsConnected(false);
        // GAP-08 / DEBT-1: the worker only reports the fatal read error - the
        // backend SerialState still holds the port handle, so the UI would say
        // "disconnected" while the OS handle stays open. close_port is
        // idempotent (it takes the Option and stops reception), so calling it
        // here is safe and puts both sides back in the same state.
        invoke('close_port').catch(console.error);
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

  // Log write failures (disk full etc.) - GAP-09 / SYS-F-205.
  // The logger thread retries silently; without this the user never learns
  // that the capture stopped reaching the disk. The backend rate-limits the
  // event, so a persistent failure cannot spam alerts.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    listen<{ message: string }>('log-error', (event) => {
      alert('ログ書き込みエラー: ' + event.payload.message);
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
