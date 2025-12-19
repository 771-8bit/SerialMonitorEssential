import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

function App() {
  const [ports, setPorts] = useState<string[]>([]);
  const [selectedPort, setSelectedPort] = useState<string>("");
  const [isConnected, setIsConnected] = useState(false);

  useEffect(() => {
    updatePorts();
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

        <button onClick={updatePorts} disabled={isConnected}>
          Refresh
        </button>

        <button onClick={() => setIsConnected(!isConnected)}>
          {isConnected ? "Close" : "Open"}
        </button>
      </div>

      <div className="monitor-area">
        <p>Monitor area will be here</p>
      </div>
    </main>
  );
}

export default App;
