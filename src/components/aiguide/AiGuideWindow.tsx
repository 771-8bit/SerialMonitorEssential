import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import './AiGuideWindow.css';

/** バックエンド `bridge_guide_info` の戻り値 */
interface BridgeGuideInfo {
  exe_path: string;
  bridge_enabled: boolean;
  bridge_port: number;
  app_version: string;
}

const POLL_MS = 2000;
const DOCS_URL = 'https://github.com/771-8bit/SerialMonitorEssential/blob/master/docs/04_api.md';

/** クリップボードへコピー（clipboard API が使えない WebView では execCommand に落とす） */
async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    try {
      const area = document.createElement('textarea');
      area.value = text;
      area.style.position = 'fixed';
      area.style.opacity = '0';
      document.body.appendChild(area);
      area.select();
      const ok = document.execCommand('copy');
      document.body.removeChild(area);
      return ok;
    } catch {
      return false;
    }
  }
}

function CopyBlock({ label, text }: { label: string; text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="guide-copy-block">
      <div className="guide-copy-header">
        <span>{label}</span>
        <button
          type="button"
          onClick={() => {
            void copyText(text).then((ok) => {
              setCopied(ok);
              if (ok) setTimeout(() => setCopied(false), 1500);
            });
          }}
        >
          {copied ? 'Copied!' : 'Copy'}
        </button>
      </div>
      <pre>{text}</pre>
    </div>
  );
}

export default function AiGuideWindow() {
  const [info, setInfo] = useState<BridgeGuideInfo | null>(null);

  // ブリッジの ON/OFF とポートを表示に反映するため 2 秒ごとに取得する
  // （このウィンドウは invoke のみを使う: 未登録ウィンドウでも動く実績のある経路）
  useEffect(() => {
    let cancelled = false;
    const fetchInfo = () => {
      invoke<BridgeGuideInfo>('bridge_guide_info')
        .then((next) => {
          if (!cancelled && next) setInfo(next);
        })
        .catch((e) => console.error(e));
    };
    fetchInfo();
    const timer = setInterval(fetchInfo, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  const exePath = info?.exe_path || '<path-to>\\serial-monitor-essential.exe';
  const port = info?.bridge_port ?? 57320;
  const enabled = info?.bridge_enabled ?? false;

  const claudeCommand = `claude mcp add serial-monitor -- "${exePath}" --mcp`;
  const mcpJson = [
    '{',
    '  "mcpServers": {',
    '    "serial-monitor": {',
    `      "command": ${JSON.stringify(exePath)},`,
    '      "args": ["--mcp"]',
    '    }',
    '  }',
    '}',
  ].join('\n');

  return (
    <div className="ai-guide-scroll">
      <div className="ai-guide">
        <header className="guide-header">
          <h1>AI Integration (MCP)</h1>
          <span className={`guide-status ${enabled ? 'on' : 'off'}`} data-testid="bridge-status">
            {enabled ? `AI Bridge: ON (127.0.0.1:${port})` : 'AI Bridge: OFF'}
          </span>
        </header>

        <section>
          <h2>How it works</h2>
          <p>
            A serial port can only be opened by one program at a time. While this app holds the
            port, it can act as a multiplexer: an AI agent (such as Claude Code) reads the same
            capture and sends to the same port through a local bridge, while you keep watching
            everything in this window. Every send from the agent is shown in the GUI.
          </p>
        </section>

        <section>
          <h2>Setup</h2>
          <ol>
            <li>
              Turn on <strong>AI Bridge</strong> in the Settings panel of the main window.{' '}
              {enabled ? (
                <span className="guide-ok">
                  Done — the bridge is listening on 127.0.0.1:{port}.
                </span>
              ) : (
                <span className="guide-warn">It is currently OFF.</span>
              )}
            </li>
            <li>
              Register this app as an MCP server in your AI client. The MCP adapter is built into
              this executable (no Node.js or other runtime required).
            </li>
          </ol>

          <CopyBlock label="Claude Code" text={claudeCommand} />
          <CopyBlock label="Other MCP clients (mcpServers JSON)" text={mcpJson} />
          <p className="guide-note">
            If you change the bridge port in the future, set the environment variable{' '}
            <code>SME_BRIDGE_PORT</code> for the MCP server entry.
          </p>
        </section>

        <section>
          <h2>Try it</h2>
          <p>Ask your agent things like:</p>
          <ul>
            <li>&quot;Send AT+GMR to the board and tell me the firmware version.&quot;</li>
            <li>&quot;Watch the serial log and tell me when BOOT OK appears.&quot;</li>
            <li>&quot;Check the last 16 KB of the log for CRC errors.&quot;</li>
          </ul>
          <pre className="guide-transcript">
            {`You    : Send AT+GMR to the board and tell me the firmware version.
Claude : serial_send("AT+GMR") -> serial_wait_for("OK|ERROR")
         -> "AT version:2.1.0.0 ... OK". The firmware version is 2.1.0.0.`}
          </pre>
        </section>

        <section>
          <h2>Available tools</h2>
          <table className="guide-tools">
            <tbody>
              <tr>
                <td>
                  <code>serial_status</code>
                </td>
                <td>Port open? Name, bytes received, app version.</td>
              </tr>
              <tr>
                <td>
                  <code>serial_ports</code>
                </td>
                <td>List serial ports on this machine.</td>
              </tr>
              <tr>
                <td>
                  <code>serial_read_tail</code>
                </td>
                <td>Read the most recent bytes (text or hex dump).</td>
              </tr>
              <tr>
                <td>
                  <code>serial_read_range</code>
                </td>
                <td>Read an explicit byte range of the capture.</td>
              </tr>
              <tr>
                <td>
                  <code>serial_send</code>
                </td>
                <td>Send text with an optional line ending.</td>
              </tr>
              <tr>
                <td>
                  <code>serial_send_hex</code>
                </td>
                <td>Send raw bytes given as hex.</td>
              </tr>
              <tr>
                <td>
                  <code>serial_wait_for</code>
                </td>
                <td>
                  Wait until a regex matches newly received data (use right after serial_send).
                </td>
              </tr>
            </tbody>
          </table>
        </section>

        <section>
          <h2>Without MCP (raw TCP)</h2>
          <p>
            Any program can talk to the bridge directly: connect to <code>127.0.0.1:{port}</code>{' '}
            and exchange one JSON object per line. Send <code>{'{"id":1,"method":"help"}'}</code> as
            the first request to receive the full machine-readable protocol specification.
          </p>
        </section>

        <section>
          <h2>Security</h2>
          <ul>
            <li>The bridge listens on 127.0.0.1 only and is OFF by default.</li>
            <li>
              Every send from an agent appears in the Settings panel (byte count, time, preview) —
              there is no invisible transmit path.
            </li>
          </ul>
        </section>

        <footer className="guide-footer">
          <span>
            Full protocol reference: <code>{DOCS_URL}</code>
          </span>
          <span>App version: {info?.app_version ?? '...'}</span>
        </footer>
      </div>
    </div>
  );
}
