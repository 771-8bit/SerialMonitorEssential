# E2E Automation Harness

Scripts for real-machine E2E testing using a com0com virtual COM pair (COM15⇔COM16)
and Windows UI Automation. The goal is that **the instructions in this directory alone
are enough to reproduce the full E2E suite on a fresh machine, with no physical hardware**.

The only script that can run in CI (GitHub Actions) is `mcp_stdio_smoke.py`, which needs
no GUI (already wired into `.github/workflows/ci.yml`). For why the rest cannot run in CI
(com0com is a kernel-mode driver / UIA requires an interactive desktop), see
[docs/25 §3.4](../../docs/25_release_strategy.md) (Japanese).

## Setup (first time only)

### 1. Install com0com

1. Get the installer from [com0com (SourceForge)](https://sourceforge.net/projects/com0com/)
   and run it.
   - On Windows 10/11 (x64, Secure Boot enabled) you need a **signed-driver**
     build (the v3.0.0.0 line). Unsigned builds require test-signing mode.
2. Right after installation there is one default pair, `CNCA0⇔CNCB0` (leave it as is).

### 2. Create the COM15⇔COM16 pair (requires admin)

```powershell
cd "C:\Program Files (x86)\com0com"
.\setupc.exe install PortName=COM15 PortName=COM16
```

Verify (either way works):

```powershell
.\setupc.exe list
# or
Get-ItemProperty "HKLM:\HARDWARE\DEVICEMAP\SERIALCOMM"
#   \Device\com0com11 : COM15
#   \Device\com0com21 : COM16  — if you see these, you're set
```

If you chose different port numbers, pass them via each script's `-PortApp` / `--port`
arguments.

### 3. Python dependencies

```powershell
pip install pyserial
```

### 4. Build the app

```powershell
cd src-tauri
cargo build --release   # release is recommended for E2E (no DEV debug overlay)
```

### Virtual-pair limitations (important)

- **Baud rate and DTR/RTS are effectively no-ops on the virtual pair.** They are fine for
  checking that the settings UI applies them, but they do not verify anything at the
  signal level.
- Therefore the pair **cannot be used for performance acceptance such as 12 Mbps**.
  Performance testing is done on real hardware (Raspberry Pi Pico) — see
  [docs/25 §2](../../docs/25_release_strategy.md) (Japanese),
  "passing on com0com alone is not evidence for 1.0".

## Script list

| File | Role | GUI | com0com | CI |
|---|---|---|---|---|
| `ui.ps1` | Shared UIA actions. `-Action list / select-port / select-combo / click / toggle / click-text / wheel / close-window / shot` | Required | - | No |
| `pairwise_gen.py` / `pairwise_gen2.py` | Pairwise (t=2) covering-array generators (greedy, deterministic). When factors change, regenerate and update `$rows` in `pairwise_run*.ps1` | - | - | Yes (generation only) |
| `pairwise_run.ps1` / `pairwise_run2.ps1` | Apply each row of the covering array via UIA and check a health oracle (process alive, no panics in the log, window state) | Required | Required | No |
| `mcp_stdio_smoke.py` | **Smoke test for the built-in MCP adapter (`--mcp`)**. JSON-RPC over a real pipe (initialize / ping / tools / error cases / clean shutdown). Uses an isolated port to make the bridge-not-running case deterministic | Not needed | Not needed | **Wired in** |
| `pong_bot.py` | Responder bot on COM15 that replies `PONG 42` to `PING` | Not needed | Required | No |
| `mcp_bridge_live.py` | **Live round-trip verification of the AI Bridge** (status / send→wait_for / read_tail / prompt ping response while blocked / cancelled interruption) | Required (started separately) | Required | No |
| `run_bridge_e2e.ps1` | **Runs the three above in one command** (launch → UIA setup → round-trip verification → cleanup) | Auto-launched | Required | No |

## Usage

```powershell
# --- Same smoke test as CI (no GUI needed) ---
python .\mcp_stdio_smoke.py

# --- Full AI Bridge E2E (scripted version of GAP-31) ---
.\run_bridge_e2e.ps1                 # auto-detects the release binary, COM15/COM16

# --- Individual UIA actions ---
.\ui.ps1 -Action select-combo -Path "COM|CNC" -Name "COM16"
.\ui.ps1 -Action click -Name "Connect"
.\ui.ps1 -Action shot -WindowTitle "Serial Plotter" -Path out.png

# --- Batch pairwise run ---
# Start the app beforehand with `npm run tauri dev -- --no-watch`
# (without --no-watch, file changes during the test restart the app)
.\pairwise_run.ps1 -LogPath <the app's stdout log file>

# --- Send plotter data ---
python ..\serial_test.py --source virtual --port COM15 --mode plot:label
```

## Notes

- Screenshots (`shot`) bring the target window to the foreground
- `select-combo` activates the window in order to expand the dropdown
- Right after a window appears, UIA actions can fail because React has not mounted yet
  (`run_bridge_e2e.ps1` waits a few seconds before acting)
- Killing `npm run tauri dev` from a script can leave vite holding port 1420:
  look up the PID with `Get-NetTCPConnection -LocalPort 1420` and kill it
- For the detailed plan and pass/fail criteria, see `docs/24_vv_plan.md` §5–6 (Japanese)
