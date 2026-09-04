# AI 向け API 設計 — トークン効率の Insight Bridge (v2)

## 目的

12 Mbps 級（1.5 MB/s）の受信中でも、**AI エージェントのトークン消費がデバイスの
スループットに比例しない** API を定義する。

現状の問題は帯域ではない（[25 §5.8](25_release_strategy.md) / ループバック TCP は
GB/s 出るので 1.5 MB/s は利用率 0.1%）。問題は **AI が読める量**である:

- 1.5 MB/s のテキストは毎秒 **約 37 万トークン**相当。どの LLM も消費できない。
- `serial_read_tail` の 1 MiB 上限は 1 回で **約 25 万トークン**。実質使えない。
- 特に `hex_dump` は 16 バイトあたり約 79 文字を出すため、`looks_like_text` が
  false になった瞬間、既定の 4096 バイト要求が **約 20 KB（6〜8k トークン）**を返す。

## 統括原則

> **アプリは「問い」に答える。干し草の山を送りつけない。**

AI 向けの応答は常に「**検証可能なハンドル付きの導出された事実**」であり、
生バイトはエージェントが**自分で辿り着いた 1 点の座標で意図的に買う**ものとする。

全ツールが従う 5 原則:

1. **バイト/呼び出しではなく、洞察/トークン。** 応答サイズは**文字数予算**で上限を
   持ち、到着データ量に依存しない。
2. **見栄えより正確さ。** 件数は正確な整数か、明示的に `est` を付ける。丸めない。
   **稀な事象を隠す要約は、要約しないより悪い。**
3. **ペイロードではなくハンドル。** バイトオフセット・テンプレート ID・カーソル
   （各 8 トークン程度）を返し、必要な箇所だけを 1 回だけ買わせる。
4. **時計は 1 つ、アドレス空間は 1 つ。** 唯一の真のアドレスは**ストリーム絶対
   バイトオフセット**。時刻は `DataStore::get_timestamp_for_offset`（実 epoch ms）
   のみ。行番号は参考値で、必ずオフセットを併記する。
5. **省略による嘘をつかない。** 切り詰め・サンプリング・索引遅延・ギャップ・世代
   リセット・テンプレート表の追い出しは、**予算に押し出されない固定枠**で必ず報告する。

**`max_tokens` ではなく `max_chars` を採る理由**: サーバはクライアントのトークナイザを
知り得ず、hex や CJK では推定が必ず外れる。全ツールは**レンダリング後の UTF-8 文字数**で
厳密に予算を課す（ログ的 ASCII では約 3.6 文字/トークンと説明文に明記する）。

---

## 1. 既存機構を「正」として流用してはいけない理由（コードで検証済み）

当初は「プロッタの集約器・パーサ・行インデックスを再利用すれば安上がり」と考えたが、
**いずれも AI 向けの正としては使えない**ことがコード上で確認された。

| 流用候補 | 使えない理由（本リポジトリで検証） |
|---|---|
| `PlotterState.aggregator` をチャンネル統計に | **GUI ライフサイクルに縛られている**。プロッタ窓の `Destroyed` で `set_enabled(false)`（`lib.rs:59`）、`start_plotter_thread` で `aggregator.clear()`（`lib.rs:243`）。人間がプロット窓を閉じるとエージェントのデータが消え、開き直すと過去の回答が遡って変わる |
| `PlotterThread` のタイムスタンプ | `timestamp_ms = start_time.elapsed()`（`thread.rs:157,181`）で **epoch ではない**。さらに `spread_batch_timestamps`（`thread.rs:62`）で再配分され、高レートでは多数行が同一 ms に潰れる。`serial::DataStore` 側に単調性を保証した実 epoch 索引が既にある（`serial/data_store.rs:383-437`） |
| `PlotterParser` を汎用ログの項目抽出に | セパレータ推定が空白まで落ちる。ラベルなしトークンは位置で `ch{i}` になり、非数値は全て `ChannelValue::State` になる。自由形式ログから**それらしいゴミのチャンネル**が生成され、動的キーでチャンネル配列が無制限に増える |
| `DataStore::total_lines()` / `get_line_offsets` をアドレス体系に | `record_line_offsets`（`serial/worker_thread.rs:29`）は **LF のみ**を記録し、`line_index` は `vec![0]` で初期化（`serial/data_store.rs:98`）されるため**空でも 1 行**と報告する。CR 単独終端のデバイスでは永遠に 1 行。**参考値としてのみ**使う |

