//! 送信経路（ADR-14 / DEBT-10 の解消）
//!
//! ポートの I/O は受信ワーカーが**単独で**行う（INV-14）。送信・DTR・RTS は
//! [`PortCommand`] としてチャネルでワーカーへ渡し、ワーカーは
//! 「read → チャンク処理 → 送信を 1 スライスだけ書く」を繰り返す（INV-16）。
//!
//! 旧実装は `Arc<Mutex<SerialPort>>` をワーカーと送信側で共有し、送信が
//! ロックを握ったまま `write_all` していたため、書き込み中は受信が止まり、
//! ドライバの受信キューから溢れた分が失われていた（FT232H 12 Mbaud で
//! 16 KiB 送信時に 60% 欠落、docs/24 §6.5 T2-5b）。Windows の serialport は
//! 同期ハンドルなので、ハンドルを複製しても read / write は直列化される。
//! よって「1 スレッドが交互に行う」以外に両 OS で同じ挙動を保証する手段がない。
//!
//! タイムアウトは 2 段（ADR-14 改訂 3）:
//! - **スライス単位の OS タイムアウト**: 1 回の書き込みが受信と停止確認を止める上限。
//!   CTS 停止などで書けなかった分は失敗ではなく、受信判定を挟んで残りから再開する。
//! - **無進捗タイムアウト**（[`STALL_TIMEOUT`]）: 取り出し後、または最後に 1 バイトでも
//!   受理されてから進まなければ `Err`。総時間の期限は置かない: ハードウェアフロー制御では
//!   相手が正当に送信を絞るので所要時間は予測できない（実測で 64 KiB が 86〜320 ms に
//!   ばらつき、総時間の期限が偽のタイムアウトを出した）。問題は「遅い」ではなく「止まった」。
//!
//! 呼び出し側は返信を**期限なしで**待つ。キュー待ちの要求に呼び出し側の期限を
//! 置くと「後で確実に送られるのに Err」になり、再送で二重送信を招くため。
//! ワーカーの終了経路はすべて返信の送り手か要求キューを破棄するので、待ちは必ず戻る。
//! 相手が 1 バイトずつ流し続ける（トリクル）場合、要求は終わらないことがあり、
//! そのときの脱出口は停止・切断（close）だけである。

use crossbeam::channel::{self, Receiver, Sender, TryRecvError};
use log::{info, warn};
use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 1 スライスの線上時間（ミリ秒）。この間だけ受信が止まる。
pub const SLICE_MS: usize = 1;
/// スライス長の下限（低ボーレートでの syscall 過多を防ぐ）
pub const MIN_SLICE: usize = 64;
/// スライス長の上限（1 回の書き込みで受信を止める時間の上限）
pub const MAX_SLICE: usize = 2048;
/// 送信が無いときの read タイムアウト。送信の取り上げ遅延の上限になる。
pub const IDLE_READ_TIMEOUT: Duration = Duration::from_millis(2);
/// スライス単位の OS タイムアウトの下限
const MIN_SLICE_TIMEOUT: Duration = Duration::from_millis(5);
/// 無進捗タイムアウト: この間 1 バイトも受理されなければ送信を失敗にする。
/// 相手がこれ以上 CTS を止める用途（フラッシュ消去中のブートローダ等）は対象外
pub const STALL_TIMEOUT: Duration = Duration::from_secs(3);

/// 線の時間特性（open 時に一度だけ求めてキャッシュする。Windows の `baud_rate()` は
/// 毎回 `GetCommState` を呼ぶため、スライスごとに問い合わせない）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineTiming {
    /// ボーレート（1200 未満は 1200 とみなす: 0 除算と極端な値を避ける）
    baud: u32,
    /// 1 文字のビット数 = start 1 + data + parity(0/1) + stop
    bits_per_char: u32,
}

impl LineTiming {
    pub fn new(baud: u32, bits_per_char: u32) -> Self {
        Self {
            baud: baud.max(1200),
            bits_per_char: bits_per_char.clamp(7, 12),
        }
    }

    /// 8N1 相当（試験と、設定が分からない場合の既定）
    pub fn from_baud(baud: u32) -> Self {
        Self::new(baud, 10)
    }

    /// 1 文字の線上時間（マイクロ秒、切り上げ）
    fn char_micros(&self, chars: usize) -> u64 {
        (chars as u64 * self.bits_per_char as u64 * 1_000_000).div_ceil(self.baud as u64)
    }

    /// 1 スライスのバイト数: 線上 [`SLICE_MS`] ms 分を [`MIN_SLICE`]..=[`MAX_SLICE`] に収める。
    /// 8N1 で 12 Mbaud → 1200 B、115200 → 64 B（下限）。
    pub fn slice_len(&self) -> usize {
        (self.baud as usize / self.bits_per_char as usize * SLICE_MS / 1000)
            .clamp(MIN_SLICE, MAX_SLICE)
    }

