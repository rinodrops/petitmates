#![cfg(target_os = "windows")]
#![allow(non_snake_case)]

//! Windows window manager: enumerates on-screen windows and maps them to
//! `Surface` values using the same logic as the macOS `wm.rs`.
//!
//! ## Coordinate system
//! Windows uses a top-left origin with Y increasing downward — identical to
//! the CG coordinate system used throughout the rest of the engine. No Y-flip
//! is required when converting between `ScreenInfo`/`WinInfo` values and
//! character positions.

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::behavior::{Side, Surface};

// ---- Filter constants ----

/// Minimum width (px) a window must have to be a Petit Mates surface.
const MIN_WIN_W: f64 = 300.0;

/// Minimum height (px) a window must have to be a Petit Mates surface.
const MIN_WIN_H: f64 = 150.0;

/// Fraction of screen/usable area above which a window is treated as
/// fullscreen and excluded.
const FULLSCREEN_FRAC: f64 = 0.95;

/// Snap tolerance (px): how close a character anchor must be to a surface
/// edge to register as "on" that surface.
#[allow(dead_code)]
const SNAP: f64 = 8.0;

// ---- Types ----

/// Information about one on-screen window in Windows screen coordinates
/// (origin = top-left of primary monitor, Y increases downward).
#[derive(Debug, Clone)]
pub struct WinInfo {
    /// Raw HWND truncated to u32 (safe — Windows user-object handles are
    /// always ≤ 32 bits even in 64-bit processes).
    pub id: u32,
    /// Left edge (screen X).
    pub x: f64,
    /// Top edge (screen Y).
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl WinInfo {
    pub fn right(&self) -> f64 { self.x + self.w }
    pub fn bottom(&self) -> f64 { self.y + self.h }
}

/// Primary-monitor geometry in Windows screen coordinates.
#[derive(Debug, Clone, Copy)]
pub struct ScreenInfo {
    pub width: f64,
    pub height: f64,
    /// Height of the taskbar (bottom inset of the usable work area).
    /// If the taskbar is at the top/side this may be 0; only bottom taskbar
    /// is fully supported in this release.
    pub taskbar_height: f64,
}

impl ScreenInfo {
    /// Y coordinate of the desktop floor (top of the taskbar) in screen
    /// coordinates. The character stands at this Y level.
    pub fn floor_y(&self) -> f64 {
        self.height - self.taskbar_height
    }
}

// ---- Screen info ----

pub fn screen_info() -> ScreenInfo {
    unsafe {
        let w = GetSystemMetrics(SM_CXSCREEN) as f64;
        let h = GetSystemMetrics(SM_CYSCREEN) as f64;
        // Work area = screen minus taskbar.
        let mut wa = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut RECT as *mut _, 0);
        // Assume taskbar is at the bottom. If the call failed (wa.bottom == 0),
        // fall back to a typical 40 px taskbar height so floor_y() stays valid.
        let taskbar_height = if wa.bottom > 0 {
            (h - wa.bottom as f64).max(0.0)
        } else {
            40.0
        };
        ScreenInfo { width: w, height: h, taskbar_height }
    }
}

// ---- Window enumeration ----