**帰結**: AI 向けインデックスは**ブリッジが所有する独立モジュール**として新設する。

---

## 2. アーキテクチャ

### 2.1 新規モジュール

```
src-tauri/src/insight/
  mod.rs      // InsightIndex, IndexHandle, Snapshot
  mask.rs     // 純粋: 行 -> マスク済みテンプレート + スロット値（重点的に単体/プロパティ試験）
  index.rs    // テンプレート表・リング・重大度・チャンネル
  thread.rs   // DataStore を読む唯一の追加リーダ
  window.rs   // 純粋: ウィンドウ指定の解決、カーソルの生成/解釈
  find.rs     // DataStore 上のストリーミング正規表現走査（索引非依存）
  binary.rs   // 行指向でないストリームのフレーム要約
  render.rs   // 純粋: セクション組み立て + max_chars 予算 + フッタ
```

### 2.2 所有とライフサイクル（設計の要）

```rust
pub struct BridgeState {
    // ...既存...
    pub index: Arc<InsightIndex>,            // BridgeState::new() で常時生成
    index_thread: Mutex<Option<IndexThread>>,
}
pub struct BridgeCtx {
    // ...既存...
    pub index: Arc<InsightIndex>,            // 追加
}
```

- インデックスは**ブリッジの所有物**であり、プロッタ窓やメイン窓の開閉に影響されない。
- `IndexThread` は `bridge_set(enabled=true)`（`bridge.rs:989` 付近）で開始し、
  `false` で停止する。**AI Bridge は既定 OFF** なので、使わない利用者は CPU/メモリを
  一切払わない。
- スレッド構造は実績のある `PlotterThread::run` に倣う（毎回ストアを再解決、
  `Arc::ptr_eq` で世代差し替え検知、1 MiB で読み出し上限、読み取り失敗が続いたら
  スキップアヘッド）。ただし**スキップは必ず `gap` として明示記録し、隠さない**。
- `--mcp` は別プロセス（`lib.rs:30`）で TCP しか持たないため、**ロジックは全て
  ブリッジ側**に置き、MCP アダプタは渡された `text` を出すだけの薄い描画器とする。

### 2.3 応答エンベロープ（新メソッド共通）

```json
{
  "text":   "…レンダリング済み・予算内の本文…",
  "data":   { /* GUI や非 LLM クライアント向けの構造化等価物 */ },
  "meta":   { "gen":3, "mode":"text", "exact":true, "coverage_pct":100,
              "index_lag_bytes":0, "sampled":null, "gaps":0 },
  "budget": { "max_chars":1800, "used_chars":1642, "truncated":true,
              "next":"serial_patterns(window=\"60s\", limit=41)" }
}
```

MCP ツールは `text` をそのまま返す。予算の適用は `insight::render` の**1 箇所のみ**で行う。

---

## 3. 時刻・ハンドル・世代・カーソル

### 3.1 唯一の時計

`timestamp_index` は `UiNotifier` が 100 ms ごと（バイトが増えたときのみ）に追記し、
NTP のステップに対して単調性が保証されている（`serial/data_store.rs:383-414`）。したがって:

- 壁時計値は必ず **±100ms** を併記する。
- `window="30s"` は**バイト範囲に解決**し、その範囲を応答に明記する
  （`window=30s → off 128,204,001..132,412,000 (±100ms)`）。エージェントが再導出できる。