    /// スライス単位の OS タイムアウト = max(スライス線上時間 × 2, 5 ms)
    pub fn slice_timeout(&self) -> Duration {
        let twice = Duration::from_micros(self.char_micros(self.slice_len()) * 2);
        twice.max(MIN_SLICE_TIMEOUT)
    }
}

/// ワーカーが使うポート操作。実ポート（`port::SerialPort`）と試験用の偽ポートが実装する。
pub trait PortIo: Send {
    /// 受信。タイムアウトは `Ok(0)`、`Err` は致命的（切断など）。
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String>;
    /// 1 回の書き込み。タイムアウトで書けなかった分は `Ok(n < len)`（0 も可）か
    /// `TimedOut` / `Interrupted` / `WouldBlock` で返ってよい（再開の対象）。
    fn write_some(&mut self, data: &[u8]) -> std::io::Result<usize>;
    /// ドライバの受信キューにあるバイト数
    fn bytes_to_read(&mut self) -> Result<u32, String>;
    /// read / write 共通のタイムアウト（Windows ではデバイス単位で共通）
    fn set_timeout(&mut self, timeout: Duration) -> Result<(), String>;
    fn timing(&self) -> LineTiming;
    fn write_dtr(&mut self, level: bool) -> Result<(), String>;
    fn write_rts(&mut self, level: bool) -> Result<(), String>;
}

/// 送信の受領書
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SendReceipt {
    /// ドライバが受理したバイト数（= 要求の全長。部分書き込みは `Err` になる）
    pub bytes_written: usize,
    /// ワーカーが要求を取り出した時点の**公開済み**受信位置。返信はこれより後ろに
    /// しか着かない。投入前の位置だとキュー待ちの間に届いたデータまで
    /// `wait_for` の走査対象に入る。未公開分（現チャンク）は含めない
    /// （公開済み総量を超えると `wait_for` が世代交代と誤認する）。
    pub offset_before: u64,
}

/// ワーカーへ渡す要求。投入順に 1 つずつ完結する（INV-17）。
pub enum PortCommand {
    Send {
        payload: Vec<u8>,
        reply: Sender<Result<SendReceipt, String>>,
    },
    SetDtr {
        level: bool,
        reply: Sender<Result<(), String>>,
    },
    SetRts {
        level: bool,
        reply: Sender<Result<(), String>>,
    },
}

impl PortCommand {
    /// 実行せずに失敗で返す（停止・切断時のキュー掃き出し用）
    fn fail(self, reason: &str) {
        match self {
            PortCommand::Send { reply, .. } => {
                let _ = reply.send(Err(reason.to_string()));
            }
            PortCommand::SetDtr { reply, .. } | PortCommand::SetRts { reply, .. } => {
                let _ = reply.send(Err(reason.to_string()));
            }
        }
    }
}

/// ワーカー非稼働時に退避しておくもの: I/O、要求の受け手、進行中の送信状態。
/// 送信状態ごと退避するので、Clear（ワーカー再始動）で送信は失われない。
pub struct Parked {
    pub io: Box<dyn PortIo>,
    pub rx: Receiver<PortCommand>,
    pub engine: TxEngine,
}

/// 開いているポート。`SerialState.port` / ブリッジが共有する。
///
/// ポートの I/O 本体は「受信ワーカーが所有」か「ここに退避（parked）」の
/// どちらかにしか存在しない。`Mutex<SerialPort>` を誰かが lock して I/O する
/// 経路は型として存在しない（INV-14）。
pub struct OpenPort {
    name: String,
    cmd_tx: Sender<PortCommand>,
    parked: Mutex<Option<Parked>>,
    /// 致命エラー（切断）または close で false。ブリッジの `status.connected`
    connected: AtomicBool,
}

impl OpenPort {
    pub fn new(name: &str, io: Box<dyn PortIo>) -> Self {
        let (cmd_tx, rx) = channel::unbounded();
        Self {
            name: name.to_string(),
            cmd_tx,
            parked: Mutex::new(Some(Parked {
                io,
                rx,
                engine: TxEngine::default(),
            })),
            connected: AtomicBool::new(true),
        }
    }

    /// デバイスパス（例: "COM7", "/dev/ttyUSB0"）
    pub fn name(&self) -> &str {
        &self.name
    }

    /// I/O が生きているか（切断・close 前）。ハンドルが残っていても切断後は false
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// ワーカーが致命エラーで I/O を破棄したことを記録する
    pub(crate) fn mark_disconnected(&self) {
        self.connected.store(false, Ordering::Relaxed);
    }

    /// ワーカーが I/O を引き取る。既に引き取られている / 閉じている場合は `None`。
    pub(crate) fn take_io(&self) -> Option<Parked> {
        self.parked.lock().ok().and_then(|mut slot| slot.take())
    }

