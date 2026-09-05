# Raspberry Pi Pico Dual CDC Port Test Guide

> Legacy document for the retired Arduino firmware. The current firmware is the embedded-Rust implementation in the parent directory.

## Overview

The Raspberry Pi Pico creates two CDC ports, allowing SerialMonitorEssential and a Python script to control it simultaneously.

- **Serial (CDC1)**: 12 Mbps, data transmission only → SerialMonitorEssential connects here
- **SerialControl (CDC2)**: 115200 bps, command control only → the Python script connects here

## Setup

### 1. Upload the firmware

Upload `test_tools/pico_serial_tx_test/pico_serial_tx_test.ino` to the Pico

### 2. Check the ports

When the Pico is connected, **two COM ports** are recognized.

#### Automatic identification (recommended)

```bash
python test_tools/identify_pico_ports.py
```

This script automatically determines and displays:
- which port is the control port
- which port is the data port

#### Manual check

Open each port in a serial terminal (Arduino Serial Monitor, etc.) and check the startup message:

**Control port:**
```
PORT_TYPE: CONTROL
BAUD_RATE: 115200
PURPOSE: Command control from Python script
---
=== SerialMonitorEssential Pico Test (Dual CDC) ===
Control Port (this port): 115200bps
Data Port (Serial): 12Mbps

Commands:
  START:<duration>  - Start test for <duration> seconds
  STOP              - Stop test
  STATUS            - Show current status
  IDENTIFY          - Show port identification info

Ready for commands.
```

**Data port:**
```
PORT_TYPE: DATA
BAUD_RATE: 12000000
PURPOSE: Data transmission to SerialMonitorEssential
---
```

## Running a test

### Steps

1. **Start SerialMonitorEssential**
   ```bash
   npm run tauri dev
   ```

2. **Connect to the data port**
   - In SerialMonitorEssential, select the **data port** (e.g. COM13)
   - Baud rate: **12000000**
   - Click "Open"

3. **Start the test with the Python controller**
   ```bash
   python test_tools/pico_stress_test_controller.py --port COM14 --duration 60
   ```
   **Note:** pass the **control port** to `--port`

4. **Test runs**
   - The Pico transmits data for 60 seconds
   - SerialMonitorEssential receives the data
   - The Python script receives the test results (byte count, checksum)

5. **Click "Close" in SerialMonitor**

6. **Verify the received data**
   ```powershell
   # Find SerialMonitorEssential's PID
   $processes = Get-Process | Where-Object {$_.ProcessName -like "*tauri*"}
   $pid = $processes[0].Id

   # Received-data file
   $file = "$env:TEMP\SerialMonitorEssential\$pid\data.bin"

   # Byte count
   (Get-Item $file).Length

   # Checksum
   Get-FileHash -Algorithm SHA256 $file
   ```

7. **Compare the results**
   Compare against the values in `test_result.txt`

## Commands

Commands accepted on the control port:

- `START:<seconds>` - Start the test (e.g. START:60)
- `STOP` - Stop the test
- `STATUS` - Show the current status
- `IDENTIFY` - Show the port identification info again

## Troubleshooting

### Two COM ports are not recognized

- Check that the Adafruit TinyUSB library is installed correctly
- Check that the firmware was uploaded correctly
- Reconnect the Pico

### Cannot tell which port is which

Run:
```bash
python test_tools/identify_pico_ports.py
```

### "PermissionError" from the Python script

- Check that SerialMonitorEssential is connected to the **data port**
- The Python script must connect to the **control port**
- Check that the two ports are different COM port numbers

## How it works

```
┌─────────────────┐
│ Raspberry Pi    │
│     Pico        │
├─────────────────┤
│                 │
│  Serial (CDC1)  │──12Mbps──→ SerialMonitorEssential
│   data TX       │             (e.g. COM13)
│                 │
│ SerialControl   │──115200bps→ Python Controller
│ (CDC2) control  │             (e.g. COM14)
│                 │
└─────────────────┘
```

With this layout, the Python script can send control commands even while SerialMonitorEssential holds the data port open.
