# インターフェース設計 (IPC API)

## Commands (Frontend -> Backend)

| コマンド名 | 引数 | 説明 |
| --- | --- | --- |
| `open_port` | `port_name`, `config: SerialConfig` | 指定設定でポートを開く |
| `close_port` | なし | ポートを閉じる |
| `reopen_port` | `port_name`, `config: SerialConfig` | 自動再接続（SYS-F-807）。ポートを開き、**既存のデータストアに追記を再開**する（ストアが無ければ新規）。切断済みのハンドルが残っていても動くが、生きている接続があれば `Err` |
| `set_reconnecting` | `waiting: bool` | 自動再接続の待機に入った / 出たことを記録する。ブリッジの `status.reconnecting` に反映 |
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
| `bridge_status` | なし | ブリッジ（設定画面の MCP Server）の状態を取得（`{ enabled, port, connections, last_activity }`）。失敗しない |
| `bridge_set` | `enabled: bool`, `port: Option<u16>` | ブリッジの起動 / 停止。`enabled=true` で同じポートが稼働中なら no-op、ポートが変われば再起動。戻り値は `bridge_status` と同じ形 |
| `bridge_guide_info` | なし | AI 連携ガイド用の情報（`{ exe_path, bridge_enabled, bridge_port, app_version }`）。`exe_path` は実行中バイナリの絶対パスで、ガイドが MCP 登録コマンドに埋め込む（SYS-F-1108） |
| `open_ai_guide_window` | なし | AI 連携ガイドウィンドウ（label: `ai-guide`, `ai-guide.html`）を開く。既に開いていればフォーカスする |

## Events (Backend -> Frontend)

| イベント名 | ペイロード | 説明 |
| --- | --- | --- |
| `serial-status` | `{ connected: bool, error: string \| null }` | 接続状態の変化。受信スレッドが致命的 I/O エラーを検知したときに `connected=false` で発火する。フロントエンドはこれを受けて `close_port` を呼び、バックエンド状態を再同期する（SYS-F-107） |
| `data-update` | `{ total_bytes: u64 }` | 新規データ受信通知。フロントエンドは `get_display_rows` で表示データを取得。 |
| `log-error` | `{ message: string }` | ディスクフルなどのエラー通知。Logger スレッドの `on_error` コールバック経由で発火し、**同種のエラーは 5 秒に 1 回まで**に抑制される（SYS-F-205） |
| `bridge-activity` | `{ kind: string, bytes: usize, preview: string }` | ブリッジ経由の活動通知（現状 `kind: "send"` のみ）。`preview` は送信内容の先頭 64 文字。**AI の送信を人間が画面で確認できるようにするための必須経路**（SYS-F-1103） |

> **Note:** `data-update` イベントはフレームレート（約60fps = 16ms間隔）に合わせて発火する。高頻度なデータ受信があってもUIへの通知はフレームレートで間引かれる。データ本体はイベントに含めず、`get_display_rows` APIで必要な範囲のみを取得する。

> **Note:** ホットプラグの能動検知は Tauri のイベントではなく、フロントエンド側の `list_ports` ポーリング（2 秒間隔）で行う。`serial-status` はあくまで「受信中に落ちた」ことの通知であり、ポート一覧の更新契機ではない。

---

## ブリッジプロトコル (NDJSON / TCP)

Tauri IPC とは別に、外部プロセス（MCP アダプタ等）向けの**ローカル専用ソケット**がある。

| 項目 | 値 |
| --- | --- |
| バインド | `127.0.0.1` のみ。既定ポート `57320` |
| 既定 | **無効**。`bridge_set(enabled=true)` でのみ起動 |
| 形式 | NDJSON（1 行 1 JSON、UTF-8）。要求 `{id, method, params}` / 応答 `{id, ok, result\|error}` |
| メソッド | `help` / `auth` / `status` / `digest` / `watch` / `sample` / `find` / `wait_for` / `read_range` / `tail` / `subscribe` / `send` / `ports` |
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
| ツール | `serial_status` / `serial_ports` / `serial_digest` / `serial_watch` / `serial_sample` / `serial_read_tail` / `serial_read_range` / `serial_find` / `serial_send` / `serial_send_hex` / `serial_wait_for`（Node 版 `mcp/server.mjs` と同一） |
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

### `serial_wait_for` / `serial_send(expect=)` / ブリッジ `wait_for`

待機の**走査はブリッジが行う**（[26 §7.5](26_ai_api_design.md)）。クライアントは
区切って呼び直し、その合間にキャンセルを見るだけ。

