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

## Phase 9: AI 連携 (AI Bridge / MCP) ✓ 完了

*   **目標:** 人間が GUI で監視したまま、AI エージェントに同じシリアルセッションを読み書きさせる。
*   **ステータス:** **完了 (2026-09-03)**
    *   AI Bridge（`127.0.0.1:57320` / NDJSON、既定 OFF） — `bridge_set` / `bridge_status`
    *   読み出し（`status` / `tail` / `read_range` / `subscribe`）と送信（`send`）、`ports`
    *   送信の GUI 可視化（`bridge-activity` → 設定画面の AI Bridge 行）
    *   MCP stdio アダプタ `mcp/`（7 ツール。別プロセス）

> [!NOTE]
> 要求は [21 §A.10](21_system_requirements.md)、設計判断は [22 ADR-12](22_architecture_description.md)、
> 使い方は [mcp/README.md](../mcp/README.md)。

---

## v0.2 の予定

| 項目 | 内容 | 参照 |
| --- | --- | --- |
| **検索 / フィルタ** | Phase 8。UI は実装済みでバックエンドの検索ロジックが未実装。正規表現か部分一致かは未確定 | SYS-F-309 / GAP-01 / TBD-N3 |
| **AI Bridge の拡張** | トークン設定 UI ほか（詳細 TBD）。protocol v1 の互換規則も併せて決める | TBD-R7 / TBD-RS10 |
| **プロッタのドッキング / フロート** | **スコープ内。実装予定だが時期未定**（オーナー決定 2026-09-03） | SYS-F-905 / GAP-06 / TBD-R6 |

---

## 関連ドキュメント

- [プロジェクト概要](01_overview.md)
- [システムアーキテクチャ](02_architecture.md)
- [テスト方針](05_testing.md)
- [プロッタ仕様](07_plotter_spec.md)
- [IPC API](04_api.md)
