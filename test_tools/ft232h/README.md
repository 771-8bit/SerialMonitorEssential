# FT232H loopback jig

Real-UART tests using a single FT232H breakout (FTDI `VID_0403 PID_6014`, max 12 Mbaud)
with **D0 (TX) jumpered to D1 (RX)**. Unlike com0com and USB-CDC (Pico), the baud rate
is real here, so this jig is used to measure line-rate reception (SYS-NF-101) and the
bridge index (MCP Server on) under load (DEBT-9).

Results and interpretation: [docs/24 §6.5](../../docs/24_vv_plan.md) "T2-5b" (Japanese).

## Wiring

| Pin | Connect to | Purpose |
|---|---|---|
| D0 (ADBUS0, TX) | D1 | Required: loopback |
| D2 (ADBUS2, RTS) | D3 (CTS) | Optional: hardware flow control loop (`--rtscts` / app "Hardware") |

## Scripts

| File | What it measures | App needed |
|---|---|---|
| `loopback_baseline.py` | Transport oracle: pyserial writes and reads the same port; SHA-256 of TX vs RX and throughput vs. the 8N1 theoretical rate. `--sweep` checks that the baud setting actually takes effect | No (the port must be free) |
| `app_loopback.py` | App path: streams deterministic text lines through the bridge `send`, reads the capture back with `read_range`, compares SHA-256, and samples `digest.meta.index_lag_bytes` every second | Yes (connected, MCP Server ON) |
| `run_app_loopback.ps1` | Runs the above end to end (launch → UIA: MCP Server ON, port, baud, Connect → test → close) | Auto-launched |
| `reconnect_check.py` | Auto Connect (SYS-F-807): sends a numbered line every 200 ms while **you unplug and replug the cable**, then checks that the app reconnected, reported `status.reconnecting`, never rewound `total_bytes`, and kept the lines from before the unplug | Yes (connected, MCP Server and Auto Connect ON) |

```powershell
python .\loopback_baseline.py --port COM5 --sweep
python .\loopback_baseline.py --port COM5 --baud 12000000 --seconds 60
python .\loopback_baseline.py --port COM5 --baud 12000000 --seconds 60 --rtscts   # needs D2-D3
.\run_app_loopback.ps1 -Port COM5 -Baud 12000000 -Seconds 60 -SendSize 4096
python .\reconnect_check.py --seconds 300   # unplug and replug the cable while it runs
```

## Caveats

- In the app test the sender is the app itself, so it is not a pure "external device"
  reception test. It does exercise the real full-duplex path, which turned out to matter:
  it found DEBT-10 (a send stalled reception; fixed by ADR-14 on 2026-11-04). The SYS-NF-101
  acceptance run is `.\run_app_loopback.ps1 -FlowControl Hardware -SendSize 65536 -Seconds 60`.
- `-LogPath <file>` captures the app's log. Every 5 s the worker prints
  `max_ms(read_busy, tx, other)`: the longest time reception was blocked by each part of
  the loop. Use it to tell an app stall from a transport loss.
- Without flow control, even the pyserial baseline loses data at ≥ 6 Mbaud over 60 s
  (2–3 KB bursts when the host stops polling for a few ms). A failing app run is only
  attributable to the app when its loss clearly exceeds the baseline.
- FTDI driver settings (Device Manager → Port Settings → Advanced: latency timer,
  USB transfer sizes) affect the baseline. Record them with each result.
