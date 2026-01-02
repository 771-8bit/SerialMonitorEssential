# SerialMonitorEssential

高速シリアル通信（12Mbps級）に対応したデスクトップアプリケーション。  
Rustの `serialport` クレートによるクロスプラットフォーム対応で、Windows, Linux, macOS で動作します。

## 特徴

- **高速通信対応**: 12Mbps以上の非標準ボーレートに対応
- **データ完全性**: 受信データを1バイトも取りこぼさない設計（Chunk-based Memory Management）
- **モダンUI**: Tauri (React + TypeScript) による応答性の高いインターフェース
- **リアルタイム表示**: 受信データをHex/ASCIIでリアルタイム表示（仮想スクロール対応）
- **クロスプラットフォーム**: Windows / Linux / macOS 対応

## 開発環境

- **Backend**: Rust (Tauri, serialport)
- **Frontend**: React + TypeScript + Vite
- **対応OS**: Windows, Linux, macOS

## 前提条件

- **Node.js**: v22 以上
- **Rust**: 最新の Stable 版 (1.92.0 以上推奨)

## セットアップ

> [!NOTE]
> CIなどで `npm ci` が失敗する場合は、`package-lock.json` が `package.json` と同期していない可能性があります。
> 手元の環境で `npm install` を実行して `package-lock.json` を更新し、コミットしてください。


```bash
# 依存関係のインストール
npm install

# 開発モードで起動
npm run tauri dev

# デバッグログあり
RUST_LOG=debug npm run tauri dev

# ビルド
npm run tauri build
```

## コード品質チェック

### Frontend (TypeScript)

```bash
# 型チェック
npm run type-check

# Lint
npm run lint

# フォーマットチェック
npm run format:check

# フォーマット適用
npm run format
```

### Backend (Rust)

```bash
cd src-tauri

# テスト
cargo test --lib

# Lint (Clippy)
cargo clippy --lib -- -D warnings

# フォーマットチェック
cargo fmt -- --check

# フォーマット適用
cargo fmt

# カバレッジ（要 cargo-llvm-cov）
cargo install cargo-llvm-cov  # 初回のみ
cargo llvm-cov --lib
cargo llvm-cov --lib --html --open  # HTMLレポート
```

## 実装状況

- ✅ **Phase 1**: 基盤構築と基本通信機能
- ✅ **Phase 2**: 高速受信エンジン (Chunk-based Memory Management)
- ✅ **Phase 3**: ビューアUIと仮想スクロール (Hex/ASCII)
- ✅ **Phase 4**: 設定機能・ログエクスポート
- ✅ **Phase 5**: 送信機能
- 🚧 **Phase 6**: UIリファイン・拡張機能 (現在進行中)
- 📅 **Phase 7**: シリアルプロッタ (予定)

詳細は [`agents/plan.md`](agents/plan.md) を参照してください。

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