    /// ワーカーが正常停止時に I/O を返す（`clear_data` の再始動で再利用する）
    pub(crate) fn park(&self, parked: Parked) {
        if let Ok(mut slot) = self.parked.lock() {
            *slot = Some(parked);
        }
    }

    /// ワーカーが停止しているときに I/O を引き取れるか（= 再始動できるか）
    pub fn is_parked(&self) -> bool {
        self.parked
            .lock()
            .map(|slot| slot.is_some())
            .unwrap_or(false)
    }

    /// 退避中の I/O を破棄して OS ハンドルを閉じる。ワーカー停止後に呼ぶ。
    /// 進行中・キュー内の要求はすべて `Err` で返る（呼び出し側の待ちが戻る）。
    pub fn close(&self) {
        self.connected.store(false, Ordering::Relaxed);
        if let Some(mut parked) = self.take_io() {
            parked.engine.abort(parked.io.as_mut(), "port closed");
            fail_pending(&parked.rx, "port closed");
            drop(parked);
            info!("[OpenPort] {} closed", self.name);
        }
    }

    /// 送信して完了を待つ。`Ok` は全バイトがドライバに受理されたことを意味する
    /// （部分書き込みは必ず `Err`、INV-15）。呼び出し側はロックを握らずに呼ぶこと。
    pub fn send(&self, payload: Vec<u8>) -> Result<SendReceipt, String> {
        let (reply, result) = channel::bounded(1);
        self.cmd_tx
            .send(PortCommand::Send { payload, reply })
            .map_err(|_| "port not open".to_string())?;
        Self::wait_reply(&result)
    }

    pub fn set_dtr(&self, level: bool) -> Result<(), String> {
        let (reply, result) = channel::bounded(1);
        self.cmd_tx
            .send(PortCommand::SetDtr { level, reply })
            .map_err(|_| "port not open".to_string())?;
        Self::wait_reply(&result)
    }

    pub fn set_rts(&self, level: bool) -> Result<(), String> {
        let (reply, result) = channel::bounded(1);
        self.cmd_tx
            .send(PortCommand::SetRts { level, reply })
            .map_err(|_| "port not open".to_string())?;
        Self::wait_reply(&result)
    }

    /// 期限なしで待つ（モジュール冒頭の理由）。要求キューの受け手と進行中要求の
    /// 送り手は、停止→close・致命エラー・`close` のいずれでも破棄されるので必ず戻る。
    fn wait_reply<T>(result: &Receiver<Result<T, String>>) -> Result<T, String> {
        result
            .recv()
            .unwrap_or_else(|_| Err("port closed".to_string()))
    }
}

/// 進行中の送信
struct ActiveSend {
    payload: Vec<u8>,
    written: usize,
    /// 取り出した時刻、または最後に 1 バイト以上受理された時刻
    last_progress: Instant,
    offset_before: u64,
    reply: Sender<Result<SendReceipt, String>>,
}

/// ワーカー側の送信状態機械。1 回の [`TxEngine::step`] で書くのは高々 1 スライス。
pub struct TxEngine {
    active: Option<ActiveSend>,
    idle_timeout: Duration,
    stall_timeout: Duration,
    /// 退避（suspend）した時刻。再開時にその間の時間を無進捗の計測から除く
    suspended_at: Option<Instant>,
}

impl Default for TxEngine {
    fn default() -> Self {
        Self::new(IDLE_READ_TIMEOUT, STALL_TIMEOUT)
    }
}

impl TxEngine {
    pub fn new(idle_timeout: Duration, stall_timeout: Duration) -> Self {
        Self {
            active: None,
            idle_timeout,
            stall_timeout,
            suspended_at: None,
        }
    }

    /// 送信が進行中か（進行中は read を `bytes_to_read` で前判定する）
    pub fn is_busy(&self) -> bool {
        self.active.is_some()
    }

    /// ワーカー開始時に呼ぶ: 退避中に過ぎた時間を無進捗の計測から除き、状態に合った
    /// タイムアウトを設定する（前のワーカーが送信途中で止まっていても正しく続く）
    pub fn resume(&mut self, io: &mut dyn PortIo) {
        if let (Some(at), Some(active)) = (self.suspended_at.take(), self.active.as_mut()) {
            active.last_progress += at.elapsed();
        }
        let timeout = if self.is_busy() {
            io.timing().slice_timeout()
        } else {
            self.idle_timeout
        };
        let _ = io.set_timeout(timeout);
    }

    /// ワーカー停止時に呼ぶ（進行中の送信は失敗にせず、次のワーカーへ引き継ぐ）
    pub fn suspend(&mut self) {
        self.suspended_at = Some(Instant::now());
    }