- `timestamp_index` が空なら、時刻指定ウィンドウは
  `no timing data yet; use window="all" or an offset window` で**失敗させる**。
  時刻を捏造しない。

### 3.2 ハンドルと世代

| ハンドル | 形式 | 保証 |
|---|---|---|
| バイトオフセット | `@off=12873456` | **正**。`serial_read_range` にそのまま使える |
| テンプレート ID | `t88` | セッション内で安定 |
| カーソル | `v1:g3:o…:l…:e…` | **世代 `g` を内包** |

世代（`gen`）は Clear / ポート再オープン / 巻き戻りで増える。**古い世代のカーソルは
エラーにせず**、`gen` が変わった事実と新しい開始位置を明示して返す（購読側が
静かに嘘のデルタを受け取らないため）。

---

## 4. ツール一覧（新規 6 / 既存 7 は維持）

機能は**パラメータに畳む**（差分専用ツールではなく `compare_to`、タイムライン専用
ツールではなく `buckets`）。MCP スキーマの常駐コストを約 1.0k トークンに抑える。

| ツール | 目的 | 代表コスト | 実装の要 |
|---|---|---|---|
| **`serial_digest`** | 1 コールで全体を把握（「とりあえず tail して眺める」を置換） | 約 500 tok（1800 字上限） | `InsightIndex::snapshot()`。**`get_data` を 1 回も呼ばない** |
| **`serial_watch`** | カーソル以降の**集約された変化のみ**。コストがスループットに非依存 | 約 180 tok（4 KB でも 40 MB でも同じ） | テンプレート別デルタ + サーバ側ロングポール |
| **`serial_find`** | サーバ側で正規表現検索し、**件数**を返す | 約 350 tok（`count_only` なら約 60） | `regex::bytes` で 256 KiB 窓・4096 B 重なりのストリーミング走査。**索引不要** |
| **`serial_patterns`** | テンプレート・ヒストグラム（最高の洞察/トークン）。`id=` で詳細、`compare_to=` で差分 | 約 180〜620 tok | テンプレート表 + スロット統計 |
| **`serial_sample`** | 生行が本当に要るときの**代表・重複畳み込み・上限付き**の抜き取り | 約 380 tok（n=20） | `tail｜head｜stratified｜around_offset｜by_template` |
| **`serial_channels`** | 数値/状態テレメトリ（**独自**抽出器。プロッタのパーサは使わない） | 約 300 tok | ラベル付き項目のみを対象にした保守的な抽出 |

### 代表例: `serial_watch`（最重要）

```
watch gen=3 · +2.10 MB · +71,204 lines · 4.31 s · 1.42 MB/s
ALARM +0 err · +3 warn (exact)
NEW   t94 "W: rx overflow q=<N:64..64>" x3  first @off=134,110,221
DELTA t1 +68,900 · t7 +2,290 · t12 +11 · other +0
CHAN  temp mean 23.4 (was 23.3) min 21.9@off=134,002,111 max 25.9@off=134,510,900
cursor v1:g3:o134612044:l4286084:e1756900000
— nothing withheld
```

0.5 Hz で追随して **約 360 tok/分**。生ストリーム換算 2,250 万 tok/分に対して
**約 6 万分の 1**。

---

## 5. 既存ツールの扱い（何も削除しない）

| ツール | 扱い | 変更点 |
|---|---|---|
| `serial_status` | **出力のみ変更** | 末尾の `raw: {全 JSON}` を廃止（情報ゼロでコスト倍）。`gen` / 索引状況 / レートを追加 |
| `serial_ports` | 維持 | — |
| `serial_read_tail` | **出力を有界化** | `max_chars`（既定 4000）、`collapse_repeats`、`format`(`auto｜text｜hex｜lines`) を追加。`bytes` 既定 4096 は互換のため据置 |
| `serial_read_range` | **出力を有界化** | 同上。バイト厳密な意味論は不変 |
| `serial_send` / `serial_send_hex` | **加算的変更** | `expect`（正規表現）/ `expect_timeout_ms` / `reply_chars` を追加。既定は現行と完全同一挙動 |
| `serial_wait_for` | **スキーマ維持 / 実装を刷新** | 待機を**ブリッジ側**へ移す（下記の実バグ修正を兼ねる） |
| ブリッジ `subscribe` | 維持 | GUI・非 LLM 用。**MCP ツールとしては公開しない**（256 KiB/50ms の base64 は LLM に不適） |

