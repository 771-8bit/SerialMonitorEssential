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

/// ByteTimestamp: バイトカウントとタイムスタンプのペア
///
/// 100ms間隔で累積バイト数とタイムスタンプを記録し、
/// 表示時に二分探索でオフセットに対応するタイムスタンプを取得する。
#[derive(Clone, Debug)]
pub struct ByteTimestamp {
    pub timestamp: u64,        // Unix time (ms)
    pub cumulative_bytes: u64, // その時点での累積バイト数
}

/// `log-error` イベントのペイロード（docs/04_api.md）
///
/// ディスクフル等でログ書き込みに失敗したことを UI に伝える。
#[derive(Clone, serde::Serialize)]
pub struct LogErrorPayload {
    pub message: String,
}

/// DataStore: データ管理の中核
///
/// ObjectPool、FinishedQueue、ArchivedIndexを統合し、
/// Worker/Logger/UiNotifier スレッドのライフサイクルを管理する。
pub struct DataStore {
    free_pool: Arc<SegQueue<Chunk>>,
    finished_list: Arc<RwLock<VecDeque<Arc<Chunk>>>>,
    archived_index: Arc<RwLock<Vec<PageMetadata>>>,
    timestamp_index: Arc<RwLock<Vec<ByteTimestamp>>>,
    line_index: Arc<RwLock<Vec<u64>>>, // 各行の開始オフセット（改行検出時に記録）
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
        use std::sync::atomic::AtomicU64;

        let pid = std::process::id();
        let base_dir = std::env::temp_dir().join("SerialMonitorEssential");

        // 起動時クリーンアップ: 古いPIDフォルダを削除
        cleanup_stale_directories(&base_dir, pid)?;