| パラメータ（ブリッジ `wait_for`） | 既定 | 意味 |
| --- | --- | --- |
| `pattern` | 必須 | Rust regex（複数行モード） |
| `from` | 0 | ここから現在の末尾まで前方走査する |
| `timeout_ms` | 0 | この 1 呼び出しがブロックしてよい上限（最大 1000ms） |

戻り値の要点: `matched` / `offset` / `match_text` / `excerpt` /
`next_from`（**重なりを含んだ再開位置**）/ `scanned_to` / `reset`（世代交代）/
`tail_base64`（タイムアウト報告用の末尾 256 バイト）。

- 窓も蓄積も無いので**読み負けが起きない**。旧実装（クライアント側 tail
  ポーリング）は実効 131 KB/s しか走査できず、約 1 Mbps で返信を取りこぼしていた
  （DEBT-8、解消済み）。
- `send` は、受信ワーカーが**要求を取り出して書き始めた時点の公開済み位置**
  （`total_bytes` と同じ尺度）を `offset_before` として返す（ADR-14）。
  `serial_send(expect=...)` はそこから走査するので、返信が最初の走査より早く
  着弾しても取りこぼさず、送信の順番待ちの間に届いたデータを返信と取り違えることもない
  （時間ではなく位置で決まる）。
- `send` は受信を止めずに書く（受信と 1 ms 分ずつ交互）。ハードウェアフロー制御で
  相手が絞ると大きな送信は時間がかかるが、失敗にするのは **3 秒間 1 バイトも
  受理されなかったときだけ**で、エラーに書けたバイト数が入る。呼び出し側の都合で
  タイムアウトして再送すると、既に線に出たバイトを二重に送ることになる。
- 世代交代（Clear / ポート再オープン）は `reset` で報告する。黙って待ち続けると
  「応答が来ているのに必ずタイムアウト」になる。
- 残る限界: 再開の重なり（`insight::find::SCAN_OVERLAP` = 4 KiB）を超える単一
  マッチは境界で取り逃がし得る。

### `serial_sample` / ブリッジ `sample`

導出情報では足りず「実際の行そのもの」が要るときの、上限付き・重複畳み込み付きの窓。
索引が無くても `tail` / `head` / `around` / `stratified` は動く（`by_template` のみ索引が要る）。

| パラメータ | 既定 | 意味 |
| --- | --- | --- |
| `mode` | `tail` | `tail`（最新）/ `head`（索引の先頭）/ `around`（`offset` 必須）/ `stratified`（全体に等間隔）/ `by_template`（`template` 必須） |
| `n` | 10（最大 50） | 行数 |
| `collapse_repeats` | `true` | 連続する同一行を `[xN]` に畳む。**N は正確**（丸めない） |
| `max_chars` | 1400 | 返す文字数の硬い上限 |

規範:

- **読み出し窓の端で切れた行は捨てる**。途中から始まる行を 1 行として見せない。
  捨てた結果 0 行になったら、その理由と次の手を必ず出す（空応答を返さない）。
- 全ての行に行頭オフセットを付ける（`serial_read_range` で確認できる）。
- `by_template` が返せるのは索引が覚えている**直近 4 件**まで。件数を水増しせず、
  「直近 N 件を出している」と明記する。

### `serial_digest` / `serial_watch` / ブリッジ `digest` / `watch`

アプリが持つ**索引**（`src-tauri/src/insight/`）から答えるツール。
`get_data` を 1 回も呼ばないので、応答の大きさが到着レートに依存しない
（[26 §7.3](26_ai_api_design.md)）。索引は **MCP Server（ブリッジ）が ON の間だけ**動く。

| パラメータ | 既定 | 意味 |
| --- | --- | --- |
| `digest.window` | `all` | `all` / 期間（`30s` `5m` `1h` `250ms`）/ `off:FROM..TO` |
| `digest.template` | なし | `t12` / `12`。指定すると**そのテンプレート 1 件の詳細**（全項目の範囲とオフセット、実際の行）を返す |
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
- 取り込んだ制御バイトが 5% を超えたら「行指向ではなさそう」と固定枠で明言し、
  行数とテンプレートを信じないよう伝える（`meta.line_oriented` / `meta.control_pct`）。
  0x80 以上は可としているので、CJK ログはバイナリ扱いにならない。

---

## 関連ドキュメント

- [システムアーキテクチャ](02_architecture.md)
- [データ構造](03_data_structures.md)
- [システム要求 §A.10（AI ブリッジ）](21_system_requirements.md)
- [アーキテクチャ記述 ADR-12](22_architecture_description.md#adr-12-ai-ポートをアプリ内マルチプレクサ--外付け-mcp-アダプタの-2-層にする)
- [MCP アダプタ](../mcp/README.md)
