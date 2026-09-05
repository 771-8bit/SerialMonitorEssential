# SerialMonitorEssential Test Tools

Testing and verification tools.

## Setup (using uv)

### 1. Install uv

```bash
# Windows PowerShell
irm https://astral.sh/uv/install.ps1 | iex
```

### 2. Install dependencies

```bash
# in the project root
uv sync
```

---

## Integrated test tool (serial_test.py)

Runs all serial tests through a single CLI.

### Basic usage

```bash
cd test_tools

# Pico stress test (12 Mbps high-speed data)
uv run python serial_test.py --source pico --port COM14 --mode stress --duration 60

# Virtual COM stress test
uv run python serial_test.py --source virtual --port COM15 --mode stress --duration 10

# Virtual COM slow test (1 line/s)
uv run python serial_test.py --source virtual --port COM15 --mode slow --duration 30

# Virtual COM plotter test (10 Hz CSV)
uv run python serial_test.py --source virtual --port COM15 --mode plotter --duration 10

# Receive mode (for SendPanel testing)
uv run python serial_test.py --receive --port COM16 --verbose
```

### Options

| Option | Description | Default |
|-----------|------|-----------|
| `--source pico\|virtual` | Data source | - |
| `--receive` | Receive mode (for SendPanel testing) | - |
| `--port` | Serial port | required |
| `--mode stress\|slow\|plotter` | Test mode | stress |
| `--duration` | Test duration (seconds) | 10 |
| `--baud` | Baud rate | 115200 |
| `--verify` | Run automatic verification | false |
| `--verbose` | Print received data as text | false |

### Test modes

| Mode | Description | Data pattern |
|--------|------|----------------|
| `stress` | High-speed binary | Repeating counter values 0-255 |
| `slow` | 1 line/s | `[NNNN] Hello from Virtual Port! Counter=N` |
| `plotter` | 10 Hz CSV | `time,sin,cos,random` |

---

## Other tools

### Pico port identification

```bash
uv run python identify_pico_ports.py
```

### Received-data verification

```bash
uv run python verify_received_data.py --result test_results/test_result.txt
```

### Memory monitoring and analysis

```bash
# memory monitoring
uv run python monitor_memory.py --duration 3600

# memory analysis
uv run python analyze_memory.py test_results/memory_log_*.csv
```

---

## Setting up virtual COM ports

### Windows (com0com)

1. **Download**: [com0com Signed Driver](https://sourceforge.net/projects/com0com/files/com0com/3.0.0.0/)

2. **Install**:
   ```powershell
   # run as administrator
   .\setup.exe
   ```

3. **Create a virtual port pair**:
   ```powershell
   cd "C:\Program Files (x86)\com0com"
   .\setupc.exe install PortName=COM15 PortName=COM16
   ```

4. **Verify**: check Device Manager → Ports (COM & LPT)

### Linux (socat)

```bash
# install
sudo apt-get install socat  # Ubuntu/Debian

# create a virtual port pair
socat -d -d pty,raw,echo=0,link=/tmp/vcom0 pty,raw,echo=0,link=/tmp/vcom1 &
sudo chmod 666 /tmp/vcom0 /tmp/vcom1
```

---

## Module layout

```
test_tools/
├── serial_test.py           # integrated test CLI
├── lib/
│   ├── data_generator.py    # test data generation
│   ├── pico_controller.py   # Pico control
│   ├── virtual_sender.py    # virtual COM sending
│   └── serial_receiver.py   # data reception
├── identify_pico_ports.py   # Pico port identification
├── verify_received_data.py  # received-data verification
├── monitor_memory.py        # memory monitoring
├── analyze_memory.py        # memory analysis
└── pico_serial_tx_test/     # Pico firmware
```


---

## Detailed verification scenarios

### 1. Raspberry Pi Pico setup

#### Hardware requirements
- **Raspberry Pi Pico** × 1
- **USB cable** (Micro-B, with data transfer support)

#### Flashing the firmware
1. Set up the **Arduino IDE** (install the `Raspberry Pi Pico/RP2040` board manager package).
2. Open `test_tools/pico_serial_tx_test/pico_serial_tx_test.ino`.
3. Connect the Pico while holding the **BOOTSEL button**.
4. Select the `Raspberry Pi Pico` board and upload.
5. After flashing, the Pico enumerates as two COM ports (one for data, one for control).

### 2. High-load endurance test (12Mbps Verification)

**Goal:** receive data at 12 Mbps for one minute and verify that not a single byte is lost.

1. **Identify the ports:**
   ```bash
   uv run python identify_pico_ports.py
   ```
   Note which is the data port (for SerialMonitor) and which is the control port (for the controller).

2. **Start the app:**
   Open the data port in SerialMonitorEssential (baud rate: 12000000).

3. **Run the test:**
   ```bash
   uv run python serial_test.py --source pico --port <Control_Port> --mode stress --duration 60
   ```

4. **Automatic verification:**
   The script automatically locates the received data (`data.bin` in the temp directory) and
   compares the byte count and SHA256 hash against what was sent.

### 3. Memory leak test

**Goal:** confirm that memory usage stays stable during long (1 hour or more) reception.

1. **Start monitoring:**
   ```bash
   uv run python monitor_memory.py --duration 3600
   ```
2. **Start the load test:**
   Start receiving in the app, then send the Pico a long-duration transmit command.
   ```bash
   uv run python serial_test.py --source pico --port <Control_Port> --mode stress --duration 3600
   ```
3. **Analyze:**
   ```bash
   uv run python analyze_memory.py test_results/memory_log_*.csv
   ```
   Check the graph and confirm memory usage is not trending upward.

### 4. Multiple-instance launch test

**Goal:** confirm that temporary files (PID folders) do not conflict when multiple instances run.

1. Launch three instances of the app (in dev mode this may require copying the directory; built binaries can be launched as-is).
2. Run each on a different port (or with repeated Open/Close).
3. Check `%TEMP%\SerialMonitorEssential\` and confirm that three PID folders exist independently.
4. Close one instance and confirm that only its PID folder is deleted.
