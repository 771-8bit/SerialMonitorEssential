# Raspberry Pi Pico シリアル送信テストファームウェア

SerialMonitorEssentialの高速シリアル通信テスト用ファームウェアです。

## 必要なもの

- **Raspberry Pi Pico** × 1
- **USBケーブル** (Micro-B, データ転送対応)
- **Arduino IDE** 2.0以降

## Arduino IDEのセットアップ

### 1. ボードマネージャーの設定

1. Arduino IDEを起動
2. `File → Preferences`
3. **Additional Board Manager URLs** に以下を追加:
   ```
   https://github.com/earlephilhower/arduino-pico/releases/download/global/package_rp2040_index.json
   ```
4. `Tools → Board → Boards Manager`
5. 「pico」で検索し、**Raspberry Pi Pico/RP2040** をインストール

### 2. 必要なライブラリ

以下のライブラリを `Tools → Manage Libraries` からインストール:

- **Adafruit TinyUSB Library** (by Adafruit)
- **Crypto** (by Arduino) ※SHA256計算用
  - 公式ドキュメント: https://docs.arduino.cc/libraries/crypto/

## ファームウェアの書き込み

### 手順

1. **Picoをブートローダーモードで接続**
   - PicoのBOOTSELボタンを押しながらUSBケーブルを接続
   - PCがPicoをUSBストレージ（RPI-RP2）として認識

2. **Arduino IDEで設定**
   - `Tools → Board → Raspberry Pi Pico → Raspberry Pi Pico`
   - `Tools → Upload Method → Default (UF2)`
   - `Tools → USB Stack → Adafruit TinyUSB`
   - `Tools → Port → <Picoのポート>`

3. **スケッチを開く**
   - `File → Open` で `pico_serial_tx_test.ino` を開く

4. **アップロード**
   - **→（Upload）** ボタンをクリック
   - コンパイル・アップロード完了を待つ

5. **動作確認**
   - アップロード完了後、Picoのオンボードled LEDが点滅開始
   - シリアルモニタを開き（`Tools → Serial Monitor`）
   - ボーレート: **12000000** に設定
   - 起動メッセージが表示されるはずです

## 使用方法

### 自動連続送信モード

ファームウェア起動後、自動的にデータ送信を開始します。

### コマンド制御モード

シリアルコマンドでテストを制御できます：

```
START:<duration>    - 高速テスト (12Mbps) 開始（例: START:60）
SLOW:<duration>     - 低速テスト (1行/秒) 開始（例: SLOW:30）
PLOTTER:<duration>  - プロッタテスト (CSV 10Hz) 開始（例: PLOTTER:30）
STOP                - テスト停止
STATUS              - 現在の状態を表示
IDENTIFY            - ポート識別情報を表示
```

### 高速テスト（Pythonコントローラー）

```bash
python ../pico_stress_test_controller.py --port COM3 --duration 60
```

### 低速テスト（1行/秒）

```bash
python ../pico_slow_test_controller.py --port COM3 --duration 30
```

### プロッタテスト（CSV 10Hz）

```bash
python ../pico_plotter_test_controller.py --port COM3 --duration 30
```

**プロッタテストのデータ形式:**
- 10Hz (100msごと) でCSVデータを送信
- フォーマット: `timestamp,sin_wave,cos_wave,random_walk`
- シリアルプロッタ機能の動作確認用

## 仕様

- **ボーレート**: 12,000,000 bps (12Mbps)
- **チャンクサイズ**: 1024バイト
- **データ形式**: カウンタベース（検証可能）
- **チェックサム**: SHA256

## データ形式

送信データは以下のパターンで生成されます：

```
[0, 1, 2, ..., 255, 0, 1, 2, ..., 255, ...]
```

SerialMonitorEssentialで受信したデータと、Picoが計算したSHA256チェックサムを比較することで、データの完全性を検証できます。

## トラブルシューティング

### アップロードエラー

- PicoがBOOTSELモードに入っているか確認
- USBケーブルがデータ転送対応か確認

### シリアル通信エラー

- ボーレートが12000000に設定されているか確認
- USBドライバが12Mbpsに対応しているか確認（一部のドライバは非対応）

### ライブラリエラー

- `Adafruit TinyUSB Library` と `Crypto` が正しくインストールされているか確認

## ライセンス

MIT License