struct EnumCtx {
    my_pid: u32,
    wins: Vec<WinInfo>,
    screen_w: f64,
    usable_h: f64,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lp: LPARAM) -> BOOL {
    let ctx = unsafe { &mut *(lp as *mut EnumCtx) };

    if unsafe { IsWindowVisible(hwnd) } == 0 { return TRUE; }
    if unsafe { IsIconic(hwnd) } != 0 { return TRUE; }

    // Exclude our own process.
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == ctx.my_pid { return TRUE; }

    // Filter well-known system window classes.
    let mut cls = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, cls.as_mut_ptr(), cls.len() as i32) } as usize;
    let class = String::from_utf16_lossy(&cls[..n]);
    match class.as_str() {
        // Taskbar and shell components.
        "Shell_TrayWnd"
        | "Shell_SecondaryTrayWnd"
        | "Progman"
        | "WorkerW"
        | "DV2ControlHost"
        | "TaskListThumbnailWnd"
        | "MSTaskSwWClass"
        | "SysListView32"
        // Windows 11 system UI overlays.
        | "Shell_InputSwitchTopLevelWindow"
        | "MultitaskingViewFrame"
        | "TaskListOverlayWnd"
        | "NotifyIconOverflowWindow"
        | "XamlExplorerHostIslandWindow"
        | "TopLevelWindowForOverflowXamlIsland"
        | "ForegroundStaging"
        | "NativeHWNDHost" => return TRUE,
        _ => {}
    }
    // Windows 11 UWP / WinUI system windows (class name starts with known prefixes).
    if class.starts_with("Windows.UI.") || class.starts_with("Microsoft.UI.") {
        return TRUE;
    }

    // Skip tool windows (notification popups, HUDs, etc.).
    let exstyle = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    if exstyle & WS_EX_TOOLWINDOW != 0 { return TRUE; }

    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    if unsafe { GetWindowRect(hwnd, &mut r) } == 0 { return TRUE; }
    let w = (r.right - r.left) as f64;
    let h = (r.bottom - r.top) as f64;

    if w < MIN_WIN_W || h < MIN_WIN_H { return TRUE; }

    // Exclude fullscreen / maximized windows.
    if w >= ctx.screen_w * FULLSCREEN_FRAC && h >= ctx.usable_h * FULLSCREEN_FRAC {
        return TRUE;
    }

    ctx.wins.push(WinInfo {
        id: hwnd as u32,
        x: r.left as f64,
        y: r.top as f64,
        w,
        h,
    });
    TRUE
}

/// Returns visible, non-minimised, non-system windows that are valid Petit
/// Mates surface candidates.
pub fn list_windows(si: &ScreenInfo) -> Vec<WinInfo> {
    let mut ctx = EnumCtx {
        my_pid: unsafe { GetCurrentProcessId() },
        wins: Vec::new(),
        screen_w: si.width,
        usable_h: si.floor_y(),
    };
    unsafe { EnumWindows(Some(enum_proc), &mut ctx as *mut EnumCtx as LPARAM) };
    ctx.wins
}

/// Look up a window by ID.
pub fn find_win(id: u32, wins: &[WinInfo]) -> Option<&WinInfo> {
    wins.iter().find(|w| w.id == id)
}

// ---- Surface detection ----

/// Given an anchor point in screen coordinates, return the best-matching
/// `Surface`. Priority: upper corners > window top > window walls > desktop
/// floor.
/// Used by the drag-and-drop handler (Phase 2).
#[allow(dead_code)]
pub fn find_surface_near(
    cx: f64,
    cy: f64,
    wins: &[WinInfo],
    si: &ScreenInfo,
) -> Option<Surface> {
    for win in wins {
        let on_left  = (cx - win.x).abs() < SNAP;
        let on_right = (cx - win.right()).abs() < SNAP;
        let on_top   = (cy - win.y).abs() < SNAP;
        let in_x = cx > win.x - SNAP && cx < win.right() + SNAP;
        let in_y = cy > win.y - SNAP && cy < win.bottom() + SNAP;

        if on_top && on_right {
            return Some(Surface::WindowUpperCorner { win_id: win.id, side: Side::Right });
        }
        if on_top && on_left {
            return Some(Surface::WindowUpperCorner { win_id: win.id, side: Side::Left });
        }
        if on_top && in_x {
            return Some(Surface::WindowTop { win_id: win.id, x_local: cx - win.x });
        }
        if on_right && in_y {
            return Some(Surface::WindowWall { win_id: win.id, side: Side::Right, y_local: cy - win.y });
        }
        if on_left && in_y {
            return Some(Surface::WindowWall { win_id: win.id, side: Side::Left, y_local: cy - win.y });
        }
    }

    if (cy - si.floor_y()).abs() < SNAP {
        return Some(Surface::Desktop { x: cx });
    }

    None
}

/// Returns `false` when a window-attached surface's host window has closed
/// or is no longer in the window list.
pub fn surface_still_valid(surface: &Surface, wins: &[WinInfo]) -> bool {
    match surface {
        Surface::Desktop { .. } | Surface::Airborne => true,
        Surface::WindowTop { win_id, .. }
        | Surface::WindowWall { win_id, .. }
        | Surface::WindowUpperCorner { win_id, .. } => find_win(*win_id, wins).is_some(),
    }
}
