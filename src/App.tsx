import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { save, ask } from '@tauri-apps/plugin-dialog';
import SettingsPanel, { SerialConfig } from './components/SettingsPanel';
import SendPanel from './components/SendPanel';
import ReceivePanel, { OutputFormat } from './components/ReceivePanel';
import { portPath } from './utils/portUtils';
import { isSerialConfig } from './utils/serialConfig';
import { useTheme } from './utils/theme';
import { useToast } from './utils/useToast';
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

// Auto Connect (SYS-F-807). While waiting for a device, poll faster than the hotplug
// poll: a board that resets is usually back within a second or two. The C# version
// retried every 200 ms. When the port is listed but cannot be opened (another program
// holds it), back off so that waiting indefinitely stays cheap.
const RECONNECT_POLL_MS = 200;
const RECONNECT_BUSY_POLL_MS = 1000;
const RECONNECT_BUSY_AFTER_FAILURES = 5;

const DEFAULT_SERIAL_CONFIG: SerialConfig = {
  baud_rate: 115200,
  data_bits: 8,
  flow_control: 'None',
  parity: 'None',
  stop_bits: 1,
  // Both off so that opening the port resets no board: DTR on resets Arduino Uno-type
  // boards, RTS on (without DTR) holds ESP32-style boards in reset. Native-USB boards
  // may need DTR on to send; the settings panel explains this next to the checkboxes.
  dtr: false,
  rts: false,
};

// The port the user last chose. Saved only on a user choice or Connect, never by the
// automatic fallback, so starting without the device plugged in keeps the preference.
const LAST_PORT_KEY = 'lastPort';