    /// 1 ステップ進める: 進行中の送信があれば 1 スライス書く。無ければキューから
    /// 次の要求を取り出す（DTR/RTS はその場で実行し、次の要求へ進む）。
    /// `published_offset` は公開済みの受信位置（受領書の `offset_before` になる）。
    pub fn step(&mut self, io: &mut dyn PortIo, rx: &Receiver<PortCommand>, published_offset: u64) {
        if self.active.is_some() {
            self.write_slice(io);
            return;
        }
        loop {
            match rx.try_recv() {
                Ok(PortCommand::Send { payload, reply }) => {
                    if payload.is_empty() {
                        let _ = reply.send(Ok(SendReceipt {
                            bytes_written: 0,
                            offset_before: published_offset,
                        }));
                        continue;
                    }
                    let timing = io.timing();
                    // 無進捗の計測は取り出した時刻から（キュー待ちの時間を含めない）
                    self.active = Some(ActiveSend {
                        payload,
                        written: 0,
                        last_progress: Instant::now(),
                        offset_before: published_offset,
                        reply,
                    });
                    // 要求ごとに 1 回（取り出し時）。スライスごとには設定しない
                    let _ = io.set_timeout(timing.slice_timeout());
                    // 最初のスライスもここで書く。直前のループで受信判定は済んでいる（INV-16）
                    self.write_slice(io);
                    return;
                }
                Ok(PortCommand::SetDtr { level, reply }) => {
                    let _ = reply.send(io.write_dtr(level));
                }
                Ok(PortCommand::SetRts { level, reply }) => {
                    let _ = reply.send(io.write_rts(level));
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return,
            }
        }
    }

    fn write_slice(&mut self, io: &mut dyn PortIo) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        // 判定は書く前: 取り出し直後の最初のスライスは必ず試行される
        if active.last_progress.elapsed() >= self.stall_timeout {
            let msg = format!(
                "write timed out: no progress for {:.1} s after {}/{} bytes                  (peer flow control holding the line?)",
                self.stall_timeout.as_secs_f64(),
                active.written,
                active.payload.len()
            );
            self.finish(io, Err(msg));
            return;
        }
        let slice = io.timing().slice_len();
        let end = (active.written + slice).min(active.payload.len());
        match io.write_some(&active.payload[active.written..end]) {
            Ok(n) => {
                // n < スライス（0 を含む）はスライスの OS タイムアウト = CTS 停止など。
                // 失敗にせず、受信判定を挟んで次のステップで残りから再開する。
                // 進捗は 1 バイト以上受理されたときだけ更新する
                if n > 0 {
                    active.last_progress = Instant::now();
                }
                active.written += n;
                if active.written == active.payload.len() {
                    let receipt = SendReceipt {
                        bytes_written: active.written,
                        offset_before: active.offset_before,
                    };
                    self.finish(io, Ok(receipt));
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::TimedOut | ErrorKind::Interrupted | ErrorKind::WouldBlock
                ) =>
            {
                // Linux の poll(POLLOUT) タイムアウト / EINTR。0 バイト書けたのと同じ扱い
            }
            Err(e) => {
                let msg = format!(
                    "Failed to write to port after {}/{} bytes: {}",
                    active.written,
                    active.payload.len(),
                    e
                );
                self.finish(io, Err(msg));
            }
        }
    }

    fn finish(&mut self, io: &mut dyn PortIo, result: Result<SendReceipt, String>) {
        if let Some(active) = self.active.take() {
            if let Err(ref e) = result {
                warn!("[TxEngine] {}", e);
            }
            let _ = active.reply.send(result);
        }
        // read 側を待機用の短いタイムアウトへ戻す（要求ごとに 1 回）
        let _ = io.set_timeout(self.idle_timeout);
    }

    /// 進行中の送信を失敗で打ち切る（close 時）。キュー内の未着手要求には触れない。
    pub fn abort(&mut self, io: &mut dyn PortIo, reason: &str) {
        if let Some(active) = self.active.as_ref() {
            let msg = format!(
                "{} ({}/{} bytes written)",
                reason,
                active.written,
                active.payload.len()
            );
            self.finish(io, Err(msg));
        }
    }

    /// 進行中の送信を、ポート無しで失敗させる（致命エラーで I/O を捨てる場合）
    pub fn abort_without_io(&mut self, reason: &str) {
        if let Some(active) = self.active.take() {
            let _ = active.reply.send(Err(format!(
                "{} ({}/{} bytes written)",
                reason,
                active.written,
                active.payload.len()
            )));
        }
    }
}

/// キュー内の未着手要求をすべて失敗で返す（致命エラー・close で受け手を捨てる前）
pub fn fail_pending(rx: &Receiver<PortCommand>, reason: &str) {
    for cmd in rx.try_iter() {
        cmd.fail(reason);
    }
}

/// 試験用の偽ポート（ワーカーループの試験からも使う）
#[cfg(test)]
pub(crate) mod fake {
    use super::PortIo;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Ev {
        Read(usize),
        /// bytes_to_read の前判定
        Check(u32),
        Write(Vec<u8>),
        Timeout(Duration),
        Dtr(bool),
        Rts(bool),
    }

