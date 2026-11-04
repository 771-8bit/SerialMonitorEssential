//! 索引スレッド — `DataStore` を読む唯一の追加リーダ
//!
//! 構造は実績のある `PlotterThread::run` に倣う（毎回ストアを再解決、
//! `Arc::ptr_eq` で世代差し替えを検知、1 回の読み出しを上限つきに、
//! 読み取り失敗が続いたらスキップアヘッド）。ただし docs/26 の要求により
//! **スキップは必ず `gap` として記録し、隠さない**。
//!
//! `bridge_set(enabled=true)` で開始し `false` で停止する。ブリッジ（MCP Server）は既定
//! OFF なので、使わない利用者は CPU もメモリも一切払わない。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::bridge::SharedDataStore;
use crate::serial::data_store::DataStore;

use super::index::InsightIndex;

/// 1 回の読み出し上限 (1 MiB)。`bridge::MAX_READ_LENGTH` と同じ考え方
const MAX_READ: u32 = 1024 * 1024;
/// ポーリング間隔
const POLL_MS: u64 = 10;
/// 同じオフセットで何回読み失敗したら諦めて飛ばすか
const MAX_READ_FAILURES: u32 = 5;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub struct IndexThread {
    stop_flag: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl IndexThread {
    pub fn start(store_handle: SharedDataStore, index: Arc<InsightIndex>) -> Self {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let flag = stop_flag.clone();
        index.set_running(true);
        let handle = thread::spawn(move || run(store_handle, index, flag));
        Self {
            stop_flag,
            handle: Some(handle),
        }
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for IndexThread {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(store_handle: SharedDataStore, index: Arc<InsightIndex>, stop_flag: Arc<AtomicBool>) {
    let mut current_store: Option<Arc<DataStore>> = None;
    let mut next_offset: u64 = 0;
    let mut read_failures: u32 = 0;

    loop {
        if stop_flag.load(Ordering::SeqCst) {
            break;
        }

        let resolved = store_handle.lock().ok().and_then(|guard| guard.clone());
        let store = match resolved {
            Some(store) => {
                let changed = current_store
                    .as_ref()
                    .map(|c| !Arc::ptr_eq(c, &store))
                    .unwrap_or(true);
                if changed {
                    // ポート再オープン / Clear でストアそのものが差し替わる。
                    // 世代を上げ、古いカーソルが黙って嘘のデルタを返さないようにする
                    log::info!("[InsightIndex] DataStore changed, starting a new generation");
                    index.reset(0);
                    next_offset = 0;
                    read_failures = 0;
                    current_store = Some(Arc::clone(&store));
                }
                store
            }
            None => {
                current_store = None;
                thread::sleep(Duration::from_millis(50));
                continue;
            }
        };

        let total = store.total_bytes();
        index.note_store_total(total);
        if total <= next_offset {
            thread::sleep(Duration::from_millis(POLL_MS));
            continue;
        }
        let want = (total - next_offset).min(MAX_READ as u64) as u32;
        match store.get_data(next_offset, want) {
            Ok(data) if !data.is_empty() => {
                read_failures = 0;
                index.ingest(next_offset, &data, now_ms());
                next_offset += data.len() as u64;
                // まだ残っていれば間を空けずに続ける（追随を優先）
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                read_failures += 1;
                if read_failures == 1 || read_failures >= MAX_READ_FAILURES {
                    log::warn!(
                        "[InsightIndex] get_data failed at offset {} (attempt {}): {}",
                        next_offset,
                        read_failures,
                        e
                    );
                }
                if read_failures >= MAX_READ_FAILURES {
                    // 諦めて飛ばす。ただし飛ばしたバイト数は必ず記録する
                    let skipped = total.saturating_sub(next_offset);
                    log::warn!(
                        "[InsightIndex] Skipping unreadable region {}..{} ({} bytes)",
                        next_offset,
                        total,
                        skipped
                    );
                    index.note_gap(skipped);
                    next_offset = total;
                    read_failures = 0;
                }
            }
        }
        thread::sleep(Duration::from_millis(POLL_MS));
    }
    index.set_running(false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn shared(store: Option<Arc<DataStore>>) -> SharedDataStore {
        Arc::new(Mutex::new(store))
    }

    /// 索引が `expect` 行に届くまで待つ（最大 2 秒）
    fn wait_for_lines(index: &InsightIndex, expect: u64) -> u64 {
        for _ in 0..200 {
            let lines = index.snapshot().lines;
            if lines >= expect {
                return lines;
            }
            thread::sleep(Duration::from_millis(10));
        }
        index.snapshot().lines
    }

    #[test]
    fn test_thread_indexes_data_and_reports_running() {
        let store = Arc::new(DataStore::new().expect("DataStore::new"));
        let handle = shared(Some(store.clone()));
        let index = Arc::new(InsightIndex::new());
        let mut thread = IndexThread::start(handle, index.clone());
        assert!(index.snapshot().running);

        store.push_test_data(b"T: 1.0 C\nT: 2.0 C\nE: fail\n");
        assert_eq!(wait_for_lines(&index, 3), 3);
        let snap = index.snapshot();
        assert_eq!(snap.err, 1);
        assert_eq!(snap.templates.len(), 2);

        thread.stop();
        assert!(!index.snapshot().running);
    }

    #[test]
    fn test_store_swap_starts_a_new_generation() {
        let first = Arc::new(DataStore::new().expect("DataStore::new"));
        let handle = shared(Some(first.clone()));
        let index = Arc::new(InsightIndex::new());
        let mut thread = IndexThread::start(handle.clone(), index.clone());

        first.push_test_data(b"A: 1\n");
        wait_for_lines(&index, 1);
        let gen_before = index.snapshot().gen;

        // ポート再オープン相当: ストアそのものを差し替える
        let second = Arc::new(DataStore::new().expect("DataStore::new"));
        *handle.lock().unwrap() = Some(second.clone());
        second.push_test_data(b"B: 2\nB: 3\n");
        wait_for_lines(&index, 2);

        let snap = index.snapshot();
        assert!(snap.gen > gen_before, "generation must advance on swap");
        assert_eq!(snap.lines, 2);
        assert_eq!(snap.templates.len(), 1);
        thread.stop();
    }

    #[test]
    fn test_detached_store_leaves_the_index_idle() {
        let handle = shared(None);
        let index = Arc::new(InsightIndex::new());
        let mut thread = IndexThread::start(handle, index.clone());
        thread::sleep(Duration::from_millis(50));
        assert_eq!(index.snapshot().lines, 0);
        thread.stop();
    }
}
