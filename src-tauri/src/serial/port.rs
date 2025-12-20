use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Devices::Communication::{
    GetCommState, SetCommState, SetCommTimeouts, COMMTIMEOUTS, DCB, NOPARITY, ONESTOPBIT,
};
use windows::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
};
// FILE_LOCKED_WITH_ONLY_READERS might not be standard, checking CreateFileW docs.
// Usually for Serial: OPEN_EXISTING, GENERIC_READ | GENERIC_WRITE.
// Attributes: FILE_FLAG_OVERLAPPED is essential for async/non-blocking or high perf later.
// But for Phase 1 start, maybe synchronous is easier to debug?
// Plan says "Basic Reader". "Reader Thread".
// "RustによるWin32 APIの直接制御".
// Let's stick to simple blocking or basic overlapped if needed.
// For "12Mbps", overlapped is likely better to keep the pipe full.
// But valid_len, chunk etc comes in Phase 2.
// Phase 1 is just "Open, Write, Read".

pub struct SerialPort {
    handle: HANDLE,
}

unsafe impl Send for SerialPort {}
unsafe impl Sync for SerialPort {}

impl SerialPort {
    pub fn new(port_name: &str, baud_rate: u32) -> Result<Self, String> {
        let mut wide_name: Vec<u16> = OsStr::new(port_name).encode_wide().collect();
        wide_name.push(0);

        // Prefix with \\.\ for COM ports > 9
        let full_name = if port_name.starts_with("\\\\.\\") {
            port_name.to_string()
        } else {
            format!("\\\\.\\{}", port_name)
        };
        let wide_name = HSTRING::from(&full_name);

        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide_name.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                windows::Win32::Storage::FileSystem::FILE_SHARE_MODE(0), // Exclusive access
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
            .map_err(|e| format!("Failed to open port {}: {:?}", port_name, e))?
        };

        let mut port = Self { handle };
        port.configure(baud_rate)?;

        Ok(port)
    }

    fn configure(&mut self, baud_rate: u32) -> Result<(), String> {
        unsafe {
            let mut dcb = DCB {
                DCBlength: std::mem::size_of::<DCB>() as u32,
                ..Default::default()
            };

            if GetCommState(self.handle, &mut dcb).is_err() {
                return Err("Failed to get current comm state".to_string());
            }

            // Basic 8N1 configuration
            dcb.BaudRate = baud_rate;
            dcb.ByteSize = 8;
            dcb.Parity = NOPARITY;
            dcb.StopBits = ONESTOPBIT;

            // fBinary = 1
            // fDtrControl = DTR_CONTROL_ENABLE (1) -> 1 << 4 = 16 (0x10)
            // fRtsControl = RTS_CONTROL_ENABLE (1) -> 1 << 12 = 4096 (0x1000)
            // Total: 0x1011
            dcb._bitfield = 0x1011;

            if SetCommState(self.handle, &dcb).is_err() {
                return Err(format!("Failed to set comm state to {} baud", baud_rate));
            }

            // Configure Timeouts
            // For now, non-blocking read behavior or short timeout?
            // "12Mbps" needs fast reading.
            // ReadFile with MAXDWORD for ReadIntervalTimeout causes it to return immediately with whatever is available.
            // Behavior: return immediately with available data
            let timeouts = COMMTIMEOUTS {
                ReadIntervalTimeout: u32::MAX,
                ReadTotalTimeoutMultiplier: 0,
                ReadTotalTimeoutConstant: 0,
                WriteTotalTimeoutMultiplier: 0,
                WriteTotalTimeoutConstant: 0,
            };

            if SetCommTimeouts(self.handle, &timeouts).is_err() {
                return Err("Failed to set comm timeouts".to_string());
            }
        }
        Ok(())
    }

    pub fn read(&self, buffer: &mut [u8]) -> Result<usize, String> {
        let mut bytes_read = 0;
        unsafe {
            if ReadFile(self.handle, Some(buffer), Some(&mut bytes_read), None).is_err() {
                return Err("Failed to read from port".to_string());
            }
        }
        Ok(bytes_read as usize)
    }

    pub fn write(&self, data: &[u8]) -> Result<usize, String> {
        let mut bytes_written = 0;
        unsafe {
            if WriteFile(self.handle, Some(data), Some(&mut bytes_written), None).is_err() {
                return Err("Failed to write to port".to_string());
            }
        }
        Ok(bytes_written as usize)
    }

    pub fn close(&mut self) {
        if self.handle != INVALID_HANDLE_VALUE {
            unsafe {
                let _ = CloseHandle(self.handle);
            }
            self.handle = INVALID_HANDLE_VALUE;
        }
    }
}

impl Drop for SerialPort {
    fn drop(&mut self) {
        self.close();
    }
}
