use crossbeam::queue::SegQueue;
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, RwLock,
};
use std::thread::JoinHandle;
use tauri::AppHandle;

use log::{debug, error, info, warn};

use super::chunk::Chunk;
use super::logger_thread::{self, PageMetadata};
use super::port::SerialPort;
use super::ui_notifier;
use super::worker_thread;

const CHUNK_SIZE: usize = 64 * 1024; // 64KB
const INITIAL_POOL_SIZE: usize = 100; // 約6.4MB

/// DataStore: データ管理の中核
///
/// ObjectPool、FinishedQueue、ArchivedIndexを統合し、
/// Worker/Logger/UiNotifier スレッドのライフサイクルを管理する。
pub struct DataStore {
    free_pool: Arc<SegQueue<Chunk>>,
    finished_list: Arc<RwLock<VecDeque<Arc<Chunk>>>>,
    archived_index: Arc<RwLock<Vec<PageMetadata>>>,
    temp_dir: PathBuf,

    // スレッド管理
    worker_handle: Mutex<Option<JoinHandle<()>>>,
    logger_handle: Mutex<Option<JoinHandle<()>>>,
    ui_notifier_handle: Mutex<Option<JoinHandle<()>>>,
    stop_flag: Arc<AtomicBool>,
}

impl DataStore {
    /// 新しいDataStoreを作成
    ///
    /// PIDを使った一時ディレクトリを作成し、ObjectPoolを初期化する。
    /// 起動時に古い一時ディレクトリをクリーンアップする。
    pub fn new() -> Result<Self, String> {
        let pid = std::process::id();
        let base_dir = std::env::temp_dir().join("SerialMonitorEssential");

        // 起動時クリーンアップ: 古いPIDフォルダを削除
        cleanup_stale_directories(&base_dir, pid)?;

        // 一時ディレクトリ作成
        let temp_dir = base_dir.join(pid.to_string());
        fs::create_dir_all(&temp_dir)
            .map_err(|e| format!("Failed to create temp directory: {:?}", e))?;

        // ObjectPool初期化（直接SegQueueを使用）
        let free_pool = Arc::new(SegQueue::new());
        for _ in 0..INITIAL_POOL_SIZE {
            free_pool.push(Chunk::new(CHUNK_SIZE));
        }

        Ok(Self {
            free_pool,
            finished_list: Arc::new(RwLock::new(VecDeque::new())),
            archived_index: Arc::new(RwLock::new(Vec::new())),
            temp_dir,
            worker_handle: Mutex::new(None),
            logger_handle: Mutex::new(None),
            ui_notifier_handle: Mutex::new(None),
            stop_flag: Arc::new(AtomicBool::new(false)),
        })
    }

    /// データ受信を開始
    ///
    /// # Arguments
    /// * `port` - SerialPortのArc<Mutex>
    /// * `app_handle` - UIイベント送信用のTauri AppHandle
    /// * `self_arc` - UI Notifier用のDataStore Arc参照
    pub fn start_reception(
        &self,
        port: Arc<Mutex<SerialPort>>,
        app_handle: AppHandle,
        self_arc: Arc<Self>,
    ) -> Result<(), String> {
        info!("[DataStore] Starting reception");
        // 既に動作中の場合は停止
        self.stop_reception();

        // 停止フラグをリセット
        self.stop_flag.store(false, Ordering::Relaxed);

        // Worker Thread起動
        debug!("[DataStore] Spawning Worker Thread");
        let worker_handle = worker_thread::spawn_worker_thread(
            port,
            self.free_pool.clone(),
            self.finished_list.clone(),
            self.stop_flag.clone(),
            app_handle.clone(),
        );

        // Logger Thread起動
        debug!("[DataStore] Spawning Logger Thread");
        let logger_handle = logger_thread::spawn_logger_thread(
            self.finished_list.clone(),
            self.archived_index.clone(),
            self.temp_dir.clone(),
            self.stop_flag.clone(),
        );

        // UiNotifier Thread起動
        debug!("[DataStore] Spawning UiNotifier Thread");
        let ui_notifier_handle =
            ui_notifier::spawn_ui_notifier_thread(self_arc, self.stop_flag.clone(), app_handle);

        // ハンドルを保存
        *self.worker_handle.lock().unwrap() = Some(worker_handle);
        *self.logger_handle.lock().unwrap() = Some(logger_handle);
        *self.ui_notifier_handle.lock().unwrap() = Some(ui_notifier_handle);

        info!("[DataStore] Reception started successfully");
        Ok(())
    }

