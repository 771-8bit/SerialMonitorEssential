use log::{debug, info, warn};
use serialport::{DataBits, FlowControl, Parity, SerialPort as SerialPortTrait, StopBits};
use std::io::{Read, Write};
use std::time::Duration;

/// SerialPort wrapper using the serialport crate
pub struct SerialPort {
    inner: Box<dyn SerialPortTrait>,
    port_name: String,
}

// serialport's Box<dyn SerialPort> is Send
unsafe impl Send for SerialPort {}
unsafe impl Sync for SerialPort {}

impl SerialPort {
    /// Create a new SerialPort with the specified port name and baud rate
    ///
    /// # Arguments
    /// * `port_name` - Port name (e.g., "COM3" or "USB Serial Device (COM3)")
    /// * `baud_rate` - Baud rate (e.g., 9600, 115200, 12000000)
    pub fn new(port_name: &str, baud_rate: u32) -> Result<Self, String> {
        info!(
            "[SerialPort] Opening port {} with baud rate {}",
            port_name, baud_rate
        );

        let mut port = serialport::new(port_name, baud_rate)
            .data_bits(DataBits::Eight)
            .parity(Parity::None)
            .stop_bits(StopBits::One)
            .flow_control(FlowControl::None) // Explicitly set no flow control
            .timeout(Duration::from_millis(10)) // 10ms timeout for reading
            .open()
            .map_err(|e| format!("Failed to open port {}: {:?}", port_name, e))?;

        // Enable DTR (Data Terminal Ready) - required for Arduino to send data
        if let Err(e) = port.write_data_terminal_ready(true) {
            warn!("[SerialPort] Failed to set DTR: {:?}", e);
        } else {
            info!("[SerialPort] DTR enabled");
        }

        // Enable RTS (Request to Send) - some devices require this
        if let Err(e) = port.write_request_to_send(true) {
            warn!("[SerialPort] Failed to set RTS: {:?}", e);
        } else {
            info!("[SerialPort] RTS enabled");
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
}
