use super::chunk::Chunk;
use super::tx::{fail_pending, OpenPort, Parked};
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, RwLock,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use log::{debug, error, info};

/// Worker Thread: シリアルポートからデータを受信
///
/// ReadFileを高速に呼び出し、Chunkにデータを詰める。
/// 16ms経過 or 満杯で finished_list に移動する。
const SWAP_TIMEOUT_MS: u64 = 16; // 約60fps
/// 一度に読み取る上限。送信中はスライスの合間に受信キューを掃くので、1 回で
/// 滞留分を読み切れる大きさにする（ヒッチ後の滞留を 4 KiB ずつしか捌けないと
/// 送信中に追いつけない）。待機中は届いた分だけ返るので大きくても害はない。
const READ_BUFFER_SIZE: usize = 64 * 1024;

/// 致命的な受信エラー（切断）の通知先。tauri 非依存に保つためコールバックにする
/// （呼び出し側が `serial-status(connected=false)` を emit する）。
pub type DisconnectFn = Box<dyn Fn(String) + Send>;

/// ポートを開いている間だけ Windows の系統タイマ精度を 1 ms に上げる（ADR-14）。
///
/// `COMMTIMEOUTS` の待ち（待機中の read 2 ms、送信スライスの 5 ms）は既定の
/// タイマ粒度 15.6 ms に丸められ、12 Mbaud では 1 回の待ちで ~19 KB が届いて
/// ドライバの受信バッファ（実測 ~6.5 KB）を溢れさせる（docs/24 §6.5 T2-5b）。
/// Windows 10 2004 以降、この設定は呼び出したプロセスにだけ効く。
#[cfg(windows)]
struct TimerResolution;

#[cfg(windows)]
impl TimerResolution {
    fn acquire() -> Self {
        #[link(name = "winmm")]
        extern "system" {
            fn timeBeginPeriod(period: u32) -> u32;
        }
        // SAFETY: 引数は値渡しの u32 のみ。失敗（TIMERR_NOCANDO）しても既定精度のまま動く
        unsafe {
            timeBeginPeriod(1);
        }
        Self
    }
}

#[cfg(windows)]
impl Drop for TimerResolution {
    fn drop(&mut self) {
        #[link(name = "winmm")]
        extern "system" {
            fn timeEndPeriod(period: u32) -> u32;
        }
        // SAFETY: acquire と対の呼び出し
        unsafe {
            timeEndPeriod(1);
        }
    }
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

/// 受信ワーカーを起動する。ポートの I/O（`parked.io` / `parked.rx`）は**このスレッドが
/// 単独で所有**し、送信要求は `rx` から受けて read と交互に 1 スライスずつ書く
/// （ADR-14 / INV-14, 16）。正常停止時は進行中の送信状態ごと `port` へ返却（park）し、
/// 致命エラー時は破棄する。
pub fn spawn_worker_thread(
    port: Arc<OpenPort>,
    parked: Parked,
    free_pool: Arc<crossbeam::queue::SegQueue<Chunk>>,
    finished_list: Arc<RwLock<VecDeque<Arc<Chunk>>>>,
    line_index: Arc<RwLock<Vec<u64>>>, // Phase 2: line_index for pre-indexing
    stop_flag: Arc<AtomicBool>,
    on_disconnect: DisconnectFn,
) -> JoinHandle<()> {
    thread::spawn(move || {
        info!("[Worker] Thread started");
        #[cfg(windows)]
        let _timer_resolution = TimerResolution::acquire();
        let Parked {
            mut io,
            rx: cmd_rx,
            engine: mut tx,
        } = parked;
        // 前のワーカーが送信途中で止まっていれば、その続きから（期限は退避中の時間を除く）
        tx.resume(io.as_mut());
        let mut current_chunk: Option<Chunk> = None;
        let mut last_swap = Instant::now();
        let mut read_buffer = vec![0u8; READ_BUFFER_SIZE];
        let mut total_bytes_read = 0u64;
        let mut global_offset = 0u64; // グローバルオフセットを追跡
        let mut loop_count = 0u64;
        let mut last_status_log = Instant::now();
        // 受信が止まる時間の診断（5 秒窓の最大値）。フロー制御の無い UART では
        // 数 ms の停止がそのまま欠落になるため、どこで止まったかをログで追えるようにする
        let mut max_read = Duration::ZERO;
        let mut max_tx = Duration::ZERO;
        let mut max_other = Duration::ZERO;

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
                // 進行中の送信もキュー内の要求も失敗にしない: 状態ごと退避し、
                // clear_data の再始動なら次のワーカーが続きを書く（close なら OpenPort::close が
                // すべて Err で返す）
                tx.suspend();
                port.park(Parked {
                    io,
                    rx: cmd_rx,
                    engine: tx,
                });
                break;
            }

            // Chunkがなければ取得
            if current_chunk.is_none() {
                current_chunk = Some(free_pool.pop().unwrap_or_else(|| Chunk::new(64 * 1024)));
                last_swap = Instant::now();
            }

            // データ読み取り。送信中は read が広いタイムアウトで待たないよう、
            // 受信キューにデータがあるときだけ読む（ADR-14）
            let busy = tx.is_busy();
            let t_read = Instant::now();
            let read_result = if busy {
                match io.bytes_to_read() {
                    Ok(0) => Ok(0),
                    Ok(_) => io.read(&mut read_buffer),
                    Err(e) => Err(e),
                }
            } else {
                io.read(&mut read_buffer)
            };

            // 待機中の read はタイムアウトで待つのが正常なので、送信中だけ計測する
            if busy {
                max_read = max_read.max(t_read.elapsed());
            }
            let t_other = Instant::now();
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

                        // 送信待ちの呼び出し側を即座に失敗で返す（ハングさせない）。
                        // I/O と受け手はこのスレッドと共に破棄され、以後の send は即失敗する
                        port.mark_disconnected();
                        tx.abort_without_io("port disconnected");
                        fail_pending(&cmd_rx, "port disconnected");

                        // フロントエンドへ通知（serial-status connected=false）
                        on_disconnect(e.to_string());

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

            // 送信を 1 スライスだけ進める（無ければキューから次を取る）。
            // 次のスライスの前に必ず上の read 判定を通る（INV-16）
            max_other = max_other.max(t_other.elapsed());
            let t_tx = Instant::now();
            tx.step(io.as_mut(), &cmd_rx, global_offset);
            max_tx = max_tx.max(t_tx.elapsed());

            // Periodic status logging (every 5 seconds)
            loop_count += 1;
            if last_status_log.elapsed() >= Duration::from_secs(5) {
                info!(
                    "[Worker] Status: loop_count={}, total_bytes_read={}, chunks_in_list={},                      max_ms(read_busy={:.1}, tx={:.1}, other={:.1})",
                    loop_count,
                    total_bytes_read,
                    finished_list.read().map(|l| l.len()).unwrap_or(0),
                    max_read.as_secs_f64() * 1000.0,
                    max_tx.as_secs_f64() * 1000.0,
                    max_other.as_secs_f64() * 1000.0,
                );
                last_status_log = Instant::now();
                max_read = Duration::ZERO;
                max_tx = Duration::ZERO;
                max_other = Duration::ZERO;
            }

            // 待機は read のタイムアウト（IDLE_READ_TIMEOUT）が担うので sleep しない。
            // sleep を入れると送信の取り上げとスライス間隔がその分遅れる。
        }
        info!("[Worker] Thread exiting");
    })
}

