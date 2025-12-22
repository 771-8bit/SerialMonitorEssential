use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use serialport::{DataBits, FlowControl, Parity, SerialPort as SerialPortTrait, StopBits};
use std::io::{Read, Write};
use std::time::Duration;

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
}

// serialport's Box<dyn SerialPort> is Send
unsafe impl Send for SerialPort {}
unsafe impl Sync for SerialPort {}

impl SerialPort {
    /// Create a new SerialPort with the specified port name and configuration
    pub fn new(port_name: &str, config: SerialConfig) -> Result<Self, String> {
        info!(
            "[SerialPort] Opening port {} with config {:?}",
            port_name, config
        );

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
            .timeout(Duration::from_millis(10)) // 10ms timeout for reading
            .open()
            .map_err(|e| format!("Failed to open port {}: {:?}", port_name, e))?;

        // Enable/Disable DTR
        if let Err(e) = port.write_data_terminal_ready(config.dtr) {
            warn!("[SerialPort] Failed to set DTR: {:?}", e);
        } else {
            info!("[SerialPort] DTR set to {}", config.dtr);
        }

        // Enable/Disable RTS
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

        Ok(Self {
            inner: port,
            port_name: port_name.to_string(),
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

    /// Write data to the serial port
    ///
    /// Returns the number of bytes written.
    pub fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        debug!(
            "[SerialPort::write] {} - Writing {} bytes",
            self.port_name,
            data.len()
        );
        self.inner
            .write(data)
            .map_err(|e| format!("Failed to write to port: {:?}", e))
    }

    /// Close the serial port (no-op, handled by Drop)
    pub fn close(&mut self) {
        info!("[SerialPort::close] {} - Closing port", self.port_name);
        // serialport handles closing via Drop automatically
    }

    /// Check if the port configuration is valid (for testing purposes)
    #[allow(dead_code)]
    pub fn is_valid(&self) -> bool {
        self.inner.baud_rate().is_ok()
    }

    /// Get the number of bytes available to read
    #[allow(dead_code)]
    pub fn bytes_to_read(&self) -> Result<u32, String> {
        self.inner
            .bytes_to_read()
            .map_err(|e| format!("Failed to get bytes to read: {:?}", e))
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
