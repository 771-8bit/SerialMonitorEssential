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
| メソッド | `help` / `auth` / `status` / `digest` / `watch` / `find` / `read_range` / `tail` / `subscribe` / `send` / `ports` |
| push | `subscribe` した接続へ `{"event":"data",...}` / `{"event":"reset"}` を配信 |

`help` は**認証不要**の自己記述メソッドで、全メソッド・制限値・使い方ヒントを機械可読で返す
（SYS-F-1109）。MCP を使わず生 TCP で繋ぐエージェントは、最初に
`{"id":1,"method":"help"}` を送れば仕様を自己取得できる。

**仕様の正は `src-tauri/src/bridge.rs` のモジュールヘッダ（protocol v2）**。
v2 で `digest` / `watch` が加わり、`status` に索引の状態が付いた。新しい
パラメータを送らない v1 クライアントの挙動は変わらない。
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
| ツール | `serial_status` / `serial_ports` / `serial_digest` / `serial_watch` / `serial_read_tail` / `serial_read_range` / `serial_find` / `serial_send` / `serial_send_hex` / `serial_wait_for`（Node 版 `mcp/server.mjs` と同一） |
| 接続先 | 環境変数 `SME_BRIDGE_HOST`（既定 `127.0.0.1`）/ `SME_BRIDGE_PORT`（既定 `57320`）/ `SME_BRIDGE_TOKEN` |
| 登録例 | `claude mcp add serial-monitor -- "<exe のフルパス>" --mcp` |

登録コマンドは GUI の **Settings → Setup Guide**（AI 連携ガイドウィンドウ）が実際の
exe パス入りで表示する。`mcp/server.mjs` は開発用リファレンス実装として残る
（両者の同期は docs/22 DEBT-6）。

### 読み出しツールの出力予算

`serial_read_tail` / `serial_read_range` の**出力は文字数で上限を持つ**
（[26 §7.1](26_ai_api_design.md)）。トークンではなく文字数なのは、サーバが
クライアントのトークナイザを知り得ないため。

| 引数 | 既定 | 意味 |
| --- | --- | --- |
| `max_chars` | 4000（範囲 500〜200000） | 返す文字数の硬い上限。ヘッダ・本文・フッタの合計 |
| `format` | `auto` | `auto`（中身で text/hex を選択）/ `text` / `hex` / `lines`（両端の欠け行を捨てる） |
| `collapse_repeats` | `false` | 連続する同一行を `[xN]` に畳む。hex では無効。バイト予算の**後**に効くので、反復の多いログでは `bytes` も併せて上げる |

予算に収まらないときは**バイト単位で切り出してから**描画する。切り詰めたら
ヘッダに `shown=<start>..<end>` が付き、フッタが落とした範囲を
`serial_read_range(...)` 呼び出しの形で正確に示す（前後どちらにも落ちる場合は
両方）。フッタは本文より優先して残す。

`serial_status` は 4 行のみを返す（以前あった `raw: {全 JSON}` は同じ内容の
重複だったため廃止）。

### `serial_find` / ブリッジ `find`

キャプチャ全体をサーバ側で正規表現走査し、**件数と少数のオフセット**だけを返す
（[26 §7.2](26_ai_api_design.md)）。生データは返さないため、応答コストは
キャプチャ長に依存せず、1 MiB の読み出し上限も越えられる。実装は
`src-tauri/src/insight/find.rs`（256 KiB 窓 + 4096 B 重なりのストリーミング走査）。

| パラメータ | 既定 | 意味 |
| --- | --- | --- |
| `pattern` | 必須 | Rust regex（複数行モード。lookaround 非対応） |
| `from` / `to` | 全体 | 走査するバイト範囲 |
| `max_hits` | 20（最大 200） | 引用する行数。**件数はこれに影響されない** |
| `count_only` | `false` | 件数とカバレッジのみ（最安） |

出力の規範:

- 件数は `exact`、または `AT LEAST — <理由>`（3 秒で打ち切り／欠落を飛ばした）。
  打ち切り時は再開用の `from=` を必ず添える。
