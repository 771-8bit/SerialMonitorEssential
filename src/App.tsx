import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save } from "@tauri-apps/plugin-dialog";
import HexViewer from "./components/HexViewer";
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
  const [baudRate, setBaudRate] = useState<number>(115200);
  const [baudRateEditing, setBaudRateEditing] = useState(false);

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
        await invoke("open_port", { portName: selectedPort, baudRate: Number(baudRate) });
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

  return (
    <main className="container">
      <h1>Serial Monitor Essential</h1>

      <div className="control-panel">
        <select
          value={selectedPort}
          onChange={(e) => setSelectedPort(e.target.value)}
          disabled={isConnected}
          onClick={updatePorts}
        >
          {ports.map((port) => (
            <option key={port} value={port}>{port}</option>
          ))}
          {ports.length === 0 && <option value="">No ports found</option>}
        </select>

        {baudRateEditing ? (
          <input
            type="number"
            value={baudRate}
            onChange={(e) => setBaudRate(Number(e.target.value))}
            onClick={() => setBaudRateEditing(false)}
            onKeyDown={(e) => e.key === 'Enter' && (e.target as HTMLInputElement).blur()}
            disabled={isConnected}
            autoFocus
            style={{ marginLeft: '10px', width: '120px' }}
          />
        ) : (
          <select
            value={baudRate}
            onChange={(e) => setBaudRate(Number(e.target.value))}
            onDoubleClick={() => !isConnected && setBaudRateEditing(true)}
            disabled={isConnected}
            style={{ marginLeft: '10px' }}
          >
            <option value={9600}>9600</option>
            <option value={19200}>19200</option>
            <option value={38400}>38400</option>
            <option value={57600}>57600</option>
            <option value={115200}>115200</option>
            <option value={230400}>230400</option>
            <option value={460800}>460800</option>
            <option value={921600}>921600</option>
            <option value={1000000}>1000000 (1Mbps)</option>
            <option value={2000000}>2000000 (2Mbps)</option>
            <option value={3000000}>3000000 (3Mbps)</option>
            <option value={12000000}>12000000 (12Mbps)</option>
          </select>
        )}

        <button onClick={handleToggleConnection}>
          {isConnected ? "Close" : "Open"}
        </button>

        <button onClick={handleExport} disabled={totalBytes === 0}>
          Export
        </button>

        <label style={{ marginLeft: '20px', display: 'flex', alignItems: 'center', gap: '5px' }}>
          <input
            type="checkbox"
            checked={autoScroll}
            onChange={(e) => setAutoScroll(e.target.checked)}
          />
          Auto-scroll
        </label>
      </div>

      <div className="monitor-area" style={{ marginTop: '20px' }}>
        <HexViewer
          totalBytes={totalBytes}
          autoScroll={autoScroll}
        />
      </div>
    </main>
  );
}

export default App;