`PROTOCOL_VERSION` を 1 → 2 に上げる。新パラメータを送らない v1 クライアントは挙動不変。

### 5.1 発見された実バグ: `serial_wait_for` の読み負け

現行のクライアント側ポーリングは `WAIT_WINDOW_BYTES=65536` を `WAIT_POLL_MS=500`
ごとに読む（`mcp_stdio.rs:38-42`）＝**実効 131 KB/s の走査**。12 Mbps（1.5 MB/s）では
**約 11 倍の読み負け**となり、ポーリング間に約 750 KB 到着するのに末尾 64 KB しか
見えない。結果 `chunk_offset > cursor` がほぼ毎回成立して**累積バッファが置換**され
（gap 扱い）、デバイスの返信はポーリングが着弾の約 43 ms 以内に落ちたときしか
観測できない。

→ **待機をブリッジ側に移し**、送信前の `total_bytes` から前方走査する。これにより
`gap: true` という失敗モード自体が消える。

---

## 6. 誤誘導防止（規範。これは助言ではなく受入基準）

### 6.1 セクション優先順位（予算はこの順に埋める）

```rust
enum Section { Header, IndexHealth, Alarm, NewTemplates, Rare, Top, Channels, Timeline, Samples }
// フッタは 120 字を先に確保し、決して落とさない
```

予算が逼迫したときに削るのは**サンプル数**であって、**アラームは決して削らない**。
実行時ヒューリスティックではなくコンパイル時の順序で保証する。

### 6.2 規則

1. **正確な件数のみ。** 丸めない。不正確な値は `est` を前置し、理由（`sampled 1/8`）を併記。
2. **稀少イベントの床。** top-N を出す前に、ウィンドウ内で `count <= 5` のテンプレートと
   ウィンドウ内で初出のテンプレートを、正確な件数と初出/最終オフセット付きで
   `RARE` / `NEW` に必ず列挙する。**400 万分の 1 の行は、支配的な行より優先して出る。**
   `RARE` すら上限に当たる場合は `RARE 12 of 47 shown` と明示し、全件を出す呼び出しを
   フッタに書く。黙って短くしない。
3. **重大度は常に全バイトを正確に走査。** `error｜fail｜fatal｜panic｜timeout｜overflow｜warn`
   等の `memchr` ベースの大小文字無視走査を**取り込む全バイト**に対して行う
   （マスク処理より約 10 倍安い）。したがって `coverage_pct == 100` である限り
   `ALARM` 件数はテンプレートのサンプリングと無関係に正確。
4. **極値には必ずハンドルを付ける。** すべての `min`/`max`/`peak`/`first`/`last` に
   発生バイトオフセットを併記する。「要約が致命的事象を隠した」を、
   `serial_read_range` 1 回で検証可能な状態に変える。
5. **カバレッジを常に明示し、裸のゼロを禁じる。** `err=0` 単独は**不正な出力**。
   合法なのは `err=0 (exact, coverage 100%)` か
   `err=0 (coverage 62%, indexing 82.1 MB behind)` のみ。
6. **ダウンサンプルはスパイクを構造的に保存する。** バケットは平均だけでなく
   min/max とその発生オフセットを保持する（GUI プロッタの INV-7 / min-max バンドと
   同じ思想。[22 ADR-06](22_architecture_description.md) と整合）。

---

## 7. 段階実装計画（トークン削減効果 / 労力 の順）

