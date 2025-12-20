# SerialMonitorEssential Testing Guide (Raspberry Pi Pico版)

このドキュメントでは、Raspberry Pi Picoを使用したSerialMonitorEssentialのテスト手順を説明します。

## 目次

- [Rustバックエンドのテスト](#rustバックエンドのテスト)
- [Raspberry Pi Picoのセットアップ](#raspberry-pi-picoのセットアップ)
- [Phase 2 完了条件の検証](#phase-2-完了条件の検証)
  - [テストA: 高負荷耐久テスト（12Mbps）](#テストa-高負荷耐久テスト12mbps)
  - [テストB: メモリリーク確認](#テストb-メモリリーク確認)
  - [テストC: 複数インスタンステスト](#テストc-複数インスタンステスト)

---

## Rustバックエンドのテスト

### 単体テスト

```bash
cd src-tauri
cargo test --lib
```

すべての単体テストが実行されます。現在のテストカバレッジ:
- `chunk.rs`: 8テストケース
- `object_pool.rs`: 5テストケース

### Clippy（Linting）

```bash
cd src-tauri
cargo clippy --lib
```

### フォーマットチェック

```bash
cd src-tauri
cargo fmt -- --check
```

---

## Raspberry Pi Picoのセットアップ

### ハードウェア要件

- **Raspberry Pi Pico** × 1台
- **USBケーブル** (Micro-B, データ転送対応)
- **PCのUSBポート** × 2つ（Picoへの書き込み用とシリアル受信用）

### ファームウェアの書き込み

#### 1. Arduino IDEのセットアップ

1. Arduino IDE 2.0以降をインストール
2. Boards Managerで **「Raspberry Pi Pico/RP2040」** をインストール
   - File → Preferences → Additional Board Manager URLs に追加:
     ```
     https://github.com/earlephilhower/arduino-pico/releases/download/global/package_rp2040_index.json
     ```
   - Tools → Board → Boards Manager で「pico」で検索してインストール

#### 2. テストファームウェアの書き込み

`tools/pico_serial_tx_test/pico_serial_tx_test.ino` を開き、Picoに書き込みます。

**手順:**

1. PicoのBOOTSELボタンを押しながらUSBケーブルを接続
2. PCがPicoをUSBストレージとして認識
3. Arduino IDEで:
   - Tools → Board → Raspberry Pi Pico → **Raspberry Pi Pico**
   - Tools → Upload Method → **Default (UF2)**  
   - Tools → Port → Picoのポートを選択
4. Upload（→ボタン）をクリック

#### 3. 動作確認

書き込み完了後、PicoのLEDが点滅し、シリアルポートから自動的にデータが送信されます。

---

## Phase 2 完了条件の検証

Phase 2の完了には、以下の3つのテストをすべてパスする必要があります。

### テストA: 高負荷耐久テスト（12Mbps）

**目的:** 12Mbpsで1分間データを受信し、データの完全性を検証

**前提条件:**
- Raspberry Pi Pico（ファームウェア書き込み済み）
- uv環境セットアップ済み（`uv sync`）
- Picoが2つのCOMポートとして認識されている（デュアルCDC）

**手順:**

#### 1. Picoのポートを自動識別

```bash
cd tools
uv run python identify_pico_ports.py
```

**出力例:**
```
Control Port:  COM14
  → Use with: python pico_stress_test_controller.py --port COM14

Data Port:     COM13
  → Use with: SerialMonitorEssential (12Mbps)

✓ Both ports identified successfully!
```

Picoは2つのCOMポートを提供します：
- **データポート** (例: COM13): 12Mbps、SerialMonitorEssentialが接続
- **制御ポート** (例: COM14): 115200bps、Pythonスクリプトが接続

#### 2. SerialMonitorEssentialを起動

```bash
npm run tauri dev
```

- SerialMonitorEssentialで **データポート**（例: COM13）を選択
- ボーレート: **12000000**（12Mbps）
- 「Open」をクリック

SerialMonitorEssentialでデータ受信ログが表示されます：

```
[Worker] Read 1024 bytes (total: XXX)
[Logger] Wrote chunk #X, total bytes: XXX
```

#### 3. テスト開始と自動検証（別ターミナル）

```bash
cd tools
uv run python pico_stress_test_controller.py --port COM14 --duration 60
```

このスクリプトは：
1. **制御ポート**（COM14）に接続
2. Picoに `START:60` コマンドを送信（60秒間のテスト実行）
3. Picoからのテスト結果（送信バイト数、チェックサム）を受信
4. 結果を `../test_results/test_result.txt` に保存
5. **自動的に受信データを検証**（バイト数・チェックサム）

**出力例:**
```
✓ Test started on Pico
Waiting for 60 seconds...

✓ Test completed on Pico
  Total bytes sent: 32,617,472
  SHA256 checksum: CCCE45C4B6AF565C639A5A857FF16D3D...

✓ Results saved to: ../test_results/test_result.txt

============================================================
AUTOMATIC VERIFICATION
============================================================

Verifying received data automatically...

Found received data: C:\Users\...\Temp\SerialMonitorEssential\29648\data.bin
Expected bytes:  32,617,472
Received bytes:  32,617,472
✓ Byte count MATCHES!

Calculating SHA256 checksum...
Expected checksum: CCCE45C4B6AF565C639A5A857FF16D3D...
Actual checksum:   CCCE45C4B6AF565C639A5A857FF16D3D...
✓ Checksum MATCHES!

============================================================
✓✓✓ ALL CHECKS PASSED! ✓✓✓

The received data is CORRECT!
  Bytes received: 32,617,472
  Data integrity: 100%
============================================================
```

#### 4. SerialMonitorで受信完了を待つ

SerialMonitorEssentialのログで、データ受信が停止したことを確認したら「Close」をクリック。

**注意:** SerialMonitorEssentialを閉じる前に、上記のテストスクリプトが検証を完了するまで待ってください。

**合格基準:**
- ✅ 受信バイト数 = 送信バイト数（完全一致）
- ✅ SHA256チェックサム一致
- ✅ データ完全性: 100%
- ✅ アプリがクラッシュしない

---

### テストB: メモリリーク確認

**目的:** 長時間実行でメモリ使用量が安定することを確認

**前提条件:**
- uv環境セットアップ済み（`uv sync`）
- Raspberry Pi Pico（ファームウェア書き込み済み）

**手順:**

#### 1. メモリ監視を開始

```bash
cd tools
uv run python monitor_memory.py --duration 60
```

#### 2. SerialMonitorを起動してデータ受信

別のターミナルで:

```bash
# アプリ起動
npm run tauri dev
```

SerialMonitorEssentialで:
- データポート を選択
- ボーレート: 12000000
- 「Open」をクリック

#### 3. Picoに長時間テスト開始

別のターミナルで:

```bash
cd tools
# 1時間継続テスト
uv run python pico_stress_test_controller.py --port <制御ポート> --duration 3600
```

#### 4. 結果の分析

監視完了後、`test_results/memory_log_<timestamp>.csv` が生成されます。

```bash
cd tools
uv run python analyze_memory.py test_results/memory_log_<timestamp>.csv
```

以下が生成されます:
- `test_results/memory_analysis.png`: メモリ使用量のグラフ
- `test_results/memory_statistics.txt`: 統計情報

**合格基準:**
- ✅ メモリ使用量が30分後に安定（増加率 < 1MB/分）
- ✅ Working Setが500MB以下で推移
- ✅ グラフが右肩上がりにならない（鋸歯状は許容）
- ✅ 1時間後もアプリが正常動作

---

### テストC: 複数インスタンステスト

**目的:** 複数起動時の一時ディレクトリ管理が正しいことを確認

**手順:**

#### 1. SerialMonitorEssentialを3つ起動

```bash
# Terminal 1
npm run tauri dev

# Terminal 2（別のプロジェクトディレクトリをコピーして）
npm run tauri dev

# Terminal 3（さらに別のコピーで）
npm run tauri dev
```

**Note:** 同じプロジェクトでは同時に1つしか起動できないため、プロジェクトフォルダを一時的にコピーします。

```powershell
# プロジェクトをコピー
Copy-Item -Path "C:\Users\<USER>\Documents\SerialMonitorEssential" -Destination "C:\Users\<USER>\Documents\SerialMonitorEssential_copy1" -Recurse
Copy-Item -Path "C:\Users\<USER>\Documents\SerialMonitorEssential" -Destination "C:\Users\<USER>\Documents\SerialMonitorEssential_copy2" -Recurse
```

#### 2. それぞれ異なるCOMポートで接続

Picoは1つのCOMポートしか提供しないため、**ループバック（TX-RX短絡）**や**仮想COMポート**を使用します。

または、各インスタンスで「Open」→「Close」を繰り返して一時ディレクトリの生成を確認。

#### 3. 一時ディレクトリを確認

```powershell
# 一時ディレクトリ一覧
Get-ChildItem "$env:TEMP\SerialMonitorEssential"
```

3つのPIDフォルダが存在することを確認。

#### 4. 1つのインスタンスを終了

1つのSerialMonitorEssentialを閉じる。

```powershell
# 再度確認
Get-ChildItem "$env:TEMP\SerialMonitorEssential"
```

終了したインスタンスのPIDフォルダが削除されていることを確認。

#### 5. 新しいインスタンスを起動

```bash
npm run tauri dev
```

起動時のログで、古い（存在しないPIDの）フォルダが削除されたことを確認:

```
[cleanup] Removing stale directory for PID: 12345
```

**合格基準:**
- ✅ 各インスタンスが独立したPIDフォルダを使用
- ✅ 終了時に自分のPIDフォルダのみ削除
- ✅ 他のインスタンスのフォルダを削除しない
- ✅ 起動時に孤立したPIDフォルダを削除
- ✅ プロセス名確認により、SerialMonitorEssential以外のプロセスのフォルダは削除しない

---

## トラブルシューティング

### Picoが認識されない

- USBケーブルがデータ転送対応か確認（充電専用ケーブルは不可）
- デバイスマネージャーでCOMポートが表示されているか確認
- Arduino IDEで「Tools → Port」にPicoが表示されるか確認

### 12Mbpsで接続できない

- USBシリアルドライバによっては12Mbpsに対応していない場合があります
- Picoのファームウェアで設定したボーレートと SerialMonitorEssential の設定が一致しているか確認
- まずは 115200 bps などの標準的な速度でテストしてから、段階的に速度を上げる

### Pythonスクリプトが動作しない

```bash
# uv環境のセットアップ
cd <プロジェクトルート>
uv sync

# スクリプト実行
cd tools
uv run python identify_pico_ports.py
```

### メモリ分析スクリプトが動作しない

確認事項：
- uv環境がセットアップされているか（`uv sync`）
- Python 3.9以上がインストールされているか

### PowerShellスクリプトが実行できない

```powershell
# 実行ポリシーを一時的に変更
powershell -ExecutionPolicy Bypass -File tools/monitor_memory.ps1
```

---

## 継続的インテグレーション（CI）

GitHub Actionsでは、以下が自動実行されます:

- `cargo test --lib`: 単体テスト
- `cargo clippy --lib`: Linting
- `cargo fmt -- --check`: フォーマットチェック
- `npm run tauri build`: ビルド確認

Pull Request作成時とpush時に自動的に実行されます。
