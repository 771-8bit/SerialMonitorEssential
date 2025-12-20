# SerialMonitorEssential Tools

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

### 3. スクリプトの実行

```bash
# uv環境で実行
cd tools
uv run python identify_pico_ports.py
uv run python pico_stress_test_controller.py --port COM14 --duration 60
uv run python verify_received_data.py
```

## ツール一覧

### 高負荷テスト（Raspberry Pi Pico使用）
- **identify_pico_ports.py**: Picoの制御/データポートを自動識別
- **pico_stress_test_controller.py**: Picoにテスト開始コマンドを送信 + **自動検証**
- **verify_received_data.py**: 受信データの完全性を検証（単独実行用）

### メモリリークテスト
- **monitor_memory.py**: メモリ使用量の監視（Python）
- **analyze_memory.py**: メモリログの分析・グラフ化

### Picoファームウェア
- **pico_serial_tx_test/**: Raspberry Pi Pico用テストファームウェア（Arduino）

## 依存パッケージ

- pyserial
- psutil
- pandas
- matplotlib

## テスト結果

テスト結果は `test_results/` フォルダに保存されます：
- `test_result.txt`: Picoからの送信結果（バイト数、チェックサム）
- `memory_log_*.csv`: メモリ監視ログ
- `memory_analysis.png`: メモリ使用量グラフ
- `memory_statistics.txt`: メモリ統計情報

※ `test_results/` フォルダは `.gitignore` に含まれています
