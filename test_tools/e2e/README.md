# E2E 自動操作ハーネス

com0com 仮想 COM ペア(COM15⇔COM16)と Windows UI Automation を使った
実機 E2E テストのスクリプト群。

## 前提

- com0com がインストール済みで COM15⇔COM16 ペアが存在すること
- アプリが起動していること(`npm run tauri dev -- --no-watch` 推奨。
  `--no-watch` にしないと、テスト実行中のファイル変更でアプリが再起動する)
- データ送信は `python serial_test.py --source virtual --port COM15 --mode plot:label`

## スクリプト

| ファイル | 役割 |
|---|---|
| `ui.ps1` | UIA 操作の共通アクション。`-Action list / select-port / select-combo / click / toggle / click-text / wheel / close-window / shot` |
| `pairwise_gen.py` | ペアワイズ(t=2)被覆配列の生成器(グリーディ法、決定的)。8因子・全112ペアを7行に圧縮。因子を変えたら再生成して `pairwise_run.ps1` の `$rows` を更新する |
| `pairwise_run.ps1` | 被覆配列の各行を UIA で適用し、ヘルスオラクル(プロセス生存・ログ無パニック・ウィンドウ状態・フッターステータス)を検査。`-LogPath` にアプリの標準出力ログを渡す |

## 使い方の例

```powershell
# 個別操作
.\ui.ps1 -Action select-combo -Path "COM|CNC" -Name "COM16"
.\ui.ps1 -Action click -Name "Connect"
.\ui.ps1 -Action shot -WindowTitle "Serial Plotter" -Path out.png

# ペアワイズ一括実行
.\pairwise_run.ps1 -LogPath <アプリのstdoutログファイル>
```

## 注意

- スクリーンショット(`shot`)は対象ウィンドウを前面化する
- `select-combo` はドロップダウン展開のためウィンドウをアクティブにする
- 詳細な計画・合否基準は `docs/24_vv_plan.md` §5–6 を参照
