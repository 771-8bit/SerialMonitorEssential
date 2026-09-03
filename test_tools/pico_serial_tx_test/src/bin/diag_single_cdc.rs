//! 診断用ファームウェア: 最小構成の単一 USB-CDC（rp-hal 公式 usb_serial 例と同形）
//!
//! デュアル CDC 版のエニュメレーション失敗を切り分けるための対照実験。
//! - これが列挙される → 問題はデュアル CDC / IAD / 記述子側にある
//! - これも失敗する → ビルド設定・ボード・ケーブル/ハブ側にある
//!
//! LED による状態表示:
//! - 起動直後: 3 回速い点滅（コードが走っている証拠。USB 初期化前）
//! - 主ループ: 1 秒周期の点滅（poll が回っている証拠）
//!
//! CDC は受信バイトを大文字化してエコーする。

#![no_std]
#![no_main]

use embedded_hal::digital::OutputPin;
use panic_halt as _;
use rp2040_hal as hal;
use usb_device::device::{StringDescriptors, UsbDeviceBuilder, UsbVidPid};
use usbd_serial::SerialPort;

#[link_section = ".boot2"]
#[used]
pub static BOOT2: [u8; 256] = rp2040_boot2::BOOT_LOADER_W25Q080;

const XTAL_FREQ_HZ: u32 = 12_000_000;

#[hal::entry]
fn main() -> ! {
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
    let mut led = pins.gpio25.into_push_pull_output();
    let timer = hal::Timer::new(pac.TIMER, &mut pac.RESETS, &clocks);

    // 起動確認: 3 回の速い点滅（各 100ms）
    for _ in 0..3 {
        let _ = led.set_high();
        busy_wait_ms(&timer, 100);
        let _ = led.set_low();
        busy_wait_ms(&timer, 100);
    }

    let usb_bus = usb_device::bus::UsbBusAllocator::new(hal::usb::UsbBus::new(
        pac.USBCTRL_REGS,
        pac.USBCTRL_DPRAM,
        clocks.usb_clock,
        true,
        &mut pac.RESETS,
    ));

    let mut serial = SerialPort::new(&usb_bus);

    let mut usb_dev = UsbDeviceBuilder::new(&usb_bus, UsbVidPid(0x16c0, 0x27dd))
        .strings(&[StringDescriptors::default()
            .manufacturer("SerialMonitorEssential")
            .product("Pico Diag Single CDC")
            .serial_number("DIAG")])
        .unwrap()
        .device_class(2) // USB_CLASS_CDC（公式例と同じ）
        .build();

    let mut led_on = false;
    let mut next_toggle = timer.get_counter().ticks() + 500_000;

    loop {
        let _ = usb_dev.poll(&mut [&mut serial]);

        let now = timer.get_counter().ticks();
        if now >= next_toggle {
            next_toggle = now + 500_000;
            led_on = !led_on;
            if led_on {
                let _ = led.set_high();
            } else {
                let _ = led.set_low();
            }
        }

        let mut buf = [0u8; 64];
        if let Ok(count) = serial.read(&mut buf) {
            for b in buf[..count].iter_mut() {
                b.make_ascii_uppercase();
            }
            let mut rest: &[u8] = &buf[..count];
            while !rest.is_empty() {
                match serial.write(rest) {
                    Ok(n) => rest = &rest[n..],
                    Err(_) => break,
                }
            }
        }
    }
}

fn busy_wait_ms(timer: &hal::Timer, ms: u64) {
    let deadline = timer.get_counter().ticks() + ms * 1000;
    while timer.get_counter().ticks() < deadline {}
}
