# Raspberry Pi Pico Serial TX Test Firmware (Embedded Rust)

Firmware for SerialMonitorEssential's high-speed serial communication tests (Tier 2 / T2-4, T2-5).
Implemented in **embedded Rust (`no_std` / rp2040-hal)** — the Arduino IDE is not required.

The control protocol is **fully compatible** with the old Arduino version (`legacy_arduino/`),
so the existing Python controllers (`../serial_test.py --source pico`,
`../identify_pico_ports.py`, `../lib/pico_controller.py`) work as-is.

## Layout

A composite USB device with two CDC-ACM interfaces (with IAD) on a single device:

| Port | Role |
|---|---|
| Data port | Test data transmission only. Never emits banners or anything else (pure byte stream only) |
| Control port | Command control. Responds to `IDENTIFY` with `PORT_TYPE: CONTROL` |

Which COM number maps to which port is OS-dependent, so use
`python ../identify_pico_ports.py` to tell them apart (only the control port responds
to IDENTIFY).

## Requirements

- Raspberry Pi Pico (the plain one. **On the Pico W only the LED blinking does not work,
  because its onboard LED is not on GPIO25** — the test itself still works)
- USB cable (Micro-B, with data lines)
- Rust toolchain (`rustup`; the target is installed automatically via `rust-toolchain.toml`)
- Flashing tool: `cargo install elf2uf2-rs`

## Build and flash

```powershell
cd test_tools/pico_serial_tx_test

# build only
cargo build --release

# flash: hold BOOTSEL while plugging in USB (with the RPI-RP2 drive visible)
cargo run --release        # elf2uf2-rs -d converts to UF2 and flashes directly
```

If you don't use `cargo run`, you can also create a UF2 with
`elf2uf2-rs target/thumbv6m-none-eabi/release/pico-serial-tx-test out.uf2`
and copy it to the RPI-RP2 drive.

## Usage

After flashing, two COM ports appear on the PC. Send commands to the control port:

```
START:<duration>    - High-speed test (counter pattern at full speed) e.g. START:60
SLOW:<duration>     - Slow test (1 line/sec) e.g. SLOW:30
PLOTTER:<duration>  - Plotter test (CSV 10Hz: time,sin,cos,random) e.g. PLOTTER:30
STOP                - Stop the test (prints results)
STATUS              - Current status
IDENTIFY            - Port identification info
DIAG                - Report data-port write/flush diagnostics on the control port
BOOTSEL             - Reboot into UF2 flashing mode (no button press needed for reflash)
```

`BOOTSEL` lets you reflash without touching the board: send `BOOTSEL` to the control
port, then copy the new UF2 to the RPI-RP2 drive (or `cargo run --release`).

When the test ends, the result is printed on the control port:

```
TEST_STOP
Total bytes: <N>
Checksum: <SHA-256, uppercase hex>
```

### One-shot run (recommended)

```powershell
# 1. Identify the ports
python ..\identify_pico_ports.py

# 2. Connect the app to the data port, then run against the control port
python ..\serial_test.py --source pico --port <control port> --mode stress --duration 60

# 3. Disconnect the app (flushes the last in-memory chunk to data.bin), then verify.
#    serial_test.py auto-verifies immediately, but the app keeps the final <64KB chunk
#    in memory, so run verify AFTER disconnecting for an exact byte + SHA-256 match:
python ..\verify_received_data.py --result test_results\test_result.txt
```

To validate the transport alone (no GUI), a pyserial receiver can compute the SHA-256
itself and compare against the Pico's reported checksum — see the integrity-test pattern
in docs/24 §6.5.

## Data format (high-speed test)

```
[0, 1, 2, ..., 255, 0, 1, 2, ...]   # counter pattern incrementing by one byte at a time
```

By verifying pattern continuity and the SHA-256 match on the receiving side,
you can confirm that not a single byte was lost.

## Differences from the Arduino version

- Building requires only `cargo` (no Arduino IDE / TinyUSB / Crypto libraries)
- The data port transmits continuously (no DTR gate). `dtr()` does update correctly on
  real hardware, but not gating on it is simpler and portable; the empty-count-with-no-host
  case is caught by `serial_test.py`'s zero-byte guard instead
- Random numbers use xorshift32 (for the plotter's random walk; no quality requirements)
- Adds `BOOTSEL` (button-free reflash) and `DIAG` (data-port write/flush report) commands
- The old Arduino version is kept in `legacy_arduino/` (the reference for the protocol spec)

## Real-hardware validation (2026-09-04)

Verified on a Raspberry Pi Pico: a 60 s run delivered 15,451,179 bytes with an exact
byte-count and SHA-256 match (pyserial oracle), and the app captured a 10 s run to
`data.bin` with an exact byte + SHA-256 match after disconnect. Effective throughput was
**~2.06 Mbps — the RP2040 USB Full-Speed CDC ceiling**, so this jig validates zero-loss
integrity but not a true 12 Mbps line rate (that needs a High-Speed USB-serial device).

Bugs found and fixed only on real hardware:
- **Enumeration failure** ("Configuration Descriptor Request Failed"): usb-device's default
  8-byte EP0 and 128-byte control buffer cannot carry the dual-CDC configuration descriptor
  (~141 bytes). Fixed with `.max_packet_size_0(64)` and the `control-buffer-256` feature.
- **Zero bytes sent**: the loop-top `now_us` was captured before command handling, so on the
  START iteration `now_us - started_us` underflowed (u64) and the test ended instantly. Fixed
  by re-reading the timer inside the test-execution block.
- **Nothing delivered**: `write()` only fills a buffer; `flush()` must be called to push it to
  the endpoint (the first transfer needs an explicit flush).

## Troubleshooting

- **Cannot flash**: hold BOOTSEL while plugging in (RPI-RP2 drive visible) and use a
  data-capable cable. Or send `BOOTSEL` to the control port to reboot into flashing mode.
- **Only one COM port shows up**: enumeration can lag right after driver installation.
  Check Device Manager for two "USB Serial Device" entries.
- **`data.bin` is a few KB short right after a run**: the app keeps the last <64KB chunk in
  memory. Disconnect the app to flush it, then re-verify.
- **Not reaching 12 Mbps**: USB Full Speed CDC tops out around ~2 Mbps effective on RP2040.
  The baud rate is nominal for CDC; actual throughput is set by USB, not the baud value.

## License

MIT License
