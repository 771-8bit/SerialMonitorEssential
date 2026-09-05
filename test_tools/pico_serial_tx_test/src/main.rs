//! SerialMonitorEssential 高速シリアル送信テスト for Raspberry Pi Pico
//! 【組み込み Rust / デュアル USB-CDC 版】
//!
//! Arduino 版 (`legacy_arduino/pico_serial_tx_test.ino`) の置き換え。
//! **制御ポートのプロトコル（コマンドと応答文字列）は Arduino 版と完全互換**で、
//! `test_tools/lib/pico_controller.py` / `serial_test.py --source pico` /
//! `identify_pico_ports.py` がそのまま使える。
//!
//! CDC 構成（1 つの USB デバイスに 2 つの CDC-ACM、IAD 付きコンポジット）:
//! - CDC0: データ送信専用（純粋なテストデータのみ。バナーを出さない）
//! - CDC1: コマンド制御専用（115200bps 相当。IDENTIFY に PORT_TYPE: CONTROL を返す）
//!
//! コマンド（制御ポート、1 行 1 コマンド）:
//! - `START:<秒>`   高速テスト: 0,1,...,255 を繰り返すカウンタパターンを全力送信
//! - `SLOW:<秒>`    低速テスト: 1 行/秒のテキスト
//! - `PLOTTER:<秒>` プロッタテスト: 10Hz の CSV（time,sin,cos,random）
//! - `STOP` / `STATUS` / `IDENTIFY`
//!
//! 送信した全バイトの SHA-256 を計算し、終了時に
//! `TEST_STOP` / `Total bytes: N` / `Checksum: <大文字 hex>` を制御ポートへ返す
//! （受信側 `verify_received_data.py` は大文字に正規化して比較する）。
//!
//! 実機デバッグ (2026-09-04) で判明した要点:
//! - **本当の原因は送信ゼロの u64 アンダーフロー**: 主ループ先頭で取得した
//!   now_us はコマンド処理より前の値で、START を処理した同一イテレーションでは
//!   start_test が設定した started_us より古い。`now_us - started_us` が
//!   アンダーフローして即 finished 扱いになり total=0 になっていた。テスト実行
//!   ブロックで now_us を取り直して解消。
//! - USB エニュメレーション失敗も実機でのみ再現: usb-device の EP0 既定サイズ 8
//!   と、コントロール転送バッファ既定 128 バイト（デュアル CDC の構成記述子
//!   ~141 バイトが収まらない）。`max_packet_size_0(64)` と feature
//!   `control-buffer-256` で解消。
//! - DTR ゲートは不採用（常時送信）: `dtr()` 自体は実機で正しく更新される
//!   （ホストが DTR をアサートすれば true）ことを DIAG で確認済みだが、
//!   プラットフォーム間の DTR 線の扱いに依存しない方が単純で堅牢なため外した。
//!   `write()` は write_buf に積むだけなので各送信後に `flush()` が必要。
//!   ホスト不在時の空カウントは (1) write_buf 満杯で write()=0 となり頭打ち、
//!   (2) 受信側 serial_test.py の 0 バイトガード、で担保する。

#![no_std]
#![no_main]

use core::fmt::Write as _;

use embedded_hal::digital::OutputPin;
use heapless::{String, Vec};
use panic_halt as _;
use rp2040_hal as hal;
use sha2::{Digest, Sha256};
use usb_device::device::{StringDescriptors, UsbDevice, UsbDeviceBuilder, UsbVidPid};
use usb_device::UsbError;
use usbd_serial::SerialPort;

/// ブートローダ（W25Q080 互換 flash 用の標準 boot2）
#[link_section = ".boot2"]
#[used]
pub static BOOT2: [u8; 256] = rp2040_boot2::BOOT_LOADER_W25Q080;

/// Pico 基板の水晶は 12 MHz
const XTAL_FREQ_HZ: u32 = 12_000_000;

/// 高速テストで一度に書き込みを試みるサイズ。
/// USB バルクパケット(64B)より十分大きく、SHA 更新の呼び出し回数を減らす。
const FAST_CHUNK: usize = 512;

/// USB バス型（型注釈簡略化用）
type Bus = hal::usb::UsbBus;
/// データポート: 書き込みストアを大きめに取り、スループットを稼ぐ
type DataPort<'a> = SerialPort<'a, Bus, [u8; 64], [u8; 4096]>;
/// 制御ポート: テキスト行が入れば十分
type CtrlPort<'a> = SerialPort<'a, Bus, [u8; 256], [u8; 1024]>;

