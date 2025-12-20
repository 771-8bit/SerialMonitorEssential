# SerialMonitorEssential

高速シリアル通信（12Mbps級）に対応したデスクトップアプリケーション。  
Rust + Win32 APIによる直接制御で、データの完全性を保証します。

## 特徴

- **高速通信対応**: 12Mbps以上の非標準ボーレートに対応
- **データ完全性**: Win32 API直接制御により、受信データを1バイトも取りこぼさない設計
- **モダンUI**: Tauri (React + TypeScript) による応答性の高いインターフェース
- **リアルタイム表示**: 受信データを16進数でリアルタイム表示

## 開発環境

- **Backend**: Rust (windows crate, Tauri)
- **Frontend**: React + TypeScript + Vite
- **対応OS**: Windows

## セットアップ

```bash
# 依存関係のインストール
npm install

# 開発モードで起動
npm run tauri dev

# ビルド
npm run tauri build
```

## 実装状況

- ✅ **Phase 1**: Win32 API基盤構築と基本通信機能
  - COMポート列挙
  - ポート開閉
  - データ受信・表示（Hex）
  - DTR/RTS制御
  
- 🚧 **Phase 2** (予定): 高速受信エンジン
  - Chunk-based メモリ管理
  - Object Pool
  - Worker Thread分離
  - Tempファイルページング

詳細は [`agents/plan.md`](agents/plan.md) を参照してください。

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
