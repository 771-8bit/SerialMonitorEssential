use super::chunk::Chunk;
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, RwLock,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use log::{debug, error, info};

// Logger Thread: Chunkをディスクに書き込む
//
// finished_listからChunkを取り出し、一時ファイルに追記する。
// Arc<Chunk>のため、参照カウントが0になった時点で自動的にメモリ解放される。

// 1MB or 100 chunks buffered before flush
const BUFFER_THRESHOLD: usize = 1024 * 1024;
const CHUNK_COUNT_THRESHOLD: usize = 100;

#[derive(Debug, Clone)]
pub struct PageMetadata {
    pub file_path: PathBuf,
    pub file_offset: u64,
    pub data_length: usize,
    pub global_offset: u64,
}

pub fn spawn_logger_thread(
    finished_list: Arc<RwLock<VecDeque<Arc<Chunk>>>>,
    archived_index: Arc<RwLock<Vec<PageMetadata>>>,
    temp_dir: PathBuf,
    stop_flag: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        info!("[Logger] Thread started");
        let temp_file_path = temp_dir.join("data.bin");
        debug!("[Logger] Temp file path: {:?}", temp_file_path);
        let mut file = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&temp_file_path)
        {
            Ok(f) => {
                debug!("[Logger] File opened successfully");
                f
            }
            Err(e) => {
                error!("[Logger] Failed to create temp file: {:?}", e);
                return;
            }
        };

        let mut chunks_written = 0usize;
        let mut total_bytes_written = 0u64;

        loop {
            // Check if we should stop
            let should_stop = stop_flag.load(Ordering::Relaxed);

            // Process buffer (check thresholds or force flush if stopping)
            match process_buffer(
                &finished_list,
                &archived_index,
                Some(&mut file),
                &temp_file_path,
                should_stop, // Force flush if stopping
            ) {
                Ok((written_count, written_bytes)) => {
                    if written_count > 0 {
                        chunks_written += written_count;
                        total_bytes_written += written_bytes;
                        debug!(
                            "[Logger] Flushed {} chunks, total bytes: {}",
                            written_count, total_bytes_written
                        );
                    }
                }
                Err(e) => {
                    error!("[Logger] Failed to process buffer: {:?}", e);
                }
            }

            if should_stop {
                info!(
                    "[Logger] Stop flag detected. Final stats: {} chunks, {} bytes",
                    chunks_written, total_bytes_written
                );
                break;
            }

            // Sleep briefly to reduce CPU usage (buffering mode)
            thread::sleep(Duration::from_millis(50));
        }
        info!("[Logger] Thread exiting");
    })
}