/// ワーカーループ全体を偽ポートで駆動する試験（ADR-14 / INV-14, 16）
#[cfg(test)]
mod loop_tests {
    use super::super::tx::fake::{Ev, FakePort, WriteMode};
    use super::super::tx::OpenPort;
    use super::*;
    use crossbeam::queue::SegQueue;
    use std::sync::Mutex;

    struct Harness {
        port: Arc<OpenPort>,
        fake: FakePort,
        finished: Arc<RwLock<VecDeque<Arc<Chunk>>>>,
        stop: Arc<AtomicBool>,
        disconnects: Arc<Mutex<Vec<String>>>,
        handle: Option<JoinHandle<()>>,
    }

    impl Harness {
        fn start(fake: FakePort) -> Self {
            let port = Arc::new(OpenPort::new("FAKE", Box::new(fake.clone())));
            let mut h = Self {
                port,
                fake,
                finished: Arc::new(RwLock::new(VecDeque::new())),
                stop: Arc::new(AtomicBool::new(false)),
                disconnects: Arc::new(Mutex::new(Vec::new())),
                handle: None,
            };
            h.spawn();
            h
        }

        fn spawn(&mut self) {
            let parked = self.port.take_io().expect("io parked");
            let sink = self.disconnects.clone();
            self.stop.store(false, Ordering::Relaxed);
            self.handle = Some(spawn_worker_thread(
                self.port.clone(),
                parked,
                Arc::new(SegQueue::new()),
                self.finished.clone(),
                Arc::new(RwLock::new(vec![0])),
                self.stop.clone(),
                Box::new(move |e| sink.lock().unwrap().push(e)),
            ));
        }

        fn stop(&mut self) -> Duration {
            let started = Instant::now();
            self.stop.store(true, Ordering::Relaxed);
            if let Some(h) = self.handle.take() {
                h.join().unwrap();
            }
            started.elapsed()
        }

