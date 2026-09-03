# E2E 自動操作ハーネス

com0com 仮想 COM ペア(COM15⇔COM16)と Windows UI Automation を使った
実機 E2E テストのスクリプト群。**このディレクトリの手順だけで、実機なしに
全 E2E を新しいマシンで再現できる**ことを目標にする。

CI(GitHub Actions)で回せるのは GUI 不要の `mcp_stdio_smoke.py` のみ
(`.github/workflows/ci.yml` に組み込み済み)。それ以外が CI 不可能な理由
(com0com はカーネルモードドライバ / UIA は対話デスクトップ必須)は
[docs/25 §3.4](../../docs/25_release_strategy.md) を参照。

## セットアップ(初回のみ)

### 1. com0com のインストール

1. [com0com (SourceForge)](https://sourceforge.net/projects/com0com/) から
   インストーラを取得して実行する。
   - Windows 10/11 (x64, Secure Boot 有効)では**署名済みドライバ**の
     ビルド(v3.0.0.0 系)が必要。未署名版はテスト署名モードを要求される。
2. インストール直後は既定ペア `CNCA0⇔CNCB0` が 1 組できる(これはそのままでよい)。

### 2. COM15⇔COM16 ペアの作成(管理者権限)

```powershell
cd "C:\Program Files (x86)\com0com"
.\setupc.exe install PortName=COM15 PortName=COM16
```

確認(どちらでもよい):

```powershell
.\setupc.exe list
# または
Get-ItemProperty "HKLM:\HARDWARE\DEVICEMAP\SERIALCOMM"
#   \Device\com0com11 : COM15
#   \Device\com0com21 : COM16  が見えれば OK
```

別のポート番号にした場合は、各スクリプトの `-PortApp` / `--port` 引数で指定する。

### 3. Python 依存

```powershell
pip install pyserial
```

### 4. アプリのビルド

```powershell
cd src-tauri
cargo build --release   # E2E は release 推奨(DEV デバッグ表示が出ない)
```

### 仮想ペアの制約(重要)

- **ボーレート・DTR/RTS は仮想ペアでは実質 no-op**。設定 UI の反映確認には
  使えるが、信号レベルの検証にはならない。
- したがって **12 Mbps 等の性能受入には使えない**。性能系は実機
  (Raspberry Pi Pico)で行う([docs/25 §2](../../docs/25_release_strategy.md)
  「com0com のみでの合格は 1.0 の根拠にしない」)。

## スクリプト一覧

| ファイル | 役割 | GUI | com0com | CI |
|---|---|---|---|---|
| `ui.ps1` | UIA 操作の共通アクション。`-Action list / select-port / select-combo / click / toggle / click-text / wheel / close-window / shot` | 要 | - | 不可 |
| `pairwise_gen.py` / `pairwise_gen2.py` | ペアワイズ(t=2)被覆配列の生成器(グリーディ法、決定的)。因子を変えたら再生成して `pairwise_run*.ps1` の `$rows` を更新する | - | - | 可(生成のみ) |
| `pairwise_run.ps1` / `pairwise_run2.ps1` | 被覆配列の各行を UIA で適用し、ヘルスオラクル(プロセス生存・ログ無パニック・ウィンドウ状態)を検査 | 要 | 要 | 不可 |
| `mcp_stdio_smoke.py` | **内蔵 MCP アダプタ(`--mcp`)のスモーク**。実パイプ越しの JSON-RPC(initialize / ping / tools / エラー系 / クリーン終了)。ブリッジ未起動を隔離ポートで決定化 | 不要 | 不要 | **組み込み済み** |
| `pong_bot.py` | COM15 で `PING`→`PONG 42` を返す応答ボット | 不要 | 要 | 不可 |
| `mcp_bridge_live.py` | **AI Bridge のライブ往復検証**(status / send→wait_for / read_tail / ブロック中 ping 即応答 / cancelled 中断) | 要(別途起動) | 要 | 不可 |
| `run_bridge_e2e.ps1` | 上記 3 つを**ワンコマンドで一括実行**(起動→UIA 設定→往復検証→後片付け) | 自動起動 | 要 | 不可 |

## 使い方

```powershell
# --- CI と同じスモーク(GUI 不要) ---
python .\mcp_stdio_smoke.py

# --- AI Bridge E2E 一括(GAP-31 のスクリプト化) ---
.\run_bridge_e2e.ps1                 # release バイナリ自動検出、COM15/COM16

# --- 個別 UIA 操作 ---
.\ui.ps1 -Action select-combo -Path "COM|CNC" -Name "COM16"
.\ui.ps1 -Action click -Name "Connect"
.\ui.ps1 -Action shot -WindowTitle "Serial Plotter" -Path out.png

# --- ペアワイズ一括実行 ---
# アプリは `npm run tauri dev -- --no-watch` で起動しておく
# (--no-watch にしないとテスト中のファイル変更でアプリが再起動する)
.\pairwise_run.ps1 -LogPath <アプリのstdoutログファイル>

# --- プロッタ用データ送出 ---
python ..\serial_test.py --source virtual --port COM15 --mode plot:label
```

## 注意

- スクリーンショット(`shot`)は対象ウィンドウを前面化する
- `select-combo` はドロップダウン展開のためウィンドウをアクティブにする
- ウィンドウ出現直後は React のマウント前で UIA 操作が失敗することがある
  (`run_bridge_e2e.ps1` は数秒待ってから操作する)
- `npm run tauri dev` をスクリプトから kill すると vite が port 1420 を
  掴んだまま残ることがある: `Get-NetTCPConnection -LocalPort 1420` で
  PID を引いて kill する
- 詳細な計画・合否基準は `docs/24_vv_plan.md` §5–6 を参照
