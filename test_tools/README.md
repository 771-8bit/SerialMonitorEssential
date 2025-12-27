# SerialMonitorEssential Test Tools

テストおよび検証ツール

## セットアップ (uv使用)

### 1. uvのインストール

```bash
# Windows PowerShell
irm https://astral.sh/uv/install.ps1 | iex
```

### 2. 依存関係のインストール

```bash
# プロジェクトルートで
uv sync
```

---

## 統合テストツール (serial_test.py)

すべてのシリアルテストを統一したCLIで実行できます。

### 基本的な使い方

```bash
cd test_tools

# Picoストレステスト（12Mbps高速データ）
uv run python serial_test.py --source pico --port COM14 --mode stress --duration 60

# 仮想COMストレステスト
uv run python serial_test.py --source virtual --port COM15 --mode stress --duration 10

# 仮想COM低速テスト（1行/秒）
uv run python serial_test.py --source virtual --port COM15 --mode slow --duration 30

# 仮想COMプロッタテスト（10Hz CSV）
uv run python serial_test.py --source virtual --port COM15 --mode plotter --duration 10

# 受信モード（SendPanelテスト用）
uv run python serial_test.py --receive --port COM16 --verbose
```

### オプション一覧

| オプション | 説明 | デフォルト |
|-----------|------|-----------|
| `--source pico\|virtual` | データソース選択 | - |
| `--receive` | 受信モード（SendPanelテスト用） | - |
| `--port` | シリアルポート | 必須 |
| `--mode stress\|slow\|plotter` | テストモード | stress |
| `--duration` | テスト時間（秒） | 10 |
| `--baud` | ボーレート | 115200 |
| `--verify` | 自動検証を実行 | false |
| `--verbose` | 受信データをテキスト表示 | false |

### テストモード

| モード | 説明 | データパターン |
|--------|------|----------------|
| `stress` | 高速バイナリ | カウンタ値 0-255 の繰り返し |
| `slow` | 1行/秒 | `[NNNN] Hello from Virtual Port! Counter=N` |
| `plotter` | 10Hz CSV | `time,sin,cos,random` |

---

## その他のツール

### Picoポート識別

```bash
uv run python identify_pico_ports.py
```

### 受信データ検証

```bash
uv run python verify_received_data.py --result test_results/test_result.txt
```

### メモリ監視・分析

```bash
# メモリ監視
uv run python monitor_memory.py --duration 60

# メモリ分析
uv run python analyze_memory.py test_results/memory_log_*.csv
```

---

## 仮想COMポートのセットアップ

### Windows (com0com)

1. **ダウンロード**: [com0com Signed Driver](https://sourceforge.net/projects/com0com/files/com0com/3.0.0.0/)

2. **インストール**:
   ```powershell
   # 管理者権限で実行
   .\setup.exe
   ```

3. **仮想ポートペア作成**:
   ```powershell
   cd "C:\Program Files (x86)\com0com"
   .\setupc.exe install PortName=COM15 PortName=COM16
   ```

4. **確認**: デバイスマネージャー → ポート (COM & LPT) で確認

### Linux (socat)

```bash
# インストール
sudo apt-get install socat  # Ubuntu/Debian

# 仮想ポートペア作成
socat -d -d pty,raw,echo=0,link=/tmp/vcom0 pty,raw,echo=0,link=/tmp/vcom1 &
sudo chmod 666 /tmp/vcom0 /tmp/vcom1
```

---

## モジュール構成

```
test_tools/
├── serial_test.py           # 統合テストCLI
├── lib/
│   ├── data_generator.py    # テストデータ生成
│   ├── pico_controller.py   # Pico制御
│   ├── virtual_sender.py    # 仮想COM送信
│   └── serial_receiver.py   # データ受信
├── identify_pico_ports.py   # Picoポート識別
├── verify_received_data.py  # 受信データ検証
├── monitor_memory.py        # メモリ監視
├── analyze_memory.py        # メモリ分析
└── pico_serial_tx_test/     # Picoファームウェア
```


---

## 詳細な検証シナリオ

### 1. Raspberry Pi Pico セットアップ

#### ハードウェア要件
- **Raspberry Pi Pico** × 1台
- **USBケーブル** (Micro-B, データ転送対応)

#### ファームウェア書き込み
1. **Arduino IDE** をセットアップ（`Raspberry Pi Pico/RP2040` ボードマネージャをインストール）。
2. `test_tools/pico_serial_tx_test/pico_serial_tx_test.ino` を開く。
3. Picoを **BOOTSELボタン** を押しながら接続。
4. ボード `Raspberry Pi Pico` を選択し、アップロード。
5. 書き込み後、Picoは2つのCOMポート（データ用・制御用）として認識されます。

### 2. 高負荷耐久テスト (12Mbps Verification)

**目的:** 12Mbpsで1分間データを受信し、1バイトの欠落もないことを検証する。

1. **ポート識別:**
   ```bash
   uv run python identify_pico_ports.py
   ```
   データポート（SerialMonitor用）と制御ポート（Controller用）を確認。

2. **アプリ起動:**
   SerialMonitorEssentialでデータポートを開く（Baudrate: 12000000）。

3. **テスト実行:**
   ```bash
   uv run python pico_stress_test_controller.py --port <Control_Port> --duration 60
   ```

4. **自動検証:**
   スクリプトが自動的に受信データ（tempディレクトリ内の `data.bin`）を探し、送信バイト数とSHA256ハッシュを比較検証します。

### 3. メモリリークテスト

**目的:** 長時間（1時間〜）の受信でメモリ使用量が安定していることを確認する。

1. **監視開始:**
   ```bash
   uv run python monitor_memory.py --duration 60
   ```
2. **負荷テスト開始:**
   アプリで受信を開始し、Picoへ長時間送信指示を送る。
   ```bash
   uv run python pico_stress_test_controller.py --port <Control_Port> --duration 3600
   ```
3. **分析:**
   ```bash
   uv run python analyze_memory.py test_results/memory_log_*.csv
   ```
   グラフを確認し、右肩上がりになっていないことを確認。

### 4. 複数インスタンス起動テスト

**目的:** 複数起動時に一時ファイル（PIDフォルダ）が競合しないことを確認する。

1. アプリを3つ起動（開発モードではディレクトリコピーが必要な場合あり、ビルド済みバイナリならそのまま起動可）。
2. それぞれ異なるポート（またはOpen/Close繰り返し）で動作させる。
3. `%TEMP%\SerialMonitorEssential\` を確認し、3つのPIDフォルダが独立して存在することを確認。
4. 1つを閉じると、該当するPIDフォルダのみが削除されることを確認。

