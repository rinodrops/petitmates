/// Detect whether the system is currently running on AC (wall) power.
///
/// Returns `true` on AC power or when the power source cannot be determined
/// (fail-safe: prefer performance over saving).
///
/// # macOS
/// Call [`register_power_notification`] once at startup to receive OS-level
/// callbacks when the power source changes; re-query this function inside that
/// callback to get the new state.

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

/// Register a callback that fires on the main run loop whenever the power
/// source changes (AC plugged/unplugged). Call once at app startup.
///
/// The source is added to `kCFRunLoopCommonModes` and intentionally kept
/// alive for the process lifetime (CFRunLoop retains it after `CFRelease`).
#[cfg(target_os = "macos")]
pub fn register_power_notification(
    callback: unsafe extern "C" fn(*mut std::ffi::c_void),
) {
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPSNotificationCreateRunLoopSource(
            callback: unsafe extern "C" fn(*mut std::ffi::c_void),
            context:  *mut std::ffi::c_void,
        ) -> *mut std::ffi::c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRunLoopGetMain() -> *mut std::ffi::c_void;
        fn CFRunLoopAddSource(
            rl:     *mut std::ffi::c_void,
            source: *mut std::ffi::c_void,
            mode:   *const std::ffi::c_void,
        );
        fn CFRelease(cf: *mut std::ffi::c_void);
        // Use the CoreFoundation constant directly to avoid ambiguity with
        // the objc2_foundation re-export of NSRunLoopCommonModes.
        static kCFRunLoopCommonModes: *const std::ffi::c_void;
    }

    unsafe {
        let source = IOPSNotificationCreateRunLoopSource(callback, std::ptr::null_mut());
        if source.is_null() {
            eprintln!("[petitmates] IOPSNotificationCreateRunLoopSource returned null");
            return;
        }
        CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopCommonModes);
        // CFRunLoop retains the source; release our own reference.
        CFRelease(source);
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