    #[derive(Clone, Copy, Debug)]
    pub enum WriteMode {
        /// すべて受理
        Accept,
        /// TX をそのまま RX へ戻す（FT232H の D0-D1 ジャンパ相当）
        Loopback,
        /// 合計 n バイトまで受理し、以降は 0 を返す（Windows のタイムアウト相当）
        StallAfter(usize),
        /// 書き込みエラー
        Fail,
    }

    pub struct State {
        pub events: Vec<Ev>,
        pub inbound: VecDeque<u8>,
        pub mode: WriteMode,
        pub accepted: usize,
        pub fatal_read: bool,
        /// 1 回の write にかかる時間（線上の時間の模擬）
        pub write_delay: Duration,
        /// 受信が無いときの read の待ち（タイムアウトの模擬）
        pub idle_delay: Duration,
        /// 次の write で順に返す一過性エラー（TimedOut / Interrupted の模擬）
        pub transient_errors: Vec<std::io::ErrorKind>,
    }

    #[derive(Clone)]
    pub struct FakePort {
        pub state: Arc<Mutex<State>>,
        pub baud: u32,
    }

    impl FakePort {
        pub fn new(baud: u32, mode: WriteMode) -> Self {
            Self {
                state: Arc::new(Mutex::new(State {
                    events: Vec::new(),
                    inbound: VecDeque::new(),
                    mode,
                    accepted: 0,
                    fatal_read: false,
                    write_delay: Duration::ZERO,
                    idle_delay: Duration::from_micros(200),
                    transient_errors: Vec::new(),
                })),
                baud,
            }
        }

        pub fn events(&self) -> Vec<Ev> {
            self.state.lock().unwrap().events.clone()
        }

        pub fn written(&self) -> Vec<u8> {
            self.events()
                .into_iter()
                .filter_map(|e| match e {
                    Ev::Write(b) => Some(b),
                    _ => None,
                })
                .flatten()
                .collect()
        }
    }

    impl PortIo for FakePort {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
            let idle = {
                let mut st = self.state.lock().unwrap();
                if st.fatal_read {
                    return Err("Port disconnected: BrokenPipe".to_string());
                }
                let n = st.inbound.len().min(buf.len());
                if n > 0 {
                    for slot in buf.iter_mut().take(n) {
                        *slot = st.inbound.pop_front().unwrap();
                    }
                    st.events.push(Ev::Read(n));
                    return Ok(n);
                }
                st.idle_delay
            };
            std::thread::sleep(idle);
            Ok(0)
        }

        fn write_some(&mut self, data: &[u8]) -> std::io::Result<usize> {
            let delay = self.state.lock().unwrap().write_delay;
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            let mut st = self.state.lock().unwrap();
            if !st.transient_errors.is_empty() {
                let kind = st.transient_errors.remove(0);
                return Err(std::io::Error::new(kind, "transient"));
            }
            let n = match st.mode {
                WriteMode::Accept | WriteMode::Loopback => data.len(),
                WriteMode::StallAfter(limit) => data.len().min(limit.saturating_sub(st.accepted)),
                WriteMode::Fail => return Err(std::io::Error::other("boom")),
            };
            st.accepted += n;
            st.events.push(Ev::Write(data[..n].to_vec()));
            if let WriteMode::Loopback = st.mode {
                st.inbound.extend(&data[..n]);
            }
            Ok(n)
        }

        fn bytes_to_read(&mut self) -> Result<u32, String> {
            let mut st = self.state.lock().unwrap();
            if st.fatal_read {
                return Err("Port disconnected: BrokenPipe".to_string());
            }
            let n = st.inbound.len() as u32;
            st.events.push(Ev::Check(n));
            Ok(n)
        }

        fn set_timeout(&mut self, timeout: Duration) -> Result<(), String> {
            self.state.lock().unwrap().events.push(Ev::Timeout(timeout));
            Ok(())
        }

        fn timing(&self) -> super::LineTiming {
            super::LineTiming::from_baud(self.baud)
        }

        fn write_dtr(&mut self, level: bool) -> Result<(), String> {
            self.state.lock().unwrap().events.push(Ev::Dtr(level));
            Ok(())
        }

        fn write_rts(&mut self, level: bool) -> Result<(), String> {
            self.state.lock().unwrap().events.push(Ev::Rts(level));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Ev, FakePort, WriteMode};
    use super::*;
    use std::sync::Arc;
    use std::thread;

    type Reply = Receiver<Result<SendReceipt, String>>;

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
    }