/// テストモード
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Fast,
    Slow,
    Plotter,
}

/// 実行中テストの状態
struct Test {
    mode: Mode,
    /// 0 なら無限
    duration_s: u64,
    started_us: u64,
    total_bytes: u64,
    sha: Sha256,
    /// Fast: 次に送るパターンバイト
    next_pattern: u8,
    /// Slow/Plotter: 送信行数
    lines: u32,
    /// Slow/Plotter: 最後に送った時刻 (µs)
    last_send_us: u64,
    /// Plotter: ランダムウォーク値と乱数状態
    random_walk: f32,
    rng: u32,
    /// Plotter: ヘッダー行が未送信（データポートが開いてから送る）
    header_pending: bool,
}

/// xorshift32（Arduino の random() 相当の用途には十分）
fn xorshift32(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

#[hal::entry]
fn main() -> ! {
    // ------------------------------------------------------------------ 初期化
    let mut pac = hal::pac::Peripherals::take().unwrap();
    let mut watchdog = hal::Watchdog::new(pac.WATCHDOG);
    let clocks = hal::clocks::init_clocks_and_plls(
        XTAL_FREQ_HZ,
        pac.XOSC,
        pac.CLOCKS,
        pac.PLL_SYS,
        pac.PLL_USB,
        &mut pac.RESETS,
        &mut watchdog,
    )
    .ok()
    .unwrap();

    let sio = hal::Sio::new(pac.SIO);
    let pins = hal::gpio::Pins::new(
        pac.IO_BANK0,
        pac.PADS_BANK0,
        sio.gpio_bank0,
        &mut pac.RESETS,
    );
    // オンボード LED（Pico W は GPIO25 に LED が無い点に注意。README 参照）
    let mut led = pins.gpio25.into_push_pull_output();
    let timer = hal::Timer::new(pac.TIMER, &mut pac.RESETS, &clocks);

    let usb_bus = usb_device::bus::UsbBusAllocator::new(hal::usb::UsbBus::new(
        pac.USBCTRL_REGS,
        pac.USBCTRL_DPRAM,
        clocks.usb_clock,
        true,
        &mut pac.RESETS,
    ));

    // 生成順 = インターフェース順。どちらのポートが先に見えるかは OS 依存なので、
    // 判別は identify_pico_ports.py の IDENTIFY プローブで行う（Arduino 版と同じ）。
    let mut data: DataPort = SerialPort::new_with_store(&usb_bus, [0u8; 64], [0u8; 4096]);
    let mut ctrl: CtrlPort = SerialPort::new_with_store(&usb_bus, [0u8; 256], [0u8; 1024]);

    let mut usb_dev: UsbDevice<Bus> = UsbDeviceBuilder::new(&usb_bus, UsbVidPid(0x16c0, 0x27dd))
        .strings(&[StringDescriptors::default()
            .manufacturer("SerialMonitorEssential")
            .product("Pico TX Test (Rust, dual CDC)")
            .serial_number("PICO-TX-RS")])
        .unwrap()
        // usb-device 0.3 の既定 EP0 サイズは 8。rp2040-hal では 64 を明示しないと
        // 構成記述子のマルチパケット転送が失敗し、ホストに
        // 「Configuration Descriptor Request Failed」と認識される（実機で確認済み）。
        .max_packet_size_0(64)
        .unwrap()
        // 複数 CDC のコンポジットは IAD 必須（Misc / Common Class / IAD）
        .composite_with_iads()
        .build();

    // ------------------------------------------------------------------ 状態
    let mut test: Option<Test> = None;
    let mut cmd_line: Vec<u8, 64> = Vec::new();
    let mut banner_sent = false;
    let mut led_on = false;
    let mut last_blink_us: u64 = 0;
    // 直近に完了したテストの送信バイト数（STATUS 用。Arduino 版と同じく保持する）
    let mut last_total: u64 = 0;
    let mut fast_buf = [0u8; FAST_CHUNK];

    // ------------------------------------------------------------------ 主ループ
    loop {
        usb_dev.poll(&mut [&mut data, &mut ctrl]);
        let now_us = timer.get_counter().ticks();

        // データポートへの入力は読み捨てる（純粋な送信専用ポート）
        let mut sink = [0u8; 64];
        let _ = data.read(&mut sink);

        // 制御ホストが最初に開いたときに一度だけバナーを出す
        if !banner_sent && ctrl.dtr() {
            banner_sent = true;
            send_banner(&mut usb_dev, &mut data, &mut ctrl, &timer);
        }

        // ---------------- コマンド受信（1 行単位） ----------------
        let mut rx = [0u8; 64];
        if let Ok(n) = ctrl.read(&mut rx) {
            for &b in &rx[..n] {
                if b == b'\n' {
                    let line = trim_ascii(&cmd_line);
                    if !line.is_empty() {
                        handle_command(
                            line,
                            &mut test,
                            &mut last_total,
                            &mut usb_dev,
                            &mut data,
                            &mut ctrl,
                            &timer,
                        );
                    }
                    cmd_line.clear();
                } else if cmd_line.push(b).is_err() {
                    // 長すぎる行は捨てて仕切り直す
                    cmd_line.clear();
                }
            }
        }

        // ---------------- テスト実行 ----------------
        let mut finished = false;
        if let Some(t) = test.as_mut() {
            // now_us をここで取り直す。ループ先頭(l.172)の now_us はコマンド処理
            // より前に取得しており、START を処理した同一イテレーションでは
            // start_test が設定した started_us より古い。u64 の減算がアンダー
            // フローして巨大値になり、即 finished 扱いで total=0 になっていた
            // （実機デバッグ 2026-09-04 で判明した本当の原因）。
            let now_us = timer.get_counter().ticks();
            // 時間切れ判定
            if t.duration_s > 0 && now_us - t.started_us >= t.duration_s * 1_000_000 {
                finished = true;
            } else {
                match t.mode {
                    Mode::Fast => {
                        // 全力送信。DTR ゲートは使わない（NOTE 参照。実機で dtr()
                        // が更新されず送信が全ブロックされた）。write() は write_buf
                        // に積むだけなので、flush() で初めてエンドポイントへ出る。
                        let mut b = t.next_pattern;
                        for slot in fast_buf.iter_mut() {
                            *slot = b;
                            b = b.wrapping_add(1);
                        }
                        match data.write(&fast_buf) {
                            Ok(written) if written > 0 => {
                                t.sha.update(&fast_buf[..written]);
                                t.total_bytes += written as u64;
                                t.next_pattern = t.next_pattern.wrapping_add(written as u8);
                            }
                            _ => {}
                        }
                        let _ = data.flush();
                    }
                    Mode::Slow => {
                        if now_us - t.last_send_us >= 1_000_000 {
                            t.last_send_us = now_us;
                            let elapsed_s = (now_us - t.started_us) / 1_000_000;
                            let mut line: String<64> = String::new();
                            let _ = write!(
                                line,
                                "[{:04}] Hello from Pico! Counter={}\r\n",
                                elapsed_s, t.lines
                            );
                            if send_data(
                                &mut usb_dev,
                                &mut data,
                                &mut ctrl,
                                &timer,
                                t,
                                line.as_bytes(),
                            ) {
                                t.lines += 1;
                                let mut msg: String<64> = String::new();
                                let _ =
                                    write!(msg, "Sent line {}: {} bytes\r\n", t.lines, line.len());
                                ctrl_send(&mut usb_dev, &mut data, &mut ctrl, &timer, &msg);
                            }
                        }
                    }
                    Mode::Plotter => {
                        // ヘッダー行を先頭に一度だけ送る（チェックサムにも含める）
                        if t.header_pending {
                            let header = b"time,sin,cos,random\r\n";
                            if send_data(&mut usb_dev, &mut data, &mut ctrl, &timer, t, header) {
                                t.header_pending = false;
                            }
                        }
                        if now_us - t.last_send_us >= 100_000 {
                            t.last_send_us = now_us;
                            let elapsed = (now_us - t.started_us) as f32 / 1_000_000.0;
                            const PI: f32 = core::f32::consts::PI;
                            let ch1 = 100.0 * libm::sinf(2.0 * PI * elapsed / 2.0);
                            let ch2 = 80.0 * libm::cosf(2.0 * PI * elapsed / 3.0);
                            // random(-100,101)/100*5 相当のランダムウォーク
                            let step = ((xorshift32(&mut t.rng) % 201) as i32 - 100) as f32 / 100.0;
                            t.random_walk = (t.random_walk + step * 5.0).clamp(-150.0, 150.0);
                            let mut line: String<96> = String::new();
                            let _ = write!(
                                line,
                                "{:.2},{:.2},{:.2},{:.2}\r\n",
                                elapsed, ch1, ch2, t.random_walk
                            );
                            if send_data(
                                &mut usb_dev,
                                &mut data,
                                &mut ctrl,
                                &timer,
                                t,
                                line.as_bytes(),
                            ) {
                                t.lines += 1;
                                if t.lines % 100 == 0 {
                                    let mut msg: String<64> = String::new();
                                    let _ = write!(
                                        msg,
                                        "Plotter: {} lines, {} bytes\r\n",
                                        t.lines, t.total_bytes
                                    );
                                    ctrl_send(&mut usb_dev, &mut data, &mut ctrl, &timer, &msg);
                                }
                            }
                        }
                    }
                }

                // LED 点滅（500ms）
                if now_us - last_blink_us > 500_000 {
                    last_blink_us = now_us;
                    led_on = !led_on;
                    if led_on {
                        let _ = led.set_high();
                    } else {
                        let _ = led.set_low();
                    }
                }
            }
        } else {
            // 待機中は 2 秒ごとに短く点灯（「ファームが生きているか」を目視できる。
            // 常時消灯だとエニュメレーション失敗と区別が付かない）
            if now_us % 2_000_000 < 50_000 {
                let _ = led.set_high();
            } else {
                let _ = led.set_low();
            }
        }

        if finished {
            stop_test(
                &mut test,
                &mut usb_dev,
                &mut data,
                &mut ctrl,
                &timer,
                &mut last_total,
            );
        }
    }
}

/// 前後の空白・CR を落とす
fn trim_ascii(line: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = line.len();
    while start < end && line[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && line[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &line[start..end]
}

/// `PREFIX:<数値>` の数値部を読む
fn parse_duration(rest: &[u8]) -> u64 {
    let mut value: u64 = 0;
    for &b in rest {
        if b.is_ascii_digit() {
            value = value.saturating_mul(10).saturating_add((b - b'0') as u64);
        } else {
            break;
        }
    }
    value
}

fn handle_command<'a>(
    line: &[u8],
    test: &mut Option<Test>,
    last_total: &mut u64,
    usb: &mut UsbDevice<'a, Bus>,
    data: &mut DataPort<'a>,
    ctrl: &mut CtrlPort<'a>,
    timer: &hal::Timer,
) {
    if let Some(rest) = line.strip_prefix(b"START:") {
        start_test(
            test,
            Mode::Fast,
            parse_duration(rest),
            usb,
            data,
            ctrl,
            timer,
        );
    } else if let Some(rest) = line.strip_prefix(b"SLOW:") {
        start_test(
            test,
            Mode::Slow,
            parse_duration(rest),
            usb,
            data,
            ctrl,
            timer,
        );
    } else if let Some(rest) = line.strip_prefix(b"PLOTTER:") {
        start_test(
            test,
            Mode::Plotter,
            parse_duration(rest),
            usb,
            data,
            ctrl,
            timer,
        );
    } else if line == b"STOP" {
        stop_test(test, usb, data, ctrl, timer, last_total);
    } else if line == b"STATUS" {
        send_status(test, *last_total, usb, data, ctrl, timer);
    } else if line == b"IDENTIFY" {
        send_identification(usb, data, ctrl, timer);
    } else if line == b"DIAG" {
        // データポートの状態と write/flush の生の結果を制御ポートへ報告する。
        // 200 バイトを data へ書いて、write() の戻り値と 100ms 分の flush 結果を見る。
        let payload = [0x41u8; 200];
        let wrote = match data.write(&payload) {
            Ok(n) => n as i64,
            Err(usb_device::UsbError::WouldBlock) => -1,
            Err(_) => -2,
        };
        let deadline = timer.get_counter().ticks() + 100_000;
        let mut flush_ok = false;
        let mut polls = 0u32;
        while timer.get_counter().ticks() < deadline {
            usb.poll(&mut [data, ctrl]);
            polls += 1;
            if data.flush().is_ok() {
                flush_ok = true;
                break;
            }
        }
        let mut msg: String<160> = String::new();
        let _ = write!(
            msg,
            "DIAG: data.dtr={} data.rts={} write={} flush_ok={} polls={}\r\n",
            data.dtr(),
            data.rts(),
            wrote,
            flush_ok,
            polls
        );
        ctrl_send(usb, data, ctrl, timer, &msg);
    } else if line == b"BOOTSEL" {
        // ボタンを押さずに BOOTSEL（UF2 書き込み）モードへ再起動する。
        // 以後のファーム更新は「BOOTSEL 送信 → UF2 コピー」だけで済む。
        ctrl_send(usb, data, ctrl, timer, "REBOOTING TO BOOTSEL\r\n");
        hal::rom_data::reset_to_usb_boot(0, 0);
        // reset_to_usb_boot は戻らないはずだが、型上の保険
        loop {
            cortex_m::asm::nop();
        }
    } else {
        let mut msg: String<128> = String::new();
        let _ = write!(msg, "ERROR: Unknown command: ");
        for &b in line.iter().take(48) {
            let _ = msg.push(if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '?'
            });
        }
        let _ = write!(msg, "\r\n");
        ctrl_send(usb, data, ctrl, timer, &msg);
        ctrl_send(
            usb,
            data,
            ctrl,
            timer,
            "Available commands: START:<seconds>, SLOW:<seconds>, PLOTTER:<seconds>, STOP, STATUS, IDENTIFY, BOOTSEL\r\n",
        );
    }
}

fn start_test<'a>(
    test: &mut Option<Test>,
    mode: Mode,
    duration_s: u64,
    usb: &mut UsbDevice<'a, Bus>,
    data: &mut DataPort<'a>,
    ctrl: &mut CtrlPort<'a>,
    timer: &hal::Timer,
) {
    // 前のテストの未送出バイトが書き込みストアに残っていると、次のキャプチャの
    // 先頭に紛れ込む。ホストが読める状態なら、ここで掃き出してから始める。
    flush_data(usb, data, ctrl, timer);

    let now_us = timer.get_counter().ticks();
    let t = Test {
        mode,
        duration_s,
        started_us: now_us,
        total_bytes: 0,
        sha: Sha256::new(),
        next_pattern: 0,
        lines: 0,
        last_send_us: 0,
        random_walk: 0.0,
        rng: (now_us as u32) | 1, // xorshift の 0 種を避ける
        header_pending: mode == Mode::Plotter,
    };

    let (start_msg, mode_name) = match mode {
        Mode::Fast => ("TEST_START\r\n", "FAST (12Mbps)"),
        Mode::Slow => ("SLOW_TEST_START\r\n", "SLOW (1 line/sec)"),
        Mode::Plotter => ("PLOTTER_TEST_START\r\n", "PLOTTER (CSV data, 10Hz)"),
    };
    ctrl_send(usb, data, ctrl, timer, start_msg);
    let mut msg: String<96> = String::new();
    let _ = write!(msg, "Mode: {}\r\nDuration: {}", mode_name, duration_s);
    let _ = write!(
        msg,
        "{}\r\n",
        if duration_s == 0 {
            " seconds (infinite)"
        } else {
            " seconds"
        }
    );
    ctrl_send(usb, data, ctrl, timer, &msg);

    // プロッタモードのヘッダー行は、データポートの DTR が立ってから主ループが
    // 送る（header_pending）。ここで無条件に送ると、ホスト不在でもハッシュと
    // カウントだけが進み、次のキャプチャの先頭を汚染する。

    *test = Some(t);
}

