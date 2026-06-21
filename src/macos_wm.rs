#![cfg(target_os = "macos")]

use std::ffi::CStr;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSScreen;
use objc2_foundation::{MainThreadMarker, NSArray, NSDictionary, NSNumber, NSString};

use crate::behavior::{Side, Surface};
use crate::physics::PhysicsScreen;

// Re-export shared geometry from physics so call sites can use `wm::WinInfo` etc.
pub use crate::physics::{WinInfo, find_win, surface_still_valid};

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
const OPT_INCLUDING_WINDOW: u32 = 1 << 3;
const NULL_WINDOW: u32 = 0;

// ---- Types ----

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

    /// Convert to the platform-independent view used by physics functions.
    pub fn physics_screen(&self) -> PhysicsScreen {
        PhysicsScreen { width: self.width, height: self.height, floor_y: self.floor_y() }
    }
}

// ---- Window list ----

/// Fills `buf` with on-screen windows (layer == 0) that are valid Petit Mates
/// surface candidates, applying the same filters as the original `list_windows`.
/// The buffer is cleared before use so it can be reused across ticks.
pub fn list_windows_into(buf: &mut Vec<WinInfo>, si: &ScreenInfo) {
    buf.clear();
    let my_pid = std::process::id() as i32;
    let usable_h = (si.height - si.dock_height - si.menu_bar_height).max(1.0);
    let raw =
        unsafe { CGWindowListCopyWindowInfo(OPT_ON_SCREEN | OPT_EXCL_DESKTOP, NULL_WINDOW) };
    if raw.is_null() {
        return;
    }
    let arr: Retained<NSArray<AnyObject>> =
        unsafe { Retained::from_raw(raw as *mut NSArray<AnyObject>).unwrap() };

    let k_id         = NSString::from_str("kCGWindowNumber");
    let k_pid        = NSString::from_str("kCGWindowOwnerPID");
    let k_layer      = NSString::from_str("kCGWindowLayer");
    let k_bounds     = NSString::from_str("kCGWindowBounds");
    let k_owner_name = NSString::from_str("kCGWindowOwnerName");

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

        if w < MIN_WIN_W || h < MIN_WIN_H {
            continue;
        }

        if w >= si.width * FULLSCREEN_FRAC && h >= usable_h * FULLSCREEN_FRAC {
            continue;
        }

        // Exclude windows that don't overlap the primary screen rectangle.
        // This prevents characters from jumping to windows on secondary displays.
        if x + w <= 0.0 || x >= si.width || y + h <= 0.0 || y >= si.height {
            continue;
        }

        buf.push(WinInfo { id, x, y, w, h });
    }
}

/// Returns on-screen windows (layer == 0) that are valid Petit Mates surface
/// candidates, applying the following filters:
///
/// - Own process excluded (PID check)
/// - `WindowManager`-owned windows excluded (Stage Manager UI, macOS 13+)
/// - Width < `MIN_WIN_W` or height < `MIN_WIN_H` excluded (thumbnails / tooltips)
/// - Fullscreen / maximized windows excluded (≥ `FULLSCREEN_FRAC` of screen
///   width **and** of usable height)
pub fn list_windows(si: &ScreenInfo) -> Vec<WinInfo> {
    let mut result = Vec::new();
    list_windows_into(&mut result, si);
    result
}

