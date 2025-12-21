use super::data_store::DataStore;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

use log::{debug, info, warn};

/// UiNotifier Thread: フロントエンドへのデータ更新通知
///
/// DataStoreのtotal_bytesを監視し、60fps（16ms間隔）で間引いてイベントを発火する。
/// 高速データ受信時もUIへの通知頻度を制限し、パフォーマンスを維持する。
const NOTIFY_INTERVAL_MS: u64 = 16; // 約60fps

#[derive(Clone, serde::Serialize)]
pub struct DataUpdatePayload {
    pub total_bytes: u64,
}

pub fn spawn_ui_notifier_thread(
    data_store: Arc<DataStore>,
    stop_flag: Arc<AtomicBool>,
    app_handle: AppHandle,
) -> JoinHandle<()> {
    thread::spawn(move || {
        info!("[UiNotifier] Thread started");
        let mut last_notify = Instant::now();
        let mut last_total_bytes: u64 = 0;
        let mut event_count: u64 = 0;

        loop {
            // 停止フラグチェック
            if stop_flag.load(Ordering::Relaxed) {
                info!(
                    "[UiNotifier] Stop flag detected, exiting. Total events sent: {}",
                    event_count
                );
                break;
            }

            // 60fps間隔でチェック
            let elapsed = last_notify.elapsed();
            if elapsed < Duration::from_millis(NOTIFY_INTERVAL_MS) {
                thread::sleep(Duration::from_millis(
                    NOTIFY_INTERVAL_MS - elapsed.as_millis() as u64,
                ));
            }

            // DataStoreから正確な総バイト数を取得
            let total_bytes = data_store.total_bytes();

            // データが増えた場合のみ通知
            if total_bytes > last_total_bytes {
                let payload = DataUpdatePayload { total_bytes };

                if let Err(e) = app_handle.emit("data-update", payload) {
                    warn!("[UiNotifier] Failed to emit event: {:?}", e);
                } else {
                    event_count += 1;
                    // 最初の10回と100回ごとにログ出力
                    if event_count <= 10 || event_count.is_multiple_of(100) {
                        debug!(
                            "[UiNotifier] Event #{}: total_bytes = {}",
                            event_count, total_bytes
                        );
                    }
                }

                last_total_bytes = total_bytes;
            }

            last_notify = Instant::now();
        }
        info!("[UiNotifier] Thread exiting");
    })
}