    fn send_cmd(tx: &Sender<PortCommand>, payload: Vec<u8>) -> Reply {
        let (reply, result) = channel::bounded(1);
        tx.send(PortCommand::Send { payload, reply }).unwrap();
        result
    }

    /// エンジンを手で回す（ワーカーの代わり）
    fn drive(engine: &mut TxEngine, io: &mut FakePort, rx: &Receiver<PortCommand>, steps: usize) {
        for _ in 0..steps {
            engine.step(io, rx, 0);
        }
    }

    fn written_ok(r: &Reply) -> usize {
        r.try_recv().unwrap().unwrap().bytes_written
    }

    #[test]
    fn test_slice_len_boundaries() {
        let len = |baud| LineTiming::from_baud(baud).slice_len();
        assert_eq!(len(0), MIN_SLICE); // 1200 とみなす
        assert_eq!(len(115_200), MIN_SLICE); // 11.52 B/ms -> 下限
        assert_eq!(len(640_000), 64); // ちょうど下限
        assert_eq!(len(3_000_000), 300);
        assert_eq!(len(12_000_000), 1200);
        assert_eq!(len(20_480_000), MAX_SLICE); // ちょうど上限
        assert_eq!(len(u32::MAX), MAX_SLICE);
        // 8N2 (11 bit/char) は 1 ms に入る文字が少ない
        assert_eq!(LineTiming::new(11_000_000, 11).slice_len(), 1000);
    }

    #[test]
    fn test_slice_timeout_has_a_floor_and_scales() {
        // 12 Mbaud: 1200 B = 1 ms -> ×2 = 2 ms -> 下限 5 ms
        assert_eq!(
            LineTiming::from_baud(12_000_000).slice_timeout(),
            MIN_SLICE_TIMEOUT
        );
        // 9600: 64 B = 66.7 ms -> ×2 ≈ 133.4 ms
        let t = LineTiming::from_baud(9600).slice_timeout();
        assert!(
            t > Duration::from_millis(133) && t < Duration::from_millis(134),
            "{t:?}"
        );
    }

    /// 16 KiB の送信は 1 スライス以下の write の列になり、順序どおり全バイトが出て Ok
    #[test]
    fn test_send_is_sliced_in_order_and_complete() {
        let mut fake = FakePort::new(12_000_000, WriteMode::Accept);
        let (tx, rx) = channel::unbounded();
        let payload = pattern(16_384);
        let result = send_cmd(&tx, payload.clone());

        let mut engine = TxEngine::default();
        drive(&mut engine, &mut fake, &rx, 100);
        assert_eq!(written_ok(&result), 16_384);
        assert!(!engine.is_busy());

        let writes: Vec<usize> = fake
            .events()
            .iter()
            .filter_map(|e| {
                if let Ev::Write(b) = e {
                    Some(b.len())
                } else {
                    None
                }
            })
            .collect();
        assert!(writes.iter().all(|&n| n <= 1200), "slices: {writes:?}");
        assert_eq!(writes.len(), 16_384usize.div_ceil(1200));
        assert_eq!(fake.written(), payload);
    }

    /// CTS 停止の模擬: 書けない（Ok(0)）間は失敗にせず待ち、戻れば残りから再開して完走する。
    /// 再開位置はドライバが返した n に一致し、重複も欠落もない
    #[test]
    fn test_stall_then_resume_completes_without_duplication() {
        let mut fake = FakePort::new(12_000_000, WriteMode::StallAfter(5000));
        let (tx, rx) = channel::unbounded();
        let payload = pattern(16_384);
        let result = send_cmd(&tx, payload.clone());
        let mut engine = TxEngine::default();
        drive(&mut engine, &mut fake, &rx, 20);
        assert!(engine.is_busy(), "a stall must not fail the request");
        assert!(result.try_recv().is_err(), "no reply while stalled");

        fake.state.lock().unwrap().mode = WriteMode::Accept; // CTS が戻る
        drive(&mut engine, &mut fake, &rx, 100);
        assert_eq!(written_ok(&result), 16_384);
        assert_eq!(fake.written(), payload);
    }

    /// Linux の poll タイムアウト / EINTR も再開の対象（致命扱いしない）
    #[test]
    fn test_timed_out_and_interrupted_errors_are_retried() {
        let mut fake = FakePort::new(12_000_000, WriteMode::Accept);
        fake.state.lock().unwrap().transient_errors =
            vec![ErrorKind::TimedOut, ErrorKind::Interrupted];
        let (tx, rx) = channel::unbounded();
        let result = send_cmd(&tx, pattern(3000));
        let mut engine = TxEngine::default();
        drive(&mut engine, &mut fake, &rx, 20);
        assert_eq!(written_ok(&result), 3000);
        assert_eq!(fake.written(), pattern(3000));
    }

    /// 試験用の短い無進捗タイムアウト
    const TEST_STALL: Duration = Duration::from_millis(150);