/// バッファの状態を確認し、条件を満たせば書き込みを行う
///
/// # Returns
/// Ok((chunks_written, bytes_written))
pub fn process_buffer(
    finished_list: &Arc<RwLock<VecDeque<Arc<Chunk>>>>,
    archived_index: &Arc<RwLock<Vec<PageMetadata>>>,
    file: Option<&mut File>, // Option for testing without file IO (mocking not fully implemented, but allows None)
    file_path: &std::path::Path,
    force_flush: bool,
) -> std::io::Result<(usize, u64)> {
    // 1. Check current buffer size without locking for write yet
    let (total_size, chunk_count) = {
        let list = finished_list.read().unwrap();
        let mut size = 0;
        for chunk in list.iter() {
            size += chunk.len();
        }
        (size, list.len())
    };

    // 2. Determine if flush is needed
    if !force_flush && total_size < BUFFER_THRESHOLD && chunk_count < CHUNK_COUNT_THRESHOLD {
        return Ok((0, 0));
    }

    if chunk_count == 0 {
        return Ok((0, 0));
    }

    // 3. Snapshot the chunks WITHOUT removing them from finished_list.
    //
    // 重要: 先に pop してから書き込むと、書き込み〜index 反映の間そのデータが
    // finished_list にも archived_index にも存在しない瞬間ができ、
    // get_data / total_bytes が一時的に失敗・後退する。また I/O エラー時には
    // pop 済みチャンクが失われ、恒久的なデータ欠損になる。
    // そのため「チャンクごとに 書き込み → index 公開 → pop」の順序を守る。
    // チャンク単位で確定させることで、途中で I/O エラーが起きても
    // 成功済みのチャンクは再送されず、再試行はバッチ全体ではなく
    // 残りのチャンクのみを対象にできる。
    let chunks_to_write: Vec<Arc<Chunk>> = {
        let list = finished_list.read().unwrap();
        list.iter().cloned().collect()
    };

    if chunks_to_write.is_empty() {
        return Ok((0, 0));
    }

    // 4. Write to file
    let mut bytes_written_in_batch = 0u64;
    let chunks_count_in_batch = chunks_to_write.len();

    if let Some(f) = file {
        let mut file_offset = f.metadata()?.len();

        for chunk in &chunks_to_write {
            let data = chunk.data();
            if !data.is_empty() {
                if let Err(e) = f.write_all(data) {
                    // Roll the file back to the last indexed offset so a partial
                    // write doesn't leave orphaned bytes (a retry would otherwise
                    // duplicate them).
                    let _ = f.set_len(file_offset);
                    return Err(e);
                }

                // Record metadata for EACH chunk to maintain granular seeking
                let metadata = PageMetadata {
                    file_path: file_path.to_path_buf(),
                    file_offset,
                    data_length: data.len(),
                    global_offset: chunk.global_offset(),
                };

                // Publish metadata BEFORE removing the chunk so readers always
                // find the data in at least one source (get_data scans
                // archived_index first).
                match archived_index.write() {
                    Ok(mut index) => index.push(metadata),
                    Err(e) => {
                        let _ = f.set_len(file_offset);
                        return Err(std::io::Error::other(format!(
                            "archived_index lock poisoned: {e}"
                        )));
                    }
                }

                file_offset += data.len() as u64;
                bytes_written_in_batch += data.len() as u64;
            }

            // This chunk is durable (or empty) — remove it from finished_list now.
            // Worker thread only push_backs, so the front entry is still the
            // chunk we just archived.
            if let Ok(mut list) = finished_list.write() {
                list.pop_front();
            }
        }
        f.flush()?;
    } else {
        // Test mode or dry run
        for chunk in &chunks_to_write {
            bytes_written_in_batch += chunk.len() as u64;
        }
        let mut list = finished_list.write().unwrap();
        for _ in 0..chunks_to_write.len() {
            list.pop_front();
        }
    }

    Ok((chunks_count_in_batch, bytes_written_in_batch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn test_process_buffer_no_flush() {
        let finished_list = Arc::new(RwLock::new(VecDeque::new()));
        let archived_index = Arc::new(RwLock::new(Vec::new()));

        // Add small data (under threshold)
        let mut chunk = Chunk::new(1024);
        chunk.push_data(&[0u8; 100]);
        {
            let mut list = finished_list.write().unwrap();
            list.push_back(Arc::new(chunk));
        }

        // Call process_buffer (force_flush = false)
        let result = process_buffer(
            &finished_list,
            &archived_index,
            None,
            std::path::Path::new("dummy"),
            false,
        )
        .unwrap();

        assert_eq!(result, (0, 0)); // No flush
        assert_eq!(finished_list.read().unwrap().len(), 1); // Chunk remains
    }

    #[test]
    fn test_process_buffer_force_flush() {
        let finished_list = Arc::new(RwLock::new(VecDeque::new()));
        let archived_index = Arc::new(RwLock::new(Vec::new()));

        // Add data
        let mut chunk = Chunk::new(1024);
        chunk.push_data(&[0u8; 100]);
        {
            let mut list = finished_list.write().unwrap();
            list.push_back(Arc::new(chunk));
        }

        // Call process_buffer (force_flush = true)
        let result = process_buffer(
            &finished_list,
            &archived_index,
            None,
            std::path::Path::new("dummy"),
            true,
        )
        .unwrap();

        assert_eq!(result, (1, 100)); // Flushed
        assert!(finished_list.read().unwrap().is_empty()); // List empty
    }

    #[test]
    fn test_process_buffer_threshold_flush() {
        let finished_list = Arc::new(RwLock::new(VecDeque::new()));
        let archived_index = Arc::new(RwLock::new(Vec::new()));

        // Add 2MB data (over 1MB threshold)
        // Simulate by one large chunk for simplicity in test logic
        // Note: Chunk::new allocates, so we simulate size check by mocking if possible,
        // but here we just use actual allocation or multiple chunks.
        // Using multiple chunks to avoid large allocation if possible,
        // but test env usually handles 2MB fine.
        let chunk_size = 64 * 1024;
        let num_chunks = (1024 * 1024 / chunk_size) + 5; // > 1MB

        {
            let mut list = finished_list.write().unwrap();
            for _ in 0..num_chunks {
                let mut chunk = Chunk::new(chunk_size);
                // Fill with dummy data to have valid_len
                chunk.push_data(&vec![0u8; chunk_size]);
                list.push_back(Arc::new(chunk));
            }
        }

        // Call process_buffer (force_flush = false)
        let result = process_buffer(
            &finished_list,
            &archived_index,
            None,
            std::path::Path::new("dummy"),
            false,
        )
        .unwrap();

        assert_eq!(result.0, num_chunks); // Flushed all
        assert!(finished_list.read().unwrap().is_empty());
    }

    // Helper to create a chunk with specific data
    fn create_test_chunk(size: usize, pattern: u8) -> Chunk {
        let mut chunk = Chunk::new(size);
        chunk.push_data(&vec![pattern; size]);
        chunk
    }

    #[test]
    fn test_process_buffer_with_file_io() {
        let finished_list = Arc::new(RwLock::new(VecDeque::new()));
        let archived_index = Arc::new(RwLock::new(Vec::new()));

        // Create a temporary directory and file path
        let temp_dir = std::env::temp_dir().join("serial_monitor_test_io");
        if !temp_dir.exists() {
            std::fs::create_dir_all(&temp_dir).unwrap();
        }
        let file_path = temp_dir.join("test_data.bin");
        // Ensure clean start
        if file_path.exists() {
            std::fs::remove_file(&file_path).unwrap();
        }

        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&file_path)
                .unwrap();

            // Add data
            let chunk_size = 100;
            let chunk = create_test_chunk(chunk_size, 0xAA);
            {
                let mut list = finished_list.write().unwrap();
                list.push_back(Arc::new(chunk));
            }

            // Call process_buffer with real file
            let result = process_buffer(
                &finished_list,
                &archived_index,
                Some(&mut file),
                &file_path,
                true, // Force flush
            )
            .unwrap();

            assert_eq!(result, (1, 100));
        } // file closed here

        // Verify file content
        let mut file = File::open(&file_path).unwrap();
        let mut content = Vec::new();
        file.read_to_end(&mut content).unwrap();
        assert_eq!(content.len(), 100);
        assert_eq!(content[0], 0xAA);

        // Verify index
        let index = archived_index.read().unwrap();
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].file_path, file_path);
        assert_eq!(index[0].data_length, 100);

        // Cleanup
        std::fs::remove_file(&file_path).ok();
        std::fs::remove_dir(&temp_dir).ok();
    }

    #[test]
    fn test_spawn_logger_thread_integration() {
        let finished_list = Arc::new(RwLock::new(VecDeque::new()));
        let archived_index = Arc::new(RwLock::new(Vec::new()));
        let stop_flag = Arc::new(AtomicBool::new(false));

        // Use a unique temp dir for this test
        let temp_dir = std::env::temp_dir().join("serial_monitor_test_thread");
        if !temp_dir.exists() {
            std::fs::create_dir_all(&temp_dir).unwrap();
        }
        // clean any previous run
        if temp_dir.join("data.bin").exists() {
            std::fs::remove_file(temp_dir.join("data.bin")).unwrap();
        }

        let handle = spawn_logger_thread(
            finished_list.clone(),
            archived_index.clone(),
            temp_dir.clone(),
            stop_flag.clone(),
        );

        // Add some data
        let chunk = create_test_chunk(200, 0xBB);
        {
            let mut list = finished_list.write().unwrap();
            list.push_back(Arc::new(chunk));
        }

        // Wait a bit for the thread to pick it up (thread sleep is 50ms)
        thread::sleep(Duration::from_millis(150));

        // Signal stop
        stop_flag.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        // Verify result file
        let file_path = temp_dir.join("data.bin");
        assert!(file_path.exists());
        let mut file = File::open(&file_path).unwrap();
        let mut content = Vec::new();
        file.read_to_end(&mut content).unwrap();
        assert_eq!(content.len(), 200);
        assert_eq!(content[0], 0xBB);

        // Cleanup
        std::fs::remove_file(&file_path).ok();
        std::fs::remove_dir(&temp_dir).ok();
    }
}
