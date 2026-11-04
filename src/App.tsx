import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { save, ask } from '@tauri-apps/plugin-dialog';
import SettingsPanel, { SerialConfig } from './components/SettingsPanel';
import SendPanel from './components/SendPanel';
import ReceivePanel, { OutputFormat } from './components/ReceivePanel';
import { portPath } from './utils/portUtils';
import { isSerialConfig } from './utils/serialConfig';
import { usePersistentState, loadSetting, saveSetting, isBoolean, isString } from './utils/persist';
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

// Auto reconnect (SYS-F-807). While waiting for a lost device, poll faster than the
// hotplug poll: a board that resets is usually back within a second or two.
const RECONNECT_POLL_MS = 500;

const DEFAULT_SERIAL_CONFIG: SerialConfig = {
  baud_rate: 115200,
  data_bits: 8,
  flow_control: 'None',
  parity: 'None',
  stop_bits: 1,
  dtr: true,
  rts: true,
};

// The port the user last chose. Saved only on a user choice or Connect, never by the
// automatic fallback, so starting without the device plugged in keeps the preference.
const LAST_PORT_KEY = 'lastPort';

function App() {
  const [ports, setPorts] = useState<string[]>([]);
  const [selectedPort, setSelectedPort] = useState<string>('');
  const [isConnected, setIsConnected] = useState(false);
  const [totalBytes, setTotalBytes] = useState<number>(0);

  // Settings remembered across restarts (issue #7)
  const [autoScroll, setAutoScroll] = usePersistentState('receive.autoScroll', true, isBoolean);
  const [config, setConfig] = usePersistentState<SerialConfig>(
    'serialConfig',
    DEFAULT_SERIAL_CONFIG,
    isSerialConfig
  );
  const preferredPortRef = useRef(loadSetting(LAST_PORT_KEY, '', isString));
  const rememberPort = useCallback((port: string) => {
    if (!port) return;
    preferredPortRef.current = port;
    saveSetting(LAST_PORT_KEY, port);
  }, []);

  // Read in the poll callback without making it a dependency (the interval
  // must not be torn down and restarted on every connect/disconnect).
  const isConnectedRef = useRef(isConnected);
  useEffect(() => {
    isConnectedRef.current = isConnected;
  }, [isConnected]);

  // --- Auto reconnect (SYS-F-807) -------------------------------------------
  // When the device we were connected to disappears (unplug, board reset), wait
  // for the same device path to come back and reopen it with `reopen_port`,
  // which continues the SAME capture (offsets continue, nothing is cleared).
  // A user-initiated Disconnect never produces `serial-status connected:false`,
  // so every such event is an unexpected disconnect.
  const [autoReconnect, setAutoReconnect] = usePersistentState('autoReconnect', false, isBoolean);
  const autoReconnectRef = useRef(autoReconnect);
  useEffect(() => {
    autoReconnectRef.current = autoReconnect;
  }, [autoReconnect]);

  // Device path we are waiting for (null = not waiting). The ref is updated
  // synchronously so the port poll and the event listener see it immediately.
  const [waitingFor, setWaitingFor] = useState<string | null>(null);
  const waitingRef = useRef<string | null>(null);
  const [reconnectedAt, setReconnectedAt] = useState<number | null>(null);
  // The list entry of the current connection (the device to wait for if it goes away)
  const connectedPortRef = useRef('');
  // Reconnect uses the settings as they are when the device comes back
  const configRef = useRef(config);
  useEffect(() => {
    configRef.current = config;
  }, [config]);

  const beginWaiting = useCallback((path: string) => {
    waitingRef.current = path;
    setWaitingFor(path);
    setReconnectedAt(null);
    invoke('set_reconnecting', { waiting: true }).catch(console.error);
  }, []);

  const endWaiting = useCallback(() => {
    if (waitingRef.current === null) return;
    waitingRef.current = null;
    setWaitingFor(null);
    invoke('set_reconnecting', { waiting: false }).catch(console.error);
  }, []);

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
        // fresh (GAP-07). The same holds while waiting to reconnect: the lost
        // device is absent from the list by definition, and moving the selection
        // would point the wait (and the user) at a different device.
        if (isConnectedRef.current || waitingRef.current !== null) return current;
        // Fallback when nothing valid is selected: the port the user last chose if
        // it is present (issue #7), else the first one
        const fallback = () =>
          p.includes(preferredPortRef.current) ? preferredPortRef.current : (p[0] ?? '');
        if (current && !p.includes(current)) {
          return fallback();
        }
        if (!current && p.length > 0) {
          return fallback();
        }
        return current;
      });
    } catch (e) {
      console.error(e);
    }
  }, []);

  useEffect(() => {
    updatePorts();
    const timer = setInterval(updatePorts, PORT_POLL_INTERVAL_MS);
    return () => clearInterval(timer);
  }, [updatePorts]);

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
        const lost = connectedPortRef.current;
        if (autoReconnectRef.current && lost) {
          // No alert: window.alert blocks the JS thread, so the reconnect would
          // stall until the user dismissed it. The settings row shows the state.
          beginWaiting(portPath(lost));
        } else if (event.payload.error) {
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
  }, [beginWaiting]);

  // While waiting: look for the lost device path and reopen it as soon as it is
  // back. A failed open is expected right after re-enumeration (Windows refuses
  // the port for a few hundred ms), so failures are retried silently.
  useEffect(() => {
    if (waitingFor === null) return;
    let stopped = false;
    let busy = false;

    const attempt = async () => {
      if (busy || stopped) return;
      busy = true;
      try {
        const list = await invoke<string[]>('list_ports');
        const entry = list.find((p) => portPath(p) === waitingFor);
        if (!entry || stopped) return;
        await invoke('reopen_port', { portName: entry, config: configRef.current });
        // Even if the wait was cancelled while this open was in flight, the port is
        // now open: show it as connected rather than leave an invisible open port.
        connectedPortRef.current = entry;
        setPorts(list);
        setSelectedPort(entry);
        setIsConnected(true);
        setReconnectedAt(Date.now());
        endWaiting();
      } catch {
        // not ready yet - try again on the next tick
      } finally {
        busy = false;
      }
    };

    void attempt();
    const timer = setInterval(() => void attempt(), RECONNECT_POLL_MS);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  }, [waitingFor, endWaiting]);

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
      alert('Log write error: ' + event.payload.message);
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
    // While waiting the button reads "Cancel": stop waiting, keep the capture
    if (waitingRef.current !== null) {
      endWaiting();
      connectedPortRef.current = '';
      return;
    }
    if (isConnected) {
      try {
        await invoke('close_port');
        connectedPortRef.current = '';
        setReconnectedAt(null);
        setIsConnected(false);
      } catch (e) {
        console.error(e);
        alert('Failed to close port: ' + e);
      }
    } else {
      if (!selectedPort) return;
      try {
        // Manual Connect starts a new session (SYS-F-803); only this path resets the count
        await invoke('open_port', { portName: selectedPort, config: config });
        connectedPortRef.current = selectedPort;
        rememberPort(selectedPort);
        setReconnectedAt(null);
        setIsConnected(true);
        setTotalBytes(0);
      } catch (e) {
        console.error(e);
        alert('Failed to open port: ' + e);
      }
    }
  }

  async function handleExport(format: OutputFormat) {
    if (totalBytes === 0) {
      alert('No data to export');
      return;
    }

    // Timestamp ON (ASCII view): text with a timestamp on every line (issue #4).
    // Otherwise the received bytes exactly as they arrived.
    const withTimestamps = format.timestampSeparator !== null;
    try {
      const filePath = await save(
        withTimestamps
          ? {
              filters: [
                { name: 'Text', extensions: ['txt', 'log'] },
                { name: 'All Files', extensions: ['*'] },
              ],
              defaultPath: 'serial_log.txt',
            }
          : {
              filters: [
                { name: 'Binary', extensions: ['bin'] },
                { name: 'All Files', extensions: ['*'] },
              ],
              defaultPath: 'serial_log.bin',
            }
      );

      if (filePath) {
        const bytesExported = withTimestamps
          ? await invoke<number>('export_text_with_timestamps', {
              path: filePath,
              separator: format.timestampSeparator,
            })
          : await invoke<number>('export_log', { path: filePath });
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

  async function handleCopy(format: OutputFormat) {
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
      const text = await invoke<string>('get_clipboard_text', {
        mode: format.mode,
        timestampSeparator: format.timestampSeparator,
      });
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
          onPortChange={(port) => {
            // Picking another device means the user is no longer waiting for the old one
            endWaiting();
            setSelectedPort(port);
            rememberPort(port);
          }}
          onRefreshPorts={updatePorts}
          config={config}
          onConfigChange={setConfig}
          isConnected={isConnected}
          onConnect={handleToggleConnection}
          onDisconnect={handleToggleConnection}
          autoReconnect={autoReconnect}
          onAutoReconnectChange={(enabled) => {
            setAutoReconnect(enabled);
            if (!enabled) endWaiting();
          }}
          reconnectWaitingFor={waitingFor}
          reconnectedAt={reconnectedAt}
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
