# インターフェース設計 (IPC API)

## Commands (Frontend -> Backend)

| コマンド名 | 引数 | 説明 |
| --- | --- | --- |
| `open_port` | `port_name`, `config: SerialConfig` | 指定設定でポートを開く |
| `close_port` | なし | ポートを閉じる |
| `write_data` | `data: Vec<u8>` | データを送信する |
| `write_dtr` | `level: bool` | DTR信号を設定する |
| `write_rts` | `level: bool` | RTS信号を設定する |
| `get_read_data` | `offset: u64`, `length: u32` | 指定範囲のバイナリデータを取得 |
| `get_display_rows` | `start_row: u64`, `row_count: u32` | Hex表示用行データを取得 |
| `get_ascii_lines` | `start_line`, `line_count`, `show_ctrl`, `show_timestamp` | ASCII表示用行データを取得 |
| `list_ports` | なし | 利用可能なシリアルポート一覧を取得 |
| `export_log` | `path: String` | 受信データをファイルにエクスポート |
| `clear_data` | なし | 受信データをクリア |
| `get_clipboard_text` | `mode: String` | クリップボード用テキストを取得（hex/ascii） |
| `bridge_status` | なし | AI Bridge の状態を取得（`{ enabled, port, connections, last_activity }`）。失敗しない |
| `bridge_set` | `enabled: bool`, `port: Option<u16>` | AI Bridge の起動 / 停止。`enabled=true` で同じポートが稼働中なら no-op、ポートが変われば再起動。戻り値は `bridge_status` と同じ形 |
| `bridge_guide_info` | なし | AI 連携ガイド用の情報（`{ exe_path, bridge_enabled, bridge_port, app_version }`）。`exe_path` は実行中バイナリの絶対パスで、ガイドが MCP 登録コマンドに埋め込む（SYS-F-1108） |
| `open_ai_guide_window` | なし | AI 連携ガイドウィンドウ（label: `ai-guide`, `ai-guide.html`）を開く。既に開いていればフォーカスする |

## Events (Backend -> Frontend)

| イベント名 | ペイロード | 説明 |
| --- | --- | --- |
| `serial-status` | `{ connected: bool, error: string \| null }` | 接続状態の変化。受信スレッドが致命的 I/O エラーを検知したときに `connected=false` で発火する。フロントエンドはこれを受けて `close_port` を呼び、バックエンド状態を再同期する（SYS-F-107） |
| `data-update` | `{ total_bytes: u64 }` | 新規データ受信通知。フロントエンドは `get_display_rows` で表示データを取得。 |
| `log-error` | `{ message: string }` | ディスクフルなどのエラー通知。Logger スレッドの `on_error` コールバック経由で発火し、**同種のエラーは 5 秒に 1 回まで**に抑制される（SYS-F-205） |
| `bridge-activity` | `{ kind: string, bytes: usize, preview: string }` | AI Bridge 経由の活動通知（現状 `kind: "send"` のみ）。`preview` は送信内容の先頭 64 文字。**AI の送信を人間が画面で確認できるようにするための必須経路**（SYS-F-1103） |

> **Note:** `data-update` イベントはフレームレート（約60fps = 16ms間隔）に合わせて発火する。高頻度なデータ受信があってもUIへの通知はフレームレートで間引かれる。データ本体はイベントに含めず、`get_display_rows` APIで必要な範囲のみを取得する。

> **Note:** ホットプラグの能動検知は Tauri のイベントではなく、フロントエンド側の `list_ports` ポーリング（2 秒間隔）で行う。`serial-status` はあくまで「受信中に落ちた」ことの通知であり、ポート一覧の更新契機ではない。

---

## AI Bridge プロトコル (NDJSON / TCP)

Tauri IPC とは別に、外部プロセス（MCP アダプタ等）向けの**ローカル専用ソケット**がある。

| 項目 | 値 |
| --- | --- |
| バインド | `127.0.0.1` のみ。既定ポート `57320` |
| 既定 | **無効**。`bridge_set(enabled=true)` でのみ起動 |
| 形式 | NDJSON（1 行 1 JSON、UTF-8）。要求 `{id, method, params}` / 応答 `{id, ok, result\|error}` |
| メソッド | `help` / `auth` / `status` / `read_range` / `tail` / `subscribe` / `send` / `ports` |
| push | `subscribe` した接続へ `{"event":"data",...}` / `{"event":"reset"}` を配信 |

`help` は**認証不要**の自己記述メソッドで、全メソッド・制限値・使い方ヒントを機械可読で返す
（SYS-F-1109）。MCP を使わず生 TCP で繋ぐエージェントは、最初に
`{"id":1,"method":"help"}` を送れば仕様を自己取得できる。

**仕様の正は `src-tauri/src/bridge.rs` のモジュールヘッダ（protocol v1）**。
本書はその存在と入口を示すだけに留め、メソッドごとの引数・クランプ規則・エラー文字列は
コード側のドキュメントコメントを参照する（二重管理を避けるため）。

---

## MCP stdio アダプタ（アプリ内蔵, `--mcp`）

インストール済み環境では Node.js が無いため、MCP アダプタは**アプリ本体に内蔵**されている
（SYS-F-1107 / ADR-13）。

```
serial-monitor-essential --mcp        # GUI を起動せず MCP stdio サーバとして動く
```

| 項目 | 値 |
| --- | --- |
| 実装 | `src-tauri/src/mcp_stdio.rs`（JSON-RPC 2.0 行区切り、stdout は JSON-RPC 専用・ログは stderr） |
| ツール | `serial_status` / `serial_ports` / `serial_read_tail` / `serial_read_range` / `serial_send` / `serial_send_hex` / `serial_wait_for`（Node 版 `mcp/server.mjs` と同一） |
| 接続先 | 環境変数 `SME_BRIDGE_HOST`（既定 `127.0.0.1`）/ `SME_BRIDGE_PORT`（既定 `57320`）/ `SME_BRIDGE_TOKEN` |
| 登録例 | `claude mcp add serial-monitor -- "<exe のフルパス>" --mcp` |

登録コマンドは GUI の **Settings → Setup Guide**（AI 連携ガイドウィンドウ）が実際の
exe パス入りで表示する。`mcp/server.mjs` は開発用リファレンス実装として残る
（両者の同期は docs/22 DEBT-6）。

---

## 関連ドキュメント

- [システムアーキテクチャ](02_architecture.md)
- [データ構造](03_data_structures.md)
- [システム要求 §A.10（AI ブリッジ）](21_system_requirements.md)
- [アーキテクチャ記述 ADR-12](22_architecture_description.md#adr-12-ai-ポートをアプリ内マルチプレクサ--外付け-mcp-アダプタの-2-層にする)
- [MCP アダプタ](../mcp/README.md)