function App() {
  const [ports, setPorts] = useState<string[]>([]);
  // With Auto Connect on, start on the remembered port even if it is absent, so the
  // app waits for that device (shown as "not present") instead of another one
  const [selectedPort, setSelectedPort] = useState<string>(() =>
    loadSetting('autoConnect', false, isBoolean) ? loadSetting(LAST_PORT_KEY, '', isString) : ''
  );
  const [isConnected, setIsConnected] = useState(false);
  const [totalBytes, setTotalBytes] = useState<number>(0);
  const [theme, setTheme] = useTheme();
  // Confirms Copy / Save, as the C# version's auto-closing message box did
  const [toast, showToast] = useToast();

  // Settings remembered across restarts (issue #7)
  const [autoScroll, setAutoScroll] = usePersistentState('receive.autoScroll', true, isBoolean);
  const [config, setConfig] = usePersistentState<SerialConfig>(
    'serialConfig',
    DEFAULT_SERIAL_CONFIG,
    isSerialConfig
  );
  const preferredPortRef = useRef(loadSetting(LAST_PORT_KEY, '', isString));
  // Auto Connect, as in the C# version: while it is on and no port is open, connect to
  // the selected port as soon as it can be opened - at startup, when the device is
  // plugged in, and after it went away (unplug, board reset). Stored under a new key:
  // the old "Auto Reconnect" never opened a port at startup, so it is not carried over.
  const [autoConnect, setAutoConnect] = usePersistentState('autoConnect', false, isBoolean);
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

  // --- Auto Connect (SYS-F-807) ---------------------------------------------
  // While waiting, look for the device path and open it with `reopen_port`, which
  // continues the SAME capture (offsets continue, nothing is cleared); only the
  // Connect button starts a new one. When the connected device disappears, wait for
  // the same path again. A user-initiated Disconnect never produces
  // `serial-status connected:false`, so every such event is an unexpected disconnect.
  const autoConnectRef = useRef(autoConnect);
  useEffect(() => {
    autoConnectRef.current = autoConnect;
  }, [autoConnect]);

  // Device path we are waiting for (null = not waiting). The ref is updated
  // synchronously so the port poll and the event listener see it immediately.
  const [waitingFor, setWaitingFor] = useState<string | null>(null);
  const waitingRef = useRef<string | null>(null);
  const [reconnectedAt, setReconnectedAt] = useState<number | null>(null);
  // Why the last attempt to open the waited-for port failed (e.g. in use elsewhere)
  const [waitError, setWaitError] = useState<string | null>(null);
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
    setWaitError(null);
    setReconnectedAt(null);
    invoke('set_reconnecting', { waiting: true }).catch(console.error);
  }, []);

  const endWaiting = useCallback(() => {
    if (waitingRef.current === null) return;
    waitingRef.current = null;
    setWaitingFor(null);
    setWaitError(null);
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
    // updatePorts is async and sets state only after awaiting list_ports
    // eslint-disable-next-line react-hooks/set-state-in-effect
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
        if (autoConnectRef.current && lost) {
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

  // Auto Connect: whenever it is on and nothing is open or awaited, wait for the
  // selected port (at startup, after turning it on, after picking another port).
  // Started synchronously on purpose: waitingRef must be set before the first port
  // poll resolves, or the poll would move the selection off an absent remembered port.
  useEffect(() => {
    if (autoConnect && !isConnected && waitingFor === null && selectedPort) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      beginWaiting(portPath(selectedPort));
    }
  }, [autoConnect, isConnected, waitingFor, selectedPort, beginWaiting]);

  // While waiting: look for the device path and open it as soon as it is there. A
  // failed open is expected right after re-enumeration (Windows refuses the port for a
  // few hundred ms), so failures are retried; after several in a row (the port is in
  // use by another program) the retries slow down and the reason is shown.
  useEffect(() => {
    if (waitingFor === null) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let failures = 0;

    const attempt = async () => {
      if (stopped) return;
      try {
        const list = await invoke<string[]>('list_ports');
        const entry = list.find((p) => portPath(p) === waitingFor);
        if (stopped) return;
        if (!entry) {
          failures = 0;
          setWaitError(null);
        } else {
          await invoke('reopen_port', { portName: entry, config: configRef.current });
          // Even if the wait was cancelled while this open was in flight, the port is
          // now open: show it as connected rather than leave an invisible open port.
          connectedPortRef.current = entry;
          setPorts(list);
          setSelectedPort(entry);
          setIsConnected(true);
          setReconnectedAt(Date.now());
          endWaiting();
          return;
        }
      } catch (e) {
        failures += 1;
        if (failures >= RECONNECT_BUSY_AFTER_FAILURES && !stopped) setWaitError(String(e));
      }
      if (!stopped) {
        const delay =
          failures >= RECONNECT_BUSY_AFTER_FAILURES ? RECONNECT_BUSY_POLL_MS : RECONNECT_POLL_MS;
        timer = setTimeout(() => void attempt(), delay);
      }
    };

    void attempt();
    return () => {
      stopped = true;
      clearTimeout(timer);
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
    // While waiting the button reads "Cancel": stop waiting, keep the capture. Like
    // Disconnect it turns Auto Connect off, which would otherwise start waiting again.
    if (waitingRef.current !== null) {
      setAutoConnect(false);
      endWaiting();
      connectedPortRef.current = '';
      return;
    }
    if (isConnected) {
      // C# version: Disconnect turns Auto Connect off, or the port would reopen at once
      setAutoConnect(false);
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
      showToast('Nothing to save');
      return;
    }

    // Timestamp ON (ASCII view): text with a timestamp on every line (issue #4).
    // Otherwise, and always in the Hex view, the received bytes exactly as they arrived.
    const withTimestamps = format.mode === 'ascii' && format.timestampSeparator !== null;
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
        showToast('Saved');
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
    if (totalBytes === 0) {
      showToast('Nothing to copy');
      return;
    }

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
        showToast('Copied');
      } else {
        showToast('Nothing to copy');
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
          autoConnect={autoConnect}
          onAutoConnectChange={(enabled) => {
            setAutoConnect(enabled);
            if (!enabled) endWaiting();
          }}
          reconnectWaitingFor={waitingFor}
          reconnectError={waitError}
          reconnectedAt={reconnectedAt}
          theme={theme}
          onThemeChange={setTheme}
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

      {toast !== null && (
        <div className="toast" role="status" aria-live="polite">
          {toast}
        </div>
      )}
    </div>
  );
}

export default App;
