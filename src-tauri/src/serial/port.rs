use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use serialport::{DataBits, FlowControl, Parity, SerialPort as SerialPortTrait, StopBits};
use std::io::{Read, Write};
use std::time::Duration;

use super::tx::{LineTiming, PortIo};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerialConfig {
    pub baud_rate: u32,
    pub data_bits: u8,        // 5, 6, 7, 8
    pub flow_control: String, // "None", "Software", "Hardware"
    pub parity: String,       // "None", "Odd", "Even"
    pub stop_bits: u8,        // 1, 2
    pub dtr: bool,
    pub rts: bool,
}

/// SerialPort wrapper using the serialport crate
pub struct SerialPort {
    inner: Box<dyn SerialPortTrait>,
    port_name: String,
    /// open 時に一度だけ求める（Windows の `baud_rate()` は毎回 `GetCommState`）
    timing: LineTiming,
}

impl SerialPort {
    /// Create a new SerialPort with the specified port name and configuration
    pub fn new(port_name: &str, config: SerialConfig) -> Result<Self, String> {
        info!(
            "[SerialPort] Opening port {} with config {:?}",
            port_name, config
        );

        if config.baud_rate == 0 {
            return Err("Invalid baud rate: 0".to_string());
        }

        let data_bits = match config.data_bits {
            5 => DataBits::Five,
            6 => DataBits::Six,
            7 => DataBits::Seven,
            8 => DataBits::Eight,
            _ => return Err(format!("Invalid data bits: {}", config.data_bits)),
        };

        let flow_control = match config.flow_control.as_str() {
            "None" => FlowControl::None,
            "Software" => FlowControl::Software,
            "Hardware" => FlowControl::Hardware,
            _ => return Err(format!("Invalid flow control: {}", config.flow_control)),
        };

        let parity = match config.parity.as_str() {
            "None" => Parity::None,
            "Odd" => Parity::Odd,
            "Even" => Parity::Even,
            _ => return Err(format!("Invalid parity: {}", config.parity)),
        };

        let stop_bits = match config.stop_bits {
            1 => StopBits::One,
            2 => StopBits::Two,
            _ => return Err(format!("Invalid stop bits: {}", config.stop_bits)),
        };

        let mut port = serialport::new(port_name, config.baud_rate)
            .data_bits(data_bits)
            .parity(parity)
            .stop_bits(stop_bits)
            .flow_control(flow_control)
            // 待機中の read タイムアウト。送信の取り上げ遅延の上限になる（ADR-14）。
            // 送信中は TxEngine が要求の残り期限へ広げ、終わると戻す。
            .timeout(super::tx::IDLE_READ_TIMEOUT)
            .open()
            .map_err(|e| format!("Failed to open port {}: {:?}", port_name, e))?;

        // Opening leaves DTR and RTS off (Windows DCB). DTR must be set BEFORE RTS:
        // on ESP32-style auto-reset circuits the transient (RTS on, DTR off) pulls EN
        // low and resets the board, while (DTR on, RTS off) only holds IO0 low.
        // (serialport's dtr_on_open does the same write but ignores its error.)
        if let Err(e) = port.write_data_terminal_ready(config.dtr) {
            warn!("[SerialPort] Failed to set DTR: {:?}", e);
        } else {
            info!("[SerialPort] DTR set to {}", config.dtr);
        }

        // Enable/Disable RTS (after DTR, see above)
        if let Err(e) = port.write_request_to_send(config.rts) {
            warn!("[SerialPort] Failed to set RTS: {:?}", e);
        } else {
            info!("[SerialPort] RTS set to {}", config.rts);
        }

        // Log the actual configuration
        info!(
            "[SerialPort] Port opened successfully. Actual baud rate: {:?}",
            port.baud_rate()
        );

        // 1 文字のビット数 = start 1 + data + parity + stop（スライス長と OS タイムアウトの計算に使う）
        let bits_per_char = 1
            + config.data_bits as u32
            + u32::from(config.parity != "None")
            + config.stop_bits as u32;
        let baud = port.baud_rate().unwrap_or(config.baud_rate);

        Ok(Self {
            inner: port,
            port_name: port_name.to_string(),
            timing: LineTiming::new(baud, bits_per_char),
        })
    }

    /// Read data from the serial port
    ///
    /// Returns the number of bytes read. Returns 0 on timeout (no data available).
    pub fn read(&mut self, buffer: &mut [u8]) -> Result<usize, String> {
        match self.inner.read(buffer) {
            Ok(n) => {
                if n > 0 {
                    debug!("[SerialPort::read] {} - Read {} bytes", self.port_name, n);
                }
                Ok(n)
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                // This is normal - no data available
                Ok(0)
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                warn!("[SerialPort::read] {} - BrokenPipe error", self.port_name);
                Err(format!("Port disconnected: {:?}", e))
            }
            Err(e) => {
                warn!(
                    "[SerialPort::read] {} - Error: {:?} (kind: {:?})",
                    self.port_name,
                    e,
                    e.kind()
                );
                Err(format!("Failed to read from port: {:?}", e))
            }
        }
    }

    /// Set DTR line state
    pub fn write_dtr(&mut self, level: bool) -> Result<(), String> {
        self.inner
            .write_data_terminal_ready(level)
            .map_err(|e| format!("Failed to set DTR: {:?}", e))
    }

    /// Set RTS line state
    pub fn write_rts(&mut self, level: bool) -> Result<(), String> {
        self.inner
            .write_request_to_send(level)
            .map_err(|e| format!("Failed to set RTS: {:?}", e))
    }
}

/// 受信ワーカー（`tx::TxEngine`）から使うポート操作。I/O はワーカーだけが行う（ADR-14）。
impl PortIo for SerialPort {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        SerialPort::read(self, buf)
    }

    fn write_some(&mut self, data: &[u8]) -> std::io::Result<usize> {
        // 1 回の write のみ（write_all はしない）。Windows はスライスの OS タイムアウトで
        // 書けた分だけを Ok(n < len)（0 も可）で返す。続きは TxEngine が受信判定を挟んで再開し、
        // 失敗にするのは進捗が STALL_TIMEOUT 止まったときだけ。
        self.inner.write(data)
    }

    fn bytes_to_read(&mut self) -> Result<u32, String> {
        self.inner
            .bytes_to_read()
            .map_err(|e| format!("Failed to query input queue: {:?}", e))
    }

    fn set_timeout(&mut self, timeout: Duration) -> Result<(), String> {
        self.inner
            .set_timeout(timeout)
            .map_err(|e| format!("Failed to set timeout: {:?}", e))
    }

    fn timing(&self) -> LineTiming {
        self.timing
    }

    fn write_dtr(&mut self, level: bool) -> Result<(), String> {
        SerialPort::write_dtr(self, level)
    }

    fn write_rts(&mut self, level: bool) -> Result<(), String> {
        SerialPort::write_rts(self, level)
    }
}
