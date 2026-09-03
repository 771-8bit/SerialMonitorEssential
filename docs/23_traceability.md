# トレーサビリティ・マトリクス (Traceability)

## 目的

**ニーズ → 要求 → 実装要素 → 検証**の連鎖を 1 枚で確認できるようにする。
目的は 2 つ:

1. ある要求を変更するとき、**どのコードとどのテストを直すべきか**を即座に引けること。
2. **どこが検証されていないか（GAP）** を隠さずに可視化し、次のテスト整備の入力にすること。

## スコープと限界

- 検証列には**現存するもののみ**を書く。存在しないテストを「予定」として書かない。
- 埋められない行には `GAP-xx` を明記する。「多分カバーされている」は書かない。
- テスト名は `src-tauri` 内の `#[test]` 関数名、および `src` 内の vitest ファイル名を実際に参照している。
- E2E 列は [07_plotter_spec.md](07_plotter_spec.md) の 2026-09-03 検証節に記録された実施項目を指す。

## 関連文書

- [20_user_needs.md](20_user_needs.md) / [21_system_requirements.md](21_system_requirements.md) / [22_architecture_description.md](22_architecture_description.md) / [24_vv_plan.md](24_vv_plan.md)

## 凡例

| 記号 | 意味 |
|------|------|
| `UT` | Rust 単体テスト（`cargo test --lib`）。関数名を記載 |
| `PROP` | Rust プロパティベーステスト（`proptest`）。関数名を記載 |
| `FE` | フロントエンド単体テスト（vitest）。ファイル名を記載 |
| `E2E` | com0com 仮想 COM ペア + UIA 自動操作による実演確認（[07 検証節](07_plotter_spec.md#検証)） |
| `INSP` | コード / 設定のレビューによる確認 |
| `GAP-xx` | 自動検証が存在しない。§4 の一覧を参照 |

### 現在のテスト規模（作業ツリー時点）

| 種別 | 件数 | 内訳 |
|------|------|------|
| Rust 例示テスト `#[test]` | **183** | aggregator 37 / data_store 36 / parser 31 / **bridge 27** / thread 22 / logger 8 / worker 8 / chunk 6 / serial::mod 6 / state_transition 2 |
| Rust プロパティテスト `proptest` | **11** | aggregator 7 / parser 3 / data_store 1 |
| Rust 合計（`cargo test --lib` 実行結果） | **194 passed** | |
| vitest | **167 passed** | 10 ファイル（scrollUtils / LineChart / PlotterWindow / PlotterViewFsm / HexViewer / SendPanel / useByteScroll / **App** / **SettingsPanel** / **calculateYRange**） |

> [07_plotter_spec.md](07_plotter_spec.md) の 2026-09-03 時点の記録は Rust 114 件。以後、
> 絶対時刻整列グリッド関連の例示テスト 4 件（`test_quantize_bucket_width_125`,
> `test_aligned_buckets_are_stable_across_sliding_windows`, `test_aligned_cells_on_absolute_grid`,
> `test_realtime_raw_points_not_lost`）と、`proptest` によるプロパティテストが追加されている。
>
> **2026-09-03（AI Bridge / GAP 解消回）の増分**: Rust +66（`bridge.rs` 27 = プロトコル単体 + 実ソケット結合、
> `logger_thread.rs` のエラー通知 3、ほか集約・ストア・パーサの補強）、
> vitest +23（`App.test.tsx` 6 / `SettingsPanel.test.tsx` 7 / `calculateYRange.test.ts` 10）。

#### プロパティテスト一覧

`src-tauri/Cargo.toml` の `[dev-dependencies]` に `proptest` を追加して実装されている。

| 関数 | 場所 | 守っている性質 |
|------|------|----------------|
| `prop_chunking_invariance` | `plotter/parser.rs` | 任意位置・任意個に分割して与えた結果が、一括で与えた結果と同一（UTF-8 文字の途中・CRLF の途中での分割を含む） |
| `prop_parser_never_panics_on_arbitrary_bytes` | `plotter/parser.rs` | 任意のバイト列で panic せず、`channel_order` が非空かつ `channels` と整合する |
| `prop_numeric_roundtrip` | `plotter/parser.rs` | CSV 数値列がビット単位で往復する（精度の欠落・捏造がない） |
| `prop_minmax_envelope_preserved` | `plotter/aggregator.rs` | 集約後に報告される極値が、生データの極値と**完全一致**する（スパイクが消えない。誤差許容なし） |
| `prop_invariant_min_le_avg_le_max` | `plotter/aggregator.rs` | 全出力点で `min ≤ avg ≤ max`、かつ ts が申告レンジ内に収まる |
| `prop_timestamps_sorted` | `plotter/aggregator.rs` | ts は非減少、`aligned_data[0]` は狭義単調増加、全チャンネル列が同一長 |
| `prop_version_monotonic` | `plotter/aggregator.rs` | `data_version` が後退しない（`clear` を含む任意の操作列で） |
| `prop_aligned_realtime_stability` | `plotter/aggregator.rs` | 任意のスライド量・データ分布・ピクセル幅について、両ウィンドウの内部セルが `(ts, min, max, avg)` まで一致 |
| `prop_count_conservation` | `plotter/aggregator.rs` | 再集約の前後で `count` の総和が保存される（誤差許容なし） |
| `prop_batch_equivalence` | `plotter/aggregator.rs` | N 点の一括投入と 1 点ずつの投入が一致する（SYS-NF-604） |
| `prop_get_data_split_read_consistency` | `serial/data_store.rs` | 任意の分割読み出しの連結が一括読み出しと一致する（INV-13 の発見に寄与） |

---

## 1. ニーズ → 要求（UN → SYS）

| UN | ニーズ要約 | 対応する SYS 要求 |
|----|-----------|-------------------|
| UN-01 | 表示の定常性 | SYS-F-501, 502, 504, 511, 512, 515, 522, 523, 705, SYS-NF-107, 205, 602 |
| UN-02 | ストリーム停止の可視化 | SYS-F-503 |
| UN-03 | スパイクを消さない / Y 即時拡大 | SYS-F-513, 514, 521, 525, 710, SYS-NF-601 |
| UN-04 | 時間を止めて調べる | SYS-F-601, 602, 603, 604, 606 |
| UN-05 | 現在のモードが分かる | SYS-F-605, 607, 901, 905, SYS-NF-301, 303 |
| UN-06 | 状態と数値を同一時間軸で | SYS-F-706, 707, 708, SYS-NF-304 |
| UN-07 | 生データ確認 | SYS-F-301, 302, 305, 306, 307, 309, 207, 208, 211 |
| UN-08 | Hex/ASCII でも位置を失わない | SYS-F-303 |
| UN-09 | 送信 | SYS-F-401〜407, SYS-F-104 |
| UN-10 | 取りこぼしゼロ | SYS-F-108, 201, 202, 203, 204, 205, SYS-NF-101, 203 |
| UN-11 | Clear の意味論 | SYS-F-801, 802, 806 |
| UN-12 | 再接続でセッションが切り替わる | SYS-F-109, 803, 804 |
| UN-13 | エクスポート | SYS-F-210, 904 |
| UN-14 | ディスクを汚さない | SYS-F-209 |
| UN-15 | 12 Mbps でも固まらない | SYS-F-103, 206, 304, 516, SYS-NF-102, 105, 106 |
| UN-16 | メモリが増え続けない | SYS-F-202, 711, 712, SYS-NF-103, 104, 108, 405 |
| UN-17 | 誤操作耐性 | SYS-F-105, 106, 308, 402, 406, 608, 702, 704, 709, 804, 805, SYS-NF-202 |
| UN-18 | 閉じたら終わる | SYS-F-902, 903 |
| UN-19 | 切断検知 | SYS-F-107 |
| UN-20 | Windows / com0com / 実機 | SYS-F-101, 102, SYS-NF-501, 502 |
| UN-21 | 複数インスタンス | SYS-NF-503 |
| UN-22 | 自動で回帰を止める | SYS-NF-401, 402, 403 |
| UN-23 | 設計理由が残る | SYS-NF-404 |
| UN-24 | AI 協調デバッグ（AI が同じセッションを読み書き、送信は人間に可視） | SYS-F-1101, 1102, 1103, 1104, 1105, 1106 |

---

## 2. 要求 → 実装 → 検証

### 2.1 シリアル接続 (SYS-F-1xx)

| SYS | 要求要約 | 実装要素 | 検証 |
|-----|----------|----------|------|
| SYS-F-101 | ポート列挙と識別名 | `serial/mod.rs::list_ports` | `E2E`（COM15/COM16 の選択）、`INSP` / **GAP-13**（単体テストなし） |
| SYS-F-102 | 通信パラメータ指定 | `serial/port.rs::SerialPort::new` | `E2E`、**GAP-13** |
| SYS-F-103 | 12 Mbps 級ボーレート | `serial/port.rs::SerialPort::new` | `E2E`（[07 7-8](07_plotter_spec.md#7-8-パフォーマンスチューニング)） |
| SYS-F-104 | DTR/RTS 駆動と動的変更 | `serial/port.rs::write_dtr` / `write_rts`、`serial/mod.rs::write_dtr` / `write_rts`、`src/App.tsx`（config 変更時の反映） | `E2E`（DTR/RTS トグル）、`INSP` |
| SYS-F-105 | 不正設定の事前拒否 | `serial/port.rs::SerialPort::new`（baud 0 チェック、data_bits/parity/stop_bits の照合）、`src/components/SettingsPanel.tsx`（入力検証） | `INSP`、**GAP-13** |
| SYS-F-106 | オープン失敗で旧データを守る | `serial/mod.rs::open_port`（`SerialPort::new` 成功後に旧ストアを破棄） | `INSP`、**GAP-14** |
| SYS-F-107 | 致命的エラーで切断通知 + 2 秒間隔の能動再列挙 | `serial/worker_thread.rs`（`serial-status` emit）、`src/App.tsx`（listen、`PORT_POLL_INTERVAL_MS = 2000` の `list_ports` ポーリング、`close_port` による再同期） | `FE`: `src/test/App.test.tsx`（`polls list_ports on an interval so hotplugged devices appear`, `stops polling once unmounted`, `releases the backend port handle when a disconnect is detected`, `still releases the handle when the disconnect carries no error text`, `does not close the port on a connected=true status`）/ `E2E` — **GAP-07 / GAP-08 解消（2026-09-03）** |
| SYS-F-108 | 非致命エラーで受信継続 | `serial/port.rs::read`（TimedOut を 0 バイト扱い） | `INSP` |
| SYS-F-109 | 再オープン時のハンドル解放順序 | `serial/mod.rs::open_port`（`stop_reception` を先に実行） | `E2E`（切断→再接続）、`INSP` |

### 2.2 受信・保存 (SYS-F-2xx)

| SYS | 要求要約 | 実装要素 | 検証 |
|-----|----------|----------|------|
| SYS-F-201 | 全バイト保持 | `serial/worker_thread.rs`、`serial/chunk.rs`、`serial/data_store.rs` | `UT`: `test_chunk_push_data`, `test_chunk_full`, `test_chunk_partial_write`, `test_chunk_empty_data` / `E2E` / `test_tools/verify_received_data.py` |
| SYS-F-202 | ディスク退避 | `serial/logger_thread.rs::spawn_logger_thread` / `process_buffer` | `UT`: `test_process_buffer_no_flush`, `test_process_buffer_threshold_flush`, `test_process_buffer_force_flush`, `test_process_buffer_with_file_io`, `test_spawn_logger_thread_integration` |
| SYS-F-203 | 2 記憶域をまたぐ読み出し | `serial/data_store.rs::get_data` | `UT`: `test_get_data_from_archived_only`, `test_get_data_from_finished_list_only`, `test_get_data_archived_then_finished`, `test_get_data_spanning_archived_and_finished`, `test_get_data_exact_boundary`, `test_get_data_multiple_archived_chunks`, `test_get_data_multiple_finished_chunks`, `test_get_data_partial_read_within_chunk`, `test_get_data_zero_length`, `test_get_data_empty_store`, `test_get_data_offset_out_of_range` |
| SYS-F-204 | どちらにも無い瞬間を作らない (INV-3) | `serial/logger_thread.rs::process_buffer`（書き込み → 索引公開 → pop） | `INSP`（コメントで順序を固定）／ **GAP-15**（順序を破ると落ちるテストがない） |
| SYS-F-205 | I/O 失敗でも失わない + `log-error` 通知 | `serial/logger_thread.rs::process_buffer`（`set_len` によるロールバック）、同 `on_error` コールバック（`Box<dyn Fn(String) + Send>`、5 秒レート制限）、`serial/data_store.rs`（`log-error` の emit）、`src/App.tsx`（利用者への提示） | `UT`: `test_process_buffer_io_error_keeps_chunks`, `test_spawn_logger_thread_notifies_error_rate_limited`, `test_spawn_logger_thread_notifies_open_failure` / `FE`: `src/test/App.test.tsx`（`surfaces log-error events to the user`） — **GAP-09 解消（2026-09-03）** |
| SYS-F-206 | 16 ms 間引き通知 | `serial/ui_notifier.rs::spawn_ui_notifier_thread` | `INSP` / **GAP-16** |
| SYS-F-207 | 時刻索引 | `serial/data_store.rs::record_timestamp` / `get_timestamp_for_offset`、`serial/ui_notifier.rs` | `UT`: `test_record_timestamp_basic`, `test_record_timestamp_skip_duplicate`, `test_record_timestamp_with_data`, `test_get_timestamp_for_offset_empty`, `test_get_timestamp_for_offset_exact_match`, `test_get_timestamp_for_offset_binary_search`, `test_get_timestamp_for_offset_before_first`, `test_clear_timestamps` |
| SYS-F-208 | 行索引 | `serial/worker_thread.rs::record_line_offsets`、`serial/data_store.rs::get_line_offsets` | `UT`: `test_record_line_offsets_basic`, `_with_global_offset`, `_no_duplicates`, `_empty`, `_no_newlines`, `_consecutive_newlines`, `_ignores_cr`, `_crlf`, `test_get_line_offsets`, `test_get_line_offsets_out_of_range`, `test_get_line_offsets_last_line`, `test_get_line_offsets_partial_count`, `test_total_lines_initial`, `test_clear_lines` |
| SYS-F-209 | インスタンス単位の temp (INV-10) | `serial/data_store.rs::DataStore::new`（`INSTANCE_COUNTER`）、`cleanup_stale_directories` | `E2E`（ログで `<pid>/<n>` の削除を確認）／ **GAP-17**（自動テストなし） |
| SYS-F-210 | バイナリエクスポート | `serial/mod.rs::export_log`（1 MB チャンク書き出し） | `INSP` / **GAP-18** |
| SYS-F-211 | クリップボード | `serial/mod.rs::get_clipboard_text`、`src/App.tsx::handleCopy`（10 MB 警告） | `INSP` / **GAP-18** |

### 2.3 表示 (SYS-F-3xx)

| SYS | 要求要約 | 実装要素 | 検証 |
|-----|----------|----------|------|
| SYS-F-301 | Hex ダンプ | `serial/mod.rs::get_display_rows` / `bytes_to_hex`、`src/components/viewers/HexViewer.tsx` | `UT`: `test_bytes_to_hex` / `FE`: `src/components/HexViewer.test.ts` |
| SYS-F-302 | ASCII 表示と制御文字可視化 | `serial/mod.rs::byte_to_ascii` / `bytes_to_ascii` / `get_ascii_lines`、`src/components/viewers/AsciiViewer.tsx` | `UT`: `test_byte_to_ascii_printable`, `test_byte_to_ascii_special_chars`, `test_byte_to_ascii_other_non_printable`, `test_bytes_to_ascii` |
| SYS-F-303 | Hex↔ASCII のオフセット維持 | `src/components/ReceivePanel.tsx`（`scrollOffset` を親で保持）、`src/components/viewers/scrollUtils.ts` | `FE`: `src/test/scrollUtils.test.ts` / `E2E`（Hex↔ASCII 切替） |
| SYS-F-304 | バックエンド駆動ページング | `serial/mod.rs::get_display_rows` / `get_ascii_lines`、`src/components/viewers/useByteScroll.ts` | `FE`: `src/components/viewers/useByteScroll.test.ts` / `INSP` |
| SYS-F-305 | DOM 高さ制限のスケーリング | `src/components/viewers/scrollUtils.ts`（`calculateScale`, `calculateScrollHeight`） | `FE`: `src/test/scrollUtils.test.ts`, `src/components/HexViewer.test.ts`（1GB シナリオ） |
| SYS-F-306 | Auto Scroll | `src/components/viewers/useByteScroll.ts` | `FE`: `useByteScroll.test.ts`（`handles auto-scroll correctly`, `anchors position when data grows`）/ `E2E` |
| SYS-F-307 | 表示オプションの即時反映 | `src/components/viewers/AsciiViewer.tsx`（`showTimestamp` 変更時の強制再取得） | `E2E`（Timestamp トグル）／ **GAP-19** |
| SYS-F-308 | in-flight 中の再取得を捨てない | `src/components/viewers/AsciiViewer.tsx` / `HexViewer.tsx`（ペンディングキュー、force 保持） | `E2E`（高速スクロール）／ **GAP-19** |
| SYS-F-309 | 検索・フィルタ | `src/components/ReceivePanel.tsx`（UI のみ） | **GAP-01**（未実装） |

### 2.4 送信 (SYS-F-4xx)

| SYS | 要求要約 | 実装要素 | 検証 |
|-----|----------|----------|------|
| SYS-F-401 | テキスト/Hex 送信 | `serial/mod.rs::write_data`、`serial/port.rs::write`、`src/components/SendPanel.tsx`、`src/utils/sendUtils.ts` | `FE`: `src/components/SendPanel.test.ts` / `E2E`（ループバック） |
| SYS-F-402 | 不正 Hex の拒否 | `src/utils/sendUtils.ts`（厳密検証） | `FE`: `SendPanel.test.ts`（`returns null for invalid hex characters`, `odd number of characters`） |
| SYS-F-403 | 改行コード選択 | `src/utils/sendUtils.ts` | `FE`: `SendPanel.test.ts`（`appends CR/LF/CRLF`） |
| SYS-F-404 | 全バイト送出 | `serial/port.rs::write`（`write_all`） | `INSP` / **GAP-20** |
| SYS-F-405 | 送信時のタイムアウト拡張 | `serial/port.rs::write`（baud からの見積り、復元） | `INSP` / **GAP-20** |
| SYS-F-406 | 送信履歴と編集操作の共存 | `src/components/SendPanel.tsx` | `FE`: `SendPanel.test.ts`（一部）／ **GAP-21**（↑↓ とカーソル移動の分岐は未テスト） |
| SYS-F-407 | Enter の挙動切替 | `src/components/SendPanel.tsx` | `E2E` / **GAP-21** |

### 2.5 プロッタ LIVE (SYS-F-5xx)

| SYS | 要求要約 | 実装要素 | 検証 |
|-----|----------|----------|------|
| SYS-F-501 | 固定幅ウィンドウ（8 段階） | `src/components/plotter/PlotterWindow.tsx`（Window セレクタ + `buildRequest`） | `UT`: `PlotterWindow.test.tsx`（ウィンドウ選択・明示レンジ要求）/ `E2E`: 10s/30s 実機確認（2026-09-03） |
| SYS-F-502 | 右端＝今、毎フレーム前進 | `PlotterWindow.tsx`（`rightEdgeRef` + rAF）+ `LineChart.tsx::setXWindow` | `UT`: `PlotterWindow.test.tsx`（フレーム毎スクロール・IPC 無し）/ `E2E` |
| SYS-F-503 | 無データ時もウィンドウが進む | `PlotterWindow.tsx`（空ペイロード時に旧データ保持のまま窓のみ前進） | `UT`: 空ウィンドウ維持テスト / `E2E`: ストリーム停止で右端空白が成長することを実機確認 |
| SYS-F-504 | 既描画ピクセルが組み変わらない (INV-7) | `plotter/aggregator.rs::aggregate_buckets_aligned` + フロントの明示レンジ要求 | `UT`: `test_aligned_buckets_are_stable_across_sliding_windows` / `PROP`: **`prop_aligned_realtime_stability`** |
| SYS-F-511 | 絶対時刻整列バケット | `plotter/aggregator.rs::aggregate_buckets_aligned`、`get_ranged_data` の realtime 分岐 | `UT`: `test_aligned_cells_on_absolute_grid`, `test_realtime_raw_points_not_lost`, `test_get_ranged_data_realtime_mode_continuous` / `PROP`: `prop_aligned_realtime_stability` |
| SYS-F-512 | 1-2-5 量子化 | `plotter/aggregator.rs::quantize_bucket_width_125` | `UT`: `test_quantize_bucket_width_125` |
| SYS-F-513 | min/max/加重平均/count 保持 | `plotter/aggregator.rs::AggregatedBucket` / `to_point` | `UT`: `test_minmax_preservation_after_reaggregate`, `test_mode_switching_preserves_data`, `test_mode_switch_during_reception_no_rebuild`, `test_average_minmax_aggregation` / `PROP`: **`prop_minmax_envelope_preserved`**（極値の完全一致） |
| SYS-F-514 | Average の中心線は加重平均 | `plotter/data_store.rs::AggregatedPoint::MinMax`（`avg` フィールド）、`aggregator.rs` | `UT`: `test_average_minmax_aggregation`, `test_aggregate_buckets_preserving_correctness` |
| SYS-F-515 | LTTB は静的範囲専用 | `plotter/aggregator.rs::get_ranged_data`（`is_realtime` 分岐）、`lttb_downsample` | `UT`: `test_lttb_mode_uses_lttb_algorithm`, `test_lttb_preserves_peak`, `test_lttb_downsample_small_data`, `test_lttb_downsample_preserves_endpoints` / `INSP`（分岐の意図） |
| SYS-F-516 | ピクセル幅上限 4000 点 | `plotter/data_store.rs::PlotterConfig::max_target_points`、`aggregator.rs::get_ranged_data` | `UT`: `test_get_ranged_data_large_dataset`, `test_cache_invalidation_on_pixel_width_change` |
| SYS-F-521 | Y 軸の即時拡大 | `LineChart.tsx::setXWindow` 内ヒステリシス | `E2E`（目視: スパイク時の即拡大）/ 詳細な数値検証は GAP-22 参照 |
| SYS-F-522 | Y 軸の縮小ヒステリシス（60% / 3 秒） | `LineChart.tsx::setXWindow` 内ヒステリシス | `E2E`（実機で毎フレーム再スケールの消失を確認）/ タイミングの単体検証は GAP-22 参照 |
| SYS-F-523 | Y 軸端の 1-2-5 量子化 | `LineChart.tsx`（nice-range 計算） | `E2E`（-20〜120 等の量子化レンジを実機確認） |
| SYS-F-524 | 非表示チャンネルを Y レンジに含めない | `src/components/plotter/LineChart.tsx::calculateYRange`（hidden を除外。テストのため export 済） | `FE`: `src/test/calculateYRange.test.ts`（`ignores the band of a hidden channel`, `returns the empty sentinel range when nothing is visible`）/ `E2E`（凡例クリックで Y 再スケール） — **GAP-22 の `calculateYRange` 分は解消（2026-09-03）** |
| SYS-F-525 | min/max バンドを Y レンジに含める | `src/components/plotter/LineChart.tsx::calculateYRange`（表示中チャンネルのバンド min/max を畳み込む） | `FE`: `src/test/calculateYRange.test.ts`（10 件。`extends the range when the band is wider than the values`, `keeps the value range when the band is narrower`, `only folds in band samples inside the x window`, `ignores null entries inside the band arrays` ほか） — **GAP-04 解消（2026-09-03）** |

### 2.6 プロッタ表示状態 (SYS-F-6xx)

| SYS | 要求要約 | 実装要素 | 検証 |
|-----|----------|----------|------|
| SYS-F-601 | LIVE/Inspect/Paused の 3 状態 | `PlotterWindow.tsx`（`isRunning × isFollowing`） | `ST`: `PlotterViewFsm.test.tsx`（49 ペア） |
| SYS-F-602 | ズーム/パンで Inspect へ自動遷移 | `LineChart.tsx`（`onUserInteraction`） | `ST`: `PlotterViewFsm.test.tsx` / `E2E`: ホイールで Inspect 遷移を実機確認 |
| SYS-F-603 | 過去へのスクロールバック | `PlotterWindow.tsx`（Inspect 時の `onTimeRangeChange` → 明示レンジ取得） | `UT`: inspect-range フェッチテスト |
| SYS-F-604 | Inspect で範囲が動かない | `PlotterWindow.tsx`（Inspect で rAF ループ停止） | `ST`: `PlotterViewFsm.test.tsx`（Inspect 中 DataTick が無反応） |
| SYS-F-605 | ▶LIVE ボタンとダブルクリック | `PlotterWindow.tsx`（▶LIVE ボタン）+ `LineChart.tsx`（`onLiveRequest`） | `ST` / `E2E`: ▶LIVE 復帰を実機確認 |
| SYS-F-606 | Paused でポーリング停止・収集継続 | `PlotterWindow.tsx::toggleRunning`（`set_plotter_enabled` を呼ばない） | `FE`: `src/test/PlotterWindow.test.tsx`（`displays pause/play button`）/ `E2E`（Pause↔Resume） |
| SYS-F-607 | フッターに状態常時表示 | `PlotterWindow.tsx` フッター（● LIVE / 🔍 Inspect / ⏸ Paused） | `ST`: 全遷移でラベル検証 / `E2E` |
| SYS-F-608 | 任意タイミングの遷移で壊れない | 全体 | `E2E`（[07 モード切替の総当たり](07_plotter_spec.md#検証)）／ `UT`: `zero_switch_all_events`, `one_switch_all_event_pairs`（`state_transition_tests.rs`）/ `FE`: `src/test/PlotterViewFsm.test.tsx` |
| SYS-F-609 | Resume → LIVE | `PlotterWindow.tsx`（Resume 時の再アンカー。Paused-from-Inspect からは Inspect へ復帰 = TBD-R1 は「元の状態へ戻る」で暫定確定） | `ST`: `PlotterViewFsm.test.tsx` |

### 2.7 パース / State Timeline (SYS-F-7xx)

| SYS | 要求要約 | 実装要素 | 検証 |
|-----|----------|----------|------|
| SYS-F-701 | CR/LF/CRLF 対応 | `plotter/parser.rs` | `UT`: `test_parse_lf_only`, `test_parse_cr_only`, `test_parse_single_value` |
| SYS-F-702 | 区切りの行ごと再判定 | `plotter/parser.rs` | `UT`: `test_banner_line_does_not_lock_separator`, `test_separator_can_change_between_lines`, `test_parse_tab_separated`, `test_parse_csv` |
| SYS-F-703 | ラベル付き値 / 自動ラベル | `plotter/parser.rs` | `UT`: `test_parse_labeled_value`, `test_parse_multiple_labeled_values` / `PROP`: `prop_parser_never_panics_on_arbitrary_bytes` |
| SYS-F-704 | ヘッダー検出（数値受信前のみ） | `plotter/parser.rs` | `UT`: `test_parse_header_detection` |
| SYS-F-705 | CSV 列順の保持 | `plotter/parser.rs::ParsedDataPoint::channel_order`、`aggregator.rs::add_data_points_batch` | `UT`: `test_data_ordering_preserved`, `test_multiple_channels_independent` |
| SYS-F-706 | 数値と状態の混在 | `plotter/parser.rs::ChannelValue`、`aggregator.rs::state_data` | `UT`: `test_parse_state_value`, `test_parse_mixed_state_and_numeric`, `test_state_change_recording` |
| SYS-F-707 | 状態のみでも描画 | `aggregator.rs::get_chart_data`（x レンジのみのペイロード）、`PlotterWindow.tsx::hasChartContent` | `E2E`（state のみのストリーム）／ `FE`: `src/test/PlotterWindow.test.tsx`（間接）／ **GAP-23** |
| SYS-F-708 | 同一時間軸での State Timeline | `src/components/plotter/stateTimelinePlugin.ts`（同一 uPlot インスタンス内で描画） | `E2E`（ライン 4ch + state 2ch 同時描画）／ `FE`: `src/test/LineChart.test.tsx`（`state row structure is valid`） |
| SYS-F-709 | 不正行 / NaN の除外 | `plotter/parser.rs` | `UT`: `test_parse_error_skip`, `test_parse_nan_excluded`, `test_parse_negative_numbers` / `PROP`: `prop_parser_never_panics_on_arbitrary_bytes`, `prop_numeric_roundtrip` |
| SYS-F-710 | バッチ内タイムスタンプ分散 (INV-9) | `plotter/thread.rs::run`（`prev = max(candidate, last_batch_ts)`） | `UT`: `test_batch_timestamps_are_spread` |
| SYS-F-711 | 改行なしバッファの 64 KB 上限 | `plotter/parser.rs` | `INSP` / **GAP-24** |
| SYS-F-712 | state_data の 10,000 件上限 | `plotter/aggregator.rs`（`MAX_STATE_CHANGES`） | `INSP` / **GAP-24** |

### 2.8 データ管理 (SYS-F-8xx)

| SYS | 要求要約 | 実装要素 | 検証 |
|-----|----------|----------|------|
| SYS-F-801 | Clear はストア差し替え | `serial/mod.rs::clear_data` | `E2E`（Clear 後にプロットがリセットされ継続更新）／ **GAP-14** |
| SYS-F-802 | Clear でプロッタも同期リセット (INV-2) | `plotter/thread.rs::run`（世代変化検知で `aggregator.clear()`） | `UT`: **`test_thread_survives_store_swap`** / `E2E` |
| SYS-F-803 | 再接続も同じ意味論 | `serial/mod.rs::open_port` | `E2E`（切断→再接続でリセットして継続）／ **GAP-14** |
| SYS-F-804 | 10 ms 以内の世代追従 (INV-1) | `plotter/thread.rs::run`（`Arc::ptr_eq` による識別、10 ms ポーリング） | `UT`: `test_thread_survives_store_swap`, `test_thread_sleeps_between_polls`, `test_thread_reads_data_from_store` |
| SYS-F-805 | ストア不在でも動作継続 | `plotter/thread.rs::run`（`None` 分岐で 50 ms 待機） | `UT`: `test_aggregator_enabled_empty_data_store`, `test_parser_is_new_each_thread` / `E2E`（プロッタを先に開いてから接続） |
| SYS-F-806 | ポート閉状態の Clear | `serial/mod.rs::clear_data`（`None` にする分岐） | `INSP` / **GAP-14** |

### 2.9 ウィンドウ・ライフサイクル (SYS-F-9xx)

| SYS | 要求要約 | 実装要素 | 検証 |
|-----|----------|----------|------|
| SYS-F-901 | プロッタは単一インスタンス | `lib.rs::open_plotter_window`（既存ウィンドウにフォーカス） | `INSP` / **GAP-25** |
| SYS-F-902 | X 閉じでバックエンドが後始末 (INV-5) | `lib.rs::run` の `on_window_event`（`Destroyed` / label=plotter） | `E2E`（X 閉じでスレッド停止・収集停止をログ確認）／ **GAP-25** |
| SYS-F-903 | メイン閉で全終了 (INV-6) | `lib.rs::run` の `on_window_event`（`Destroyed` / label=main） | `E2E`（exit 0、vite ポート 1420 解放を確認）／ **GAP-25** |
| SYS-F-904 | 切断後も閲覧・エクスポート可 | `serial/mod.rs::close_port`（ストアを保持） | `INSP` / **GAP-14** |
| SYS-F-905 | ドッキング / フロート | （未実装。**スコープ内・実装予定**。オーナー決定 2026-09-03） | **GAP-06** |

### 2.10 AI ブリッジ / MCP (SYS-F-11xx)

| SYS | 要求要約 | 実装要素 | 検証 |
|-----|----------|----------|------|
| SYS-F-1101 | 既定 OFF・`127.0.0.1` 限定・トークン任意 | `src-tauri/src/bridge.rs`（`BridgeServer`、`Ipv4Addr::LOCALHOST` バインド、`DEFAULT_BRIDGE_PORT = 57320`、`MAX_CONNECTIONS = 4`）、`bridge_set` / `bridge_status` コマンド、`src/components/SettingsPanel.tsx`（AI Bridge トグル） | `UT`: `test_server_binds_loopback_only`, `test_integration_connection_limit`, `test_auth_required_when_token_configured`, `test_auth_is_noop_without_token` / `FE`: `src/test/SettingsPanel.test.tsx`（`renders the bridge toggle off by default and hides the endpoint`, `has the localhost-only tooltip`, `calls bridge_set and shows the endpoint when enabled`, `calls bridge_set with enabled=false when toggled back off`, `reverts the toggle when the backend rejects the start`, `shows the port reported by the backend, not the hardcoded default`）/ `E2E`（2026-09-03） |
| SYS-F-1102 | 読み出し API（status / tail / read_range / subscribe） | `bridge.rs`（メソッドディスパッチ、`clamp_read_length`、`MAX_READ_LENGTH = 1 MiB`、push ループ 50 ms / 1 フレーム 256 KiB） | `UT`: `test_status_without_store`, `test_status_reports_total_bytes`, `test_read_range_basic_and_clamping`, `test_read_range_bad_params_and_no_store`, `test_tail_default_and_window`, `test_tail_bad_params`, `test_clamp_read_length_caps_at_1mib`, `test_subscribe_params`, `test_integration_status_read_range_tail`, `test_integration_subscribe_pushes_new_data` / `E2E`（TCP 経由で status/tail/read_range と subscribe push を確認、2026-09-03） |
| SYS-F-1103 | 送信 + GUI 可視化（`bridge-activity`） | `bridge.rs`（`send` メソッド、`build_send_payload`、`preview_of`、`record_send` → `bridge-activity` emit）、`SerialState.port` の `Arc` 化、`src/components/SettingsPanel.tsx`（活動表示行） | `UT`: `test_build_send_payload_line_endings`, `test_build_send_payload_base64_and_errors`, `test_preview_of_truncates_to_64_chars`, `test_record_send_updates_activity_and_emits`, `test_send_without_port_is_error`, `test_send_bad_params_before_port_check`, `test_integration_send_without_port` / `FE`: `SettingsPanel.test.tsx`（`renders connection count and last send activity from bridge_status`）/ `E2E`（COM16→COM15 へ `PING`、GUI に「送信 5 bytes HH:MM:SS」、2026-09-03。スクリーンショット取得済） |
| SYS-F-1104 | `ports`（GUI と同一列挙） | `bridge.rs`（`ports` メソッド → `serial::list_ports`） | `UT`: `test_ports_returns_list` / `E2E`（2026-09-03） |
| SYS-F-1105 | MCP アダプタ（プロセス外）の提供 | `mcp/server.mjs`（7 ツール）、`mcp/README.md`（`claude mcp add serial-monitor -- node .../mcp/server.mjs`）、`mcp/package.json` | `T`: `mcp/smoke.mjs`（偽ブリッジに対する結合。status ラウンドトリップ / base64 デコード / 改行付き送信 / `wait_for` のポーリング一致 / アプリ未起動時のメッセージ）/ `E2E`（MCP クライアントから実ブリッジ経由で `PING` → `PONG 42` のラウンドトリップ、2026-09-03） |
| SYS-F-1106 | 世代変化で `reset` フレーム | `bridge.rs`（push ループの世代検知: `Arc` 差し替え or `total_bytes` の巻き戻り） | `UT`: `test_integration_subscribe_emits_reset_on_store_swap` |

補助: `test_parse_request_errors`, `test_unknown_method`, `test_response_json_shapes` が protocol v1 のワイヤ形式（`{id, ok, result/error}`、parse error、未知メソッド）を契約として固定している。

### 2.11 品質要求 (SYS-NF)

| SYS-NF | 要求要約 | 実装要素 | 検証 |
|--------|----------|----------|------|
| SYS-NF-101 | 12 Mbps 60 秒で欠落 0 | 受信パス全体 | `E2E` + `test_tools/verify_received_data.py` / `test_tools/serial_test.py` |
| SYS-NF-102 | 受信中も UI が固まらない | 3 スレッド分離 + バックエンド駆動ページング | `E2E`（[07 7-8 中間確認](07_plotter_spec.md#7-8-パフォーマンスチューニング)） |
| SYS-NF-103 | フロント メモリ 0 MB/s | `PlotterChartPayload`（ADR-08） | `E2E` + `test_tools/monitor_memory.py` / `test_tools/analyze_memory.py`（実測: 修正前 +13.6 MB/s → 修正後 定常） |
| SYS-NF-104 | バックエンド メモリ定常 | `Arc<Chunk>` 解放、集約レベルアップ | `E2E`（約 40 MB）／ **GAP-26**（自動計測なし） |
| SYS-NF-105 | rAF 同期・多重取得なし | `PlotterWindow.tsx::updateLoop`（`isFetchingRef`） | `FE`: `src/test/PlotterWindow.test.tsx` / `INSP` |
| SYS-NF-106 | 軽量バージョン照会 | `aggregator.rs::check_version`、`lib.rs::check_plotter_version` | `UT`: `test_check_version_is_lightweight`, `test_version_increments_on_data_add`, `test_version_increments_on_clear` / `PROP`: `prop_version_monotonic` |
| SYS-NF-107 | 1 kHz で遅延 200 ms 以下 | 受信 → プロッタ → 描画の全経路 | **GAP-10**（未測定） |
| SYS-NF-108 | メモリ上のチャンク量 | `data_store.rs`（`INITIAL_POOL_SIZE`=100、`CHUNK_SIZE`=64KB） | `INSP` / **GAP-26** |
| SYS-NF-201 | panic しない | 全体（`unwrap_or`、poisoned lock 回避） | `UT`（境界値系多数）／ `PROP`: `prop_parser_never_panics_on_arbitrary_bytes`（パーサのみ）／ 他モジュールは **GAP-12** |
| SYS-NF-202 | 複合操作後の回復 | 全体 | `E2E`（Clear×3 + Pause/Resume + モード切替×2 + 切断/再接続の連打）／ `UT`: `one_switch_all_event_pairs`（各遷移対の後に復帰プローブ）/ `FE`: `src/test/PlotterViewFsm.test.tsx`（同） |
| SYS-NF-203 | データ可用性 | `logger_thread.rs::process_buffer` の順序 | `INSP` / **GAP-15** |
| SYS-NF-204 | 読み出し失敗の読み飛ばし | `plotter/thread.rs::run`（`MAX_READ_FAILURES`=500） | `INSP` / **GAP-24** |
| SYS-NF-205 | LIVE の決定性 | `aggregator.rs::aggregate_buckets_aligned` | `UT`: `test_aligned_buckets_are_stable_across_sliding_windows` / `PROP`: **`prop_aligned_realtime_stability`**（任意のスライド量・データ分布） |
| SYS-NF-301 | 状態の常時表示 | `SettingsPanel.tsx`（接続状態）、`PlotterWindow.tsx`（フッター 3 状態） | `INSP` / `ST` |
| SYS-NF-302 | 破壊的操作の確認 | `src/App.tsx::handleCopy`（10 MB 警告） | `INSP` / **GAP-27**（Clear には確認がない） |
| SYS-NF-303 | 無効オプションの明示 | `ReceivePanel.tsx`（`disabled` 属性） | `INSP` |
| SYS-NF-304 | DPI スケーリング | `stateTimelinePlugin.ts`（`devicePixelRatio`） | `E2E`（125/150% での確認）／ **GAP-23** |
| SYS-NF-401 | 全テストゲート | `.github/workflows/rust-ci.yml`, `frontend-ci.yml` | `INSP`（CI 設定）+ CI 実行 |
| SYS-NF-402 | 挙動変更に回帰テスト | 2026-09-03 の追加分 | `INSP`（`test_thread_survives_store_swap`, `test_batch_timestamps_are_spread`, `test_separator_can_change_between_lines`） |
| SYS-NF-403 | ライフサイクルのログ | 各モジュールの `log::info!` | `INSP` |
| SYS-NF-404 | ADR の存在 | [22_architecture_description.md §8](22_architecture_description.md#8-アーキテクチャ決定記録-adr) | `INSP` |
| SYS-NF-405 | フロントで解釈しない | `PlotterWindow.tsx`（変換なしで `setData`） | `INSP` / `FE`: `src/test/LineChart.test.tsx`（形式の妥当性） |
| SYS-NF-501 | プラットフォーム抽象の使用 | `serialport`, `std::env::temp_dir`, `sysinfo` | `INSP` / CI（ubuntu / windows マトリクス） |
| SYS-NF-502 | com0com での E2E | 検証環境 | `E2E`（COM15⇔COM16） |
| SYS-NF-503 | 複数インスタンスの分離 | `data_store.rs`（PID + instance） | `INSP` / **GAP-17** |
| SYS-NF-504 | 正式対応 OS（Windows 10/11 x64・Ubuntu 22.04+ x64） | `.github/workflows/ci.yml`（test-linux ジョブ）+ `serial/mod.rs::extract_port_path` | `UT`: `test_extract_port_path` / `CI`: Linux ジョブ / `E2E`: WSL2 スモーク（2026-09-03） |
| SYS-NF-505 | Linux 配布（deb/AppImage、dialout 権限の案内） | `.github/workflows/release.yml`（build-linux ジョブ）+ README「Linux での利用」 | `INSP` / リリース時の Tier 2 |
| SYS-NF-601 | min ≤ avg ≤ max、count 保存 | `aggregator.rs` | `UT`: `test_aggregate_buckets_preserving_correctness`, `test_level_up_preserves_data_range`, `test_minmax_preservation_after_reaggregate`, `test_rebuild_aggregation`, `test_dynamic_level_creation`, `test_data_accumulation` / `PROP`: **`prop_invariant_min_le_avg_le_max`**, **`prop_minmax_envelope_preserved`** |
| SYS-NF-602 | タイムスタンプ単調性 | `aggregator.rs`, `plotter/thread.rs` | `UT`: `test_data_ordering_preserved`, `test_chart_data_timestamps_aligned`, `test_batch_timestamps_are_spread` / `PROP`: **`prop_timestamps_sorted`**（列長の一致も検査） |
| SYS-NF-603 | チャンク分割不変性 | `plotter/parser.rs`（バイトレベル 1 パス） | `UT`: `test_parse_incomplete_line` / `PROP`: **`prop_chunking_invariance`**（任意位置・任意個の分割、UTF-8 / CRLF 途中を含む）, `prop_numeric_roundtrip` |
| SYS-NF-604 | バッチ等価性 | `aggregator.rs::add_data_points_batch` | `UT`: `test_batch_processing_equals_individual`, `test_add_data_point`, `test_direct_data_point_addition`, `test_aggregator_clone_is_shared` ／ プロパティ化は **GAP-12**（P-7） |

### 2.12 検証にのみ現れる補助テスト

要求に直接対応しないが、契約を固定しているテスト。削除すると回帰検知が弱まる。

| テスト | 固定している契約 |
|--------|------------------|
| `test_thread_start_stop`, `test_thread_multiple_start_stop`, `test_thread_drop_stops_cleanly`, `test_stop_flag_atomic_ordering` | PlotterThread のライフサイクル（SM-3）。`Drop` での停止、多重 start/stop の冪等性 |
| `test_aggregator_receives_no_data_when_disabled` | `enabled=false` の間はデータを蓄積しない（プロッタ未使用時のメモリ削減） |
| `test_get_chart_data_format`, `test_chart_data_null_handling`, `test_chart_data_band_data`, `test_chart_data_empty` | `PlotterChartPayload` の形式契約（ADR-08）。フロント側 `src/test/LineChart.test.tsx` と対になる |
| `test_time_range_filtering_boundary`, `test_time_range_filtering_correctness`, `test_get_ranged_data` | 時間範囲抽出の境界値（Inspect の基盤） |
| `test_chunk_new`, `test_chunk_clear` | Chunk の初期状態と再利用時の契約 |
| `test_total_bytes_empty`, `test_total_bytes_from_finished_list`, `test_total_bytes_from_archived_and_finished` | INV-4（total_bytes の一貫性） |
| `src/test/scrollUtils.test.ts` | バイトオフセット ⇔ scrollTop 変換の可逆性（SYS-F-303 / 305 の基盤） |

---

## 3. 不変条件 → 検証

[22_architecture_description.md §5.8](22_architecture_description.md#58-不変条件) の不変条件が、何によって守られているか。

| INV | 検証 |
|-----|------|
| INV-1（10 ms 以内の世代追従） | `UT`: `test_thread_survives_store_swap`, `test_thread_sleeps_between_polls` |
| INV-2（Clear 後は新世代のみ） | `UT`: `test_thread_survives_store_swap` / `E2E` |
| INV-3（常にどちらかに存在） | **GAP-15**（`INSP` のみ） |
| INV-4（total_bytes 単調非減少） | `UT`: `test_total_bytes_*` 系（静的な一貫性のみ。並行実行下は **GAP-15**） |
| INV-5（ウィンドウ Closed ⇒ スレッド Stopped） | `E2E` / **GAP-25** |
| INV-6（メイン閉で全終了） | `E2E` / **GAP-25** |
| INV-7（確定セルの不変性） | `UT`: `test_aligned_buckets_are_stable_across_sliding_windows`, `test_aligned_cells_on_absolute_grid` / `PROP`: **`prop_aligned_realtime_stability`** |
| INV-8（min ≤ avg ≤ max、count 保存） | `UT`: `test_aggregate_buckets_preserving_correctness`, `test_level_up_preserves_data_range` / `PROP`: `prop_invariant_min_le_avg_le_max`, `prop_minmax_envelope_preserved` |
| INV-9（ts 単調非減少） | `UT`: `test_batch_timestamps_are_spread`, `test_data_ordering_preserved` / `PROP`: `prop_timestamps_sorted` |
| INV-10（temp の分離） | `E2E`（ログ確認）／ **GAP-17** |
| INV-11（60 Hz IPC が非ブロッキング） | `INSP`（`async fn` 宣言）／ **GAP-16** |
| INV-12（フロントで解釈しない） | `INSP` / `FE`: `src/test/LineChart.test.tsx` |

---

## 4. GAP 一覧

**未カバーの一覧。これが [24_vv_plan.md](24_vv_plan.md) のテスト整備計画の入力になる。**
優先度は「壊れたときの被害 × 壊れやすさ」で判断する。

### 4.1 機能そのものが未実装（要求はあるが実装がない）

| GAP | 内容 | 関連要求 | 優先度 |
|-----|------|----------|--------|
| **GAP-01** | 検索・フィルタが未実装（UI のみ存在） | SYS-F-309 | 低 |
| **GAP-02** | ~~固定幅スライディングウィンドウが未実装~~ **解消済（2026-09-03）**: フロント主導の明示レンジ要求＋rAF ローカルスクロール実装。UT/E2E で検証 | SYS-F-501〜504 | 解消 |
| **GAP-03** | ~~Y 軸ヒステリシスが未実装~~ **解消済（2026-09-03）**: `LineChart.tsx::setXWindow` に実装。数値タイミングの単体検証は残（GAP-22 に統合） | SYS-F-521〜523 | 解消 |
| **GAP-04** | ~~min/max バンドが Y オートレンジに含まれない~~ **解消済（2026-09-03）**: `LineChart.tsx::calculateYRange` が表示中チャンネルのバンド min/max を畳み込む（関数を export して単体テスト可能に）。`src/test/calculateYRange.test.ts` 10 件で検証（非表示チャンネル除外・X ウィンドウ外除外・null 混入を含む） | SYS-F-525 | 解消 |
| **GAP-05** | ~~3 状態モデルが未実装~~ **解消済（2026-09-03）**: LIVE/Inspect/Paused 実装、FSM テスト（`PlotterViewFsm.test.tsx` 49 ペア）+ E2E で検証 | SYS-F-601〜609, SYS-NF-301 | 解消 |
| **GAP-06** | プロッタのドッキング / フロート切替が未実装。**オーナー決定（2026-09-03）: スコープ外化を撤回し、実装予定として維持する（時期未定）**。[25 §1.3 条件 A](25_release_strategy.md#条件-a-機能未実装gap-の扱いが全件判断済であること) の暫定「スコープ外化の候補」は無効 | SYS-F-905 | 低（実装予定・時期未定） |
| **GAP-07** | ~~ホットプラグの能動検知が未実装~~ **解消済（2026-09-03）**: `src/App.tsx` に `PORT_POLL_INTERVAL_MS = 2000` のポート一覧ポーリングを実装。切断の権威は read エラー経路のまま、ポーリングは列挙更新のみを担い、接続中に選択が黙って移動しないことをテストで固定。`FE`: `src/test/App.test.tsx`（`polls list_ports on an interval so hotplugged devices appear`, `stops polling once unmounted`） | SYS-F-107, DEBT-2 | 解消 |
| **GAP-08** | ~~切断検知後もバックエンドがポートハンドルを保持し、UI 状態と不一致~~ **解消済（2026-09-03）**: `serial-status(connected=false)` を受けて `src/App.tsx` が `close_port` を呼び、状態を再同期（DEBT-1 / TBD-R4 決定済）。`FE`: `src/test/App.test.tsx`（`releases the backend port handle when a disconnect is detected`, `still releases the handle when the disconnect carries no error text`, `does not close the port on a connected=true status`） | SYS-F-107 | 解消 |
| **GAP-09** | ~~`log-error` イベントの発火経路が未実装~~ **解消済（2026-09-03）**: `logger_thread.rs` に `on_error` コールバック（`Box<dyn Fn(String) + Send>`、5 秒レート制限）を追加し、`data_store.rs` が `log-error` を emit、`src/App.tsx` が利用者へ提示。`UT`: `test_process_buffer_io_error_keeps_chunks`, `test_spawn_logger_thread_notifies_error_rate_limited`, `test_spawn_logger_thread_notifies_open_failure` / `FE`: `src/test/App.test.tsx`（`surfaces log-error events to the user`） | SYS-F-205 | 解消 |

### 4.2 実装はあるが自動検証がない

| GAP | 内容 | 関連要求 | 優先度 |
|-----|------|----------|--------|
| **GAP-10** | 1 kHz データの表示遅延（SYS-NF-107）が未測定。閾値自体も未合意（TBD-R5） | SYS-NF-107 | 中 |
| ~~**GAP-11**~~ | **解消済（2026-09-03）**。状態遷移の網羅テスト（Chow 0-switch / 1-switch）を自動化。バックエンド（ストア世代 × プロッタスレッド × 集約有効）は `src-tauri/src/state_transition_tests.rs` の `zero_switch_all_events` / `one_switch_all_event_pairs`（7 イベントの全 49 順序対）、フロントの SM-5 は `src/test/PlotterViewFsm.test.tsx`（同 49 対 + 0-switch 走査） | SYS-F-608, SYS-NF-202 | — |
| **GAP-12** | ~~プロパティ残件~~ **主要分は解消済（2026-09-03）**: (a) `prop_get_data_split_read_consistency`（INV-13 の発見に寄与）、(b) `prop_batch_equivalence` + `prop_count_conservation` を追加し計 11 プロパティ。残: (c) パーサ以外の panic 耐性、P-2/P-6（低優先、24 §3 参照） | SYS-NF-201, 603, 604 | 中 |
| **GAP-13** | ポート列挙・設定検証（`list_ports` / `SerialConfig` の分岐）に単体テストがない | SYS-F-101, 102, 105 | 中 |
| **GAP-14** | Tauri コマンド層（`open_port` / `clear_data` / `close_port` / `export_log` の状態遷移）に自動テストがない。E2E のみ | SYS-F-106, 801, 803, 806, 904 | **高** |
| **GAP-15** | Logger の順序不変条件（INV-3）を壊すと落ちるテストがない。並行実行下での `get_data` の可用性が未検証 | SYS-F-204, SYS-NF-203, INV-3/4 | **高** |
| **GAP-16** | UiNotifier の間引き（16 ms / 60 fps 上限）に自動検証がない。INV-11（async 化）も宣言の目視確認のみ | SYS-F-206, INV-11 | 低 |
| **GAP-17** | temp ディレクトリのインスタンス分離（INV-10）と起動時クリーンアップに自動テストがない | SYS-F-209, SYS-NF-503 | 中 |
| **GAP-18** | `export_log` / `get_clipboard_text` に自動テストがない | SYS-F-210, 211 | 中 |
| **GAP-19** | ビューアの表示オプション即時反映・in-flight 再取得のキュー化に自動テストがない | SYS-F-307, 308 | 中 |
| **GAP-20** | `SerialPort::write` の全バイト送出・タイムアウト拡張/復元に自動テストがない（実ポートが必要） | SYS-F-404, 405 | 中 |
| **GAP-21** | 送信履歴の ↑↓ とカーソル移動の分岐、Enter 挙動切替に自動テストがない | SYS-F-406, 407 | 低 |
| **GAP-22** | **部分解消（2026-09-03）**: ~~`calculateYRange`（非表示チャンネル除外・バンドの畳み込み）に単体テストがない~~ → `src/test/calculateYRange.test.ts` 10 件で解消。**残**: Y 軸ヒステリシス（SYS-F-521/522 の即時拡大・60% / 3 秒の縮小）のタイミングを数値で検証するテストがない（GAP-03 から統合された分） | SYS-F-521, 522, 524, 525 | 中 |
| **GAP-23** | State Timeline プラグインの描画（DPI スケーリング、状態のみの描画）に自動テストがない | SYS-F-707, SYS-NF-304 | 中 |
| **GAP-24** | 防御的な上限（パーサ 64 KB、state_data 10,000 件、読み出し失敗 500 回）に自動テストがない | SYS-F-711, 712, SYS-NF-204 | 低 |
| **GAP-25** | ウィンドウライフサイクル（単一インスタンス、X 閉じ、メイン閉での全終了）に自動テストがない | SYS-F-901〜903, INV-5/6 | 中 |
| **GAP-26** | メモリ定常性（バックエンド、チャンク上限）の自動計測がない。手動の `monitor_memory.py` のみ | SYS-NF-104, 108 | 中 |
| **GAP-27** | Clear に確認ダイアログがない（不可逆な操作） | SYS-NF-302 | 低 |
| **GAP-31** | AI Bridge の E2E（実アプリ + com0com + 実 MCP クライアントでの送受信ラウンドトリップ）が `test_tools/e2e/` のハーネスに入っておらず、2026-09-03 の手動実施の記録のみ。また `mcp/smoke.mjs` は**偽ブリッジ**に対する結合であり、実ブリッジとの突き合わせは自動化されていない。トークン設定 UI 自体が未提供（TBD-R7） | SYS-F-1101〜1106 | 中 |

### 4.3 テスト技法としての不足

| GAP | 内容 | 対応計画 |
|-----|------|----------|
| **GAP-28** | ~~ペアワイズ未実施~~ **解消済（2026-09-03）**: 8 因子・112 ペアを 7 行で網羅、全行 PASS（`test_tools/e2e/pairwise_gen.py` + `pairwise_run.ps1`） | [24_vv_plan.md §5](24_vv_plan.md) |
| **GAP-29** | ~~ミューテーション基線なし~~ **第 1 弾実施済（2026-09-03）**: parser+thread で 103 ミュータント、スコア 66.3%。生存 31 件の分布 = アサーション強化 TODO（詳細は 24 §7.4）。aggregator の第 2 弾と missed 半減が残 | [24_vv_plan.md §7](24_vv_plan.md) |
| **GAP-30** | ~~E2E がスクリプト化されていない~~ **解消済（2026-09-03）**: UIA ハーネスを `test_tools/e2e/` に永続化（`ui.ps1` / `pairwise_run.ps1` / README） | [24_vv_plan.md §6](24_vv_plan.md) |

---

## 5. GAP の優先順位（次の作業の入力）

| 順位 | GAP | 理由 |
|------|-----|------|
| — | GAP-02, GAP-03, GAP-04, GAP-05, GAP-07, GAP-08, GAP-09, GAP-11, GAP-28, GAP-30 | **解消済**（GAP-02/03/05/11/28/30 は 2026-09-03 前半、GAP-04/07/08/09 は 2026-09-03 の AI Bridge 回） |
| 1 | GAP-14 | 2026-09-03 の重大バグはすべて「状態遷移 × コマンド層」で発生した。E2E の手動実施では次の回帰を止められない。状態遷移側（GAP-11）は 0-switch / 1-switch の自動化で解消済、残るのは Tauri コマンド層（GAP-14） |
| 2 | GAP-15 | INV-3 は 1 度壊れて表示欠落を起こしている。順序を戻す変更を検知できないのは危険 |
| 3 | GAP-31 | AI Bridge は**外部プロセスへ開く唯一の口**であり、回帰が起きたときの影響がプロセス境界を越える。手動 E2E の記録だけでは次の変更を守れない |
| 4 | GAP-12 の残り | パーサ・集約器は整備済。残るのはパーサ以外の panic 耐性（P-2 / P-6） |
| 5 | GAP-22 の残り | Y 軸ヒステリシスのタイミング検証（`calculateYRange` 分は 2026-09-03 に解消） |
| 6 | GAP-29 | 上記を整備した後、スイート全体の有効性を測る基線として |
| 7 | GAP-01, GAP-06 | 機能そのものが未着手（検索/フィルタは v0.2、ドッキングは時期未定・スコープ内） |
| 8 | それ以外 | 被害が限定的 |

---

## 関連ドキュメント

- [20_user_needs.md](20_user_needs.md)
- [21_system_requirements.md](21_system_requirements.md)
- [22_architecture_description.md](22_architecture_description.md)
- [24_vv_plan.md](24_vv_plan.md)