- **`matches: 0` を単独で出さない。** 走査バイト数・カバレッジ率・欠落・
  所要時間を必ず併記し、「見つからなかった」と「見に行けなかった」を区別できる
  ようにする。
- ヒットを打ち切ったら `showing N of M hits` と明示する。

### `serial_digest` / `serial_watch` / ブリッジ `digest` / `watch`

アプリが持つ**索引**（`src-tauri/src/insight/`）から答えるツール。
`get_data` を 1 回も呼ばないので、応答の大きさが到着レートに依存しない
（[26 §7.3](26_ai_api_design.md)）。索引は **AI Bridge が ON の間だけ**動く。

| パラメータ | 既定 | 意味 |
| --- | --- | --- |
| `digest.window` | `all` | `all` / 期間（`30s` `5m` `1h` `250ms`）/ `off:FROM..TO` |
| `digest.top` | 8（最大 50） | TOP に載せる頻出テンプレート数 |
| `digest.samples` | 3（最大 8） | 引用する直近の生行数 |
| `digest.buckets` | 0（最大 24） | 時系列バケット数。0 で TIME セクションなし |
| `watch.cursor` | なし | 前回応答のカーソル。省略時は索引全体を返して基準点を作る |
| `watch.wait_ms` | 0（最大 3000） | サーバ側ロングポール。MCP クライアントの 5s タイムアウトより手前で切る |
| 共通 `max_chars` | digest 1800 / watch 1200 | 返す文字数の硬い上限 |

応答は共通のエンベロープ（[26 §2.3](26_ai_api_design.md)）:

```json
{ "text": "…予算内の本文…", "data": { /* 構造化等価物 */ },
  "meta": { "gen": 3, "exact": true, "coverage_pct": 100, "index_lag_bytes": 0 },
  "budget": { "max_chars": 1800, "used_chars": 901, "truncated": false,
              "withheld": [], "next": "serial_digest(...)" } }
```

MCP ツールは `text` をそのまま返すだけ。予算も文言も
`insight::render` 1 箇所で決まる（Rust 版・Node 版が食い違わない構造）。

出力の規範（テストで固定）:

- セクションの並び `Header → INDEX → ALARM → NEW → RARE → TOP → TIME → SAMPLES → cursor`
  が**そのまま優先順位**。予算が足りなければ末尾（サンプル）から落ち、
  **ALARM とカーソルは決して落ちない**。落とした行数は必ずフッタに出る。
- **`ALARM 0 err` を単独で出さない。** 常に `(exact, coverage 100%)` か
  `(AT LEAST - coverage 62%, index 82.10 MB behind)` を伴う。
- 初出（NEW）と件数 5 以下（RARE）は **TOP より先に**、正確な件数と
  オフセット付きで列挙する。RARE が上限に当たったら `N of M shown` と明示する。
- 出す極値（first / last / min / max）には**必ずバイトオフセットを併記**し、
  `serial_read_range` 1 回で検証できるようにする。
- 差分（`watch` と窓つき `digest`）では、窓の外を指しうる first / min / max は
  出さない。窓での増分の隣に全期間の極値を並べると、要約が事象を隠す。
- 時刻指定のウィンドウは `timestamp_index` が空なら**失敗させる**（時刻を捏造しない）。
- カーソルは世代 `gen` を内包する。Clear / ポート再オープン後の古いカーソルは
  **エラーにせず**、世代が変わった事実と再開位置を返す。

---

## 関連ドキュメント

- [システムアーキテクチャ](02_architecture.md)
- [データ構造](03_data_structures.md)
- [システム要求 §A.10（AI ブリッジ）](21_system_requirements.md)
- [アーキテクチャ記述 ADR-12](22_architecture_description.md#adr-12-ai-ポートをアプリ内マルチプレクサ--外付け-mcp-アダプタの-2-層にする)
- [MCP アダプタ](../mcp/README.md)