fn stop_test<'a>(
    test: &mut Option<Test>,
    usb: &mut UsbDevice<'a, Bus>,
    data: &mut DataPort<'a>,
    ctrl: &mut CtrlPort<'a>,
    timer: &hal::Timer,
    last_total: &mut u64,
) {
    let Some(t) = test.take() else {
        ctrl_send(usb, data, ctrl, timer, "ERROR: Test is not running\r\n");
        return;
    };

    // データポートのバッファを掃き出してから結果を出す
    flush_data(usb, data, ctrl, timer);

    *last_total = t.total_bytes;
    let digest = t.sha.finalize();
    let mut msg: String<160> = String::new();
    let _ = write!(
        msg,
        "\r\nTEST_STOP\r\nTotal bytes: {}\r\nChecksum: ",
        t.total_bytes
    );
    for b in digest.iter() {
        let _ = write!(msg, "{:02X}", b);
    }
    let _ = write!(msg, "\r\n");
    ctrl_send(usb, data, ctrl, timer, &msg);
}

fn send_status<'a>(
    test: &Option<Test>,
    last_total: u64,
    usb: &mut UsbDevice<'a, Bus>,
    data: &mut DataPort<'a>,
    ctrl: &mut CtrlPort<'a>,
    timer: &hal::Timer,
) {
    let mut msg: String<192> = String::new();
    let _ = write!(msg, "STATUS:\r\n  Running: ");
    match test {
        Some(t) => {
            let _ = write!(msg, "YES\r\n  Total bytes sent: {}\r\n", t.total_bytes);
            if t.duration_s > 0 {
                let elapsed = (timer.get_counter().ticks() - t.started_us) / 1_000_000;
                let _ = write!(msg, "  Elapsed: {} / {} seconds\r\n", elapsed, t.duration_s);
            }
        }
        None => {
            // Arduino 版と同じく、直近に完了したテストの値を報告する
            let _ = write!(msg, "NO\r\n  Total bytes sent: {}\r\n", last_total);
        }
    }
    ctrl_send(usb, data, ctrl, timer, &msg);
}

