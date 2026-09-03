# Raspberry Pi Pico デュアルCDCポート テストガイド

## 概要

Raspberry Pi Picoは2つのCDCポートを作成し、SerialMonitorEssentialとPythonスクリプトによる同時制御を可能にします。

- **Serial (CDC1)**: 12Mbps データ送信専用 → SerialMonitorEssentialが接続
- **SerialControl (CDC2)**: 115200bps コマンド制御専用 → Pythonスクリプトが接続

## セットアップ

### 1. ファームウェアのアップロード

`test_tools/pico_serial_tx_test/pico_serial_tx_test.ino` を Pico にアップロード

### 2.ポートの確認

Picoを接続すると、**2つのCOMポート**が認識されます。

#### 自動識別（推奨）

```bash
python test_tools/identify_pico_ports.py
```

このスクリプトが自動的に：
- どちらが制御ポートか
- どちらがデータポートか
を判定し、表示します。

#### 手動確認

各ポートをシリアルターミナル（Arduino Serial Monitor等）で開き、起動メッセージを確認：

**制御ポート:**
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

**データポート:**
```
PORT_TYPE: DATA
BAUD_RATE: 12000000
PURPOSE: Data transmission to SerialMonitorEssential
---
```

## テスト実行

### 手順

1. **SerialMonitorEssentialを起動**
   ```bash
   npm run tauri dev
   ```

2. **データポートに接続**
   - SerialMonitorEssentialで **データポート**（例: COM13）を選択
   - ボーレート: **12000000**
   - 「Open」をクリック

3. **Pythonコントローラーでテスト開始**
   ```bash
   python test_tools/pico_stress_test_controller.py --port COM14 --duration 60
   ```
   **注意:** `--port` には **制御ポート** を指定

4. **テスト実行**
   - Picoが60秒間データ送信
   - SerialMonitorEssentialでデータ受信
   - Pythonスクリプトがテスト結果（バイト数、チェックサム）を受信

5. **SerialMonitorで「Close」**

6. **受信データ検証**
   ```powershell
   # SerialMonitorEssentialのPIDを確認
   $processes = Get-Process | Where-Object {$_.ProcessName -like "*tauri*"}
   $pid = $processes[0].Id

   # 受信ファイル
   $file = "$env:TEMP\SerialMonitorEssential\$pid\data.bin"

   # バイト数確認
   (Get-Item $file).Length

   # チェックサム確認
   Get-FileHash -Algorithm SHA256 $file
   ```

7. **結果比較**
   `test_result.txt` の値と比較

## コマンド

制御ポートから送信可能なコマンド：

- `START:<秒数>` - テスト開始（例: START:60）
- `STOP` - テスト停止
- `STATUS` - 現在の状態を表示
- `IDENTIFY` - ポート識別情報を再表示

## トラブルシューティング

### 2つのCOMポートが認識されない

- Adafruit TinyUSBライブラリが正しくインストールされているか確認
- ファームウェアが正しくアップロードされているか確認
- Picoを再接続

### どちらがどのポートかわからない

```bash
python test_tools/identify_pico_ports.py
```
を実行してください。

### Pythonスクリプトで "PermissionError"

- SerialMonitorEssentialが**データポート**に接続していることを確認
- Pythonスクリプトは**制御ポート**に接続する必要があります
- 両ポートが異なるCOMポート番号であることを確認

## 仕組み

```
┌─────────────────┐
│ Raspberry Pi    │
│     Pico        │
├─────────────────┤
│                 │
│  Serial (CDC1)  │──12Mbps──→ SerialMonitorEssential
│    データ送信    │             (COM13など)
│                 │
│ SerialControl   │──115200bps→ Python Controller
│  (CDC2) 制御    │             (COM14など)
│                 │
└─────────────────┘
```

この構成により、SerialMonitorEssentialがデータポートを占有していても、Pythonスクリプトから制御コマンドを送信できます。
