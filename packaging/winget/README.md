# winget マニフェスト（テンプレート）

このディレクトリのファイルは **winget-pkgs へ提出するマニフェストの雛形**である。
そのままでは提出できない。`<VERSION>` / `<SHA256>` / `<PRODUCT_CODE>` を
リリースごとに置換して（＝レンダリングして）使う。

> **提出の前提（オーナー規約）**
> winget-pkgs への提出は、**本リポジトリでのリリース公開 + リリース済みバイナリでの動作確認 + オーナーの明示的な許可**の後に限る。
> 自動提出は行わない。[.github/workflows/winget-publish.yml](../../.github/workflows/winget-publish.yml) は
> `workflow_dispatch` 専用であり、確認フレーズの入力なしには実行できない。

## ファイル

winget-pkgs は**新規パッケージに singleton マニフェストを許可しない**。
version / installer / defaultLocale の 3 点セットが必須である。

| ファイル | ManifestType | 内容 |
|----------|--------------|------|
| `771-8bit.serial-monitor-essential.yaml` | `version` | パッケージ ID とバージョン、既定ロケールの宣言のみ |
| `771-8bit.serial-monitor-essential.installer.yaml` | `installer` | インストーラ種別（nullsoft / user スコープ）、URL、SHA-256、更新照合キー |
| `771-8bit.serial-monitor-essential.locale.en-US.yaml` | `defaultLocale` | 表示名、説明、ライセンス、各種 URL |

3 ファイルの `PackageIdentifier` と `PackageVersion` は**必ず一致させる**。ずれると検証で落ちる。
スキーマは 1.6.0。提出先の winget-pkgs 側で要求バージョンが上がっていたら、
3 ファイルの `ManifestVersion` を揃えて上げる。

winget-pkgs 上の配置先は `manifests/7/771-8bit/serial-monitor-essential/<VERSION>/` になる
（`PackageIdentifier` からパスが決まる）。

### 置換する値

| プレースホルダ | 取得方法 |
|----------------|----------|
| `<VERSION>` | `src-tauri/tauri.conf.json` の `version`（バージョンの単一情報源。docs/25 §1.2）。タグは `v<VERSION>` |
| `<SHA256>` | Release から `.exe` を取得し `Get-FileHash -Algorithm SHA256 <file>`。**ビルド直後のローカル成果物ではなく、公開済み Release アセットから算出する**（アップロードで壊れていないことの確認を兼ねる） |
| `<PRODUCT_CODE>` | 下記「ProductCode の確認」 |

### ProductCode の確認

Tauri の NSIS インストーラはアンインストール情報を **HKCU**（ユーザー単位）に書く。
winget はここを見て「導入済みか / 更新が必要か」を判定するため、実測値を入れる必要がある。
**初回リリースのバイナリを実際にインストールしてから**、次で実キー名を確認する。

```powershell
Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall' |
  ForEach-Object { $p = Get-ItemProperty $_.PSPath
    if ($p.DisplayName -eq 'serial-monitor-essential') { $_.PSChildName } }
```

出力されたキー名を `AppsAndFeaturesEntries[].ProductCode` に入れる。
**推測で埋めない。** 誤った ProductCode は「更新があるのに検出されない」形で静かに壊れる。

### なぜ NSIS だけか

Release には NSIS（`.exe`, per-user）と MSI（`.msi`, per-machine）の両方を添付するが、
winget に登録するのは NSIS のみ。理由は次の 2 点。

- winget の既定インストールは非管理者で走る。per-user の NSIS なら昇格を要求しない。
- 同一パッケージに per-user と per-machine を混在させると、更新時にどちらを見て判定するかが
  ProductCode 単位で分かれ、二重インストールの原因になる。

MSI は企業環境での配布ツール向けに Release へ残す（docs/25 §5.1）。

## 公開手順

順序を守る。**1〜3 を飛ばして 4 を実行しない。**

1. **リリースを publish する** — docs/25 §4 のチェックリストを完了し、draft Release を publish する。
   draft のままだとアセット URL が外部から取得できず、winget の検証パイプラインが落ちる。
2. **公開済みバイナリで動作確認する** — Release からインストーラを落とし、
   インストール → 起動 → COM 接続 → プロッタ描画 → 終了（exit 0）を確認する（docs/25 §4 手順 8）。
3. **オーナーの明示的な許可を得る**。
4. **workflow_dispatch で `Winget Publish (manual, approval-gated)` を実行する** —
   `version` に `0.1.0` 形式、`confirm` に `I-HAVE-OWNER-APPROVAL` を入力する。

### 初回提出は手動 PR でよい

`wingetcreate update` は**既存パッケージの更新用**であり、初回には使えない。
初回は次のいずれかを選ぶ。手動 PR が最も確実で、レビュー指摘にも対応しやすい。

- **手動 PR（推奨）**: このディレクトリの 3 ファイルをレンダリングし、
  [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) のフォークの
  `manifests/7/771-8bit/serial-monitor-essential/<VERSION>/` に置いて PR を出す。
  提出前にローカル検証する: `winget validate --manifest <dir>` と
  `winget install --manifest <dir>`（後者は実際に入るので確認後アンインストールする）。
- **`wingetcreate new`**: 対話的に生成する。生成物と本テンプレートの差分を確認してから提出する。
- **`komac new`**: 同等。komac / wingetcreate のどちらでもよい。ワークフローは wingetcreate を使う。

2 回目以降は `wingetcreate update`（＝ワークフローの経路）が使える。

### レビューについて

- 初回は winget-pkgs 側のレビューに**数日**かかる。自動検証（マニフェストのスキーマ、
  URL の到達性、SHA-256 の一致、インストール試験、マルウェアスキャン）を通過したのち人手のレビューが入る。
- **コード署名がない**ため、SmartScreen / SmartScreen 由来の警告が検証で指摘される可能性がある
  （docs/25 §6 R-1 / TBD-RS4）。
- `PackageIdentifier` の表記（`771-8bit` は数字始まり）について、レビューで
  publisher 表記の変更を求められる可能性がある。求められたら 3 ファイルすべてで揃えて変更する。

## 必要なシークレット

| シークレット | 用途 | 権限 |
|--------------|------|------|
| `WINGET_TOKEN` | `wingetcreate` が microsoft/winget-pkgs をフォークし、ブランチを push し、PR を作るための GitHub PAT | classic PAT なら `public_repo`。fine-grained なら「すべてのパブリックリポジトリ」に対する Contents: Read and write + Pull requests: Read and write |

- リポジトリの Settings → Secrets and variables → Actions に `WINGET_TOKEN` として登録する。
- **`GITHUB_TOKEN` は使えない**。他リポジトリ（winget-pkgs）へのフォーク・PR 作成権限がないため。
- 未登録の場合、ワークフローは処理に入る前に明示的なメッセージで失敗する。
- トークンの管理（有効期限、失効時の再発行）は docs/25 §6 TBD-RS7 で追跡する。