        // 一時ディレクトリ作成
        //
        // インスタンスごとに一意のサブディレクトリを使う。同一プロセス内で
        // DataStore が作り直されるとき（clear / ポート再オープン）、古い
        // インスタンスの Drop はプロッタスレッド等が Arc を離した後に遅延
        // 実行され得る。ディレクトリを共有していると、その Drop が新しい
        // インスタンスのライブなデータファイルまで削除してしまう。
        static INSTANCE_COUNTER: AtomicU64 = AtomicU64::new(0);
        let instance = INSTANCE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_dir = base_dir.join(pid.to_string()).join(instance.to_string());
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
            timestamp_index: Arc::new(RwLock::new(Vec::new())),
            line_index: Arc::new(RwLock::new(vec![0])), // 最初の行はオフセット0から始まる
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
            self.line_index.clone(), // Phase 2: pass line_index for pre-indexing
            self.stop_flag.clone(),
            app_handle.clone(),
        );

        // Logger Thread起動
        //
        // ディスク書き込みエラーは logger_thread からコールバックで戻り、
        // ここで `log-error` イベントとして UI に送る（SYS-F-205 / GAP-09）。
        // logger_thread 側を tauri 非依存に保つための構成。
        debug!("[DataStore] Spawning Logger Thread");
        let log_error_handle = app_handle.clone();
        let logger_handle = logger_thread::spawn_logger_thread(
            self.finished_list.clone(),
            self.archived_index.clone(),
            self.temp_dir.clone(),
            self.stop_flag.clone(),
            Box::new(move |message: String| {
                use tauri::Emitter;
                if let Err(e) = log_error_handle.emit("log-error", LogErrorPayload { message }) {
                    warn!("[DataStore] Failed to emit log-error: {:?}", e);
                }
            }),
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
        debug!(
            "[get_data] offset={}, length={}, archived_pages={}, finished_chunks={}",
            offset, length, archived_count, finished_count
        );

        // 1. まずディスク上（archived_index）を検索 - 確定済みデータ
        if let Ok(index) = self.archived_index.read() {
            // 二分探索で開始位置を特定: offsetが含まれる可能性のある最初のページを探す
            // offsetよりもstartが大きい最初のページの一つ前が候補
            let start_idx = match index.binary_search_by(|p| p.global_offset.cmp(&offset)) {
                Ok(i) => i,
                Err(i) => {
                    if i > 0 {
                        i - 1
                    } else {
                        0
                    }
                }
            };

            for page in index.iter().skip(start_idx) {
                let page_start = page.global_offset;

                // 最適化: 要求範囲を超えたら検索終了
                if page_start >= offset + length as u64 {
                    break;
                }

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
            // finished_listは短い前提だが、念のため最適化
            for chunk in list.iter() {
                let chunk_start = chunk.global_offset();

                // 最適化: 要求範囲を超えたら検索終了
                if chunk_start >= offset + length as u64 {
                    break;
                }

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

    /// テスト用: バイト列を finished_list に直接追加する
    ///
    /// 実際の受信スレッドを起動せずに get_data / total_bytes を検証するための
    /// ヘルパー（プロッタスレッドのテストからも使用）。
    #[cfg(test)]
    pub fn push_test_data(&self, data: &[u8]) {
        self.push_test_data_at(self.total_bytes(), data);
    }

    /// テスト用: 任意のグローバルオフセットにチャンクを直接置く
    ///
    /// オフセットに空隙を作ると、その手前を読む `get_data` は
    /// "Insufficient data" で失敗する。読み取り失敗パス
    /// （プロッタスレッドの read_failures / スキップアヘッド）の
    /// フォールト注入に使う。
    #[cfg(test)]
    pub fn push_test_data_at(&self, offset: u64, data: &[u8]) {
        let mut chunk = Chunk::new(data.len().max(1));
        chunk.push_data(data);
        chunk.set_global_offset(offset);
        if let Ok(mut list) = self.finished_list.write() {
            list.push_back(Arc::new(chunk));
        }
    }

    /// 現在のバイトカウントとタイムスタンプを記録
    ///
    /// 100ms間隔でUiNotifierから呼び出される。
    /// 前回と同じバイト数の場合は記録しない（データ未受信時）。
    pub fn record_timestamp(&self) {
        use std::time::{SystemTime, UNIX_EPOCH};

        let current_bytes = self.total_bytes();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        if let Ok(mut index) = self.timestamp_index.write() {
            // 前回と同じバイト数の場合はスキップ
            // また、システム時刻の巻き戻り（NTP調整等）で timestamp が
            // 逆行すると二分探索が壊れるため、単調性を保証する
            let mut timestamp = timestamp;
            if let Some(last) = index.last() {
                if last.cumulative_bytes == current_bytes {
                    return;
                }
                timestamp = timestamp.max(last.timestamp);
            }

            index.push(ByteTimestamp {
                timestamp,
                cumulative_bytes: current_bytes,
            });

            // 定期的にログ出力（1000エントリごと）
            if index.len() % 1000 == 0 {
                debug!("[DataStore] timestamp_index size: {} entries", index.len());
            }
        }
    }

    /// 指定オフセットに対応するタイムスタンプを取得
    ///
    /// 二分探索でcumulative_bytes <= offsetとなる最大のエントリを検索。
    /// 見つからない場合はNoneを返す。
    pub fn get_timestamp_for_offset(&self, offset: u64) -> Option<u64> {
        if let Ok(index) = self.timestamp_index.read() {
            if index.is_empty() {
                return None;
            }

            // 二分探索: cumulative_bytes <= offset となる最大のエントリを探す
            let result = index.binary_search_by(|entry| entry.cumulative_bytes.cmp(&offset));

            match result {
                Ok(i) => Some(index[i].timestamp),
                Err(0) => None, // offset が最小値よりも小さい
                Err(i) => Some(index[i - 1].timestamp),
            }
        } else {
            None
        }
    }

    /// timestamp_indexをクリア（データクリア時に使用）
    #[allow(dead_code)] // Will be used when clear_data is implemented
    pub fn clear_timestamps(&self) {
        if let Ok(mut index) = self.timestamp_index.write() {
            index.clear();
        }
    }

    /// 総行数を取得
    pub fn total_lines(&self) -> u64 {
        self.line_index
            .read()
            .map(|idx| idx.len() as u64)
            .unwrap_or(0)
    }

    /// 指定範囲の行オフセットを取得
    pub fn get_line_offsets(&self, start_line: u64, count: u32) -> Vec<(u64, u64)> {
        // ... (existing implementation)
        if let Ok(index) = self.line_index.read() {
            let total = index.len();
            let start = start_line as usize;
            let end = std::cmp::min(start + count as usize, total);

            if start >= total {
                return Vec::new();
            }

            let mut result = Vec::with_capacity(end - start);
            for i in start..end {
                let line_start = index[i];
                // 次の行の開始位置、または total_bytes
                let line_end = if i + 1 < total {
                    index[i + 1]
                } else {
                    // 最後の行はtotal_bytesまで
                    self.total_bytes()
                };
                result.push((line_start, line_end));
            }
            result
        } else {
            Vec::new()
        }
    }

    /// 指定バイトオフセットに対応する行インデックスを取得
    pub fn get_line_index_for_offset(&self, offset: u64) -> u64 {
        if let Ok(index) = self.line_index.read() {
            if index.is_empty() {
                return 0;
            }

            // Allow exact match or find insertion point (index of first element >= offset)
            match index.binary_search(&offset) {
                Ok(i) => i as u64, // Exact match, this is the start of line i
                Err(i) => {
                    // Insertion point is 'i'.
                    // If i > 0, the offset falls within line i-1.
                    // If i == 0, the offset is before the first line (should assume line 0)
                    if i > 0 {
                        (i - 1) as u64
                    } else {
                        0
                    }
                }
            }
        } else {
            0
        }
    }

    /// line_indexをクリア（データクリア時に使用）
    #[allow(dead_code)]
    pub fn clear_lines(&self) {
        if let Ok(mut index) = self.line_index.write() {
            index.clear();
            index.push(0); // 最初の行はオフセット0から
        }
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
        // temp_dir はインスタンス固有のディレクトリなので、他のインスタンスの
        // ファイルを巻き込むことはない。親の PID ディレクトリは意図的に残す
        // （並行する create_dir_all との競合を避けるため。次回起動時の
        // cleanup_stale_directories が回収する）。
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
    ///
    /// ディレクトリ名にはナノ秒に加えて単調カウンタを含める。プロパティテストは
    /// 1 テスト内で数百個のストアを作るため、時計の分解能（Windows では約 100ns）
    /// では衝突し得る。衝突すると別ケースの一時ファイルを共有してしまう。
    fn create_test_data_store() -> DataStore {
        static DIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

        let pid = std::process::id();
        let base_dir = std::env::temp_dir().join("SerialMonitorEssential_test");
        let temp_dir = base_dir.join(format!(
            "{}_{}_{}",
            pid,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            DIR_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&temp_dir).unwrap();

        DataStore {
            free_pool: Arc::new(SegQueue::new()),
            finished_list: Arc::new(RwLock::new(VecDeque::new())),
            archived_index: Arc::new(RwLock::new(Vec::new())),
            timestamp_index: Arc::new(RwLock::new(Vec::new())),
            line_index: Arc::new(RwLock::new(vec![0])),
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

    /// archived_indexにデータがあり、finished_listにもデータがある場合、
    /// 両方から正しい順序でデータを取得できることを確認
    /// (archived → finished の順で連続して取得するケース)
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
        // finished_listを先に検索するが、offset 0のデータはfinished_listにないので
        // スキップされ、次にarchived_indexを検索して正しく取得されるはず
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

    // ====== Timestamp Tests ======

    /// record_timestamp が正しくタイムスタンプを記録することを確認
    #[test]
    fn test_record_timestamp_basic() {
        let store = create_test_data_store();

        // データがないので total_bytes() は 0
        // 最初の record_timestamp は記録される
        store.record_timestamp();

        {
            let index = store.timestamp_index.read().unwrap();
            assert_eq!(index.len(), 1);
            assert_eq!(index[0].cumulative_bytes, 0);
        }

        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// record_timestamp が同じバイト数の場合は記録しないことを確認
    #[test]
    fn test_record_timestamp_skip_duplicate() {
        let store = create_test_data_store();

        // 1回目の記録
        store.record_timestamp();

        // 2回目の記録（バイト数変わらず）
        store.record_timestamp();

        {
            let index = store.timestamp_index.read().unwrap();
            // バイト数が同じなので1エントリのみ
            assert_eq!(index.len(), 1);
        }

        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// record_timestamp がバイト数増加時に記録することを確認
    #[test]
    fn test_record_timestamp_with_data() {
        let store = create_test_data_store();

        // 最初の記録（0バイト）
        store.record_timestamp();

        // データを追加
        let mut chunk = Chunk::new(100);
        chunk.push_data(&[1, 2, 3, 4, 5]);
        chunk.set_global_offset(0);
        {
            let mut list = store.finished_list.write().unwrap();
            list.push_back(Arc::new(chunk));
        }

        // 2回目の記録（5バイト）
        store.record_timestamp();

        {
            let index = store.timestamp_index.read().unwrap();
            assert_eq!(index.len(), 2);
            assert_eq!(index[0].cumulative_bytes, 0);
            assert_eq!(index[1].cumulative_bytes, 5);
        }

        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// get_timestamp_for_offset が空の状態でNoneを返すことを確認
    #[test]
    fn test_get_timestamp_for_offset_empty() {
        let store = create_test_data_store();

        let result = store.get_timestamp_for_offset(0);
        assert!(result.is_none());

        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// get_timestamp_for_offset が正しいタイムスタンプを返すことを確認
    #[test]
    fn test_get_timestamp_for_offset_exact_match() {
        let store = create_test_data_store();

        // タイムスタンプを手動で追加
        {
            let mut index = store.timestamp_index.write().unwrap();
            index.push(ByteTimestamp {
                timestamp: 1000,
                cumulative_bytes: 0,
            });
            index.push(ByteTimestamp {
                timestamp: 2000,
                cumulative_bytes: 100,
            });
            index.push(ByteTimestamp {
                timestamp: 3000,
                cumulative_bytes: 200,
            });
        }

        // 完全一致のケース
        assert_eq!(store.get_timestamp_for_offset(0), Some(1000));
        assert_eq!(store.get_timestamp_for_offset(100), Some(2000));
        assert_eq!(store.get_timestamp_for_offset(200), Some(3000));

        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// get_timestamp_for_offset が二分探索で正しいタイムスタンプを返すことを確認
    #[test]
    fn test_get_timestamp_for_offset_binary_search() {
        let store = create_test_data_store();

        {
            let mut index = store.timestamp_index.write().unwrap();
            index.push(ByteTimestamp {
                timestamp: 1000,
                cumulative_bytes: 0,
            });
            index.push(ByteTimestamp {
                timestamp: 2000,
                cumulative_bytes: 100,
            });
            index.push(ByteTimestamp {
                timestamp: 3000,
                cumulative_bytes: 200,
            });
        }

        // 中間のオフセット (cumulative_bytes <= offset となる最大エントリ)
        assert_eq!(store.get_timestamp_for_offset(50), Some(1000)); // 0 <= 50
        assert_eq!(store.get_timestamp_for_offset(150), Some(2000)); // 100 <= 150
        assert_eq!(store.get_timestamp_for_offset(250), Some(3000)); // 200 <= 250

        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// clear_timestamps がタイムスタンプをクリアすることを確認
    #[test]
    fn test_clear_timestamps() {
        let store = create_test_data_store();

        // タイムスタンプを追加
        {
            let mut index = store.timestamp_index.write().unwrap();
            index.push(ByteTimestamp {
                timestamp: 1000,
                cumulative_bytes: 0,
            });
        }

        // クリア
        store.clear_timestamps();

        {
            let index = store.timestamp_index.read().unwrap();
            assert!(index.is_empty());
        }

        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    // ====== Line Index Tests ======

    /// total_lines が初期状態で0を返すことを確認
    #[test]
    fn test_total_lines_initial() {
        let store = create_test_data_store();

        // line_indexは[0]で初期化されているので1エントリあるが、
        // total_linesは行数を返す（エントリ数）
        // 初期状態は1行目のオフセット0のみ
        assert_eq!(store.total_lines(), 1);

        let _ = fs::remove_dir_all(&store.temp_dir);
    }
    /// get_line_offsets が正しい範囲を返すことを確認
    #[test]
    fn test_get_line_offsets() {
        let store = create_test_data_store();

        // データを追加してtotal_bytesを設定
        let mut chunk = Chunk::new(100);
        let test_data = b"Line1\nLine2\nLine3";
        chunk.push_data(test_data);
        chunk.set_global_offset(0);
        {
            let mut list = store.finished_list.write().unwrap();
            list.push_back(Arc::new(chunk));
        }

        // 行インデックスを直接設定
        {
            let mut index = store.line_index.write().unwrap();
            index.push(6); // "Line1\n" の後
            index.push(12); // "Line2\n" の後
        }

        // 行0から2行取得
        let offsets = store.get_line_offsets(0, 2);
        assert_eq!(offsets.len(), 2);
        assert_eq!(offsets[0], (0, 6)); // Line1\n
        assert_eq!(offsets[1], (6, 12)); // Line2\n

        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// get_line_offsets が範囲外の場合に空を返すことを確認
    #[test]
    fn test_get_line_offsets_out_of_range() {
        let store = create_test_data_store();

        // 行インデックスは初期状態 [0] のみ
        let offsets = store.get_line_offsets(100, 10);
        assert!(offsets.is_empty());

        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// get_line_offsets が最後の行でtotal_bytesを使うことを確認
    #[test]
    fn test_get_line_offsets_last_line() {
        let store = create_test_data_store();

        // データを追加（改行なし）
        let mut chunk = Chunk::new(100);
        let test_data = b"NoNewline";
        chunk.push_data(test_data);
        chunk.set_global_offset(0);
        {
            let mut list = store.finished_list.write().unwrap();
            list.push_back(Arc::new(chunk));
        }

        // 行インデックスは [0] のみ（改行なし）
        let offsets = store.get_line_offsets(0, 1);
        assert_eq!(offsets.len(), 1);
        assert_eq!(offsets[0], (0, 9)); // 0 から total_bytes (9) まで

        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// clear_lines が行インデックスをリセットすることを確認
    #[test]
    fn test_clear_lines() {
        let store = create_test_data_store();

        // 行を直接追加
        {
            let mut index = store.line_index.write().unwrap();
            index.push(6);
            index.push(12);
        }

        // クリア
        store.clear_lines();

        {
            let index = store.line_index.read().unwrap();
            // [0] にリセット
            assert_eq!(index.len(), 1);
            assert_eq!(index[0], 0);
        }

        let _ = fs::remove_dir_all(&store.temp_dir);
    }
    /// get_data with length 0 should return empty vec
    #[test]
    fn test_get_data_zero_length() {
        let store = create_test_data_store();

        // Add some data
        let mut chunk = Chunk::new(100);
        let test_data: Vec<u8> = (0..50).collect();
        chunk.push_data(&test_data);
        chunk.set_global_offset(0);
        {
            let mut list = store.finished_list.write().unwrap();
            list.push_back(Arc::new(chunk));
        }

        // Request zero bytes
        let result = store.get_data(0, 0).unwrap();
        assert!(result.is_empty());

        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// get_data from multiple archived chunks
    #[test]
    fn test_get_data_multiple_archived_chunks() {
        let store = create_test_data_store();

        // Create two separate archived pages
        let test_file = store.temp_dir.join("test_data.bin");
        let data1: Vec<u8> = (0..100).collect();
        let data2: Vec<u8> = (100..200).collect();
        {
            let mut file = std::fs::File::create(&test_file).unwrap();
            file.write_all(&data1).unwrap();
            file.write_all(&data2).unwrap();
        }

        // Add two pages to archived_index
        {
            let mut index = store.archived_index.write().unwrap();
            index.push(PageMetadata {
                file_path: test_file.clone(),
                file_offset: 0,
                data_length: 100,
                global_offset: 0,
            });
            index.push(PageMetadata {
                file_path: test_file.clone(),
                file_offset: 100,
                data_length: 100,
                global_offset: 100,
            });
        }

        // Read spanning both chunks
        let result = store.get_data(50, 100).unwrap();
        let expected: Vec<u8> = (50..150).collect();
        assert_eq!(result.len(), 100);
        assert_eq!(result, expected);

        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// get_data binary search edge case: exact match at page boundary
    #[test]
    fn test_get_data_exact_boundary() {
        let store = create_test_data_store();

        let test_file = store.temp_dir.join("test_data.bin");
        let data: Vec<u8> = (0..100).collect();
        {
            let mut file = std::fs::File::create(&test_file).unwrap();
            file.write_all(&data).unwrap();
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

        // Read exactly from start (offset 0)
        let result = store.get_data(0, 50).unwrap();
        let expected: Vec<u8> = (0..50).collect();
        assert_eq!(result, expected);

        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// get_data from multiple finished_list chunks
    #[test]
    fn test_get_data_multiple_finished_chunks() {
        let store = create_test_data_store();

        // Add two chunks to finished_list
        {
            let mut chunk1 = Chunk::new(100);
            let data1: Vec<u8> = (0..50).collect();
            chunk1.push_data(&data1);
            chunk1.set_global_offset(0);

            let mut chunk2 = Chunk::new(100);
            let data2: Vec<u8> = (50..100).collect();
            chunk2.push_data(&data2);
            chunk2.set_global_offset(50);

            let mut list = store.finished_list.write().unwrap();
            list.push_back(Arc::new(chunk1));
            list.push_back(Arc::new(chunk2));
        }

        // Read spanning both chunks
        let result = store.get_data(25, 50).unwrap();
        let expected: Vec<u8> = (25..75).collect();
        assert_eq!(result.len(), 50);
        assert_eq!(result, expected);

        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// get_timestamp_for_offset should return None when offset is before first entry
    #[test]
    fn test_get_timestamp_for_offset_before_first() {
        let store = create_test_data_store();

        // Add timestamp starting at offset 100
        {
            let mut index = store.timestamp_index.write().unwrap();
            index.push(ByteTimestamp {
                timestamp: 1000,
                cumulative_bytes: 100,
            });
        }

        // Query for offset 50 (before first entry)
        let result = store.get_timestamp_for_offset(50);
        assert!(result.is_none());

        // Query for offset 100 (exact match)
        let result = store.get_timestamp_for_offset(100);
        assert_eq!(result, Some(1000));

        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    /// Test get_line_offsets with partial count request
    #[test]
    fn test_get_line_offsets_partial_count() {
        let store = create_test_data_store();

        // Add data with newlines
        let mut chunk = Chunk::new(100);
        let test_data = b"A\nB\nC\nD\n";
        chunk.push_data(test_data);
        chunk.set_global_offset(0);
        {
            let mut list = store.finished_list.write().unwrap();
            list.push_back(Arc::new(chunk));
        }

        // Set line index directly: [0, 2, 4, 6, 8]
        {
            let mut index = store.line_index.write().unwrap();
            index.push(2); // "A\n"
            index.push(4); // "B\n"
            index.push(6); // "C\n"
            index.push(8); // "D\n"
        }

        // Request more lines than available
        let offsets = store.get_line_offsets(0, 100);
        // Should return only available lines: [0, 2, 4, 6, 8] = 5 entries
        assert_eq!(offsets.len(), 5);

        let _ = fs::remove_dir_all(&store.temp_dir);
    }

    // ================================================================
    // get_data: 読み出しの一貫性（P-8）と境界値分析
    //
    // docs/24_vv_plan.md §3.3 P-8 / §2.2 の 1 行目に対応する。
    //
    // ## 2 記憶域の順序契約（実装を読んで確定した実際の不変条件）
    //
    // `get_data` は archived_index → finished_list の順に **1 パスずつ** 走り、
    // `current_offset` を前へ進めることしかしない。したがって 1 回の読み出しで
    // 満たすべき条件は次のとおり:
    //
    // > 要求範囲 `[offset, offset+length)` のうち archived_index が供給する部分は、
    // > 必ず **先頭側の連続した前半**でなければならない。finished_list は残りの
    // > 後半だけを供給する。
    //
    // つまり「archived と finished をオフセット順で任意に交互配置してよい」わけ
    // ではない。finished のチャンクより後ろに archived のページが来ると、その
    // 継ぎ目をまたぐ読み出しは（データが全部存在していても）"Insufficient data"
    // で失敗する。これは `test_get_data_ordering_contract_archived_must_precede_finished`
    // で明示的に固定してある。
    //
    // 本番でこの契約が成り立つ理由: logger_thread は finished_list の **先頭から**
    // 順にディスクへ落として archived_index へ push する（logger_thread.rs
    // `process_buffer`）。よって archived は常に `[0, A)` を、finished は常に
    // `[A, total)` を担当する。下のプロパティはこの実際の不変条件を生成する。
    // ================================================================

    use proptest::prelude::*;

    /// アーカイブファイルの先頭に詰める junk。`file_offset != 0` の経路を通す。
    const LEADING_JUNK: [u8; 16] = [0xAA; 16];
    /// アーカイブファイルの末尾に詰める junk。`data_length` を無視して読む実装なら
    /// これを拾ってしまうので、境界の取り違えを検出できる。
    const TRAILING_JUNK: [u8; 4] = [0x55; 4];

    /// 論理ストリームの `i` バイト目。
    ///
    /// 251 は素数で、以下で使うどのページ長・チャンク長とも互いに素なので、
    /// 不一致が起きたときに「実際に読まれたグローバルオフセット」が値から逆算できる。
    fn expected_byte(i: usize) -> u8 {
        (i % 251) as u8
    }

    fn expected_bytes(total: usize) -> Vec<u8> {
        (0..total).map(expected_byte).collect()
    }

    /// `data` を temp_dir 内の新規ファイルへ書き、archived_index へ登録する。
    ///
    /// 先頭に `junk_len` バイトの junk を置くので `file_offset` は 0 以外になる。
    fn add_archived_page(
        store: &DataStore,
        name: &str,
        global_offset: u64,
        data: &[u8],
        junk_len: usize,
    ) {
        let junk_len = junk_len.min(LEADING_JUNK.len());
        let path = store.temp_dir.join(name);
        {
            let mut file = std::fs::File::create(&path).unwrap();
            file.write_all(&LEADING_JUNK[..junk_len]).unwrap();
            file.write_all(data).unwrap();
            file.write_all(&TRAILING_JUNK).unwrap();
        }
        store.archived_index.write().unwrap().push(PageMetadata {
            file_path: path,
            file_offset: junk_len as u64,
            data_length: data.len(),
            global_offset,
        });
    }

    /// `archived` → `finished` の長さ列からストアを組み立てる。
    ///
    /// 内容は `expected_byte` 列。グローバルオフセットは 0 から連続で、
    /// archived が前半・finished が後半（= 上で述べた実際の順序契約）。
    /// 戻り値の `Vec<u8>` が期待されるストリーム全体。
    fn build_store_from_segments(
        archived: &[usize],
        finished: &[usize],
        junk: &[usize],
    ) -> (DataStore, Vec<u8>) {
        let total: usize = archived.iter().chain(finished.iter()).sum();
        let expected = expected_bytes(total);
        let store = create_test_data_store();

        let mut global = 0usize;
        for (i, &len) in archived.iter().enumerate() {
            let junk_len = if junk.is_empty() {
                0
            } else {
                junk[i % junk.len()]
            };
            add_archived_page(
                &store,
                &format!("page_{i}.bin"),
                global as u64,
                &expected[global..global + len],
                junk_len,
            );
            global += len;
        }
        for &len in finished {
            store.push_test_data_at(global as u64, &expected[global..global + len]);
            global += len;
        }

        (store, expected)
    }

    /// 成功するはずの読み出し。失敗したらオフセット付きで panic する。
    fn read_span(store: &DataStore, offset: u64, length: u32) -> Vec<u8> {
        match store.get_data(offset, length) {
            Ok(v) => v,
            Err(e) => panic!("get_data({offset}, {length}) unexpectedly failed: {e}"),
        }
    }

    /// BVA 用の固定レイアウト。
    ///
    /// archived: [0,40) [40,100)   finished: [100,150) [150,200)
    /// 継ぎ目は 40（archived 同士）/ 100（archived→finished）/ 150（finished 同士）。
    fn seam_fixture() -> (DataStore, Vec<u8>) {
        build_store_from_segments(&[40, 60], &[50, 50], &[0, 7])
    }

    // ---------------- P-8: プロパティ ----------------

    /// 生成された記憶域レイアウト。
    #[derive(Debug, Clone)]
    struct StorageLayout {
        /// archived_index に置くセグメント長（グローバルオフセット順の前半）
        archived: Vec<usize>,
        /// finished_list に置くセグメント長（後半）
        finished: Vec<usize>,
        /// archived ページごとの先頭 junk バイト数（file_offset を散らす）
        junk: Vec<usize>,
    }

    impl StorageLayout {
        fn total(&self) -> usize {
            self.archived.iter().chain(self.finished.iter()).sum()
        }
    }

    /// 合計 1..=4096 バイトを 1..=8 個の連続セグメントに分割し、
    /// 先頭から任意個を archived、残りを finished に割り当てる。
    fn storage_layout() -> impl Strategy<Value = StorageLayout> {
        (
            1usize..=4096usize,
            prop::collection::vec(0usize..4096, 0..8usize),
            0usize..=8usize,
            prop::collection::vec(0usize..=16, 8),
        )
            .prop_map(|(total, raw_cuts, archived_pick, junk)| {
                // 内部の切れ目を 1..total に写して整列・重複除去する。
                let mut cuts: Vec<usize> = raw_cuts
                    .into_iter()
                    .map(|c| 1 + c % total)
                    .filter(|c| *c < total)
                    .collect();
                cuts.sort_unstable();
                cuts.dedup();

                let mut segments = Vec::with_capacity(cuts.len() + 1);
                let mut prev = 0usize;
                for c in cuts {
                    segments.push(c - prev);
                    prev = c;
                }
                segments.push(total - prev);

                let archived_len = archived_pick % (segments.len() + 1);
                let finished = segments.split_off(archived_len);
                StorageLayout {
                    archived: segments,
                    finished,
                    junk,
                }
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

        /// P-8 読み出しの一貫性（SYS-F-203 / docs/24_vv_plan.md §3.3）
        ///
        /// 任意の記憶域レイアウトに対して:
        /// 1. 全体読み出しが論理ストリームと一致する
        /// 2. 任意の (offset, length) を 1..=5 回に分割して読んで連結したものが、
        ///    一括読み出しと一致する（= 読み出し境界は観測できない）
        /// 3. 任意の範囲内 (offset, length) が `expected[offset..offset+length]` そのもの
        /// 4. 範囲外は Err（部分成功しない）／`length == 0` は常に Ok(空)
        #[test]
        fn prop_get_data_split_read_consistency(
            layout in storage_layout(),
            windows in prop::collection::vec((any::<u64>(), any::<u64>()), 3),
            split_seeds in prop::collection::vec(prop::collection::vec(any::<u64>(), 0..5), 3),
        ) {
            let total = layout.total();
            let (store, expected) =
                build_store_from_segments(&layout.archived, &layout.finished, &layout.junk);

            prop_assert_eq!(store.total_bytes(), total as u64);

            // (1) 全体読み出し
            let whole = read_span(&store, 0, total as u32);
            prop_assert_eq!(whole.as_slice(), expected.as_slice());

            for (w, seeds) in windows.iter().zip(split_seeds.iter()) {
                let offset = (w.0 % (total as u64 + 1)) as usize;
                let length = (w.1 % (total as u64 - offset as u64 + 1)) as usize;

                // (3) 任意窓は期待バイト列そのもの
                let got = read_span(&store, offset as u64, length as u32);
                prop_assert_eq!(got.as_slice(), &expected[offset..offset + length]);

                // (2) 同じ窓を 1..=5 個の連続部分読み出しに割ってから連結する
                let mut cuts: Vec<usize> = seeds
                    .iter()
                    .map(|s| (s % (length as u64 + 1)) as usize)
                    .collect();
                cuts.sort_unstable();

                let mut concat = Vec::with_capacity(length);
                let mut cursor = offset;
                for end in cuts
                    .iter()
                    .map(|c| offset + c)
                    .chain(std::iter::once(offset + length))
                {
                    let part_len = end - cursor;
                    let part = read_span(&store, cursor as u64, part_len as u32);
                    prop_assert_eq!(part.len(), part_len);
                    concat.extend_from_slice(&part);
                    cursor = end;
                }
                prop_assert_eq!(concat.as_slice(), got.as_slice());

                // (4a) length == 0 は常に Ok(空)
                let empty = read_span(&store, offset as u64, 0);
                prop_assert!(empty.is_empty());

                // (4b) 1 バイトでも範囲を超えたら Err（部分成功しない）
                let over_len = (total - offset) as u32 + 1;
                let over = store.get_data(offset as u64, over_len);
                prop_assert!(
                    over.is_err(),
                    "get_data({}, {}) should fail past the end of {} bytes",
                    offset,
                    over_len,
                    total
                );
            }

            // store の Drop が temp_dir ごと削除する（アサート失敗時も同じ）
        }
    }

    // ---------------- 順序契約（実装の実際の不変条件を固定する） ----------------

    /// **順序契約**: 1 回の読み出しの中では archived が必ず finished より前に来る。
    ///
    /// `get_data` は archived_index を 1 パス走ってから finished_list を 1 パス
    /// 走るだけで、`current_offset` を後戻りさせない。したがって
    /// 「finished のチャンクより後ろにある archived のページ」は、その継ぎ目を
    /// またぐ読み出しからは見えず、データが全部そろっていても Err になる。
    ///
    /// これは本番では起こらない（logger_thread は finished_list の先頭から
    /// 順にアーカイブするため archived は常にストリームの前半）。この配置が
    /// **サポート外**であることを明示的に固定するためのテスト。
    #[test]
    fn test_get_data_ordering_contract_archived_must_precede_finished() {
        // --- ケース A: finished [0,10) の後ろに archived [10,20) ---
        {
            let expected = expected_bytes(20);
            let store = create_test_data_store();
            store.push_test_data_at(0, &expected[0..10]);
            add_archived_page(&store, "tail.bin", 10, &expected[10..20], 3);

            // 片方の記憶域に収まる読み出しは成功する
            assert_eq!(read_span(&store, 0, 10), expected[0..10].to_vec());
            assert_eq!(read_span(&store, 10, 10), expected[10..20].to_vec());

            // 継ぎ目をまたぐと、データが全部あっても失敗する
            let spanning = store.get_data(0, 20);
            assert!(
                spanning.is_err(),
                "finished -> archived の順序は未サポートのはずが成功した: {spanning:?}"
            );
        }

        // --- ケース B: archived [0,10) / finished [10,20) / archived [20,30) ---
        {
            let expected = expected_bytes(30);
            let store = create_test_data_store();
            add_archived_page(&store, "head.bin", 0, &expected[0..10], 0);
            store.push_test_data_at(10, &expected[10..20]);
            add_archived_page(&store, "tail.bin", 20, &expected[20..30], 5);

            // archived -> finished の向きなら継ぎ目をまたげる
            assert_eq!(read_span(&store, 0, 20), expected[0..20].to_vec());
            // 2 つ目の archived 単独も読める
            assert_eq!(read_span(&store, 20, 10), expected[20..30].to_vec());

            // finished を挟んで再び archived へ戻る読み出しは失敗する
            let spanning = store.get_data(0, 30);
            assert!(
                spanning.is_err(),
                "archived -> finished -> archived は未サポートのはずが成功した: {spanning:?}"
            );
        }
    }

    // ---------------- 境界値分析（docs/24_vv_plan.md §2.2 1 行目） ----------------

    /// BVA: `length = 0`（ストリーム両端・記憶域の内部・範囲外）
    ///
    /// **所見**: `length == 0` は範囲検査より先に短絡するため、`offset` が
    /// どれだけ範囲外でも Err にならず Ok(空) を返す。実挙動として固定する。
    #[test]
    fn test_get_data_bva_zero_length() {
        let (store, _expected) = seam_fixture();
        let total = store.total_bytes();
        assert_eq!(total, 200);

        // 先頭
        assert_eq!(read_span(&store, 0, 0), Vec::<u8>::new());
        // 末尾ちょうど（offset == total_bytes）
        assert_eq!(read_span(&store, total, 0), Vec::<u8>::new());
        // archived ページ内部 / finished チャンク内部 / 各継ぎ目
        for offset in [20u64, 40, 99, 100, 120, 150, 199] {
            assert_eq!(
                read_span(&store, offset, 0),
                Vec::<u8>::new(),
                "zero-length read at {offset} should be empty"
            );
        }
        // 所見: 範囲外オフセットでも Ok(空)
        assert_eq!(read_span(&store, total + 10_000, 0), Vec::<u8>::new());
    }

    /// BVA: `offset == total_bytes`（空の末尾読み出し）とその直前・直後
    #[test]
    fn test_get_data_bva_offset_equals_total_bytes() {
        let (store, expected) = seam_fixture();
        let total = store.total_bytes();

        // 末尾ちょうどの空読み出しは成功
        assert_eq!(read_span(&store, total, 0), Vec::<u8>::new());
        // 末尾ちょうどから 1 バイトは失敗
        assert!(store.get_data(total, 1).is_err());
        // 最後の 1 バイトは読める
        assert_eq!(read_span(&store, total - 1, 1), vec![expected_byte(199)]);
        // ちょうど末尾で終わる読み出しは成功、1 バイト超過は失敗
        assert_eq!(read_span(&store, 150, 50), expected[150..200].to_vec());
        assert!(store.get_data(150, 51).is_err());
        assert!(store.get_data(0, 201).is_err());
    }

    /// BVA: 各境界で「ちょうど終わる」読み出し
    #[test]
    fn test_get_data_bva_read_ends_exactly_at_boundary() {
        let (store, expected) = seam_fixture();

        // archived ページ同士の継ぎ目 40 でちょうど終わる
        assert_eq!(read_span(&store, 0, 40), expected[0..40].to_vec());
        assert_eq!(read_span(&store, 39, 1), expected[39..40].to_vec());
        // archived -> finished の継ぎ目 100 でちょうど終わる
        assert_eq!(read_span(&store, 40, 60), expected[40..100].to_vec());
        assert_eq!(read_span(&store, 0, 100), expected[0..100].to_vec());
        // finished チャンク同士の継ぎ目 150 でちょうど終わる
        assert_eq!(read_span(&store, 100, 50), expected[100..150].to_vec());
        assert_eq!(read_span(&store, 0, 150), expected[0..150].to_vec());
        // ストリーム末尾 200 でちょうど終わる
        assert_eq!(read_span(&store, 0, 200), expected.clone());
    }

    /// BVA: 各境界から「ちょうど始まる」読み出し
    #[test]
    fn test_get_data_bva_read_starts_exactly_at_boundary() {
        let (store, expected) = seam_fixture();

        // archived ページ同士の継ぎ目 40 から
        assert_eq!(read_span(&store, 40, 60), expected[40..100].to_vec());
        assert_eq!(read_span(&store, 40, 1), expected[40..41].to_vec());
        // archived -> finished の継ぎ目 100 から
        assert_eq!(read_span(&store, 100, 100), expected[100..200].to_vec());
        assert_eq!(read_span(&store, 100, 1), expected[100..101].to_vec());
        // finished チャンク同士の継ぎ目 150 から
        assert_eq!(read_span(&store, 150, 50), expected[150..200].to_vec());
        // ストリーム先頭 0 から
        assert_eq!(read_span(&store, 0, 1), expected[0..1].to_vec());
    }

    /// BVA: archived と finished の継ぎ目をちょうどまたぐ読み出し
    #[test]
    fn test_get_data_bva_read_spans_seam_exactly() {
        let (store, expected) = seam_fixture();

        // 継ぎ目 100 をまたぐ最小の読み出し（archived 最終バイト + finished 先頭バイト）
        assert_eq!(read_span(&store, 99, 2), expected[99..101].to_vec());
        // archived を丸ごと + finished 先頭 1 バイト
        assert_eq!(read_span(&store, 0, 101), expected[0..101].to_vec());
        // archived 最終 1 バイト + finished を丸ごと
        assert_eq!(read_span(&store, 99, 101), expected[99..200].to_vec());
        // archived ページ同士の継ぎ目 40 をまたぐ最小の読み出し
        assert_eq!(read_span(&store, 39, 2), expected[39..41].to_vec());
        // finished チャンク同士の継ぎ目 150 をまたぐ最小の読み出し
        assert_eq!(read_span(&store, 149, 2), expected[149..151].to_vec());
        // 3 つの継ぎ目すべてをまたぐ
        assert_eq!(read_span(&store, 39, 112), expected[39..151].to_vec());
    }

    /// BVA: 各継ぎ目の両側での 1 バイト読み出し
    #[test]
    fn test_get_data_bva_single_byte_reads_at_seams() {
        let (store, _expected) = seam_fixture();

        for seam in [40usize, 100, 150] {
            assert_eq!(
                read_span(&store, seam as u64 - 1, 1),
                vec![expected_byte(seam - 1)],
                "byte just before seam {seam}"
            );
            assert_eq!(
                read_span(&store, seam as u64, 1),
                vec![expected_byte(seam)],
                "byte just after seam {seam}"
            );
        }

        // ストリーム両端の 1 バイト
        assert_eq!(read_span(&store, 0, 1), vec![expected_byte(0)]);
        assert_eq!(read_span(&store, 199, 1), vec![expected_byte(199)]);
    }

    /// BVA: 単一チャンク／単一ページだけのストアでの端点
    #[test]
    fn test_get_data_bva_single_segment_edges() {
        // archived 1 ページのみ
        {
            let (store, expected) = build_store_from_segments(&[1], &[], &[9]);
            assert_eq!(read_span(&store, 0, 1), expected.clone());
            assert_eq!(read_span(&store, 0, 0), Vec::<u8>::new());
            assert_eq!(read_span(&store, 1, 0), Vec::<u8>::new());
            assert!(store.get_data(1, 1).is_err());
            assert!(store.get_data(0, 2).is_err());
        }

        // finished 1 チャンクのみ
        {
            let (store, expected) = build_store_from_segments(&[], &[1], &[]);
            assert_eq!(read_span(&store, 0, 1), expected.clone());
            assert_eq!(read_span(&store, 1, 0), Vec::<u8>::new());
            assert!(store.get_data(1, 1).is_err());
            assert!(store.get_data(0, 2).is_err());
        }
    }
}
