# Serial Monitor Essential

高速シリアル通信（12 Mbps 級）に対応したシリアルモニタ + リアルタイムプロッタです。
さらに **AI 連携（MCP）** を内蔵: あなたが GUI で波形を眺めている横で、Claude などの
AI エージェントが**同じシリアルポート**を読み書きしてデバッグを手伝えます。
既存のツールにはそれぞれ良さがありますが、欲しい機能が揃ったものが見つからなかったので自作しています
（旧 C# 版から Tauri / Rust で作り直したものです）。

| | 再接続 | 送信機能 | データレート | プロッタ | AI 連携 |
| ---- | ---- | ---- | ---- | ---- | ---- |
| Arduino IDE | × | ○ | ○ | ○ | × |
| Tera Term | ○ | × | ○ | × | × |
| Serial Monitor (VS Code 拡張) | ○ | ○ | × | × | × |
| **Serial Monitor Essential** | **○** | **○** | **○（12 Mbps 級）** | **○（波形＋状態）** | **○（MCP）** |

![メインウィンドウ](docs/images/main-window.png)

![シリアルプロッタ](docs/images/plotter.png)

## 特徴

### モニタ
- **12 Mbps 級の高速受信**: 受信データを取りこぼさない設計（Chunk ベースのメモリ管理＋ディスク退避）
- **Hex / ASCII 表示**: 仮想スクロールで大量データも軽快。タイムスタンプ・折り返し・制御文字表示の切替え
- **送信**: テキスト / HEX、改行コード選択（None/CR/LF/CRLF）、↑↓キーで送信履歴
- **保存・コピー**: 受信データのファイル保存（バイナリ）とクリップボードコピー（Hex/ASCII）
- **DTR / RTS 制御、再接続に強い**: ポートを開き直しても状態が壊れない

### プロッタ
- **オシロのロールモード風ライブ表示**: 固定幅スライディングウィンドウ（1s〜300s）。表示が「バタバタ」しない
  絶対時刻整列ダウンサンプリング（LTTB / Average+min-max バンド切替え）
- **ステートタイムライン**: `motor:ON` のような離散状態を色付きバーで数値波形と同じ時間軸に表示
- **LIVE / Inspect / Paused**: ズームすると自動で検査モードへ。過去に遡って拡大でき、`▶ LIVE` で追従に復帰
- **データ形式**: Arduino 互換。CSV（`25.5,60,RUNNING`）/ ラベル付き（`temp:25.5,state:RUNNING`）/ ヘッダー行

### AI 連携（MCP）
- **AI が同じポートを読み書き**: COM ポートは排他だが、アプリがマルチプレクサになるので奪い合いが起きない
- **送信 → 応答待ちを 1 ツールで**: `serial_wait_for`（正規表現マッチ）でプロトコルの対話デバッグが AI に頼める
- **人間に常時可視**: AI が送信した内容は必ず GUI に表示される（見えない送信経路は作らない設計）
- **安全側デフォルト**: 127.0.0.1 のみ・既定 OFF（詳細は下の「AI 連携 (MCP)」節）

対応 OS: **Windows 10/11 (x64)**・**Ubuntu 22.04+ (x64)**（正式サポート。macOS は配布なし、詳細は
[docs/20_user_needs.md §8.1](docs/20_user_needs.md)）。

## インストール

### Windows