    /// データ受信を停止
    pub fn stop_reception(&self) {
        // 停止フラグを設定
        self.stop_flag.store(true, Ordering::Relaxed);

        // スレッドの終了を待機
        if let Some(handle) = self.worker_handle.lock().unwrap().take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.logger_handle.lock().unwrap().take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.ui_notifier_handle.lock().unwrap().take() {
            let _ = handle.join();
        }

        // 停止フラグをリセット
        self.stop_flag.store(false, Ordering::Relaxed);
    }

    /// 指定範囲のデータを取得
    ///
    /// # Arguments
    /// * `offset` - 開始オフセット（グローバル）
    /// * `length` - 読み取りバイト数
    ///
    /// # Returns
    /// データのVec<u8>
    ///
    /// # Data Source Priority
    /// 1. archived_index (ディスク) - 確定済みの古いデータ
    /// 2. finished_list (メモリ) - 最新のデータ
    ///
    /// 両方のソースを global_offset 順に検索し、境界をまたぐリクエストにも対応。
    pub fn get_data(&self, offset: u64, length: u32) -> Result<Vec<u8>, String> {
        use std::fs::File;
        use std::io::{Read, Seek, SeekFrom};

        let length = length as usize;
        let mut result = Vec::with_capacity(length);
        let mut current_offset = offset;
        let mut remaining = length;

        // Debug: Log the state of archived_index and finished_list
        let archived_count = self.archived_index.read().map(|i| i.len()).unwrap_or(0);
        let finished_count = self.finished_list.read().map(|l| l.len()).unwrap_or(0);
        warn!(
            "[get_data] offset={}, length={}, archived_pages={}, finished_chunks={}",
            offset, length, archived_count, finished_count
        );

        // 1. まずディスク上（archived_index）を検索 - 確定済みデータ
        if let Ok(index) = self.archived_index.read() {
            for page in index.iter() {
                let page_start = page.global_offset;
                let page_end = page_start + page.data_length as u64;

                // このページに要求範囲が含まれるか
                if current_offset < page_end && page_start < current_offset + remaining as u64 {
                    // current_offset がこのページ内にあるか確認
                    if current_offset >= page_start {
                        let page_offset = current_offset - page_start;
                        let to_read =
                            remaining.min((page.data_length as u64 - page_offset) as usize);

                        // ファイルから読み取り
                        let mut file = File::open(&page.file_path).map_err(|e| {
                            format!("Failed to open file {:?}: {:?}", page.file_path, e)
                        })?;

                        file.seek(SeekFrom::Start(page.file_offset + page_offset))
                            .map_err(|e| {
                                format!(
                                    "Failed to seek to offset {}: {:?}",
                                    page.file_offset + page_offset,
                                    e
                                )
                            })?;

                        let mut buffer = vec![0u8; to_read];
                        file.read_exact(&mut buffer)
                            .map_err(|e| format!("Failed to read {} bytes: {:?}", to_read, e))?;

                        result.extend_from_slice(&buffer);

                        current_offset += to_read as u64;
                        remaining -= to_read;

                        if remaining == 0 {
                            return Ok(result);
                        }
                    }
                }
            }
        }

        // 2. 次にメモリ上（finished_list）を検索 - 最新データ
        if let Ok(list) = self.finished_list.read() {
            for chunk in list.iter() {
                let chunk_start = chunk.global_offset();
                let chunk_end = chunk_start + chunk.len() as u64;

                // このチャンクに要求範囲が含まれるか
                if current_offset < chunk_end && chunk_start < current_offset + remaining as u64 {
                    // current_offset がこのチャンク内にあるか確認
                    if current_offset >= chunk_start {
                        let chunk_offset = (current_offset - chunk_start) as usize;
                        let to_read = remaining.min(chunk.len() - chunk_offset);
                        let data = chunk.data();
                        result.extend_from_slice(&data[chunk_offset..chunk_offset + to_read]);

                        current_offset += to_read as u64;
                        remaining -= to_read;

                        if remaining == 0 {
                            return Ok(result);
                        }
                    }
                }
            }
        }

        // 要求されたデータがすべて読み取れなかった場合はエラー
        if remaining > 0 {
            return Err(format!(
                "Insufficient data: requested {} bytes at offset {}, but only {} bytes available",
                length,
                offset,
                result.len()
            ));
        }

        Ok(result)
    }

