use super::chunk::Chunk;
use super::port::SerialPort;
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, RwLock,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use log::{debug, error, info, warn};
use tauri::{AppHandle, Emitter};

/// Worker Thread: シリアルポートからデータを受信
///
/// ReadFileを高速に呼び出し、Chunkにデータを詰める。
/// 16ms経過 or 満杯で finished_list に移動する。
const SWAP_TIMEOUT_MS: u64 = 16; // 約60fps
const READ_BUFFER_SIZE: usize = 4096; // 一度に読み取るサイズ

/// Payload for serial-status event
#[derive(Clone, serde::Serialize)]
struct SerialStatusPayload {
    connected: bool,
    error: Option<String>,
}

/// Record line offsets (LF positions) in the given data
fn record_line_offsets(line_index: &RwLock<Vec<u64>>, data: &[u8], global_offset: u64) {
    if let Ok(mut index) = line_index.write() {
        for (i, &byte) in data.iter().enumerate() {
            // LF (\n) を改行として検出
            if byte == b'\n' {
                let next_line_offset = global_offset + i as u64 + 1;
                // 重複を避ける
                if index.last().is_none_or(|&last| last < next_line_offset) {
                    index.push(next_line_offset);
                }
            }
        }
    }
}

pub fn spawn_worker_thread(
    port: Arc<Mutex<SerialPort>>,
    free_pool: Arc<crossbeam::queue::SegQueue<Chunk>>,
    finished_list: Arc<RwLock<VecDeque<Arc<Chunk>>>>,
    line_index: Arc<RwLock<Vec<u64>>>, // Phase 2: line_index for pre-indexing
    stop_flag: Arc<AtomicBool>,
    app_handle: AppHandle,
) -> JoinHandle<()> {
    thread::spawn(move || {
        info!("[Worker] Thread started");
        let mut current_chunk: Option<Chunk> = None;
        let mut last_swap = Instant::now();
        let mut read_buffer = vec![0u8; READ_BUFFER_SIZE];
        let mut total_bytes_read = 0u64;
        let mut global_offset = 0u64; // グローバルオフセットを追跡
        let mut loop_count = 0u64;
        let mut last_status_log = Instant::now();

        loop {
            // 停止フラグチェック
            if stop_flag.load(Ordering::Relaxed) {
                info!(
                    "[Worker] Stop flag detected, total bytes read: {}",
                    total_bytes_read
                );
                // 残っているChunkがあればfinished_listに送る
                if let Some(mut chunk) = current_chunk.take() {
                    if chunk.has_data() {
                        chunk.set_global_offset(global_offset);
                        // Phase 2: Record line offsets before pushing
                        record_line_offsets(&line_index, chunk.data(), chunk.global_offset());
                        debug!("[Worker] Pushing final chunk with {} bytes", chunk.len());
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
            let read_result = {
                if let Ok(mut p) = port.lock() {
                    p.read(&mut read_buffer)
                } else {
                    warn!("[Worker] Mutex poisoned");
                    break;
                }
            };

            let bytes_read = match read_result {
                Ok(n) => n,
                Err(e) => {
                    // Check if it's a timeout (normal) or a fatal error (disconnect)
                    let error_str = e.to_string().to_lowercase();
                    if error_str.contains("timed out") || error_str.contains("timeout") {
                        // Normal timeout, continue
                        0
                    } else {
                        // Fatal error - device probably disconnected
                        error!("[Worker] Fatal read error (device disconnected?): {}", e);

                        // Emit serial-status event to notify frontend
                        let payload = SerialStatusPayload {
                            connected: false,
                            error: Some(e.to_string()),
                        };
                        if let Err(emit_err) = app_handle.emit("serial-status", payload) {
                            error!("[Worker] Failed to emit serial-status event: {}", emit_err);
                        }

                        // Push remaining data if any
                        if let Some(mut chunk) = current_chunk.take() {
                            if chunk.has_data() {
                                chunk.set_global_offset(global_offset);
                                if let Ok(mut list) = finished_list.write() {
                                    list.push_back(Arc::new(chunk));
                                }
                            }
                        }
                        break;
                    }
                }
            };

            if bytes_read > 0 {
                total_bytes_read += bytes_read as u64;
                debug!(
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
                            // Phase 2: Record line offsets before pushing
                            record_line_offsets(
                                &line_index,
                                full_chunk.data(),
                                full_chunk.global_offset(),
                            );
                            global_offset += full_chunk.len() as u64;
                            debug!("[Worker] Chunk full, pushing to finished_list");
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
                    debug!("[Worker] Timeout, pushing chunk with {} bytes", chunk.len());
                    let mut timeout_chunk = current_chunk.take().unwrap();
                    timeout_chunk.set_global_offset(global_offset);
                    // Phase 2: Record line offsets before pushing
                    record_line_offsets(
                        &line_index,
                        timeout_chunk.data(),
                        timeout_chunk.global_offset(),
                    );
                    global_offset += timeout_chunk.len() as u64;
                    if let Ok(mut list) = finished_list.write() {
                        list.push_back(Arc::new(timeout_chunk));
                    }
                    last_swap = Instant::now();
                }
            }

            // Periodic status logging (every 5 seconds)
            loop_count += 1;
            if last_status_log.elapsed() >= Duration::from_secs(5) {
                info!(
                    "[Worker] Status: loop_count={}, total_bytes_read={}, chunks_in_list={}",
                    loop_count,
                    total_bytes_read,
                    finished_list.read().map(|l| l.len()).unwrap_or(0)
                );
                last_status_log = Instant::now();
            }

            // CPU負荷軽減：データがない場合は短時間スリープ
            if bytes_read == 0 {
                thread::sleep(Duration::from_millis(1));
            }
        }
        info!("[Worker] Thread exiting");
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// record_line_offsets should detect LF and record offset after newline
    #[test]
    fn test_record_line_offsets_basic() {
        let line_index = Arc::new(RwLock::new(vec![0u64]));
        let data = b"Hello\nWorld\n";

        record_line_offsets(&line_index, data, 0);

        let index = line_index.read().unwrap();
        // Initial [0] + 2 newlines: [0, 6, 12]
        assert_eq!(index.len(), 3);
        assert_eq!(index[0], 0);
        assert_eq!(index[1], 6); // After "Hello\n"
        assert_eq!(index[2], 12); // After "World\n"
    }

    /// record_line_offsets should handle global_offset correctly
    #[test]
    fn test_record_line_offsets_with_global_offset() {
        let line_index = Arc::new(RwLock::new(vec![0u64]));
        let data = b"Line\n";

        record_line_offsets(&line_index, data, 100);

        let index = line_index.read().unwrap();
        assert_eq!(index.len(), 2);
        assert_eq!(index[0], 0);
        assert_eq!(index[1], 105); // 100 + 4 + 1 = 105
    }

    /// record_line_offsets should not add duplicates
    #[test]
    fn test_record_line_offsets_no_duplicates() {
        let line_index = Arc::new(RwLock::new(vec![0u64]));
        let data = b"A\n";

        record_line_offsets(&line_index, data, 0);
        record_line_offsets(&line_index, data, 0); // Same data again

        let index = line_index.read().unwrap();
        // Should not duplicate: [0, 2]
        assert_eq!(index.len(), 2);
    }

    /// record_line_offsets should handle empty data
    #[test]
    fn test_record_line_offsets_empty() {
        let line_index = Arc::new(RwLock::new(vec![0u64]));
        let data = b"";

        record_line_offsets(&line_index, data, 0);

        let index = line_index.read().unwrap();
        // No change: [0]
        assert_eq!(index.len(), 1);
    }

    /// record_line_offsets should handle data without newlines
    #[test]
    fn test_record_line_offsets_no_newlines() {
        let line_index = Arc::new(RwLock::new(vec![0u64]));
        let data = b"NoNewlineHere";

        record_line_offsets(&line_index, data, 0);

        let index = line_index.read().unwrap();
        // No change: [0]
        assert_eq!(index.len(), 1);
    }

    /// record_line_offsets should handle consecutive newlines
    #[test]
    fn test_record_line_offsets_consecutive_newlines() {
        let line_index = Arc::new(RwLock::new(vec![0u64]));
        let data = b"\n\n\n";

        record_line_offsets(&line_index, data, 0);

        let index = line_index.read().unwrap();
        // [0, 1, 2, 3]
        assert_eq!(index.len(), 4);
        assert_eq!(index[0], 0);
        assert_eq!(index[1], 1);
        assert_eq!(index[2], 2);
        assert_eq!(index[3], 3);
    }

    /// record_line_offsets should only detect LF, not CR
    #[test]
    fn test_record_line_offsets_ignores_cr() {
        let line_index = Arc::new(RwLock::new(vec![0u64]));
        let data = b"Hello\rWorld"; // CR only, no LF

        record_line_offsets(&line_index, data, 0);

        let index = line_index.read().unwrap();
        // No change: [0] - CR is not treated as line boundary
        assert_eq!(index.len(), 1);
    }

    /// record_line_offsets should detect LF in CRLF sequences
    #[test]
    fn test_record_line_offsets_crlf() {
        let line_index = Arc::new(RwLock::new(vec![0u64]));
        let data = b"Hello\r\nWorld\r\n";

        record_line_offsets(&line_index, data, 0);

        let index = line_index.read().unwrap();
        // [0, 7, 14] - LF at positions 6 and 13
        assert_eq!(index.len(), 3);
        assert_eq!(index[0], 0);
        assert_eq!(index[1], 7); // After "Hello\r\n"
        assert_eq!(index[2], 14); // After "World\r\n"
    }
}
