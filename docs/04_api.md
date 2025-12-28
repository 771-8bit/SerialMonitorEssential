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

## Events (Backend -> Frontend)

| イベント名 | ペイロード | 説明 |
| --- | --- | --- |
| `serial-status` | `{ connected: bool, port: string }` | 接続状態の変化（`WM_DEVICECHANGE` によるホットプラグ検知含む） |
| `data-update` | `{ total_bytes: u64 }` | 新規データ受信通知。フロントエンドは `get_display_rows` で表示データを取得。 |
| `log-error` | `{ message: string }` | ディスクフルなどのエラー通知 |

> **Note:** `data-update` イベントはフレームレート（約60fps = 16ms間隔）に合わせて発火する。高頻度なデータ受信があってもUIへの通知はフレームレートで間引かれる。データ本体はイベントに含めず、`get_display_rows` APIで必要な範囲のみを取得する。

---

## 関連ドキュメント

- [システムアーキテクチャ](02_architecture.md)
- [データ構造](03_data_structures.md)
