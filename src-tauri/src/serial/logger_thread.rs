use super::chunk::Chunk;
use crossbeam::queue::SegQueue;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, RwLock,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Logger Thread: Chunkをディスクに書き込む
///
/// finished_queueからChunkを取り出し、一時ファイルに追記する。
/// 書き込み後はfree_poolに返却して再利用する。

#[derive(Debug, Clone)]
pub struct PageMetadata {
    pub file_path: PathBuf,
    pub file_offset: u64,
    pub data_length: usize,
    pub global_offset: u64,
}

pub fn spawn_logger_thread(
    free_pool: Arc<SegQueue<Chunk>>,
    finished_queue: Arc<SegQueue<Chunk>>,
    archived_index: Arc<RwLock<Vec<PageMetadata>>>,
    temp_dir: PathBuf,
    stop_flag: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        println!("[Logger] Thread started");
        let temp_file_path = temp_dir.join("data.bin");
        println!("[Logger] Temp file path: {:?}", temp_file_path);
        let mut file = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&temp_file_path)
        {
            Ok(f) => {
                println!("[Logger] File opened successfully");
                f
            }
            Err(e) => {
                eprintln!("[Logger] Failed to create temp file: {:?}", e);
                return;
            }
        };

        let mut global_offset: u64 = 0;
        let mut chunks_written = 0usize;

        loop {
            // 停止フラグチェック
            if stop_flag.load(Ordering::Relaxed) {
                println!("[Logger] Stop flag detected, flushing remaining chunks");
                // 残っているChunkをすべて処理
                while let Some(mut chunk) = finished_queue.pop() {
                    if let Err(e) = write_chunk(
                        &mut file,
                        &chunk,
                        &temp_file_path,
                        &archived_index,
                        &mut global_offset,
                    ) {
                        eprintln!("Failed to write chunk: {:?}", e);
                    }
                    // チャンクをクリアしてからプールに返却
                    chunk.clear();
                    free_pool.push(chunk);
                    chunks_written += 1;
                }
                println!(
                    "[Logger] Total chunks written: {}, total bytes: {}",
                    chunks_written, global_offset
                );
                break;
            }

            // Chunkを取得
            if let Some(mut chunk) = finished_queue.pop() {
                println!("[Logger] Got chunk from queue with {} bytes", chunk.len());
                if let Err(e) = write_chunk(
                    &mut file,
                    &chunk,
                    &temp_file_path,
                    &archived_index,
                    &mut global_offset,
                ) {
                    eprintln!("[Logger] Failed to write chunk: {:?}", e);
                } else {
                    chunks_written += 1;
                    println!(
                        "[Logger] Wrote chunk #{}, total bytes: {}",
                        chunks_written, global_offset
                    );
                }
                // チャンクをクリアしてからプールに返却
                chunk.clear();
                free_pool.push(chunk);
            } else {
                // キューが空の場合は短時間スリープ
                thread::sleep(Duration::from_millis(5));
            }
        }
        println!("[Logger] Thread exiting");
    })
}

fn write_chunk(
    file: &mut File,
    chunk: &Chunk,
    file_path: &std::path::Path,
    archived_index: &Arc<RwLock<Vec<PageMetadata>>>,
    global_offset: &mut u64,
) -> std::io::Result<()> {
    let data = chunk.data();
    if data.is_empty() {
        return Ok(());
    }

    let file_offset = file.metadata()?.len();

    // ファイルに書き込み
    file.write_all(data)?;
    file.flush()?;

    // メタデータを記録
    let metadata = PageMetadata {
        file_path: file_path.to_path_buf(),
        file_offset,
        data_length: data.len(),
        global_offset: *global_offset,
    };

    if let Ok(mut index) = archived_index.write() {
        index.push(metadata);
    }

    *global_offset += data.len() as u64;
    Ok(())
}
