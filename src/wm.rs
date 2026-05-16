#![cfg(target_os = "macos")]

use std::ffi::CStr;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSScreen;
use objc2_foundation::{MainThreadMarker, NSArray, NSDictionary, NSNumber, NSString};

use crate::behavior::{Side, Surface};

// ---- Window list filter constants ----

/// Minimum width (points) a window must have to be a Petit Mates surface.
/// Excludes Stage Manager thumbnails (~141 px) and tooltip-sized windows.
const MIN_WIN_W: f64 = 300.0;

/// Minimum height (points) a window must have to be a Petit Mates surface.
const MIN_WIN_H: f64 = 150.0;

/// Fraction of screen width / usable height at or above which a window is
/// considered fullscreen / maximized and is excluded from surface candidates.
const FULLSCREEN_FRAC: f64 = 0.95;

// ---- CoreGraphics FFI ----

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relativeToWindow: u32) -> *mut AnyObject;
}

const OPT_ON_SCREEN: u32 = 1 << 0;
const OPT_EXCL_DESKTOP: u32 = 1 << 4;
const NULL_WINDOW: u32 = 0;

// ---- Types ----

/// Information about one on-screen window in CG coordinates
/// (origin = screen top-left, Y increases downward).
#[derive(Debug, Clone)]
pub struct WinInfo {
    /// kCGWindowNumber
    pub id: u32,
    /// Left edge
    pub x: f64,
    /// Top edge
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl WinInfo {
    pub fn right(&self) -> f64 { self.x + self.w }
    pub fn bottom(&self) -> f64 { self.y + self.h }
}

/// Primary screen geometry (CG coordinate space).
#[derive(Debug, Clone, Copy)]
pub struct ScreenInfo {
    pub width: f64,
    /// Total screen height.
    pub height: f64,
    /// Dock height (bottom inset of the usable desktop area).
    pub dock_height: f64,
    /// Menu bar height (top inset; NSPanel cannot go above this in CG coords).
    pub menu_bar_height: f64,
}

impl ScreenInfo {
    /// Y coordinate of the desktop floor in CG space.
    pub fn floor_y(&self) -> f64 {
        self.height - self.dock_height
    }
}

// ---- Window list ----

/// Returns on-screen windows (layer == 0) that are valid Petit Mates surface
/// candidates, applying the following filters:
///
/// - Own process excluded (PID check)
/// - `WindowManager`-owned windows excluded (Stage Manager UI, macOS 13+)
/// - Width < `MIN_WIN_W` or height < `MIN_WIN_H` excluded (thumbnails / tooltips)
/// - Fullscreen / maximized windows excluded (≥ `FULLSCREEN_FRAC` of screen
///   width **and** of usable height)
pub fn list_windows(si: &ScreenInfo) -> Vec<WinInfo> {
    let my_pid = std::process::id() as i32;
    let usable_h = (si.height - si.dock_height - si.menu_bar_height).max(1.0);
    let raw =
        unsafe { CGWindowListCopyWindowInfo(OPT_ON_SCREEN | OPT_EXCL_DESKTOP, NULL_WINDOW) };
    if raw.is_null() {
        return Vec::new();
    }
    let arr: Retained<NSArray<AnyObject>> =
        unsafe { Retained::from_raw(raw as *mut NSArray<AnyObject>).unwrap() };

    let k_id         = NSString::from_str("kCGWindowNumber");
    let k_pid        = NSString::from_str("kCGWindowOwnerPID");
    let k_layer      = NSString::from_str("kCGWindowLayer");
    let k_bounds     = NSString::from_str("kCGWindowBounds");
    let k_owner_name = NSString::from_str("kCGWindowOwnerName");

    let mut result = Vec::new();
    let n = arr.count();

    for i in 0..n {
        let obj: Retained<AnyObject> = arr.objectAtIndex(i);
        let dict: &NSDictionary<NSString, AnyObject> = unsafe {
            &*(Retained::as_ptr(&obj) as *const NSDictionary<NSString, AnyObject>)
        };

        let pid = dict.objectForKey(&k_pid)
            .and_then(|v| num_i32(&v))
            .unwrap_or(-1);
        if pid == my_pid {
            continue;
        }

        let layer = dict.objectForKey(&k_layer)
            .and_then(|v| num_i32(&v))
            .unwrap_or(-1);
        if layer != 0 {
            continue;
        }

        // Exclude Stage Manager UI (macOS 13+). WindowManager owns the Recent
        // Apps strip, group containers, and other Stage Manager chrome.
        let owner = dict_str(dict, &k_owner_name).unwrap_or_default();
        if owner == "WindowManager" {
            continue;
        }

        let id = match dict.objectForKey(&k_id).and_then(|v| num_i32(&v)) {
            Some(v) if v >= 0 => v as u32,
            _ => continue,
        };

        let bobj = match dict.objectForKey(&k_bounds) {
            Some(o) => o,
            None => continue,
        };
        let bd: &NSDictionary<NSString, AnyObject> = unsafe {
            &*(Retained::as_ptr(&bobj) as *const NSDictionary<NSString, AnyObject>)
        };

        let (x, y, w, h) = match (
            dict_f64(bd, "X"),
            dict_f64(bd, "Y"),
            dict_f64(bd, "Width"),
            dict_f64(bd, "Height"),
        ) {
            (Some(x), Some(y), Some(w), Some(h)) => (x, y, w, h),
            _ => continue,
        };

        // Exclude tiny windows: tooltips, HUD shadows, and Stage Manager
        // thumbnails (Stage Manager compresses app windows to ~141×145 px).
        if w < MIN_WIN_W || h < MIN_WIN_H {
            continue;
        }

        // Exclude fullscreen / maximized windows.  A window that covers ≥ 90%
        // of the screen width AND ≥ 90% of the usable height fills the display;
        // the character cannot meaningfully sit on its edges.
        if w >= si.width * FULLSCREEN_FRAC && h >= usable_h * FULLSCREEN_FRAC {
            continue;
        }

        result.push(WinInfo { id, x, y, w, h });
    }

    result
}

/// Look up a window by its CGWindowID.
pub fn find_win(id: u32, wins: &[WinInfo]) -> Option<&WinInfo> {
    wins.iter().find(|w| w.id == id)
}

// ---- Screen info ----

/// Query screen dimensions and Dock height from the main NSScreen.
///
/// NSScreen uses bottom-left origin; `visibleFrame.origin.y` equals the Dock
/// height when the Dock is positioned at the bottom.
pub fn screen_info(mt: MainThreadMarker) -> Option<ScreenInfo> {
    let screen = NSScreen::mainScreen(mt)?;
    let frame = screen.frame();
    let visible = screen.visibleFrame();
    let height = frame.size.height;
    let width = frame.size.width;
    // When the Dock is at the bottom, visible.origin.y == dock height.
    let dock_height = visible.origin.y.max(0.0);
    // Menu bar height = total height - dock - visible height.
    let menu_bar_height = (height - dock_height - visible.size.height).max(0.0);
    Some(ScreenInfo { width, height, dock_height, menu_bar_height })
}

/// Screen height in NS points — usable without a `MainThreadMarker` token
/// because it is only called from the main thread (event monitor callbacks).
///
/// # Safety
/// Must be called on the main thread.
pub fn screen_info_raw() -> f64 {
    unsafe {
        let mt = MainThreadMarker::new_unchecked();
        NSScreen::mainScreen(mt)
            .map(|s| s.frame().size.height)
            .unwrap_or(800.0)
    }
}

/// Full `ScreenInfo` without a `MainThreadMarker` token.
///
/// # Safety
/// Must be called on the main thread.
pub fn screen_info_raw_full() -> ScreenInfo {
    unsafe {
        let mt = MainThreadMarker::new_unchecked();
        screen_info(mt).unwrap_or(ScreenInfo { width: 1280.0, height: 800.0, dock_height: 0.0, menu_bar_height: 24.0 })
    }
}

// ---- Surface detection ----

/// Snap tolerance (display px): how close the character's anchor must be
/// to a surface edge to register as "on" that surface.
const SNAP: f64 = 8.0;

/// Given a character anchor point in CG coordinates, return the best-matching
/// `Surface` from the visible window list plus the desktop floor.
///
/// Priority: corners > window top > window walls > desktop floor.
pub fn find_surface_near(
    char_x: f64,
    char_y: f64,
    wins: &[WinInfo],
    si: &ScreenInfo,
) -> Option<Surface> {
    for win in wins {
        let on_left = (char_x - win.x).abs() < SNAP;
        let on_right = (char_x - win.right()).abs() < SNAP;
        let on_top = (char_y - win.y).abs() < SNAP;
        let in_x = char_x > win.x - SNAP && char_x < win.right() + SNAP;
        let in_y = char_y > win.y - SNAP && char_y < win.bottom() + SNAP;

        // Upper corners (checked before top/wall to avoid ambiguity)
        if on_top && on_right {
            return Some(Surface::WindowUpperCorner { win_id: win.id, side: Side::Right });
        }
        if on_top && on_left {
            return Some(Surface::WindowUpperCorner { win_id: win.id, side: Side::Left });
        }
        // Top edge
        if on_top && in_x {
            return Some(Surface::WindowTop { win_id: win.id, x_local: char_x - win.x });
        }
        // Side walls
        if on_right && in_y {
            return Some(Surface::WindowWall {
                win_id: win.id,
                side: Side::Right,
                y_local: char_y - win.y,
            });
        }
        if on_left && in_y {
            return Some(Surface::WindowWall {
                win_id: win.id,
                side: Side::Left,
                y_local: char_y - win.y,
            });
        }
    }

    // Desktop floor
    if (char_y - si.floor_y()).abs() < SNAP {
        return Some(Surface::Desktop { x: char_x });
    }

    None
}

/// Check whether a window-attached `Surface` is still valid given the current
/// window list. Returns `false` if the window has been closed or moved away.
pub fn surface_still_valid(surface: &Surface, wins: &[WinInfo]) -> bool {
    match surface {
        Surface::Desktop { .. } | Surface::Airborne => true,
        Surface::WindowTop { win_id, .. }
        | Surface::WindowWall { win_id, .. }
        | Surface::WindowUpperCorner { win_id, .. }
        | Surface::WindowBottom { win_id, .. } => find_win(*win_id, wins).is_some(),
    }
}

// ---- Helpers ----

fn num_i32(obj: &AnyObject) -> Option<i32> {
    let n: &NSNumber = obj.downcast_ref()?;
    Some(n.intValue())
}

fn dict_f64(d: &NSDictionary<NSString, AnyObject>, key: &str) -> Option<f64> {
    let k = NSString::from_str(key);
    let v: Retained<AnyObject> = d.objectForKey(&k)?;
    let n: &NSNumber = v.downcast_ref()?;
    Some(n.doubleValue())
}

/// Read an NSString value from a dictionary as a Rust `String`.
/// Returns `None` if the key is absent or the value is not an NSString.
fn dict_str(d: &NSDictionary<NSString, AnyObject>, key: &NSString) -> Option<String> {
    unsafe {
        let v: Retained<AnyObject> = d.objectForKey(key)?;
        let ptr: *const std::ffi::c_char = objc2::msg_send![&*v, UTF8String];
        if ptr.is_null() { return None; }
        Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
    }
}