    /// 受信済み総バイト数を取得
    pub fn total_bytes(&self) -> u64 {
        let mut total = 0u64;

        // archived_indexから取得
        if let Ok(index) = self.archived_index.read() {
            total = index
                .last()
                .map(|p| p.global_offset + p.data_length as u64)
                .unwrap_or(0);
        }

        // finished_listの最後のチャンクから取得（より新しい）
        if let Ok(list) = self.finished_list.read() {
            if let Some(last_chunk) = list.back() {
                total = last_chunk.global_offset() + last_chunk.len() as u64;
            }
        }

        total
    }

    /// 一時ディレクトリパスを取得（診断/デバッグ用）
    #[allow(dead_code)]
    pub fn temp_dir(&self) -> &PathBuf {
        &self.temp_dir
    }
}

impl Drop for DataStore {
    fn drop(&mut self) {
        info!(
            "[DataStore::Drop] Cleaning up, temp_dir: {:?}",
            self.temp_dir
        );

        // スレッドを停止
        self.stop_reception();

        // 一時ファイルを削除（ベストエフォート）
        match fs::remove_dir_all(&self.temp_dir) {
            Ok(_) => info!("[DataStore::Drop] Successfully removed temp directory"),
            Err(e) => warn!("[DataStore::Drop] Failed to remove temp directory: {:?}", e),
        }

        debug!("[DataStore::Drop] Cleanup complete");
    }
}

/// 古い一時ディレクトリをクリーンアップ
///
/// プロセスが存在しないPIDフォルダを削除する
fn cleanup_stale_directories(base_dir: &std::path::Path, current_pid: u32) -> Result<(), String> {
    if !base_dir.exists() {
        return Ok(());
    }

    debug!(
        "[cleanup] Checking for stale directories in: {:?}",
        base_dir
    );

    match fs::read_dir(base_dir) {
        Ok(entries) => {
            for entry in entries.filter_map(Result::ok) {
                let dir_name = entry.file_name().to_string_lossy().to_string();

                if let Ok(pid) = dir_name.parse::<u32>() {
                    // 自分のPIDはスキップ
                    if pid == current_pid {
                        continue;
                    }

                    // プロセスが存在しないなら削除
                    if !is_process_running(pid) {
                        info!("[cleanup] Removing stale directory for PID: {}", pid);
                        if let Err(e) = fs::remove_dir_all(entry.path()) {
                            warn!(
                                "[cleanup] Failed to remove directory for PID {}: {:?}",
                                pid, e
                            );
                        }
                    } else {
                        debug!("[cleanup] Keeping directory for active PID: {}", pid);
                    }
                }
            }
        }
        Err(e) => {
            error!("[cleanup] Failed to read base directory: {:?}", e);
        }
    }

    Ok(())
}