1. [Releases](https://github.com/771-8bit/SerialMonitorEssential/releases) から
   `serial-monitor-essential_<VERSION>_x64-setup.exe` をダウンロードして実行します
   （管理者権限は不要、ユーザー単位でインストールされます）。
2. 署名がないため SmartScreen の警告が出ることがあります。
   「詳細情報」→「実行」で続行してください。ダウンロードした資産の SHA-256 はリリースノートに記載します。

> **旧 C# 版（Serial Monitor Essential 0.0.9 以前）をお使いの場合**
> 本アプリは別アプリとして共存します（旧版は上書き・削除されません）。
> 混乱を避けるため、「アプリと機能」から旧版のアンインストールを推奨します。

winget 対応は準備中です（リリース後に `winget install 771-8bit.serial-monitor-essential` を予定）。

### Linux (Ubuntu 22.04+)

[Releases](https://github.com/771-8bit/SerialMonitorEssential/releases) から入手します。

```sh
# .deb（推奨。webkit2gtk-4.1 などの依存は apt が解決します）
sudo apt install ./serial-monitor-essential_<VERSION>_amd64.deb

# または AppImage
chmod +x serial-monitor-essential_<VERSION>_amd64.AppImage
./serial-monitor-essential_<VERSION>_amd64.AppImage
```

**シリアルポートの権限（必須）**: `/dev/ttyUSB*` / `/dev/ttyACM*` を開くには `dialout` グループへの参加が必要です。

```sh
sudo usermod -aG dialout $USER   # 実行後、再ログイン（または再起動）
```

既知の制約: 性能受入（12 Mbps 欠落ゼロ・メモリソーク）と E2E は Windows でのみ実施済みで、
Linux での実測値は TBD です。Wayland / HiDPI の表示は未検証です。

## 使い方（プロッタ）

1. ポートを選んで **Connect** → 受信データがモニタに流れます
2. **Plotter** ボタンでプロッタウィンドウを開きます
3. デバイスから次のいずれかの形式で送信すると自動でチャンネルが生えます

```text
25.5,60,RUNNING                        # CSV（ch0, ch1, … 自動命名）
temp:25.5,humidity:60,state:RUNNING    # ラベル付き
temp,humidity,state                    # 先頭にヘッダー行を送ると列名になります
```

数値はラインチャート、非数値はステートタイムラインに自動で振り分けられます。

## AI 連携 (MCP)

COM ポートは排他デバイスなので、アプリが開いている間は他のプロセスが同じポートを開けません。
そこで**アプリ自身をマルチプレクサ**にしました。あなたが GUI で波形とログを眺めている横で、
Claude Code などの AI エージェントが**同じシリアルセッション**を読み、同じポートへコマンドを送れます。

![AI Bridge](docs/images/ai-bridge.png)

有効化は 2 ステップです。まずアプリの設定パネルで **AI Bridge** を ON にします
（既定は OFF。ON にすると `127.0.0.1:57320` で待ち受けます）。次に MCP サーバーを登録します。

```bash
claude mcp add serial-monitor -- node "<リポジトリのパス>/mcp/server.mjs"
```

あとは普通に頼むだけです:

```text
あなた : ボードに AT+GMR を送って、ファームのバージョンを教えて
Claude : serial_send("AT+GMR") → serial_wait_for("OK|ERROR")
         → 応答 "AT version:2.1.0.0 ... OK"。バージョンは 2.1.0.0 です
あなた : 直近のログに CRC エラーっぽい行がないか見て
Claude : serial_read_tail(16384) → "CRC mismatch" が 3 行あります（offset 12034〜）…
```

送信のたびに GUI の AI Bridge 行へ「送信 N bytes・時刻・内容プレビュー」が出るので、
AI が何をしたかは常に手元で追えます。
提供ツールは `serial_status` / `serial_ports` / `serial_read_tail` / `serial_read_range` /
`serial_send` / `serial_send_hex` / `serial_wait_for` の 7 つ。
セットアップ手順・環境変数・ツールの詳細は [`mcp/README.md`](mcp/README.md) を参照してください。

> **セキュリティ**: 待ち受けは **`127.0.0.1` のみ**で外部ネットワークからは接続できません。
> **既定は OFF** で、設定画面で明示的に有効にしたときだけ動きます。
> **AI が送信した内容は必ず GUI に表示されます**（バイト数・時刻・内容のプレビュー）。
> 人間に見えない送信経路は作らない、という方針です（設計判断は
> [docs/22_architecture_description.md](docs/22_architecture_description.md) の ADR-12）。

## 開発者向け

前提: Node.js v22+ / Rust stable。

```bash
npm install
npm run tauri dev            # 開発起動（RUST_LOG=debug で詳細ログ）
npm run tauri build          # インストーラ生成

# 品質ゲート（CI と同じ）
npm run type-check && npm run lint && npm run format:check && npm test
cd src-tauri && cargo fmt -- --check && cargo clippy --all-targets -- -D warnings && cargo test
```

設計・テストのドキュメントは `docs/` にあります:
[要求](docs/21_system_requirements.md) /
[アーキテクチャ（状態機械・ADR）](docs/22_architecture_description.md) /
[トレーサビリティ](docs/23_traceability.md) /
[V&V 計画](docs/24_vv_plan.md) /
[リリース戦略](docs/25_release_strategy.md)。
E2E ハーネス（com0com + UI Automation）は [`test_tools/e2e/`](test_tools/e2e/README.md)。

推奨 IDE: [VS Code](https://code.visualstudio.com/) +
[Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) +
[rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)

## ライセンス

[MIT](LICENSE)