fn send_identification<'a>(
    usb: &mut UsbDevice<'a, Bus>,
    data: &mut DataPort<'a>,
    ctrl: &mut CtrlPort<'a>,
    timer: &hal::Timer,
) {
    // データポートには何も出さない（純粋なデータ専用。identify はこの沈黙で判別する）
    ctrl_send(
        usb,
        data,
        ctrl,
        timer,
        "PORT_TYPE: CONTROL\r\nBAUD_RATE: 115200\r\nPURPOSE: Command control from Python script\r\n---\r\n",
    );
}

fn send_banner<'a>(
    usb: &mut UsbDevice<'a, Bus>,
    data: &mut DataPort<'a>,
    ctrl: &mut CtrlPort<'a>,
    timer: &hal::Timer,
) {
    send_identification(usb, data, ctrl, timer);
    ctrl_send(
        usb,
        data,
        ctrl,
        timer,
        "\r\n=== SerialMonitorEssential Pico Test (Rust, Dual CDC) ===\r\n\
         Control Port (this port): 115200bps\r\n\
         Data Port: 12Mbps class (USB-CDC)\r\n\r\n\
         Commands:\r\n\
         \x20 START:<duration>  - Start stress test for <duration> seconds\r\n\
         \x20 SLOW:<duration>   - Start slow test (1 line/sec) for <duration> seconds\r\n\
         \x20 PLOTTER:<duration> - Start plotter test (CSV data) for <duration> seconds\r\n\
         \x20 STOP              - Stop test\r\n\
         \x20 STATUS            - Show current status\r\n\
         \x20 IDENTIFY          - Show port identification info\r\n\r\n\
         Ready for commands.\r\n\r\n",
    );
}