    /// INV-15: 進捗が STALL 止まれば Err（Ok には決してならない）。書けた量と理由を報告する
    #[test]
    fn test_stall_is_an_error_never_ok() {
        let mut fake = FakePort::new(12_000_000, WriteMode::StallAfter(5000));
        fake.state.lock().unwrap().write_delay = Duration::from_millis(5); // スライス OS タイムアウト
        let (tx, rx) = channel::unbounded();
        let result = send_cmd(&tx, pattern(16_384));
        let mut engine = TxEngine::new(IDLE_READ_TIMEOUT, TEST_STALL);
        let started = Instant::now();
        engine.step(&mut fake, &rx, 0);
        while engine.is_busy() {
            engine.step(&mut fake, &rx, 0);
            assert!(started.elapsed() < Duration::from_secs(3), "never expired");
        }
        let err = result.try_recv().unwrap().unwrap_err();
        assert!(
            err.contains("no progress") && err.contains("5000/16384"),
            "{err}"
        );
        assert!(started.elapsed() >= TEST_STALL);
    }

    /// 遅くても進んでいれば失敗にしない: 総所要時間が無進捗タイムアウトを何倍超えても完走する
    /// （フロー制御で相手が絞る場合。総時間の期限はこれを偽のタイムアウトにしていた）
    #[test]
    fn test_slow_but_progressing_send_is_not_a_timeout() {
        let mut fake = FakePort::new(12_000_000, WriteMode::Accept);
        fake.state.lock().unwrap().write_delay = Duration::from_millis(40);
        let (tx, rx) = channel::unbounded();
        let result = send_cmd(&tx, pattern(12_000)); // 10 スライス × 40 ms = 400 ms > 150 ms
        let mut engine = TxEngine::new(IDLE_READ_TIMEOUT, TEST_STALL);
        let started = Instant::now();
        drive(&mut engine, &mut fake, &rx, 20);
        assert!(started.elapsed() > TEST_STALL * 2);
        assert_eq!(written_ok(&result), 12_000);
    }

    #[test]
    fn test_write_error_fails_the_request() {
        let mut fake = FakePort::new(12_000_000, WriteMode::Fail);
        let (tx, rx) = channel::unbounded();
        let result = send_cmd(&tx, pattern(100));
        let mut engine = TxEngine::default();
        drive(&mut engine, &mut fake, &rx, 3);
        let err = result.try_recv().unwrap().unwrap_err();
        assert!(err.contains("0/100") && err.contains("boom"), "{err}");
    }

    /// タイムアウトの設定は要求ごとに 2 回（取り出し時にスライス用、完結時に待機用）。
    /// Windows の COMMTIMEOUTS は 0 =「無期限」なので、1 ms 未満は決して設定しない
    #[test]
    fn test_timeouts_are_set_twice_per_request_and_never_below_1ms() {
        let mut fake = FakePort::new(12_000_000, WriteMode::Accept);
        let (tx, rx) = channel::unbounded();
        let _results: Vec<_> = (0..3).map(|_| send_cmd(&tx, pattern(5000))).collect();
        let mut engine = TxEngine::default();
        drive(&mut engine, &mut fake, &rx, 50);
        let timeouts: Vec<Duration> = fake
            .events()
            .iter()
            .filter_map(|e| {
                if let Ev::Timeout(t) = e {
                    Some(*t)
                } else {
                    None
                }
            })
            .collect();
        let slice = LineTiming::from_baud(12_000_000).slice_timeout();
        let expected: Vec<Duration> = (0..3).flat_map(|_| [slice, IDLE_READ_TIMEOUT]).collect();
        assert_eq!(timeouts, expected);
        assert!(timeouts.iter().all(|t| *t >= Duration::from_millis(1)));
    }

    /// INV-17: 要求は FIFO で完結し、2 要求のバイトは交互にならない。DTR/RTS も同じ順序に乗る
    #[test]
    fn test_requests_are_fifo_and_never_interleaved() {
        let mut fake = FakePort::new(3_000_000, WriteMode::Accept); // スライス 300 B
        let (tx, rx) = channel::unbounded();
        let a = vec![b'a'; 1000];
        let b = vec![b'b'; 1000];
        let res1 = send_cmd(&tx, a.clone());
        let (r2, res2) = channel::bounded(1);
        tx.send(PortCommand::SetDtr {
            level: true,
            reply: r2,
        })
        .unwrap();
        let res3 = send_cmd(&tx, b.clone());
        let (r4, res4) = channel::bounded(1);
        tx.send(PortCommand::SetRts {
            level: false,
            reply: r4,
        })
        .unwrap();

        let mut engine = TxEngine::default();
        drive(&mut engine, &mut fake, &rx, 50);
        assert_eq!(written_ok(&res1), 1000);
        assert_eq!(res2.try_recv().unwrap(), Ok(()));
        assert_eq!(written_ok(&res3), 1000);
        assert_eq!(res4.try_recv().unwrap(), Ok(()));

        let order: String = fake
            .events()
            .iter()
            .filter_map(|e| match e {
                Ev::Write(bytes) => Some(bytes[0] as char),
                Ev::Dtr(_) => Some('D'),
                Ev::Rts(_) => Some('R'),
                _ => None,
            })
            .collect();
        assert_eq!(order, "aaaaDbbbbR");
        assert_eq!(fake.written(), [a, b].concat());
    }

