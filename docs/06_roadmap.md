# 実装ロードマップ

各フェーズで「動作確認（Verification）」を徹底し、手戻りを防ぎながら進める。

---

## Phase 1: 疎通確認とシリアルポート基盤構築 (Tracer Bullet) ✓ 完了

*   **目標:** `serialport` クレートを使用してCOMポートを開き、基本的なReadができることを確認する。
*   **ステータス:** **完了 (Completed)**
    *   クロスプラットフォーム対応 (`serialport` crate)
    *   COMポート列挙 / 開閉
    *   基本設定 (Baudrate等)
    *   Rust Backend - React Frontend 連携

## Phase 2: 高速受信コアとメモリ管理 (The Engine) ✓ 完了

*   **目標:** 12Mbpsの連続受信においてもデータ欠落が発生しない「リングバッファ/チャンクシステム」を完成させる。
*   **ステータス:** **完了 (Completed)**
    *   DataStore / Chunk 構造体
    *   Worker Thread (受信)
    *   Logger Thread (ディスク退避)
    *   ObjectPool (SegQueue)
    *   自動クリーンアップメカニズム

## Phase 3: ビューアUIと仮想スクロール (The Viewer) ✓ 完了

*   **目標:** 受信した大量のデータを、React側で遅延なく表示する。
*   **ステータス:** **完了 (Completed)**
    *   `get_read_data`, `get_display_rows` API
    *   `data-update` イベント (60fps)
    *   Virtual Scrolling (スケーリング対応)
    *   HexViewer (Offset, Hex, ASCII)

## Phase 4: 基本機能の統合 (Integration) ✓ 完了

*   **目標:** 実用的なシリアルモニタとしての体裁を整える。
*   **ステータス:** **完了 (Completed)**
    *   ポート一覧の自動更新
    *   安全な切断処理
    *   バイナリログのエクスポート
    *   設定変更 UI

## Phase 5: 送信機能の実装 (Sending Capability) ✓ 完了

*   **目標:** テキストボックスからのデータ送信機能を実装する。
*   **ステータス:** **完了 (Completed)**
    *   `write` API
    *   Send Panel UI
    *   送信履歴 / Enter送信オプション
    *   Loopback テスト済み

---

## Phase 6: フロントエンド刷新とUI調整 (UI Overhaul) ✓ 完了

*   **目標:** 全体的なUIを見直し、提供されたデザインモックアップに合わせてリファインする。

### 実装状況

*   [x] **全体レイアウト** (Settings / Send / Receive)
*   [x] **Hex / ASCII モード切替**
*   [x] **Timestamp 表示** (ASCII モード)
*   [x] **Line Wrap** (ASCII モード)
*   [x] **Show Ctrl** (制御文字可視化)
*   [x] **Auto Scroll**
*   [x] **Copy / Save / Clear 機能**
*   [x] **バイトベーススクロール** (Hex/ASCII 間で一貫したスクロール位置)

> [!NOTE]
> スクロール機能の詳細仕様は [03_data_structures.md](03_data_structures.md) を参照。

### 未実装機能

以下の機能は別フェーズで対応予定：

* **Search / Filter 機能** (UI実装済み、バックエンド検索ロジック未実装) - Phase 8 で対応
* **Plotter 連携** - Phase 7 で対応

---

## Phase 7: シリアルプロッタ (Serial Plotter) 📅 予定

詳細仕様は [07_plotter_spec.md](07_plotter_spec.md) を参照。

---

## 関連ドキュメント

- [プロジェクト概要](01_overview.md)
- [システムアーキテクチャ](02_architecture.md)
- [テスト方針](05_testing.md)
- [プロッタ仕様](07_plotter_spec.md)
