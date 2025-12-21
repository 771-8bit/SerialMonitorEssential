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

/// Logger Thread: Chunkをディスクに書き込む
///
/// finished_listからChunkを取り出し、一時ファイルに追記する。
/// Arc<Chunk>のため、参照カウントが0になった時点で自動的にメモリ解放される。

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
            // 停止フラグチェック
            if stop_flag.load(Ordering::Relaxed) {
                info!("[Logger] Stop flag detected, flushing remaining chunks");
                // 残っているChunkをすべて処理（通常処理と同じ安全な順序で）
                loop {
                    let chunk = {
                        let list = finished_list.read().unwrap();
                        list.front().cloned()
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
                                error!("[Logger] Failed to write chunk during shutdown: {:?}", e);
                                // 書き込み失敗してもpopして次へ進む（無限ループ防止）
                                // ただしエラーログで記録済み
                            }
                            // 成功・失敗に関わらずpop（シャットダウン時は進行を優先）
                            {
                                let mut list = finished_list.write().unwrap();
                                list.pop_front();
                            }
                            chunks_written += 1;
                        }
                        None => break,
                    }
                }
                info!(
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
                debug!(
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
                    error!("[Logger] Failed to write chunk: {:?}", e);
                } else {
                    // 書き込み成功後にpop（archived_index更新後なのでデータは検索可能）
                    {
                        let mut list = finished_list.write().unwrap();
                        list.pop_front();
                    }
                    chunks_written += 1;
                    debug!(
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
        info!("[Logger] Thread exiting");
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
