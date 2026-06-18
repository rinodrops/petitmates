/// Detect whether the system is currently running on AC (wall) power.
///
/// Returns `true` on AC power or when the power source cannot be determined
/// (fail-safe: prefer performance over saving).

#[cfg(target_os = "macos")]
pub fn on_ac_power() -> bool {
    use std::ffi::CStr;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPSCopyPowerSourcesInfo() -> *mut std::ffi::c_void;
        fn IOPSGetProvidingPowerSourceType(snapshot: *mut std::ffi::c_void) -> *const i8;
        fn CFRelease(cf: *mut std::ffi::c_void);
    }

    unsafe {
        let info = IOPSCopyPowerSourcesInfo();
        if info.is_null() {
            return true;
        }
        let src = IOPSGetProvidingPowerSourceType(info);
        let result = if src.is_null() {
            true
        } else {
            // "AC Power" | "Battery Power" | "UPS Power"
            CStr::from_ptr(src).to_string_lossy() != "Battery Power"
        };
        CFRelease(info);
        result
    }
}

#[cfg(target_os = "windows")]
pub fn on_ac_power() -> bool {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    unsafe {
        let mut status: SYSTEM_POWER_STATUS = std::mem::zeroed();
        if GetSystemPowerStatus(&mut status) == 0 {
            return true;
        }
        // ACLineStatus: 0 = offline (battery), 1 = online (AC), 255 = unknown
        status.ACLineStatus != 0
    }
}
