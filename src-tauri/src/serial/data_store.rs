use crossbeam::queue::SegQueue;
use std::fs;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, RwLock,
};
use std::thread::JoinHandle;

use super::chunk::Chunk;
use super::logger_thread::{self, PageMetadata};
use super::object_pool::ObjectPool;
use super::port::SerialPort;
use super::worker_thread;

const CHUNK_SIZE: usize = 64 * 1024; // 64KB
const INITIAL_POOL_SIZE: usize = 100; // 約6.4MB

/// DataStore: データ管理の中核
///
/// ObjectPool、FinishedQueue、ArchivedIndexを統合し、
/// Worker/Logger スレッドのライフサイクルを管理する。
pub struct DataStore {
    free_pool: Arc<SegQueue<Chunk>>,
    finished_queue: Arc<SegQueue<Chunk>>,
    archived_index: Arc<RwLock<Vec<PageMetadata>>>,
    temp_dir: PathBuf,

    // スレッド管理
    worker_handle: Mutex<Option<JoinHandle<()>>>,
    logger_handle: Mutex<Option<JoinHandle<()>>>,
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

        // ObjectPool初期化
        let pool = ObjectPool::new(INITIAL_POOL_SIZE, CHUNK_SIZE);

        Ok(Self {
            free_pool: pool.as_arc(),
            finished_queue: Arc::new(SegQueue::new()),
            archived_index: Arc::new(RwLock::new(Vec::new())),
            temp_dir,
            worker_handle: Mutex::new(None),
            logger_handle: Mutex::new(None),
            stop_flag: Arc::new(AtomicBool::new(false)),
        })
    }

    /// データ受信を開始
    ///
    /// # Arguments
    /// * `port` - SerialPortのArc<Mutex>
    pub fn start_reception(&self, port: Arc<Mutex<SerialPort>>) -> Result<(), String> {
        println!("[DataStore] Starting reception");
        // 既に動作中の場合は停止
        self.stop_reception();

        // 停止フラグをリセット
        self.stop_flag.store(false, Ordering::Relaxed);

        // Worker Thread起動
        println!("[DataStore] Spawning Worker Thread");
        let worker_handle = worker_thread::spawn_worker_thread(
            port,
            self.free_pool.clone(),
            self.finished_queue.clone(),
            self.stop_flag.clone(),
        );

        // Logger Thread起動
        println!("[DataStore] Spawning Logger Thread");
        let logger_handle = logger_thread::spawn_logger_thread(
            self.free_pool.clone(),
            self.finished_queue.clone(),
            self.archived_index.clone(),
            self.temp_dir.clone(),
            self.stop_flag.clone(),
        );

        // ハンドルを保存
        *self.worker_handle.lock().unwrap() = Some(worker_handle);
        *self.logger_handle.lock().unwrap() = Some(logger_handle);

        println!("[DataStore] Reception started successfully");
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
    pub fn get_data(&self, offset: u64, length: u32) -> Result<Vec<u8>, String> {
        let length = length as usize;
        let mut result = Vec::with_capacity(length);
        let mut current_offset = offset;
        let mut remaining = length;

        // まずメモリ上（finished_queue）を検索
        // Note: SegQueueは順序アクセスのみなのでここでは簡易実装
        // 本格実装では finished_queue の内容をキャッシュするか、別の構造が必要

        // ディスク上のデータを検索
        if let Ok(index) = self.archived_index.read() {
            for page in index.iter() {
                if current_offset >= page.global_offset
                    && current_offset < page.global_offset + page.data_length as u64
                {
                    // このページにデータがある
                    let page_offset = (current_offset - page.global_offset) as usize;
                    let to_read = remaining.min(page.data_length - page_offset);

                    // ファイルから読み取り（簡易実装）
                    // TODO: 実際のファイル読み取り実装
                    result.resize(result.len() + to_read, 0);

                    current_offset += to_read as u64;
                    remaining -= to_read;

                    if remaining == 0 {
                        break;
                    }
                }
            }
        }

        Ok(result)
    }

    /// 受信済み総バイト数を取得
    pub fn total_bytes(&self) -> u64 {
        if let Ok(index) = self.archived_index.read() {
            index
                .last()
                .map(|p| p.global_offset + p.data_length as u64)
                .unwrap_or(0)
        } else {
            0
        }
    }

    /// 一時ディレクトリパスを取得
    pub fn temp_dir(&self) -> &PathBuf {
        &self.temp_dir
    }
}

impl Drop for DataStore {
    fn drop(&mut self) {
        println!(
            "[DataStore::Drop] Cleaning up, temp_dir: {:?}",
            self.temp_dir
        );

        // スレッドを停止
        self.stop_reception();

        // 一時ファイルを削除（ベストエフォート）
        match fs::remove_dir_all(&self.temp_dir) {
            Ok(_) => println!("[DataStore::Drop] Successfully removed temp directory"),
            Err(e) => eprintln!("[DataStore::Drop] Failed to remove temp directory: {:?}", e),
        }

        println!("[DataStore::Drop] Cleanup complete");
    }
}

/// 古い一時ディレクトリをクリーンアップ
///
/// プロセスが存在しないPIDフォルダを削除する
fn cleanup_stale_directories(base_dir: &std::path::Path, current_pid: u32) -> Result<(), String> {
    if !base_dir.exists() {
        return Ok(());
    }

    println!(
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
                        println!("[cleanup] Removing stale directory for PID: {}", pid);
                        if let Err(e) = fs::remove_dir_all(entry.path()) {
                            eprintln!(
                                "[cleanup] Failed to remove directory for PID {}: {:?}",
                                pid, e
                            );
                        }
                    } else {
                        println!("[cleanup] Keeping directory for active PID: {}", pid);
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("[cleanup] Failed to read base directory: {:?}", e);
        }
    }

    Ok(())
}

/// プロセスが実行中かどうかを確認（Windows）
#[cfg(windows)]
fn is_process_running(pid: u32) -> bool {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => {
                // プロセス名を取得して、SerialMonitorEssentialかどうか確認
                let mut buffer = [0u16; 1024];
                let mut size = buffer.len() as u32;

                let is_our_process = match QueryFullProcessImageNameW(
                    handle,
                    windows::Win32::System::Threading::PROCESS_NAME_WIN32,
                    PWSTR(buffer.as_mut_ptr()),
                    &mut size,
                ) {
                    Ok(_) => {
                        let process_path = String::from_utf16_lossy(&buffer[..size as usize]);
                        let is_ours = process_path
                            .to_lowercase()
                            .contains("serialmonitoressential");
                        println!(
                            "[is_process_running] PID {}: path={}, is_ours={}",
                            pid, process_path, is_ours
                        );
                        is_ours
                    }
                    Err(e) => {
                        println!("[is_process_running] PID {}: Failed to get process name: {:?}, assuming not ours", pid, e);
                        false
                    }
                };

                let _ = CloseHandle(handle);
                is_our_process
            }
            Err(e) => {
                println!(
                    "[is_process_running] PID {} does not exist (OpenProcess failed: {:?})",
                    pid, e
                );
                false
            }
        }
    }
}

/// プロセスが実行中かどうかを確認（非Windows）
#[cfg(not(windows))]
fn is_process_running(_pid: u32) -> bool {
    // 非Windowsでは常にfalseを返す（削除する）
    false
}
