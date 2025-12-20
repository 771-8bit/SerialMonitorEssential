use super::chunk::Chunk;
use super::port::SerialPort;
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, RwLock,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Worker Thread: シリアルポートからデータを受信
///
/// ReadFileを高速に呼び出し、Chunkにデータを詰める。
/// 16ms経過 or 満杯で finished_list に移動する。
const SWAP_TIMEOUT_MS: u64 = 16; // 約60fps
const READ_BUFFER_SIZE: usize = 4096; // 一度に読み取るサイズ

pub fn spawn_worker_thread(
    port: Arc<Mutex<SerialPort>>,
    free_pool: Arc<crossbeam::queue::SegQueue<Chunk>>,
    finished_list: Arc<RwLock<VecDeque<Arc<Chunk>>>>,
    stop_flag: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        println!("[Worker] Thread started");
        let mut current_chunk: Option<Chunk> = None;
        let mut last_swap = Instant::now();
        let mut read_buffer = vec![0u8; READ_BUFFER_SIZE];
        let mut total_bytes_read = 0u64;
        let mut global_offset = 0u64; // グローバルオフセットを追跡

        loop {
            // 停止フラグチェック
            if stop_flag.load(Ordering::Relaxed) {
                println!(
                    "[Worker] Stop flag detected, total bytes read: {}",
                    total_bytes_read
                );
                // 残っているChunkがあればfinished_listに送る
                if let Some(mut chunk) = current_chunk.take() {
                    if chunk.has_data() {
                        chunk.set_global_offset(global_offset);
                        println!("[Worker] Pushing final chunk with {} bytes", chunk.len());
                        if let Ok(mut list) = finished_list.write() {
                            list.push_back(Arc::new(chunk));
                        }
                    }
                }
                break;
            }

            // Chunkがなければ取得
            if current_chunk.is_none() {
                current_chunk = Some(free_pool.pop().unwrap_or_else(|| Chunk::new(64 * 1024)));
                last_swap = Instant::now();
            }

            // データ読み取り
            let bytes_read = {
                if let Ok(p) = port.lock() {
                    match p.read(&mut read_buffer) {
                        Ok(n) => n,
                        Err(e) => {
                            eprintln!("[Worker] Read error: {}", e);
                            0
                        }
                    }
                } else {
                    eprintln!("[Worker] Mutex poisoned");
                    break;
                }
            };

            if bytes_read > 0 {
                total_bytes_read += bytes_read as u64;
                println!(
                    "[Worker] Read {} bytes (total: {})",
                    bytes_read, total_bytes_read
                );

                let mut offset = 0;
                while offset < bytes_read {
                    // Chunkがなければ取得
                    if current_chunk.is_none() {
                        current_chunk =
                            Some(free_pool.pop().unwrap_or_else(|| Chunk::new(64 * 1024)));
                        last_swap = Instant::now();
                    }

                    if let Some(ref mut chunk) = current_chunk {
                        let written = chunk.push_data(&read_buffer[offset..bytes_read]);
                        offset += written;

                        // Chunkが満杯になったらスワップ
                        if chunk.is_full() {
                            let mut full_chunk = current_chunk.take().unwrap();
                            full_chunk.set_global_offset(global_offset);
                            global_offset += full_chunk.len() as u64;
                            println!("[Worker] Chunk full, pushing to finished_list");
                            if let Ok(mut list) = finished_list.write() {
                                list.push_back(Arc::new(full_chunk));
                            }
                            // 次のChunkは次のループで取得
                        }
                    }
                }
            }

            // タイムアウトチェック（16ms経過 & データあり）
            if let Some(ref chunk) = current_chunk {
                let elapsed = last_swap.elapsed();
                if elapsed >= Duration::from_millis(SWAP_TIMEOUT_MS) && chunk.has_data() {
                    println!("[Worker] Timeout, pushing chunk with {} bytes", chunk.len());
                    let mut timeout_chunk = current_chunk.take().unwrap();
                    timeout_chunk.set_global_offset(global_offset);
                    global_offset += timeout_chunk.len() as u64;
                    if let Ok(mut list) = finished_list.write() {
                        list.push_back(Arc::new(timeout_chunk));
                    }
                    last_swap = Instant::now();
                }
            }

            // CPU負荷軽減：データがない場合は短時間スリープ
            if bytes_read == 0 {
                thread::sleep(Duration::from_millis(1));
            }
        }
        println!("[Worker] Thread exiting");
    })
}