/// Targeted single-window lookup using `kCGWindowListOptionIncludingWindow`.
///
/// Returns the `WinInfo` for `win_id` if the window is currently on-screen,
/// or `None` if it has been closed or moved off-screen.
///
/// This call is intended for per-tick rendering updates and is cheaper than a
/// full `list_windows_into` when only one window's position needs to be
/// refreshed.
pub fn host_win_info(win_id: u32) -> Option<WinInfo> {
    let raw = unsafe {
        CGWindowListCopyWindowInfo(OPT_INCLUDING_WINDOW, win_id)
    };
    if raw.is_null() { return None; }
    let arr: Retained<NSArray<AnyObject>> =
        unsafe { Retained::from_raw(raw as *mut NSArray<AnyObject>).unwrap() };

    let k_id     = NSString::from_str("kCGWindowNumber");
    let k_layer  = NSString::from_str("kCGWindowLayer");
    let k_bounds = NSString::from_str("kCGWindowBounds");

    for i in 0..arr.count() {
        let obj: Retained<AnyObject> = arr.objectAtIndex(i);
        let dict: &NSDictionary<NSString, AnyObject> = unsafe {
            &*(Retained::as_ptr(&obj) as *const NSDictionary<NSString, AnyObject>)
        };

        let layer = dict.objectForKey(&k_layer)
            .and_then(|v| num_i32(&v))
            .unwrap_or(-1);
        if layer != 0 { continue; }

        let id = match dict.objectForKey(&k_id).and_then(|v| num_i32(&v)) {
            Some(v) if v >= 0 => v as u32,
            _ => continue,
        };
        if id != win_id { continue; }

        let bobj = dict.objectForKey(&k_bounds)?;
        let bd: &NSDictionary<NSString, AnyObject> = unsafe {
            &*(Retained::as_ptr(&bobj) as *const NSDictionary<NSString, AnyObject>)
        };
        if let (Some(x), Some(y), Some(w), Some(h)) = (
            dict_f64(bd, "X"), dict_f64(bd, "Y"),
            dict_f64(bd, "Width"), dict_f64(bd, "Height"),
        ) {
            return Some(WinInfo { id, x, y, w, h });
        }
    }
    None
}

// ---- Screen info ----

/// Query screen dimensions and Dock height from the primary NSScreen.
///
/// Uses `NSScreen::screens()[0]` (the screen that always carries the menu bar)
/// rather than `NSScreen::mainScreen()`, which tracks the key-window's screen
/// and changes when the user activates a window on a secondary display.
///
/// NSScreen uses bottom-left origin; `visibleFrame.origin.y` equals the Dock
/// height when the Dock is positioned at the bottom.
pub fn screen_info(mt: MainThreadMarker) -> Option<ScreenInfo> {
    let screen = NSScreen::screens(mt).firstObject()?;
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

/// Wall snap tolerance for ⌘+drag placement (display px).
/// Much wider than `SNAP` so the user can drop near a wall edge without
/// pixel-precise aim.
const WALL_DROP_SNAP: f64 = 60.0;

/// Like `find_surface_near` but with a wider wall-proximity threshold.
///
/// Used at ⌘+drag release (and for hover preview during drag).
/// Falls back to the exact `find_surface_near` first so that corners
/// and top edges retain their original priority.
pub fn find_drop_surface(
    foot_x: f64,
    foot_y: f64,
    wins: &[WinInfo],
    si: &ScreenInfo,
) -> Option<Surface> {
    // Exact snap: corners, top edges, 8 px wall edges, desktop floor.
    if let Some(s) = find_surface_near(foot_x, foot_y, wins, si) {
        return Some(s);
    }
    // Wider wall proximity: pick the nearest wall edge within WALL_DROP_SNAP.
    let mut best: Option<(f64, Surface)> = None;
    for win in wins {
        let in_y = foot_y >= win.y && foot_y <= win.bottom();
        if !in_y {
            continue;
        }
        let dist_r = (foot_x - win.right()).abs();
        let dist_l = (foot_x - win.x).abs();
        if dist_r < WALL_DROP_SNAP && best.as_ref().map_or(true, |(d, _)| dist_r < *d) {
            best = Some((dist_r, Surface::WindowWall {
                win_id: win.id,
                side: Side::Right,
                y_local: foot_y - win.y,
            }));
        }
        if dist_l < WALL_DROP_SNAP && best.as_ref().map_or(true, |(d, _)| dist_l < *d) {
            best = Some((dist_l, Surface::WindowWall {
                win_id: win.id,
                side: Side::Left,
                y_local: foot_y - win.y,
            }));
        }
    }
    best.map(|(_, s)| s)
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