        fn captured(&self) -> Vec<u8> {
            self.finished
                .read()
                .unwrap()
                .iter()
                .flat_map(|c| c.data().to_vec())
                .collect()
        }
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 13 + i / 97) as u8).collect()
    }

    /// INV-16: 送信中、連続する 2 つの書き込みスライスの間には必ず受信判定が入る。
    /// ループバックなので、書いた分は次の判定で読まれ、受信が止まらない
    #[test]
    fn test_reads_interleave_with_write_slices() {
        let mut h = Harness::start(FakePort::new(12_000_000, WriteMode::Loopback));
        let payload = pattern(16_384);
        assert_eq!(h.port.send(payload.clone()).unwrap().bytes_written, 16_384);
        h.stop();

        let events = h.fake.events();
        let mut last_was_write = false;
        for e in &events {
            match e {
                Ev::Write(_) => {
                    assert!(
                        !last_was_write,
                        "two slices without a read check: {events:?}"
                    );
                    last_was_write = true;
                }
                Ev::Check(_) | Ev::Read(_) => last_was_write = false,
                _ => {}
            }
        }
        assert!(events.iter().filter(|e| matches!(e, Ev::Write(_))).count() >= 14);
        // 送信中に実際に読んでいる（書き込みの後で受信キューを掃いている）
        assert!(events.iter().any(|e| matches!(e, Ev::Check(n) if *n > 0)));
    }

    /// 受信パスの回帰: ループバックで送った全バイトが順序どおりキャプチャに入る
    #[test]
    fn test_loopback_capture_is_complete_and_ordered() {
        let mut h = Harness::start(FakePort::new(12_000_000, WriteMode::Loopback));
        let mut sent = Vec::new();
        for size in [1, 100, 4096, 16_384, 70_000] {
            let payload = pattern(size);
            assert_eq!(h.port.send(payload.clone()).unwrap().bytes_written, size);
            sent.extend(payload);
        }
        thread::sleep(Duration::from_millis(50)); // 最後のスライスの折り返しを読む
        h.stop(); // 停止時に残りのチャンクも公開される
        assert_eq!(h.captured(), sent);
    }

    /// 長い送信中の停止（Clear 相当）: join は待たされず、送信は失敗しない。
    /// 状態ごと退避され、再始動したワーカーが続きから書き、キューの要求も処理する。
    /// 重複も欠落もない（ADR-14 改訂 2: Clear は送信を失わない）
    #[test]
    fn test_stop_during_long_send_is_prompt_and_resumes_after_restart() {
        let fake = FakePort::new(12_000_000, WriteMode::Accept);
        fake.state.lock().unwrap().write_delay = Duration::from_millis(1);
        let mut h = Harness::start(fake);

        let long_payload = pattern(300_000); // 250 スライス × 1 ms 以上
        let p = h.port.clone();
        let payload = long_payload.clone();
        let long = thread::spawn(move || p.send(payload));
        thread::sleep(Duration::from_millis(30));
        let p = h.port.clone();
        let queued = thread::spawn(move || p.send(b"after".to_vec()));
        thread::sleep(Duration::from_millis(20));

        let join_time = h.stop();
        assert!(
            join_time < Duration::from_millis(200),
            "join took {join_time:?}"
        );
        assert!(h.port.is_parked(), "stopped worker must park the I/O");
        assert!(
            !long.is_finished(),
            "a stop must not fail the in-flight send"
        );

        h.spawn();
        assert_eq!(long.join().unwrap().unwrap().bytes_written, 300_000);
        assert_eq!(queued.join().unwrap().unwrap().bytes_written, 5);
        h.stop();
        assert_eq!(h.fake.written(), [long_payload, b"after".to_vec()].concat());
    }

    /// 停止して再始動せずに close（ポートを閉じた / 別ポートを開いた）: 進行中も
    /// キュー内も Err で返り、書けたバイト数が報告される
    #[test]
    fn test_close_after_stop_fails_in_flight_and_queued_sends() {
        let fake = FakePort::new(12_000_000, WriteMode::Accept);
        fake.state.lock().unwrap().write_delay = Duration::from_millis(1);
        let mut h = Harness::start(fake);
        let p = h.port.clone();
        let long = thread::spawn(move || p.send(vec![0u8; 300_000]));
        thread::sleep(Duration::from_millis(30));
        let p = h.port.clone();
        let queued = thread::spawn(move || p.send(b"after".to_vec()));
        thread::sleep(Duration::from_millis(20));
        h.stop();
        h.port.close();
        let err = long.join().unwrap().unwrap_err();
        assert!(
            err.contains("port closed") && err.contains("/300000"),
            "{err}"
        );
        assert!(queued.join().unwrap().is_err());
        assert!(!h.port.is_connected());
    }

    /// 致命的な受信エラー: 待機中の送信はすべて Err、通知が 1 回、以後の送信は即 Err。
    /// I/O は破棄され再始動できない（clear_data は閉じたポートとして扱う）
    #[test]
    fn test_fatal_read_error_fails_pending_sends_and_disconnects() {
        let fake = FakePort::new(12_000_000, WriteMode::Accept);
        fake.state.lock().unwrap().write_delay = Duration::from_millis(1);
        let mut h = Harness::start(fake);

        let p = h.port.clone();
        let long = thread::spawn(move || p.send(vec![0u8; 5_000_000]));
        thread::sleep(Duration::from_millis(30));
        h.fake.state.lock().unwrap().fatal_read = true;

        let err = long.join().unwrap().unwrap_err();
        assert!(err.contains("port disconnected"), "{err}");
        h.handle.take().unwrap().join().unwrap();
        assert_eq!(h.disconnects.lock().unwrap().len(), 1);

        let started = Instant::now();
        assert!(h.port.send(b"x".to_vec()).is_err());
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(!h.port.is_parked());
    }
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
