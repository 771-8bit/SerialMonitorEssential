import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
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

  useEffect(() => {
    updatePorts();
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
      if (p.length > 0 && !selectedPort) {
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

  return (
    <main className="container">
      <h1>Serial Monitor Essential</h1>

      <div className="control-panel">
        <select
          value={selectedPort}
          onChange={(e) => setSelectedPort(e.target.value)}
          disabled={isConnected}
        >
          {ports.map((port) => (
            <option key={port} value={port}>{port}</option>
          ))}
          {ports.length === 0 && <option value="">No ports found</option>}
        </select>

        <input
          type="number"
          value={baudRate}
          onChange={(e) => setBaudRate(Number(e.target.value))}
          disabled={isConnected}
          style={{ width: '100px', marginLeft: '10px' }}
        />

        <button onClick={updatePorts} disabled={isConnected}>
          Refresh
        </button>

        <button onClick={handleToggleConnection}>
          {isConnected ? "Close" : "Open"}
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
