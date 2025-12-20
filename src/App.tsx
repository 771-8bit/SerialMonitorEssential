import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

function App() {
  const [ports, setPorts] = useState<string[]>([]);
  const [selectedPort, setSelectedPort] = useState<string>("");
  const [isConnected, setIsConnected] = useState(false);
  const [receivedData, setReceivedData] = useState<string>("");
  const [baudRate, setBaudRate] = useState<number>(115200);

  useEffect(() => {
    updatePorts();
  }, []);

  useEffect(() => {
    let unlisten: () => void;

    async function setupListener() {
      unlisten = await listen<{ data: number[] }>("data-update", (event) => {
        // Simple hex display for Phase 1
        const hex = event.payload.data.map(b => b.toString(16).padStart(2, '0')).join(' ');
        setReceivedData(prev => (prev + " " + hex).slice(-1000)); // Keep last ~300 chars
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
        setReceivedData(""); // Clear on new connection
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
          style={{ width: '80px', marginLeft: '10px' }}
        />

        <button onClick={updatePorts} disabled={isConnected}>
          Refresh
        </button>

        <button onClick={handleToggleConnection}>
          {isConnected ? "Close" : "Open"}
        </button>
      </div>

      <div className="monitor-area" style={{
        whiteSpace: 'pre-wrap',
        fontFamily: 'monospace',
        height: '400px',
        overflowY: 'scroll',
        border: '1px solid #ccc',
        padding: '10px',
        marginTop: '20px',
        textAlign: 'left'
      }}>
        {receivedData || "No data received..."}
      </div>
    </main>
  );
}

export default App;
