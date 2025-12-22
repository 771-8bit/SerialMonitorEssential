import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save, ask } from "@tauri-apps/plugin-dialog";
import SettingsPanel, { SerialConfig } from "./components/SettingsPanel";
import SendPanel from "./components/SendPanel";
import ReceivePanel from "./components/ReceivePanel";
import "./App.css";

interface DataUpdatePayload {
  total_bytes: number;
}

function App() {
  const [ports, setPorts] = useState<string[]>([]);
  const [selectedPort, setSelectedPort] = useState<string>("");
  const [isConnected, setIsConnected] = useState(false);
  const [totalBytes, setTotalBytes] = useState<number>(0);
  const [autoScroll, setAutoScroll] = useState(true);

  const [config, setConfig] = useState<SerialConfig>({
    baud_rate: 115200,
    data_bits: 8,
    flow_control: "None",
    parity: "None",
    stop_bits: 1,
    dtr: true,
    rts: true
  });

  useEffect(() => {
    updatePorts();
  }, []);

  // Listen for serial-status events (disconnection)
  useEffect(() => {
    let unlisten: () => void;

    async function setupStatusListener() {
      unlisten = await listen<{ connected: boolean; error: string | null }>("serial-status", (event) => {
        if (!event.payload.connected) {
          setIsConnected(false);
          if (event.payload.error) {
            alert("Serial disconnected: " + event.payload.error);
          }
        }
      });
    }

    setupStatusListener();

    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    let unlisten: () => void;

    async function setupListener() {
      unlisten = await listen<DataUpdatePayload>("data-update", (event) => {
        setTotalBytes(event.payload.total_bytes);
      });
    }

    setupListener();

    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  // Update DTR when config changes while connected
  useEffect(() => {
    if (isConnected) {
      invoke("write_dtr", { level: config.dtr }).catch(console.error);
    }
  }, [config.dtr, isConnected]);

  // Update RTS when config changes while connected
  useEffect(() => {
    if (isConnected) {
      invoke("write_rts", { level: config.rts }).catch(console.error);
    }
  }, [config.rts, isConnected]);

  async function updatePorts() {
    try {
      const p = await invoke<string[]>("list_ports");
      setPorts(p);
      // If selected port is no longer in the list, clear it or select first available
      if (selectedPort && !p.includes(selectedPort)) {
        setSelectedPort(p.length > 0 ? p[0] : "");
      } else if (p.length > 0 && !selectedPort) {
        setSelectedPort(p[0]);
      }
    } catch (e) {
      console.error(e);
    }
  }

  async function handleToggleConnection() {
    if (isConnected) {
      try {
        await invoke("close_port");
        setIsConnected(false);
      } catch (e) {
        console.error(e);
        alert("Failed to close port: " + e);
      }
    } else {
      if (!selectedPort) return;
      try {
        await invoke("open_port", { portName: selectedPort, config: config });
        setIsConnected(true);
        setTotalBytes(0);
      } catch (e) {
        console.error(e);
        alert("Failed to open port: " + e);
      }
    }
  }

  async function handleExport() {
    if (totalBytes === 0) {
      alert("No data to export");
      return;
    }

    try {
      const filePath = await save({
        filters: [
          { name: "Binary", extensions: ["bin"] },
          { name: "All Files", extensions: ["*"] }
        ],
        defaultPath: "serial_log.bin"
      });

      if (filePath) {
        const bytesExported = await invoke<number>("export_log", { path: filePath });
        console.log(`Exported ${bytesExported.toLocaleString()} bytes to ${filePath}`);
      }
    } catch (e) {
      console.error(e);
      alert("Failed to export: " + e);
    }
  }

  async function handleClear() {
    try {
      await invoke("clear_data");
      setTotalBytes(0);
    } catch (e) {
      console.error(e);
      alert("Failed to clear data: " + e);
    }
  }

  async function handleCopy() {
    if (totalBytes === 0) return;

    if (totalBytes > 10 * 1024 * 1024) { // 10MB
      const confirmed = await ask("The data is quite large (>10MB). Copying to clipboard might freeze the application momentarily. Continue?", {
        title: "Large Data Warning",
        kind: 'warning'
      });
      if (!confirmed) return;
    }

    try {
      const text = await invoke<string>("get_clipboard_text");
      if (text) {
        await navigator.clipboard.writeText(text);
      }
    } catch (e) {
      console.error(e);
      alert("Failed to copy: " + e);
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