    /// offset_before は要求を取り出した時点の公開済み位置（投入時ではない）
    #[test]
    fn test_offset_before_is_taken_at_dequeue() {
        let mut fake = FakePort::new(3_000_000, WriteMode::Accept);
        let (tx, rx) = channel::unbounded();
        let first = send_cmd(&tx, pattern(600)); // 2 スライス
        let second = send_cmd(&tx, pattern(10));
        let mut engine = TxEngine::default();
        engine.step(&mut fake, &rx, 100); // first を取り出す
        engine.step(&mut fake, &rx, 150); // first 完了
        engine.step(&mut fake, &rx, 400); // second を取り出す（その間に 300 B 受信が公開された）
        assert_eq!(first.try_recv().unwrap().unwrap().offset_before, 100);
        assert_eq!(second.try_recv().unwrap().unwrap().offset_before, 400);
    }

    #[test]
    fn test_empty_send_completes_without_writing() {
        let mut fake = FakePort::new(12_000_000, WriteMode::Accept);
        let (tx, rx) = channel::unbounded();
        let result = send_cmd(&tx, Vec::new());
        let mut engine = TxEngine::default();
        engine.step(&mut fake, &rx, 7);
        assert_eq!(
            result.try_recv().unwrap(),
            Ok(SendReceipt {
                bytes_written: 0,
                offset_before: 7
            })
        );
        assert!(fake.written().is_empty());
    }

    /// 退避（Clear）中の時間は無進捗に数えない
    #[test]
    fn test_suspend_does_not_count_as_a_stall() {
        let mut fake = FakePort::new(12_000_000, WriteMode::StallAfter(1200));
        let (tx, rx) = channel::unbounded();
        let result = send_cmd(&tx, pattern(3000));
        let mut engine = TxEngine::new(IDLE_READ_TIMEOUT, TEST_STALL);
        engine.step(&mut fake, &rx, 0);
        engine.suspend();
        thread::sleep(TEST_STALL * 2); // 遅いディスクでの Clear を模擬
        engine.resume(&mut fake);
        fake.state.lock().unwrap().mode = WriteMode::Accept;
        drive(&mut engine, &mut fake, &rx, 10);
        assert_eq!(written_ok(&result), 3000);
        assert_eq!(fake.written(), pattern(3000));
    }

    /// ハングしない: ワーカー不在で待っている送信は close で Err、閉じた後の送信は即 Err
    #[test]
    fn test_waiting_sender_returns_on_close_and_closed_port_fails_fast() {
        let port = Arc::new(OpenPort::new(
            "FAKE",
            Box::new(FakePort::new(12_000_000, WriteMode::Accept)),
        ));
        assert!(port.is_parked() && port.is_connected());
        let p = port.clone();
        let waiter = thread::spawn(move || p.send(b"hello".to_vec()));
        thread::sleep(Duration::from_millis(50));
        let started = Instant::now();
        port.close();
        assert_eq!(waiter.join().unwrap(), Err("port closed".to_string()));
        assert!(started.elapsed() < Duration::from_millis(500));

        let started = Instant::now();
        assert_eq!(port.send(b"x".to_vec()), Err("port not open".to_string()));
        assert_eq!(port.set_dtr(true), Err("port not open".to_string()));
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(!port.is_parked() && !port.is_connected());
    }

    /// close は退避中の進行中送信も Err で返す（Clear の途中で close された場合）
    #[test]
    fn test_close_fails_a_parked_in_flight_send() {
        let port = OpenPort::new(
            "FAKE",
            Box::new(FakePort::new(12_000_000, WriteMode::Accept)),
        );
        let mut parked = port.take_io().unwrap();
        let mut fake = FakePort::new(12_000_000, WriteMode::StallAfter(100));
        let (reply, result) = channel::bounded(1);
        port.cmd_tx
            .send(PortCommand::Send {
                payload: pattern(1000),
                reply,
            })
            .unwrap();
        parked.engine.step(&mut fake, &parked.rx, 0);
        parked.engine.suspend();
        port.park(parked);
        port.close();
        let err = result.try_recv().unwrap().unwrap_err();
        assert!(
            err.contains("port closed") && err.contains("100/1000"),
            "{err}"
        );
    }
}