| 段階 | 内容 | 効果 |
|---|---|---|
| **0. 描画の規律**（約 1 日、新スレッド・新状態なし） | `render_chunk`/`hex_dump` に `max_chars`（既定 4000）を後段適用 + 切り詰めフッタ。`serial_status` の `raw` 廃止。`format`/`collapse_repeats` 追加 | 病的ケースが **6〜8k → 約 1.1k トークン**。アーキ変更ゼロで即効。**最初に出す** |
| **1. `serial_find`**（約 2 日、索引不要） | `insight/find.rs` + ブリッジメソッド + 呼び出し単位のソケットタイムアウト | 「1 MiB 引いて grep」（25 万トークン・しばしば失敗）を **約 350 トークン**に。1 GB でも検索可 |
| **2. 索引 + `serial_digest` + `serial_watch`**（約 5 日） | `mask.rs`（純粋・プロパティ試験）/ `index.rs` / `thread.rs` / `window.rs` / `render.rs`。`Arc<InsightIndex>` を `BridgeState`→`BridgeCtx` に通す | **設計の本体**。定コストでの把握と追随（追随経路で約 1,800 倍） |
| **3. `serial_patterns` / `serial_sample`**（約 2 日） | 表ができていればほぼ無料。`compare_to` は 2 スナップショットの差 | 「送信後に何が変わったか」が 25 万トークン → **約 450 トークン** |
| **4. `send(expect=)` と `serial_wait_for` のサーバ側化**（約 2 日） | 照合をブリッジへ移動。送信前 `total_bytes` から前方走査 | 1 往復あたり 1,500〜6,000 → **約 250 トークン**。かつ §5.1 の読み負けバグを解消 |
| **5. `serial_channels` + バイナリモード**（約 3 日） | 独自のラベル付き抽出器、`binary.rs` の分類とフレーム要約 | トークン効果は中程度だが、**バイナリ/自由形式で自信満々に嘘をつくのを止める** |

---

## 8. 試験義務（本リポジトリの V&V 方針に整合）

- `mask.rs` / `window.rs` / `render.rs` は**純粋関数**として単体 + プロパティ試験
  （マスクの冪等性、予算超過が起き得ないこと、セクション優先順位の保存）。
  → これは [24 §6.1.1](24_vv_plan.md) のハーネス方針、および **DEBT-7**（IO ロジックを
  純関数へ抽出して細粒度検証する）と同じ方向であり、新規コードで最初からそれを守る。
- `find.rs` は窓の継ぎ目（4096 B 重なり）に跨るマッチのプロパティ試験。
- 索引スレッドは既存の状態遷移試験（`state_transition_tests.rs`）に
  Attach/Swap/Detach × 索引の 1-switch を追加。
- 誤誘導防止規則（§6）は**受入基準**として、稀少イベントが top-N に埋もれないこと、
  `err=0` が裸で出ないことをテストで固定する。

## 9. 受け入れたリスク（明示）

- テンプレート表は有界（追い出しあり）。追い出しは `evicted` として必ず報告する。
- 索引は AI Bridge が ON のときだけ動く。OFF の利用者はコストゼロだが、
  ON にした直後は履歴の索引が無い（`coverage_pct` で明示）。
- 12 Mbps での索引スレッドの追随性能は**未実測**（現時点の実測は ~2 Mbps まで）。
  FT232H 級の治具が入手でき次第、[24 §6.5](24_vv_plan.md) の T2-5 に
  「AI Bridge ON のままキャプチャ欠落ゼロ + 索引が追随」を追加して測る。

---

## 関連ドキュメント

- [22 ADR-12 / ADR-13](22_architecture_description.md) — AI ポートの 2 層構造と内蔵アダプタ
- [21 SYS-F-11xx](21_system_requirements.md) — AI ブリッジの現行要求
- [04_api.md](04_api.md) — 現行のブリッジ protocol v1 と MCP ツール
- [24 §6.5](24_vv_plan.md) — 実機検証の実施状況と限界
