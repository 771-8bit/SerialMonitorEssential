# バッファリング設計修正タスクリスト

## 概要

現在の実装では `SegQueue` を使用しており、UIが `finished_queue` を直接参照できない。
設計通り、UIがLoggerのディスク書き込みを待たずに最新データを参照できるよう修正する。

---

## Phase 1: データ構造の変更

### Task 1.1: Chunk に global_offset を追加
- **ファイル:** `src-tauri/src/serial/chunk.rs`
- **変更内容:**
  - `global_offset: u64` フィールドを追加
  - このチャンクがストリーム全体のどの位置から始まるかを記録
  - `set_global_offset(&mut self, offset: u64)` メソッド追加

### Task 1.2: finished_queue を VecDeque に変更
- **ファイル:** `src-tauri/src/serial/data_store.rs`
- **変更内容:**
  ```rust
  // 変更前
  finished_queue: Arc<SegQueue<Chunk>>,
  
  // 変更後
  finished_list: Arc<RwLock<VecDeque<Arc<Chunk>>>>,
  ```
- **理由:** UI読み取り（iter）とLogger取り出し（pop_front）を両立

### Task 1.3: 不要なフィールドの削除
- **ファイル:** `src-tauri/src/serial/data_store.rs`
- **削除対象:**
  - `recent_data: Arc<RwLock<RecentDataCache>>` (finished_list で代替)
  - `RecentDataCache` 構造体全体

---

## Phase 2: Worker Thread の修正

### Task 2.1: チャンク確定時の処理変更
- **ファイル:** `src-tauri/src/serial/worker_thread.rs`
- **変更内容:**
  - `finished_queue.push(chunk)` → `finished_list.write().push_back(Arc::new(chunk))`
  - チャンクに `global_offset` を設定してから追加
  - 空チャンク（0バイト）は追加しない

### Task 2.2: 16ms タイムアウト処理の確認
- タイムアウトで確定する際、データが0バイトならスワップしない
- 現在の実装を確認し、必要なら修正

---

## Phase 3: Logger Thread の修正

### Task 3.1: finished_list からの取り出し変更
- **ファイル:** `src-tauri/src/serial/logger_thread.rs`
- **変更内容:**
  ```rust
  // 変更前
  if let Some(chunk) = finished_queue.pop() { ... }
  
  // 変更後
  let chunk = {
      let mut list = finished_list.write().unwrap();
      list.pop_front()
  };
  if let Some(chunk) = chunk { ... }
  ```
- **注意:** write lock は pop_front の一瞬だけ保持し、すぐ解放

### Task 3.2: recent_data 更新の削除
- Logger から `recent_data.append()` 呼び出しを削除
- finished_list をUIが直接参照するため不要

---

## Phase 4: データ取得 API の修正

### Task 4.1: get_live_data API の実装（新規）
- **ファイル:** `src-tauri/src/serial/data_store.rs`, `mod.rs`
- **目的:** finished_list から最新N バイトを返す
- **実装:**
  ```rust
  pub fn get_live_data(&self, length: u32) -> (u64, Vec<u8>) {
      if let Ok(list) = self.finished_list.read() {
          // 末尾から length バイト分を収集
          let mut result = Vec::new();
          let mut start_offset = 0u64;
          
          for chunk in list.iter().rev() {
              // 後ろから収集...
          }
          
          (start_offset, result)
      } else {
          (0, Vec::new())
      }
  }
  ```

### Task 4.2: get_read_data の修正
- **変更内容:**
  - まず finished_list を検索（メモリ内のデータ）
  - なければ archived_index からディスク読み取り
- **重要:** 両方のソースをシームレスに結合

### Task 4.3: Tauri コマンドの更新
- `get_live_data` を Tauri コマンドとして登録
- `lib.rs` の invoke_handler に追加

---

## Phase 5: data-update イベントの実装

### Task 5.1: UiNotifier スレッドの実装
- **ファイル:** `src-tauri/src/serial/ui_notifier.rs` (新規)
- **目的:** 60fps に間引いて data-update イベントを発火
- **実装:**
  ```rust
  pub fn spawn_ui_notifier(
      finished_list: Arc<RwLock<VecDeque<Arc<Chunk>>>>,
      app_handle: tauri::AppHandle,
      stop_flag: Arc<AtomicBool>,
  ) -> JoinHandle<()> {
      thread::spawn(move || {
          let mut last_total = 0u64;
          loop {
              if stop_flag.load(Ordering::Relaxed) { break; }
              
              thread::sleep(Duration::from_millis(16)); // 60fps
              
              // finished_list から total_bytes と 最新データを取得
              if let Ok(list) = finished_list.read() {
                  let total = calculate_total(&list);
                  if total > last_total {
                      // イベント発火
                      app_handle.emit("data-update", DataUpdatePayload { ... });
                      last_total = total;
                  }
              }
          }
      })
  }
  ```

### Task 5.2: DataStore に UiNotifier 統合
- `start_reception` で UiNotifier スレッドも起動
- `stop_reception` で停止

---

## Phase 6: Frontend の修正

### Task 6.1: data-update イベントリスナーの実装
- **ファイル:** `src/App.tsx`
- **変更内容:**
  - `setInterval` によるポーリングを削除
  - `listen('data-update', ...)` でイベント駆動に変更

### Task 6.2: DataViewer の最適化
- **ファイル:** `src/components/DataViewer.tsx`
- **変更内容:**
  - `get_live_data` で最新データを取得
  - `get_read_data` で過去データを取得
  - キャッシュ戦略の見直し

---

## Phase 7: 検証

### Task 7.1: 低速テスト
- `pico_slow_test_controller.py` で30秒テスト
- UIが毎秒更新されることを確認
- "Loading" が表示されないことを確認

### Task 7.2: 高速テスト
- `pico_stress_test_controller.py` で60秒テスト @ 2Mbps
- UIがフリーズしないことを確認
- データ完全性を確認（バイト数＋チェックサム一致）

### Task 7.3: メモリ使用量確認
- 長時間（5分以上）の高速受信
- メモリリークがないことを確認

---

## 優先順位

| 順位 | フェーズ | 説明 | 工数 |
|------|----------|------|------|
| 1 | Phase 1 | データ構造変更 | 中 |
| 2 | Phase 2-3 | Worker/Logger 修正 | 中 |
| 3 | Phase 4 | API 修正 | 中 |
| 4 | Phase 5 | イベント実装 | 中 |
| 5 | Phase 6 | Frontend 修正 | 小 |
| 6 | Phase 7 | 検証 | 小 |

---

## 備考

- lint warnings (`temp_dir` is never used 等) は Phase 4 完了後に整理
- pico_serial_tx_test.ino の lint error は Arduino 用ファイルのため無視
