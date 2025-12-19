use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDeviceRegistryPropertyW, DIGCF_PRESENT,
    SP_DEVINFO_DATA, SPDRP_FRIENDLYNAME,
};
// use windows::Win32::Foundation::INVALID_HANDLE_VALUE;
// use windows::Win32::Devices::Communication::GUID_DEVINTERFACE_COMPORT; // Might need to check where this is
use windows::core::{GUID, PCWSTR};

// Standard GUID for COM ports (Ports class)
// 4D36E978-E325-11CE-BFC1-08002BE10318
const GUID_DEVINTERFACE_COMPORT: GUID = GUID::from_u128(0x4D36E978_E325_11CE_BFC1_08002BE10318);

#[tauri::command]
pub fn list_ports() -> Vec<String> {
    let mut ports = Vec::new();

    unsafe {
        let h_dev_info = SetupDiGetClassDevsW(
            Some(&GUID_DEVINTERFACE_COMPORT),
            PCWSTR::null(),
            None,
            DIGCF_PRESENT,
        );

        if h_dev_info.is_err() {
            return ports;
        }
        let h_dev_info = h_dev_info.unwrap();

        let mut device_info_data = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };

        let mut index = 0;
        while SetupDiEnumDeviceInfo(h_dev_info, index, &mut device_info_data).is_ok() {
            let mut buffer = [0u16; 256];
            let mut required_size = 0;
            
            // Get Friendly Name
            let res = SetupDiGetDeviceRegistryPropertyW(
                h_dev_info,
                &mut device_info_data,
                SPDRP_FRIENDLYNAME,
                None,
                Some(std::slice::from_raw_parts_mut(buffer.as_mut_ptr() as *mut u8, 512)),
                Some(&mut required_size),
            );

            if res.is_ok() {
                let len = (required_size / 2) as usize;
                if len > 0 {
                    let name = String::from_utf16_lossy(&buffer[..len-1]); // remove null terminator
                    // Filter for "COM"
                    if name.contains("(COM") {
                        ports.push(name);
                    }
                }
            }

            index += 1;
        }

        let _ = SetupDiDestroyDeviceInfoList(h_dev_info);
    }

    ports
}
