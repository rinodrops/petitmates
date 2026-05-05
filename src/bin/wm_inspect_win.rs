//! Windows diagnostic tool: lists all top-level windows and marks which ones
//! would be accepted as Petit Mates surfaces by the current `list_windows()`.
//!
//! Build:
//!   CARGO_TARGET_DIR=/tmp/pm-win cargo build --bin wm_inspect_win \
//!       --target x86_64-pc-windows-gnu
//!   make win-tools  (after adding to Makefile)
//!
//! Run on Windows: wm_inspect_win.exe  > windows.txt
//!   (or double-click; output goes to console)

#![cfg(target_os = "windows")]
#![allow(non_snake_case)]

use std::mem;
use std::ptr;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

fn main() {
    unsafe {
        let screen_w = GetSystemMetrics(SM_CXSCREEN) as f64;
        let screen_h = GetSystemMetrics(SM_CYSCREEN) as f64;
        let mut wa = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut RECT as *mut _, 0);
        let taskbar_h = if wa.bottom > 0 { screen_h - wa.bottom as f64 } else { 40.0 };
        let floor_y = screen_h - taskbar_h;

        println!("Screen: {screen_w}x{screen_h}  floor_y={floor_y}");
        println!();
        println!("{:<6} {:<40} {:<30} {:>6} {:>6} {:>6} {:>6}  ex_style   ACCEPT?",
            "PID", "Class", "Title", "x", "y", "w", "h");
        println!("{}", "-".repeat(130));

        let my_pid = GetCurrentProcessId();

        let mut ctx = InspCtx {
            my_pid,
            screen_w,
            floor_y,
            results: Vec::new(),
        };
        EnumWindows(Some(enum_all), &mut ctx as *mut InspCtx as LPARAM);

        for row in &ctx.results {
            println!("{}", row);
        }
    }
}

struct InspCtx {
    my_pid: u32,
    screen_w: f64,
    floor_y: f64,
    results: Vec<String>,
}

unsafe extern "system" fn enum_all(hwnd: HWND, lp: LPARAM) -> BOOL {
    let ctx = unsafe { &mut *(lp as *mut InspCtx) };

    // --- gather info ---
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };

    let mut cls = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, cls.as_mut_ptr(), cls.len() as i32) } as usize;
    let class = String::from_utf16_lossy(&cls[..n]);

    let mut title_buf = [0u16; 256];
    let tn = unsafe { GetWindowTextW(hwnd, title_buf.as_mut_ptr(), title_buf.len() as i32) } as usize;
    let title = String::from_utf16_lossy(&title_buf[..tn]);

    let visible  = unsafe { IsWindowVisible(hwnd) } != 0;
    let iconic   = unsafe { IsIconic(hwnd) } != 0;
    let exstyle  = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    let mut r    = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    unsafe { GetWindowRect(hwnd, &mut r) };
    let wx = r.left as f64;
    let wy = r.top as f64;
    let ww = (r.right - r.left) as f64;
    let wh = (r.bottom - r.top) as f64;

    // --- acceptance logic (must match windows_wm.rs exactly) ---
    let mut reject_reasons: Vec<&'static str> = Vec::new();

    if !visible        { reject_reasons.push("invisible"); }
    if iconic          { reject_reasons.push("iconic"); }
    if pid == ctx.my_pid { reject_reasons.push("own-pid"); }

    let class_blocked = matches!(class.as_str(),
        "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Progman" | "WorkerW"
        | "DV2ControlHost" | "TaskListThumbnailWnd" | "MSTaskSwWClass" | "SysListView32"
        | "Shell_InputSwitchTopLevelWindow" | "MultitaskingViewFrame"
        | "TaskListOverlayWnd" | "NotifyIconOverflowWindow"
        | "XamlExplorerHostIslandWindow" | "TopLevelWindowForOverflowXamlIsland"
        | "ForegroundStaging" | "NativeHWNDHost"
    ) || class.starts_with("Windows.UI.") || class.starts_with("Microsoft.UI.");
    if class_blocked { reject_reasons.push("class-blocked"); }

    if exstyle & WS_EX_TOOLWINDOW != 0 { reject_reasons.push("TOOLWINDOW"); }
    if ww < 300.0 { reject_reasons.push("w<300"); }
    if wh < 150.0 { reject_reasons.push("h<150"); }
    if ww >= ctx.screen_w * 0.95 && wh >= ctx.floor_y * 0.95 { reject_reasons.push("fullscreen"); }

    let accepted = reject_reasons.is_empty();
    let reason   = if accepted { String::new() } else { reject_reasons.join(",") };

    // Print all windows (accepted + rejected), but highlight accepted ones.
    let marker = if accepted { "*** YES" } else { &format!("no  ({})", reason) };
    let exstyle_hex = format!("{exstyle:#010x}");

    ctx.results.push(format!(
        "{:<6} {:<40} {:<30} {:>6.0} {:>6.0} {:>6.0} {:>6.0}  {exstyle_hex}  {marker}",
        pid,
        if class.len() > 40 { &class[..40] } else { &class },
        if title.len() > 30 { &title[..30] } else { &title },
        wx, wy, ww, wh,
    ));

    TRUE
}