/// プロセスが実行中かどうかを確認（クロスプラットフォーム）
///
/// sysinfoクレートを使用して、指定されたPIDのプロセスが
/// SerialMonitorEssentialであるかを確認する。
fn is_process_running(pid: u32) -> bool {
    use sysinfo::{Pid, System};

    let mut sys = System::new();
    let sysinfo_pid = Pid::from_u32(pid);

    // Refresh only the specific process
    sys.refresh_processes(
        sysinfo::ProcessesToUpdate::Some(&[sysinfo_pid]),
        false, // Don't get all parents
    );

    if let Some(process) = sys.process(sysinfo_pid) {
        let process_name = process.name().to_string_lossy().to_lowercase();
        let is_ours = process_name.contains("serialmonitoressential")
            || process_name.contains("serial-monitor-essential")
            || process_name.contains("serial_monitor_essential");
        debug!(
            "[is_process_running] PID {}: name={}, is_ours={}",
            pid, process_name, is_ours
        );
        is_ours
    } else {
        debug!(
            "[is_process_running] PID {} does not exist or not accessible",
            pid
        );
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// テスト用にDataStoreの内部状態を直接操作するためのヘルパー
    fn create_test_data_store() -> DataStore {
        let pid = std::process::id();
        let base_dir = std::env::temp_dir().join("SerialMonitorEssential_test");
        let temp_dir = base_dir.join(format!(
            "{}_{}",
            pid,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&temp_dir).unwrap();

        DataStore {
            free_pool: Arc::new(SegQueue::new()),
            finished_list: Arc::new(RwLock::new(VecDeque::new())),
            archived_index: Arc::new(RwLock::new(Vec::new())),
            temp_dir,
            worker_handle: Mutex::new(None),
            logger_handle: Mutex::new(None),
            ui_notifier_handle: Mutex::new(None),
            stop_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// archived_indexのみからデータを取得できることを確認
    #[test]
    fn test_get_data_from_archived_only() {
        let store = create_test_data_store();

        // テストデータをファイルに書き込む
        let test_file = store.temp_dir.join("test_data.bin");
        let test_data: Vec<u8> = (0..100).collect();
        {
            let mut file = std::fs::File::create(&test_file).unwrap();
            file.write_all(&test_data).unwrap();
        }

        // archived_indexにメタデータを追加
        {
            let mut index = store.archived_index.write().unwrap();
            index.push(PageMetadata {
                file_path: test_file.clone(),
                file_offset: 0,
                data_length: 100,
                global_offset: 0,
            });
        }

        // データを取得
        let result = store.get_data(0, 100).unwrap();
        assert_eq!(result, test_data);

        // クリーンアップ
        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// finished_listのみからデータを取得できることを確認
    #[test]
    fn test_get_data_from_finished_list_only() {
        let store = create_test_data_store();

        // finished_listにチャンクを追加
        let mut chunk = Chunk::new(100);
        let test_data: Vec<u8> = (0..50).collect();
        chunk.push_data(&test_data);
        chunk.set_global_offset(0);

        {
            let mut list = store.finished_list.write().unwrap();
            list.push_back(Arc::new(chunk));
        }

        // データを取得
        let result = store.get_data(0, 50).unwrap();
        assert_eq!(result, test_data);

        // クリーンアップ
        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// 【問題を示すテスト】
    /// archived_indexにデータがあり、finished_listにもデータがある場合、
    /// 現在の実装ではfinished_listを先に検索するため、
    /// archived_indexのデータを正しく取得できない可能性がある
    #[test]
    fn test_get_data_archived_then_finished() {
        let store = create_test_data_store();

        // --- archived_index に最初の100バイトを追加 ---
        let test_file = store.temp_dir.join("test_data.bin");
        let archived_data: Vec<u8> = (0..100).collect();
        {
            let mut file = std::fs::File::create(&test_file).unwrap();
            file.write_all(&archived_data).unwrap();
        }
        {
            let mut index = store.archived_index.write().unwrap();
            index.push(PageMetadata {
                file_path: test_file.clone(),
                file_offset: 0,
                data_length: 100,
                global_offset: 0,
            });
        }

        // --- finished_list に次の50バイトを追加 ---
        let mut chunk = Chunk::new(100);
        let finished_data: Vec<u8> = (100..150).collect();
        chunk.push_data(&finished_data);
        chunk.set_global_offset(100); // offset 100から開始
        {
            let mut list = store.finished_list.write().unwrap();
            list.push_back(Arc::new(chunk));
        }

        // --- archived_indexのデータ(offset 0-100)を取得 ---
        // 現在の実装: finished_listを先に検索するが、
        // offset 0のデータはfinished_listにないのでスキップされ、
        // 次にarchived_indexを検索して取得できるはず
        let result = store.get_data(0, 100).unwrap();
        assert_eq!(result.len(), 100);
        assert_eq!(result, archived_data, "Should get data from archived_index");

        // --- finished_listのデータ(offset 100-150)を取得 ---
        let result2 = store.get_data(100, 50).unwrap();
        assert_eq!(result2.len(), 50);
        assert_eq!(result2, finished_data, "Should get data from finished_list");

        // クリーンアップ
        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// 【境界をまたぐテスト】
    /// archived_indexとfinished_listの両方にまたがるデータの取得
    #[test]
    fn test_get_data_spanning_archived_and_finished() {
        let store = create_test_data_store();

        // --- archived_index に最初の100バイトを追加 ---
        let test_file = store.temp_dir.join("test_data.bin");
        let archived_data: Vec<u8> = (0..100).collect();
        {
            let mut file = std::fs::File::create(&test_file).unwrap();
            file.write_all(&archived_data).unwrap();
        }
        {
            let mut index = store.archived_index.write().unwrap();
            index.push(PageMetadata {
                file_path: test_file.clone(),
                file_offset: 0,
                data_length: 100,
                global_offset: 0,
            });
        }

        // --- finished_list に次の100バイトを追加 ---
        let mut chunk = Chunk::new(200);
        let finished_data: Vec<u8> = (100..200).collect();
        chunk.push_data(&finished_data);
        chunk.set_global_offset(100);
        {
            let mut list = store.finished_list.write().unwrap();
            list.push_back(Arc::new(chunk));
        }

        // --- 境界をまたぐデータ(offset 50-150)を取得 ---
        // archived: 50-100 (50 bytes) + finished: 100-150 (50 bytes) = 100 bytes
        let result = store.get_data(50, 100).unwrap();

        // 期待値: [50, 51, ..., 99, 100, 101, ..., 149]
        let expected: Vec<u8> = (50..150).collect();

        assert_eq!(result.len(), 100, "Should get 100 bytes total");
        assert_eq!(
            result, expected,
            "Data should be continuous across boundary"
        );

        // クリーンアップ
        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// total_bytes() が空の状態で0を返すことを確認
    #[test]
    fn test_total_bytes_empty() {
        let store = create_test_data_store();
        assert_eq!(store.total_bytes(), 0);
        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// total_bytes() がfinished_listからバイト数を正しく取得することを確認
    #[test]
    fn test_total_bytes_from_finished_list() {
        let store = create_test_data_store();

        // finished_listにチャンクを追加
        let mut chunk = Chunk::new(100);
        let test_data: Vec<u8> = (0..50).collect();
        chunk.push_data(&test_data);
        chunk.set_global_offset(0);

        {
            let mut list = store.finished_list.write().unwrap();
            list.push_back(Arc::new(chunk));
        }

        assert_eq!(store.total_bytes(), 50);
        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// total_bytes() がarchived_indexとfinished_listの両方から正しく計算することを確認
    #[test]
    fn test_total_bytes_from_archived_and_finished() {
        let store = create_test_data_store();

        // archived_indexにメタデータを追加
        let test_file = store.temp_dir.join("test_total.bin");
        {
            let mut file = std::fs::File::create(&test_file).unwrap();
            file.write_all(&[0u8; 100]).unwrap();
        }
        {
            let mut index = store.archived_index.write().unwrap();
            index.push(PageMetadata {
                file_path: test_file.clone(),
                file_offset: 0,
                data_length: 100,
                global_offset: 0,
            });
        }

        // finished_listにチャンクを追加
        let mut chunk = Chunk::new(100);
        let test_data: Vec<u8> = (0..50).collect();
        chunk.push_data(&test_data);
        chunk.set_global_offset(100); // archived の後から開始

        {
            let mut list = store.finished_list.write().unwrap();
            list.push_back(Arc::new(chunk));
        }

        // archived: 0-100 (100 bytes), finished: 100-150 (50 bytes) = total 150 bytes
        assert_eq!(store.total_bytes(), 150);
        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// get_data() が空の状態でエラーを返すことを確認
    #[test]
    fn test_get_data_empty_store() {
        let store = create_test_data_store();
        let result = store.get_data(0, 10);
        assert!(result.is_err());
        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// get_data() が要求範囲外のオフセットでエラーを返すことを確認
    #[test]
    fn test_get_data_offset_out_of_range() {
        let store = create_test_data_store();

        // finished_listにチャンクを追加
        let mut chunk = Chunk::new(100);
        let test_data: Vec<u8> = (0..50).collect();
        chunk.push_data(&test_data);
        chunk.set_global_offset(0);

        {
            let mut list = store.finished_list.write().unwrap();
            list.push_back(Arc::new(chunk));
        }

        // 存在しないオフセットを要求
        let result = store.get_data(100, 10);
        assert!(result.is_err());
        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// チャンク内からの部分読み取りテスト
    #[test]
    fn test_get_data_partial_read_within_chunk() {
        let store = create_test_data_store();

        // finished_listにチャンクを追加
        let mut chunk = Chunk::new(100);
        let test_data: Vec<u8> = (0..50).collect();
        chunk.push_data(&test_data);
        chunk.set_global_offset(0);

        {
            let mut list = store.finished_list.write().unwrap();
            list.push_back(Arc::new(chunk));
        }

        // 中間の10バイトを読み取り
        let result = store.get_data(20, 10).unwrap();
        let expected: Vec<u8> = (20..30).collect();
        assert_eq!(result, expected);
        let _ = fs::remove_dir_all(&store.temp_dir);
    }
}