/// 制御ポートへ文字列を送る（ホスト不在なら黙って捨てる。500ms 上限）
fn ctrl_send<'a>(
    usb: &mut UsbDevice<'a, Bus>,
    data: &mut DataPort<'a>,
    ctrl: &mut CtrlPort<'a>,
    timer: &hal::Timer,
    s: &str,
) {
    if !ctrl.dtr() {
        return;
    }
    let deadline = timer.get_counter().ticks() + 500_000;
    let mut buf = s.as_bytes();
    while !buf.is_empty() && timer.get_counter().ticks() < deadline {
        usb.poll(&mut [data, ctrl]);
        match ctrl.write(buf) {
            Ok(n) => buf = &buf[n..],
            Err(UsbError::WouldBlock) => {}
            Err(_) => break,
        }
    }
    let _ = ctrl.flush();
}

/// データポートへ全バイトを送り、成功分をテスト状態へ計上する。
/// 全量送れたら true（行単位モード用。500ms 上限）。
fn send_data<'a>(
    usb: &mut UsbDevice<'a, Bus>,
    data: &mut DataPort<'a>,
    ctrl: &mut CtrlPort<'a>,
    timer: &hal::Timer,
    t: &mut Test,
    payload: &[u8],
) -> bool {
    let deadline = timer.get_counter().ticks() + 500_000;
    let mut buf = payload;
    while !buf.is_empty() && timer.get_counter().ticks() < deadline {
        usb.poll(&mut [data, ctrl]);
        match data.write(buf) {
            Ok(n) => {
                t.sha.update(&buf[..n]);
                t.total_bytes += n as u64;
                buf = &buf[n..];
            }
            Err(UsbError::WouldBlock) => {}
            Err(_) => break,
        }
    }
    buf.is_empty()
}

/// データポートの送信バッファをホストへ掃き出す（テスト終了時。200ms 上限）
fn flush_data<'a>(
    usb: &mut UsbDevice<'a, Bus>,
    data: &mut DataPort<'a>,
    ctrl: &mut CtrlPort<'a>,
    timer: &hal::Timer,
) {
    let deadline = timer.get_counter().ticks() + 200_000;
    while timer.get_counter().ticks() < deadline {
        usb.poll(&mut [data, ctrl]);
        match data.flush() {
            Ok(()) => break,
            Err(UsbError::WouldBlock) => {}
            Err(_) => break,
        }
    }
}
