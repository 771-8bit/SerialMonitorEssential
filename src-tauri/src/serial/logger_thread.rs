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

/// Logger Thread: Chunkをディスクに書き込む
///
/// finished_listからChunkを取り出し、一時ファイルに追記する。
/// 書き込み後はfree_poolに返却して再利用する。

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

        let mut chunks_written = 0usize;
        let mut total_bytes_written = 0u64;

        loop {
            // 停止フラグチェック
            if stop_flag.load(Ordering::Relaxed) {
                println!("[Logger] Stop flag detected, flushing remaining chunks");
                // 残っているChunkをすべて処理
                loop {
                    let chunk = {
                        let mut list = finished_list.write().unwrap();
                        list.pop_front()
                    };
                    match chunk {
                        Some(chunk_arc) => {
                            if let Err(e) = write_chunk(
                                &mut file,
                                &chunk_arc,
                                &temp_file_path,
                                &archived_index,
                                &mut total_bytes_written,
                            ) {
                                eprintln!("[Logger] Failed to write chunk: {:?}", e);
                            }
                            // Arc<Chunk>なので自動的に参照カウントが減る
                            // チャンククリアや手動返却は不要
                            chunks_written += 1;
                        }
                        None => break,
                    }
                }
                println!(
                    "[Logger] Total chunks written: {}, total bytes: {}",
                    chunks_written, total_bytes_written
                );
                break;
            }

            // Chunkを参照（まだpopしない - データ完全性保証のため）
            let chunk = {
                let list = finished_list.read().unwrap();
                list.front().cloned()
            };

            if let Some(chunk_arc) = chunk {
                println!(
                    "[Logger] Got chunk from queue with {} bytes",
                    chunk_arc.len()
                );
                if let Err(e) = write_chunk(
                    &mut file,
                    &chunk_arc,
                    &temp_file_path,
                    &archived_index,
                    &mut total_bytes_written,
                ) {
                    eprintln!("[Logger] Failed to write chunk: {:?}", e);
                } else {
                    // 書き込み成功後にpop（archived_index更新後なのでデータは検索可能）
                    {
                        let mut list = finished_list.write().unwrap();
                        list.pop_front();
                    }
                    chunks_written += 1;
                    println!(
                        "[Logger] Wrote chunk #{}, total bytes: {}",
                        chunks_written, total_bytes_written
                    );
                }
                // Arc<Chunk>なので自動的に参照カウントが減る
                // チャンククリアや手動返却は不要
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
    chunk: &Arc<Chunk>,
    file_path: &std::path::Path,
    archived_index: &Arc<RwLock<Vec<PageMetadata>>>,
    total_bytes_written: &mut u64,
) -> std::io::Result<()> {
    let data = chunk.data();
    if data.is_empty() {
        return Ok(());
    }

    let file_offset = file.metadata()?.len();

    // ファイルに書き込み
    file.write_all(data)?;
    file.flush()?;

    // メタデータを記録（チャンクのglobal_offsetを使用）
    let metadata = PageMetadata {
        file_path: file_path.to_path_buf(),
        file_offset,
        data_length: data.len(),
        global_offset: chunk.global_offset(),
    };

    if let Ok(mut index) = archived_index.write() {
        index.push(metadata);
    }

    *total_bytes_written += data.len() as u64;
    Ok(())
}
