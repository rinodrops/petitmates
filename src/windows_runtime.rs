#![cfg(target_os = "windows")]
#![allow(non_snake_case)]

//! Windows runtime: transparent layered window, 10 Hz tick loop (WM_TIMER),
//! full state-machine integration, and system-tray icon.
//!
//! Phase 1: full behavior state machine, no ⌘+drag (planned for Phase 2).
//!
//! ## Coordinate system
//! All positions use Windows screen coordinates (top-left origin, Y down),
//! which are identical to CG coordinates used throughout the engine.  No
//! Y-flip is needed; `surface_to_screen_pos` converts surface-local coords
//! to screen top-left directly.

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem;
use std::ptr;
use std::rc::Rc;
use std::time::Instant;

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::behavior::{BehaviorContext, BehaviorScript, Dir, LandingMode, Side, State, Surface, SurfaceEdge, Transition};
use crate::config::{make_shared_win_for, SharedConfig};
use crate::engine::{advance_anim, vertical_offset};
use crate::manifest;
use crate::physics;
use crate::rust_behavior::RustBehavior;
use crate::sprite_map::{sprite_for_state, sprite_for_turn};
use crate::windows_assets::{self, Anchor, SpriteAssets};
use crate::windows_wm::{self, ScreenInfo, WinInfo};

// ---- Constants ----

const WM_TRAY: u32 = WM_APP + 1;
const IDM_ABOUT: usize = 1;
const IDM_EXIT: usize = 2;
const IDM_ADD_BD: usize = 3;
const IDM_REMOVE_CHAR: usize = 4;
const IDM_ADD_PT: usize = 5;
const IDM_SETTINGS: usize = 6;
const IDM_ADD_LG: usize = 7;
const TIMER_TICK: usize = 1;
/// Base command ID for debug trigger menu items (reserves 100–199).
const IDM_DEBUG_BASE: usize = 100;
/// Command ID for the debug "Remove This Character" menu item.
const IDM_DEBUG_REMOVE: usize = 200;
/// Custom window message: deferred character removal (wp = char index).
/// Posted to a SURVIVING character's hwnd so the destruction happens outside
/// any TrackPopupMenu call stack, avoiding re-entrancy issues.
const WM_APP_REMOVE_CHAR: u32 = WM_APP + 2;

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---- Theme detection (for tray icon colour) ----

fn is_dark_mode() -> bool {
    unsafe {
        let subkey = to_wide(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
        );
        let value = to_wide("SystemUsesLightTheme");
        let mut hkey: HKEY = ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
            return false;
        }
        let mut data: u32 = 1;
        let mut size = mem::size_of::<u32>() as u32;
        RegQueryValueExW(
            hkey,
            value.as_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut data as *mut u32 as *mut u8,
            &mut size,
        );
        RegCloseKey(hkey);
        data == 0 // 0 = dark mode → use white tray icon
    }
}

// ---- App state ----

// ---- Window-list tiered cache ----

/// Returns the win_id (HWND as u32) of the window this surface is anchored to.
fn surface_host_win_id(surface: &Surface) -> Option<u32> {
    match surface {
        Surface::WindowTop { win_id, .. }
        | Surface::WindowWall { win_id, .. }
        | Surface::WindowUpperCorner { win_id, .. }
        | Surface::WindowBottom { win_id, .. } => Some(*win_id),
        _ => None,
    }
}

/// Reusable window-list buffer with a countdown-based refresh schedule.
/// See the equivalent struct in `macos.rs` for the full interval rationale.
struct WinListCache {
    wins: Vec<WinInfo>,
    ticks_until_refresh: u32,
}

impl WinListCache {
    fn new() -> Self { Self { wins: Vec::new(), ticks_until_refresh: 0 } }
}

const WIN_CACHE_IMMEDIATE: u32 = 1;
const WIN_CACHE_HIGH_FREQ: u32 = 15;
const WIN_CACHE_LOW_FREQ: u32 = 150;

fn next_refresh_interval(chars: &[CharState], wins: &[WinInfo], attract_dist: f64) -> u32 {
    use crate::behavior::State;
    for ch in chars {
        if matches!(&ch.anim_state, State::Falling { .. } | State::JumpRunup { .. }) {
            return WIN_CACHE_IMMEDIATE;
        }
        if surface_host_win_id(&ch.surface).is_some() {
            return WIN_CACHE_HIGH_FREQ;
        }
    }
    for ch in chars {
        if let Surface::Desktop { x } = &ch.surface {
            for win in wins {
                let dist_r = win.x - x;
                let dist_l = x - win.right();
                if (dist_r >= 0.0 && dist_r < attract_dist)
                    || (dist_l >= 0.0 && dist_l < attract_dist)
                {
                    return WIN_CACHE_HIGH_FREQ;
                }
            }
        }
    }
    WIN_CACHE_LOW_FREQ
}

struct CharState {
    hwnd: HWND,
    assets: Rc<SpriteAssets>,
    config: SharedConfig,
    /// Cached effective config: `config.current` with personality applied.
    /// Recomputed only when `params.toml` or `behavior.toml` change is detected
    /// (checked on the window-list refresh boundary, not every tick).
    effective_config: crate::config::Config,
    behavior: Box<dyn BehaviorScript>,
    anim_state: State,
    facing: Dir,
    surface: Surface,
    /// Character position in screen coordinates (top-left of sprite bounding box).
    char_pos: (f64, f64),
    last_tick: Instant,
    visible: bool,
    /// Cursor offset from sprite top-left at drag start (screen coords).
    drag_offset: Option<(f64, f64)>,
    /// Last rendered sprite top-left in screen coords.
    last_screen_pos: (i32, i32),
    /// Pending debug forced transition: (target_state, remaining_countdown_secs).
    debug_trigger: Option<(State, f64)>,
    speech_engine: crate::speech::SpeechEngine,
    behavior_engine: crate::anim_trigger::BehaviorEngine,
    /// Active speech bubble state; None when no bubble is shown.
    bubble_state: Option<crate::speech::BubbleState>,
    /// HWND for the speech bubble layered window; null when not created yet.
    bubble_hwnd: HWND,
    /// Width of the sprite rendered last tick (scaled display pixels).
    /// Used by the resting-overlap nudge to compute the correct at_edge buffer.
    last_sprite_w: f64,
}

struct AppState {
    chars: Vec<CharState>,
    bd_assets: Rc<SpriteAssets>,
    pt_assets: Rc<SpriteAssets>,
    lg_assets: Rc<SpriteAssets>,
    bd_config: SharedConfig,
    pt_config: SharedConfig,
    lg_config: SharedConfig,
    /// Character index whose debug menu is currently being shown.
    debug_menu_char: usize,
    /// Target states stored between menu construction and WM_COMMAND dispatch.
    debug_menu_targets: Vec<State>,
    /// Global speech lock countdown (seconds). Prevents overlapping speech.
    speech_lock_remaining: f64,
    speech_cfg: crate::user_config::SpeechConfig,
    speech_tick: Instant,
    /// Font size for speech bubbles (from user.toml).
    font_size: i32,
    /// Character display size in logical pixels (from user.toml `sprite_size`).
    /// Single source of truth for physics clamping and collision spacing.
    sprite_size: f64,
    /// Resolved display language: "ja" or "en".
    lang: String,
    /// Shared weather cache updated by the background weather thread.
    weather: crate::weather::WeatherHandle,
    /// Weather configuration from user.toml (city/coordinates for menu display).
    weather_cfg: crate::user_config::WeatherConfig,
    /// Reusable window-list cache with tiered refresh schedule.
    win_cache: WinListCache,
}

thread_local! {
    static APP: RefCell<Option<AppState>> = RefCell::new(None);
}

// ---- Layered window rendering ----

/// Upload `bgra` (pre-multiplied BGRA) to a DIB and call `UpdateLayeredWindow`.
/// `x`, `y`: screen-space top-left of the window after this call.
/// `alpha`: `SourceConstantAlpha` (0 = transparent, 255 = opaque).
unsafe fn set_layered_content(
    hwnd: HWND,
    bgra: &[u8],
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    alpha: u8,
) {
    unsafe {
        let hdc_screen = GetDC(ptr::null_mut());
        let hdc_mem    = CreateCompatibleDC(hdc_screen);

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize:          mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth:         width,
                biHeight:        -height, // top-down
                biPlanes:        1,
                biBitCount:      32,
                biCompression:   BI_RGB,
                biSizeImage:     0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed:       0,
                biClrImportant:  0,
            },
            bmiColors: [RGBQUAD { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbReserved: 0 }],
        };

        let mut bits: *mut c_void = ptr::null_mut();
        let hbmp = CreateDIBSection(hdc_mem, &bmi, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
        ptr::copy_nonoverlapping(bgra.as_ptr(), bits as *mut u8, bgra.len());

        let old   = SelectObject(hdc_mem, hbmp);
        let pt_dst = POINT { x, y };
        let size   = SIZE  { cx: width, cy: height };
        let pt_src = POINT { x: 0, y: 0 };
        let blend  = BLENDFUNCTION {
            BlendOp:             AC_SRC_OVER as u8,
            BlendFlags:          0,
            SourceConstantAlpha: alpha,
            AlphaFormat:         AC_SRC_ALPHA as u8,
        };
        UpdateLayeredWindow(hwnd, hdc_screen, &pt_dst, &size, hdc_mem, &pt_src, 0, &blend, ULW_ALPHA);

        SelectObject(hdc_mem, old);
        DeleteObject(hbmp);
        DeleteDC(hdc_mem);
        ReleaseDC(ptr::null_mut(), hdc_screen);
    }
}

// ---- Speech bubble rendering (Windows GDI) ----

const WIN_BUBBLE_PADDING: i32 = 12;
const WIN_BUBBLE_CORNER:  i32 = 10; // rounded rect ellipse diameter
const WIN_BUBBLE_TAIL_H:  i32 = 10;
const WIN_BUBBLE_TAIL_W:  i32 = 14;
const WIN_BUBBLE_MARGIN:  i32 = 8;
const WIN_BUBBLE_MAX_W:   i32 = 240;
const WIN_BUBBLE_MIN_W:   i32 = 60;

/// Render a speech bubble into a BGRA pixel buffer using GDI.
///
/// Returns `(Vec<u8>, width, height)`.  Pixels outside the bubble shape are
/// transparent (`alpha = 0`); pixels inside are fully opaque (`alpha = 255`).
///
/// `tail_at_bottom` — tail points down (bubble above character).
unsafe fn render_bubble_bgra(
    text: &str,
    tail_at_bottom: bool,
    font_size: i32,
) -> (Vec<u8>, i32, i32) {
    unsafe {
    let hdc_screen = GetDC(ptr::null_mut());
    let hdc_mem    = CreateCompatibleDC(hdc_screen);

    // ---- Create font ----
    // Negative height = font size in points (logical height).
    let hfont = CreateFontW(
        -font_size,    // height (negative = pt size)
        0, 0, 0,
        FW_NORMAL as i32,
        FALSE as u32, FALSE as u32, FALSE as u32,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        (DEFAULT_PITCH | FF_DONTCARE) as u32,
        to_wide("Segoe UI").as_ptr(),
    );
    let old_font = SelectObject(hdc_mem, hfont);

    // ---- Measure text ----
    let text_wide   = to_wide(text);
    let max_text_w  = WIN_BUBBLE_MAX_W - WIN_BUBBLE_PADDING * 2;
    let mut measure = RECT { left: 0, top: 0, right: max_text_w, bottom: 2000 };
    DrawTextW(
        hdc_mem, text_wide.as_ptr(), -1,
        &mut measure,
        DT_WORDBREAK | DT_CALCRECT,
    );
    let text_w = measure.right  - measure.left;
    let text_h = measure.bottom - measure.top;

    // ---- Layout ----
    let bubble_w = (text_w + WIN_BUBBLE_PADDING * 2).max(WIN_BUBBLE_MIN_W);
    let bubble_h = text_h + WIN_BUBBLE_PADDING * 2;
    let total_h  = bubble_h + WIN_BUBBLE_TAIL_H;
    let img_w    = bubble_w;
    let img_h    = total_h;

    // body_top_y in GDI coords (Y-down from top of image)
    let body_top_y = if tail_at_bottom { 0 } else { WIN_BUBBLE_TAIL_H };

    // ---- Create DIB section ----
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize:          mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth:         img_w,
            biHeight:        -img_h, // top-down
            biPlanes:        1,
            biBitCount:      32,
            biCompression:   BI_RGB,
            biSizeImage:     0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed:       0,
            biClrImportant:  0,
        },
        bmiColors: [RGBQUAD { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbReserved: 0 }],
    };
    let mut bits: *mut c_void = ptr::null_mut();
    let hbmp = CreateDIBSection(
        hdc_mem, &bmi, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0,
    );
    let old_bmp = SelectObject(hdc_mem, hbmp);

    // ---- Draw bubble using combined GDI region (no arc rounding artifacts) ----
    // GDI arcs are always aliased: at small radii even AngleArc looks jagged.
    // Instead use CreateRoundRectRgn (body) + CreatePolygonRgn (tail) combined
    // with CombineRgn(RGN_OR).  FillRgn + FrameRgn then trace only the outer
    // boundary, so there is no seam line at the tail junction.
    let cx = bubble_w / 2;

    // Body region (rounded rect).
    let body_rgn = if tail_at_bottom {
        CreateRoundRectRgn(0, 0, bubble_w, bubble_h,
                           WIN_BUBBLE_CORNER, WIN_BUBBLE_CORNER)
    } else {
        CreateRoundRectRgn(0, WIN_BUBBLE_TAIL_H, bubble_w, total_h,
                           WIN_BUBBLE_CORNER, WIN_BUBBLE_CORNER)
    };

    // Tail triangle — base overlaps body by 2 px so CombineRgn(RGN_OR) merges
    // without a pixel gap.
    let tail_pts: [POINT; 3] = if tail_at_bottom {
        [
            POINT { x: cx - WIN_BUBBLE_TAIL_W / 2, y: bubble_h - 2 },
            POINT { x: cx + WIN_BUBBLE_TAIL_W / 2, y: bubble_h - 2 },
            POINT { x: cx,                          y: total_h      },
        ]
    } else {
        [
            POINT { x: cx - WIN_BUBBLE_TAIL_W / 2, y: WIN_BUBBLE_TAIL_H + 2 },
            POINT { x: cx + WIN_BUBBLE_TAIL_W / 2, y: WIN_BUBBLE_TAIL_H + 2 },
            POINT { x: cx,                          y: 0                     },
        ]
    };
    let tail_rgn = CreatePolygonRgn(tail_pts.as_ptr(), 3, 2 /* WINDING */);

    let combined_rgn = CreateRectRgn(0, 0, 1, 1);
    CombineRgn(combined_rgn, body_rgn, tail_rgn, 3 /* RGN_OR */);

    let fill_brush   = CreateSolidBrush(0x00FFFFFF_u32);
    let border_brush = CreateSolidBrush(0x00B3B3B3_u32);
    FillRgn(hdc_mem, combined_rgn, fill_brush);
    FrameRgn(hdc_mem, combined_rgn, border_brush, 1, 1);
    DeleteObject(combined_rgn);
    DeleteObject(body_rgn);
    DeleteObject(tail_rgn);

    // ---- Draw text ----
    let dark_text_color = 0x00333333u32;
    SetTextColor(hdc_mem, dark_text_color);
    SetBkMode(hdc_mem, TRANSPARENT as i32);
    SelectObject(hdc_mem, hfont); // ensure font is set

    let text_x = (bubble_w - text_w) / 2;
    let text_y = body_top_y + (bubble_h - text_h) / 2;
    let mut text_rect = RECT {
        left:   text_x,
        top:    text_y,
        right:  text_x + text_w + 1,
        bottom: text_y + text_h + 1,
    };
    DrawTextW(hdc_mem, text_wide.as_ptr(), -1, &mut text_rect, DT_WORDBREAK);

    // ---- Read pixels and fix alpha ----
    GdiFlush();
    let pixel_count = (img_w * img_h) as usize;
    let mut bgra = vec![0u8; pixel_count * 4];
    ptr::copy_nonoverlapping(bits as *const u8, bgra.as_mut_ptr(), bgra.len());

    // GDI doesn't write alpha (A=0). Set A=255 for all drawn (non-black) pixels.
    for chunk in bgra.chunks_mut(4) {
        if chunk[0] != 0 || chunk[1] != 0 || chunk[2] != 0 {
            chunk[3] = 255;
        }
    }

    // ---- Cleanup ----
    SelectObject(hdc_mem, old_font);
    SelectObject(hdc_mem, old_bmp);
    DeleteObject(hbmp);
    DeleteObject(fill_brush);
    DeleteObject(border_brush);
    DeleteObject(hfont as *mut _);
    DeleteDC(hdc_mem);
    ReleaseDC(ptr::null_mut(), hdc_screen);

    (bgra, img_w, img_h)
    } // unsafe
}

/// Create the speech-bubble HWND (called once per character).
unsafe fn create_bubble_hwnd(hinstance: HINSTANCE, char_hwnd: HWND) -> HWND {
    let class_name = to_wide("PetitMatesOverlay");
    unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
            class_name.as_ptr(),
            ptr::null(),
            WS_POPUP,
            0, 0, 1, 1,
            char_hwnd, // owner = character window → inherits Z-order relationship
            ptr::null_mut(), hinstance, ptr::null(),
        )
    }
}

/// Render and position the bubble HWND above or below the character sprite.
unsafe fn update_bubble_hwnd(
    bubble_hwnd: HWND,
    char_hwnd: HWND,
    text: &str,
    font_size: i32,
    char_x: i32, char_y: i32,
    char_w: i32, char_h: i32,
    screen_w: i32, screen_h: i32,
    alpha_u8: u8,
) {
    // Choose placement.
    let est_h = 60 + WIN_BUBBLE_TAIL_H;
    let tail_at_bottom =
        char_y - est_h - WIN_BUBBLE_MARGIN > 0; // space *above* char (Y-down coords)

    let (bgra, bw, bh) = unsafe { render_bubble_bgra(text, tail_at_bottom, font_size) };

    let bx = {
        let cx = char_x + char_w / 2;
        (cx - bw / 2).max(0).min(screen_w - bw)
    };
    let by = if tail_at_bottom {
        (char_y - bh - WIN_BUBBLE_MARGIN).max(0)
    } else {
        (char_y + char_h + WIN_BUBBLE_MARGIN).min(screen_h - bh)
    };

    unsafe { set_layered_content(bubble_hwnd, &bgra, bw, bh, bx, by, alpha_u8); }

    // Ensure window is visible.
    unsafe { ShowWindow(bubble_hwnd, SW_SHOWNOACTIVATE); }
    // Keep just above the character HWND.
    unsafe {
        SetWindowPos(
            bubble_hwnd, char_hwnd,
            bx, by, bw, bh,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

// ---- Surface → screen position ----

/// Convert a `Surface` + character position to the screen-space top-left
/// corner of the sprite.
///
/// Uses the same anchor math as `surface_to_ns_origin` in `macos.rs`, but
/// returns `(i32, i32)` in Windows screen coords directly (no Y-flip needed).
fn surface_to_screen_pos(
    surface: &Surface,
    char_pos: (f64, f64),
    (sw, sh): (f64, f64),
    anchor: Anchor,
    stand_anchor_y: f64,
    wins: &[WinInfo],
    si: &ScreenInfo,
) -> (i32, i32) {
    match surface {
        // Free-flight: char_pos is already the top-left in screen coords.
        Surface::Airborne => (char_pos.0 as i32, char_pos.1 as i32),

        // Floor: foot on the desktop floor, centred on x.
        // stand_anchor_y adjusts so every sprite sits at the same visual
        // foot level regardless of sprite height.
        Surface::Desktop { x } => {
            let sx = (x - sw / 2.0) as i32;
            let sy = (si.floor_y() - sh + anchor.y - stand_anchor_y) as i32;
            (sx, sy)
        }

        // Window top: foot on win.y, centred on x_local.
        Surface::WindowTop { win_id, x_local } => {
            let Some(win) = windows_wm::find_win(*win_id, wins) else {
                return (-4096, -4096);
            };
            let sx = (win.x + x_local - sw / 2.0) as i32;
            let sy = (win.y - sh + anchor.y) as i32;
            (sx, sy)
        }

        // Wall: sprite centre row aligned with y_local.
        // anchor.x = distance from LEFT of sprite to grip line.
        // For Side::Right the sprite is unmirrored (grip on LEFT side, body to RIGHT).
        // For Side::Left  the sprite is mirrored   (grip on RIGHT side, body to LEFT).
        Surface::WindowWall { win_id, side, y_local } => {
            let Some(win) = windows_wm::find_win(*win_id, wins) else {
                return (-4096, -4096);
            };
            let sy = (win.y + y_local - sh / 2.0) as i32;
            let sx = match side {
                Side::Right => (win.right() - sw + anchor.x) as i32,
                Side::Left  => (win.x - anchor.x) as i32,
            };
            (sx, sy)
        }

        // Upper corner: foot on win.y, side-aligned.
        // point attachment (hang-corner): anchor.x from left aligns grip with corner.
        // line_y attachment (f-sit, f-lie …): align sprite edge with corner.
        Surface::WindowUpperCorner { win_id, side } => {
            let Some(win) = windows_wm::find_win(*win_id, wins) else {
                return (-4096, -4096);
            };
            let sy = (win.y - sh + anchor.y) as i32;
            let sx = if anchor.x > 0.0 {
                match side {
                    Side::Right => (win.right() - anchor.x) as i32,
                    Side::Left  => (win.x - sw + anchor.x) as i32,
                }
            } else {
                match side {
                    Side::Right => (win.right() - sw) as i32,
                    Side::Left  => win.x as i32,
                }
            };
            (sx, sy)
        }

        // Window bottom: foot on win.bottom(), centred on x_local.
        Surface::WindowBottom { win_id, x_local } => {
            let Some(win) = windows_wm::find_win(*win_id, wins) else {
                return (-4096, -4096);
            };
            let sx = (win.x + x_local - sw / 2.0) as i32;
            let sy = (win.bottom() - sh + anchor.y) as i32;
            (sx, sy)
        }
    }
}

// ---- Surface helpers ----

/// Returns the HWND of the window this surface is anchored to, if any.
/// `WinInfo::id` is stored as `hwnd as u32`; safe to cast back on Windows
/// where HWNDs always fit in 32 bits.
fn surface_host_hwnd(surface: &crate::behavior::Surface) -> Option<HWND> {
    use crate::behavior::Surface;
    match surface {
        Surface::WindowTop { win_id, .. }
        | Surface::WindowWall { win_id, .. }
        | Surface::WindowUpperCorner { win_id, .. }
        | Surface::WindowBottom { win_id, .. } => Some(*win_id as HWND),
        _ => None,
    }
}

// ---- Spawn a new character window ----

/// Create a new layered `HWND` and return its initial `CharState`.
/// The window class must already be registered.
/// Compute the effective config (base `params.toml` values with the
/// character's current personality applied on top).
fn compute_effective_config(
    config: &SharedConfig,
    engine: &crate::anim_trigger::BehaviorEngine,
) -> crate::config::Config {
    let mut cfg = config.lock().unwrap().current.clone();
    crate::config::apply_personality(&mut cfg, engine.personality());
    cfg
}

unsafe fn spawn_char_hwnd(si: &ScreenInfo, assets: Rc<SpriteAssets>, config: SharedConfig, char_name: &str) -> CharState {
    let hinstance  = unsafe { GetModuleHandleW(ptr::null()) };
    let class_name = to_wide("PetitMatesOverlay");
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            class_name.as_ptr(),
            ptr::null(),
            WS_POPUP,
            0, 0, 1, 1,
            ptr::null_mut(), ptr::null_mut(), hinstance, ptr::null(),
        )
    };
    let stand_size = assets.size("s-stand", false);
    let (sx, sy) = physics::startup_drop(&si.physics_screen(), stand_size.0, -stand_size.1);
    if let Some(init) = assets.sprite("s-stand", false) {
        unsafe { set_layered_content(hwnd, &init.bgra, init.w, init.h, -4096, -4096, 255) };
    }
    let behavior_engine = {
        let behavior_data = crate::anim_trigger::load(char_name);
        // On Windows assets are embedded; watch the exe-adjacent {char}_behavior.toml if present.
        let exe_dir = std::env::current_exe().ok()
            .and_then(|e| e.parent().map(|p| p.to_path_buf()));
        let watch_path = exe_dir.map(|d| d.join(format!("{char_name}_behavior.toml")));
        let engine = crate::anim_trigger::BehaviorEngine::new(behavior_data, &assets.animations);
        if let Some(p) = watch_path { engine.with_personality_path(p) } else { engine }
    };
    let speech_engine = crate::speech::SpeechEngine::new(crate::speech::load(char_name));
    let effective_config = compute_effective_config(&config, &behavior_engine);
    CharState {
        hwnd,
        assets,
        config,
        effective_config,
        behavior:        Box::new(RustBehavior::new()),
        anim_state:      State::Falling { vx: 0.0, vy: 0.0, shocked: 0.0 },
        facing:          Dir::Left,
        surface:         Surface::Airborne,
        char_pos:        (sx, sy),
        last_tick:       Instant::now(),
        visible:         false,
        drag_offset:     None,
        last_screen_pos: (-4096, -4096),
        debug_trigger:   None,
        speech_engine,
        behavior_engine,
        bubble_state: None,
        bubble_hwnd: ptr::null_mut(),
        last_sprite_w: 150.0,
    }
}

// ---- Per-character tick ----

fn tick_char(ch: &mut CharState, cfg: &crate::config::Config, si: &ScreenInfo, wins: &[WinInfo], sprite_size: f64) {
    let assets: &SpriteAssets = &ch.assets;
    // While being dragged, skip the state machine and just render at the
    // position set by WM_MOUSEMOVE.
    if ch.drag_offset.is_some() {
        ch.last_tick = Instant::now(); // keep dt fresh so release doesn't jump
        let sr = sprite_for_state(&ch.anim_state, ch.facing, &ch.assets.animations);
        let Some(sprite) = assets.sprite(&sr.name, sr.mirror) else { return };
        let (px, py) = (ch.char_pos.0 as i32, ch.char_pos.1 as i32);
        let bgra = sprite.bgra.clone();
        unsafe { set_layered_content(ch.hwnd, &bgra, sprite.w, sprite.h, px, py, 200); }
        return;
    }

    // Compute dt, capped to avoid large jumps after pauses.
    let now = Instant::now();
    let dt  = now.duration_since(ch.last_tick).as_secs_f64().min(0.1);
    ch.last_tick = now;

    // Surface validity check.
    if !windows_wm::surface_still_valid(&ch.surface, wins) {
        let ctx = BehaviorContext {
            state: &ch.anim_state, surface: &ch.surface,
            elapsed_secs: 0.0, config: cfg, rng01: 0.0,
            surface_progress: 0.5, facing: ch.facing,
            at_edge: false, surface_edge_info: SurfaceEdge::None,
            jump_target: None, attract_target: None,
        };
        ch.anim_state = ch.behavior.on_surface_lost(&ctx);
        ch.surface = Surface::Airborne;
    }

    // Advance per-state animation timers.
    let elapsed = advance_anim(&mut ch.anim_state, dt, cfg, &ch.assets.animations);

    // Save y before position update for swept landing detection.
    let prev_cy = ch.char_pos.1;

    let psi = si.physics_screen();

    // Sprite sizes needed by physics functions.
    let stand_size = assets.size("s-stand", false);
    let jump_size  = assets.size("s-jump", false);
    let hang_h     = assets.size("s-hang-wall-0", false).1;

    physics::integrate_velocity(&ch.anim_state, &mut ch.surface, &mut ch.char_pos, cfg, &psi, sprite_size, dt, wins);
    physics::apply_gravity(&mut ch.anim_state, cfg.jump.gravity, dt);
    physics::check_airborne_arrival(&mut ch.anim_state, &mut ch.surface, &mut ch.char_pos, &mut ch.facing, wins, hang_h, stand_size.0, jump_size.1);

    // Off-screen safeguard: only applies to free-flying states.
    // Window-anchored surfaces use local coordinates; surface_still_valid handles disappearance.
    if matches!(&ch.surface, Surface::Airborne | Surface::Desktop { .. }) {
        physics::check_off_screen(&mut ch.anim_state, &mut ch.surface, &mut ch.char_pos, &psi, stand_size, -stand_size.1);
    }

    // Landing detection (swept check).
    if let Some(li) = physics::check_landing(&ch.anim_state, prev_cy, ch.char_pos, &psi, wins, sprite_size, jump_size) {
        let new_anim = {
            let ctx = BehaviorContext {
                state: &ch.anim_state, surface: &li.new_surface,
                elapsed_secs: 0.0, config: cfg, rng01: 0.0,
                surface_progress: 0.5, facing: ch.facing,
                at_edge: false, surface_edge_info: SurfaceEdge::None,
                jump_target: None, attract_target: None,
            };
            ch.behavior.on_landed(&ctx)
        };
        let stand_anchor = assets.anchor("s-stand").unwrap_or(Anchor { x: 0.0, y: 0.0 });
        ch.char_pos   = (li.foot_x - jump_size.0 / 2.0, li.surface_y - stand_size.1 + stand_anchor.y);
        ch.surface    = li.new_surface;
        ch.anim_state = new_anim;
    }

    // Compute surface_progress, at_edge, jump_target.
    let sr_for_ctx = match &ch.anim_state {
        State::TurningAround { elapsed, .. } => {
            let p = (*elapsed / cfg.floor.turn_duration).clamp(0.0, 1.0);
            sprite_for_turn(p, ch.facing)
        }
        other => sprite_for_state(other, ch.facing, &ch.assets.animations),
    };
    let sprite_sz = assets.size(&sr_for_ctx.name, sr_for_ctx.mirror);
    ch.last_sprite_w = sprite_sz.0;
    let (surface_progress, at_edge, jump_target, attract_target) = physics::surface_context(
        &ch.surface, sprite_sz.0, ch.facing,
        cfg.jump.wall_jump_max_dist, cfg.jump.wall_jump_floor_margin,
        cfg.jump.climb_attract_dist, cfg.corner.corner_jump_dist, wins, &psi,
    );

    // Save to_dir if TurningAround completes this tick.
    let turn_to_dir = if let State::TurningAround { to_dir, .. } = &ch.anim_state {
        Some(*to_dir)
    } else { None };

    // Run behavior state machine.
    let transition = {
        let ctx = BehaviorContext {
            state: &ch.anim_state, surface: &ch.surface,
            elapsed_secs: elapsed, config: cfg, rng01: 0.0,
            surface_progress, facing: ch.facing, at_edge, jump_target,
            surface_edge_info: SurfaceEdge::compute(&ch.surface, at_edge, surface_progress),
            attract_target,
        };
        ch.behavior.next_state(&ctx)
    };

    match transition {
        Transition::Stay => {}
        Transition::To(new_state) => {
            let mut new_state = new_state;
            if let Some(dir) = turn_to_dir { ch.facing = dir; }
            let sz = physics::TerrestrialSizes {
                hang_h,
                stand_w:  stand_size.0,
                walk_w:   assets.size("s-walk-0", false).0,
                jump_w:   jump_size.0,
                jump_h:   jump_size.1,
                sprite_w: sprite_sz.0,
            };
            let launch_pos = ch.char_pos;
            physics::resolve_transition(
                &mut new_state, &ch.anim_state, &mut ch.surface,
                &mut ch.char_pos, &mut ch.facing, cfg, wins, &sz,
                ch.assets.surfaces.window_bottom, launch_pos,
            );
            ch.anim_state = new_state;
        }
    }

    // Debug trigger: forced state override after countdown.
    let fired = ch.debug_trigger.as_mut()
        .map(|(_, r)| { *r -= dt; *r <= 0.0 })
        .unwrap_or(false);
    if fired {
        if let Some((target, _)) = ch.debug_trigger.take() {
            ch.anim_state = target;
        }
    }

    // Keep facing in sync with Walking/Running direction.
    if let State::Walking { dir, .. } | State::Running { dir, .. } = &ch.anim_state {
        ch.facing = *dir;
    }

    // Select sprite.
    let sr = match &ch.anim_state {
        State::TurningAround { elapsed, .. } => {
            let p = (*elapsed / cfg.floor.turn_duration).clamp(0.0, 1.0);
            sprite_for_turn(p, ch.facing)
        }
        other => sprite_for_state(other, ch.facing, &ch.assets.animations),
    };

    let Some(sprite) = assets.sprite(&sr.name, sr.mirror) else { return };
    let (sw, sh) = (sprite.w as f64, sprite.h as f64);

    let anchor         = assets.anchor(&sr.name).unwrap_or(Anchor { x: 0.0, y: 0.0 });
    let stand_anchor_y = assets.anchor("s-stand").map(|a| a.y).unwrap_or(0.0);
    let (px, py) = surface_to_screen_pos(
        &ch.surface, ch.char_pos, (sw, sh), anchor, stand_anchor_y, wins, si,
    );
    let py = py - vertical_offset(&ch.anim_state, &assets.animations) as i32;

    // Hover: check whether cursor is over the sprite.
    let alpha: u8 = unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        let over = GetCursorPos(&mut pt) != 0
            && pt.x >= px && pt.x < px + sprite.w
            && pt.y >= py && pt.y < py + sprite.h;
        if over { cfg.display.hover_alpha.clamp(0.0, 1.0).mul_add(254.0, 1.0) as u8 }
        else    { 255 }
    };

    let bgra = sprite.bgra.clone();
    unsafe {
        set_layered_content(ch.hwnd, &bgra, sprite.w, sprite.h, px, py, alpha);

        // Z-order: place the character just above its host window so the host
        // is visible underneath the character, but windows in front of the host
        // occlude the character.
        // GetWindow(host, GW_HWNDPREV) returns the window directly above host
        // in Z order; using it as hWndInsertAfter inserts the character between
        // that window and the host. If result == ch.hwnd the character is
        // already correctly positioned and SetWindowPos becomes a no-op.
        // On Desktop / Airborne: place at HWND_TOP (front of non-topmost).
        let z_host_hwnd: Option<HWND> = surface_host_hwnd(&ch.surface).or_else(|| {
            let win_id = match &ch.anim_state {
                State::JumpRunup { target_win_id, .. } |
                State::Airborne  { target_win_id, .. } => Some(*target_win_id),
                _ => None,
            }?;
            wins.iter().find(|w| w.id == win_id).map(|w| w.id as HWND)
        });
        let insert_after: HWND = if let Some(host) = z_host_hwnd {
            let above = GetWindow(host, GW_HWNDPREV);
            if above.is_null() { HWND_TOP } else { above }
        } else {
            HWND_TOP
        };
        SetWindowPos(
            ch.hwnd, insert_after,
            0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );

        if !ch.visible {
            ShowWindow(ch.hwnd, SW_SHOWNOACTIVATE);
            ch.visible = true;
        }
    }
    ch.last_screen_pos = (px, py);
}

// ---- Tick all characters (10 Hz timer callback) ----

fn tick_all() {
    APP.with(|cell| {
        let mut b = cell.borrow_mut();
        let Some(app) = b.as_mut() else { return };

        if crate::user_config::take_restart_request() {
            unsafe { PostQuitMessage(0); }
            return;
        }

        let si = windows_wm::screen_info();

        // Tiered window-list refresh.
        // Phase 1 — all mutations to win_cache happen here before we borrow wins.
        let full_refresh = app.win_cache.ticks_until_refresh == 0;
        if full_refresh {
            windows_wm::list_windows_into(&mut app.win_cache.wins, &si);
            let attract_dist = app.chars.first()
                .map(|ch| ch.config.lock().unwrap().current.jump.climb_attract_dist)
                .unwrap_or(600.0);
            app.win_cache.ticks_until_refresh =
                next_refresh_interval(&app.chars, &app.win_cache.wins, attract_dist);
        } else {
            app.win_cache.ticks_until_refresh -= 1;
            // Per-tick host-window update via GetWindowRect — essentially free on Windows.
            for i in 0..app.chars.len() {
                if let Some(host_id) = surface_host_win_id(&app.chars[i].surface) {
                    match windows_wm::host_win_info(host_id) {
                        Some(fresh) => {
                            if let Some(entry) = app.win_cache.wins.iter_mut().find(|w| w.id == host_id) {
                                *entry = fresh;
                            }
                        }
                        None => {
                            app.win_cache.wins.retain(|w| w.id != host_id);
                        }
                    }
                }
            }
        }

        // Phase 2 — tick each character using the (now stable) win_cache.
        let wins: &[WinInfo] = &app.win_cache.wins;

        let n = app.chars.len();
        for i in 0..n {
            // Hot-reload checks (file stat) and effective-config recomputation
            // are throttled to the window-list refresh boundary rather than run
            // every tick. Between refreshes the cached `effective_config` is
            // reused (a cheap stack copy — `Config` holds no heap data).
            if full_refresh {
                let params_changed = app.chars[i].config.lock().unwrap().reload_if_changed();
                let pers_changed = app.chars[i].behavior_engine.reload_personality_if_changed();
                if params_changed || pers_changed {
                    app.chars[i].effective_config =
                        compute_effective_config(&app.chars[i].config, &app.chars[i].behavior_engine);
                }
            }
            let cfg = app.chars[i].effective_config.clone();
            tick_char(&mut app.chars[i], &cfg, &si, &wins, app.sprite_size);
        }

        // Post-tick: separate resting characters that are too close on the same surface.
        {
            let n = app.chars.len();
            for _ in 0..2 {
                for i in 0..n {
                    for j in (i + 1)..n {
                        let resting_i = matches!(&app.chars[i].anim_state,
                            State::SitIdle { .. } | State::LieIdle { .. } |
                            State::Sleeping { .. } | State::CornerRest { .. }
                        );
                        let resting_j = matches!(&app.chars[j].anim_state,
                            State::SitIdle { .. } | State::LieIdle { .. } |
                            State::Sleeping { .. } | State::CornerRest { .. }
                        );
                        if !resting_i || !resting_j { continue; }

                        let info_i: Option<(bool, u32, f64)> = match &app.chars[i].surface {
                            Surface::Desktop { x }                 => Some((false, 0, *x)),
                            Surface::WindowTop { win_id, x_local } => Some((true, *win_id, *x_local)),
                            _ => None,
                        };
                        let info_j: Option<(bool, u32, f64)> = match &app.chars[j].surface {
                            Surface::Desktop { x }                 => Some((false, 0, *x)),
                            Surface::WindowTop { win_id, x_local } => Some((true, *win_id, *x_local)),
                            _ => None,
                        };
                        let (is_win_i, id_i, pos_i) = match info_i { Some(v) => v, None => continue };
                        let (is_win_j, id_j, pos_j) = match info_j { Some(v) => v, None => continue };

                        if is_win_i != is_win_j || id_i != id_j { continue; }

                        let half_sprite = app.sprite_size * 0.5;
                        let dist = (pos_i - pos_j).abs();
                        if dist >= half_sprite { continue; }

                        let nudge = if pos_j >= pos_i { half_sprite - dist } else { -(half_sprite - dist) };
                        let edge_buf = app.chars[j].last_sprite_w / 2.0 + 3.0;
                        match &mut app.chars[j].surface {
                            Surface::Desktop { x } => {
                                let new_x = (*x + nudge).clamp(edge_buf, si.width - edge_buf);
                                *x = new_x;
                                app.chars[j].char_pos.0 = new_x;
                            }
                            Surface::WindowTop { win_id, x_local } => {
                                let win_w = windows_wm::find_win(*win_id, &wins)
                                    .map(|w| w.w)
                                    .unwrap_or(f64::MAX);
                                *x_local = (*x_local + nudge).clamp(edge_buf, win_w - edge_buf);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // Speech trigger evaluation.
        if app.speech_cfg.enabled {
            let now = Instant::now();
            let speech_dt = now.duration_since(app.speech_tick).as_secs_f64().min(0.5);
            app.speech_tick = now;
            app.speech_lock_remaining = (app.speech_lock_remaining - speech_dt).max(0.0);
            let lock     = app.speech_lock_remaining;
            let lock_sec = app.speech_cfg.speech_lock_sec;
            let font_sz  = app.font_size;
            let hinstance = unsafe { GetModuleHandleW(ptr::null()) };

            // Advance existing bubbles.
            for ch in &mut app.chars {
                if let Some(bs) = &mut ch.bubble_state {
                    bs.remaining_sec -= speech_dt;
                    if bs.remaining_sec <= 0.0 {
                        if !ch.bubble_hwnd.is_null() {
                            unsafe { ShowWindow(ch.bubble_hwnd, SW_HIDE) };
                        }
                        ch.bubble_state = None;
                    } else if !ch.bubble_hwnd.is_null() {
                        // Reposition to track character.
                        let alpha = (bs.alpha() * 255.0) as u8;
                        let (cx, cy) = ch.last_screen_pos;
                        let (sw, sh) = (si.width as i32, si.height as i32);
                        let sprite_w = ch.assets.sprite("s-stand", false)
                            .map(|s| s.w).unwrap_or(150);
                        let sprite_h = ch.assets.sprite("s-stand", false)
                            .map(|s| s.h).unwrap_or(150);
                        let text = bs.text.clone();
                        unsafe {
                            update_bubble_hwnd(
                                ch.bubble_hwnd, ch.hwnd, &text, font_sz,
                                cx, cy, sprite_w, sprite_h, sw, sh, alpha,
                            );
                        }
                    }
                }
            }

            // Check for new speech lines.
            for i in 0..app.chars.len() {
                let state = app.chars[i].anim_state.clone();
                let weather_info = app.weather.get();
                if let Some(line) = app.chars[i].speech_engine.tick(&state, lock, weather_info.as_ref()) {
                    app.speech_lock_remaining = lock_sec;
                    if let Some(bs) = crate::speech::BubbleState::new(&line, &app.lang) {
                        // Create bubble HWND lazily.
                        if app.chars[i].bubble_hwnd.is_null() {
                            let char_hwnd = app.chars[i].hwnd;
                            app.chars[i].bubble_hwnd =
                                unsafe { create_bubble_hwnd(hinstance, char_hwnd) };
                        }
                        let (cx, cy) = app.chars[i].last_screen_pos;
                        let (sw, sh) = (si.width as i32, si.height as i32);
                        let sprite_w = app.chars[i].assets.sprite("s-stand", false)
                            .map(|s| s.w).unwrap_or(150);
                        let sprite_h = app.chars[i].assets.sprite("s-stand", false)
                            .map(|s| s.h).unwrap_or(150);
                        let text = bs.text.clone();
                        unsafe {
                            update_bubble_hwnd(
                                app.chars[i].bubble_hwnd, app.chars[i].hwnd, &text, font_sz,
                                cx, cy, sprite_w, sprite_h, sw, sh, 255,
                            );
                        }
                        app.chars[i].bubble_state = Some(bs);
                    }
                    // Fire OneShot animation alongside speech if specified.
                    if let Some(anim_name) = line.oneshot {
                        let ch = &mut app.chars[i];
                        if ch.assets.animations.contains_key(&anim_name) {
                            let return_to = Box::new(ch.anim_state.clone());
                            ch.anim_state = crate::behavior::State::OneShot {
                                animation: anim_name,
                                frame: 0,
                                frame_elapsed: 0.0,
                                done: false,
                                return_to,
                            };
                        }
                    }
                    break;
                }
            }
        }

        // Behavior animation triggers.
        {
            let weather_info = app.weather.get();
            for i in 0..app.chars.len() {
                let has_bubble = app.chars[i].bubble_state.is_some();
                let state      = app.chars[i].anim_state.clone();
                if let Some(anim_name) = app.chars[i].behavior_engine.tick(
                    &state, has_bubble, weather_info.as_ref(),
                ) {
                    let return_to = Box::new(state);
                    app.chars[i].anim_state = crate::behavior::State::OneShot {
                        animation: anim_name,
                        frame: 0,
                        frame_elapsed: 0.0,
                        done: false,
                        return_to,
                    };
                }
            }
        }

        // Update tray tooltip with countdown info when a debug trigger is pending.
        let min_remaining: Option<f64> = app.chars.iter()
            .filter_map(|c| c.debug_trigger.as_ref().map(|(_, r)| *r))
            .reduce(f64::min);
        if let Some(host) = app.chars.first() {
            update_tray_countdown(host.hwnd, min_remaining);
        }
    });
}

// ---- Debug countdown tray tooltip ----

fn update_tray_countdown(hwnd: HWND, remaining: Option<f64>) {
    let tip = if let Some(secs) = remaining {
        format!("Petit Mates — trigger in {:.0}s", secs.ceil().max(1.0))
    } else {
        "Petit Mates".to_owned()
    };
    unsafe {
        let tip_wide = to_wide(&tip);
        let mut nid: NOTIFYICONDATAW = mem::zeroed();
        nid.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd   = hwnd;
        nid.uID    = 1;
        nid.uFlags = NIF_TIP;
        let n = tip_wide.len().min(nid.szTip.len());
        nid.szTip[..n].copy_from_slice(&tip_wide[..n]);
        Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

// ---- Window procedure ----

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            // Pass-through by default; capture when Ctrl is held.
            WM_NCHITTEST => {
                let ctrl = GetAsyncKeyState(VK_CONTROL as i32) as u16 & 0x8000 != 0;
                if ctrl { HTCLIENT as LRESULT } else { HTTRANSPARENT as LRESULT }
            }
            WM_LBUTTONDOWN => {
                let mut pt = POINT { x: 0, y: 0 };
                GetCursorPos(&mut pt);
                APP.with(|cell| {
                    if let Some(app) = cell.borrow_mut().as_mut() {
                        let idx = app.chars.iter().position(|c| c.hwnd == hwnd);
                        if let Some(i) = idx {
                            let (lx, ly) = app.chars[i].last_screen_pos;
                            app.chars[i].drag_offset = Some((
                                pt.x as f64 - lx as f64,
                                pt.y as f64 - ly as f64,
                            ));
                            app.chars[i].char_pos   = (lx as f64, ly as f64);
                            app.chars[i].anim_state  = State::Grabbed;
                            app.chars[i].surface     = Surface::Airborne;
                        }
                    }
                });
                SetCapture(hwnd);
                0
            }
            // Alt+Ctrl+right-click: show debug context menu for this character.
            // Ctrl is already required by WM_NCHITTEST to deliver the click here.
            WM_RBUTTONDOWN => {
                let alt = GetAsyncKeyState(VK_MENU as i32) as u16 & 0x8000 != 0;
                if !alt { return 0; }

                struct MenuInfo {
                    header: String,
                    outing_str: String,
                    target_labels: Vec<String>,
                    can_remove: bool,
                }

                let result = APP.with(|cell| -> Option<MenuInfo> {
                    let mut b = cell.borrow_mut();
                    let app = b.as_mut()?;
                    let idx = app.chars.iter().position(|c| c.hwnd == hwnd)?;
                    let ch  = &app.chars[idx];
                    let cfg = ch.config.lock().unwrap().current.clone();

                    let surface_str = crate::debug_menu::surface_name(&ch.surface);
                    let state_str   = crate::debug_menu::state_name(&ch.anim_state);
                    let dur_str = crate::debug_menu::state_elapsed_duration(&ch.anim_state)
                        .map(|(e, d)| format!(" ({:.0}s / {:.0}s)", d - e, d))
                        .unwrap_or_default();
                    let header = format!("{} — {}{}", surface_str, state_str, dur_str);
                    let outing_str = ch.behavior.outing_info(&cfg)
                        .map(|(r, t)| if app.lang == "ja" {
                            format!("次の外出: {:.0}秒 / {:.0}秒", r, t)
                        } else {
                            format!("Next outing: {:.0}s / {:.0}s", r, t)
                        })
                        .unwrap_or_default();

                    let targets = crate::debug_menu::trigger_targets(
                        &ch.surface, &ch.anim_state, ch.facing, &cfg,
                    );
                    if targets.is_empty() { return None; }

                    let labels: Vec<String> = targets.iter().map(|t| t.label.clone()).collect();
                    app.debug_menu_char    = idx;
                    app.debug_menu_targets = targets.into_iter().map(|t| t.state).collect();
                    Some(MenuInfo { header, outing_str, target_labels: labels, can_remove: app.chars.len() > 1 })
                });

                let Some(info) = result else { return 0; };

                let menu = CreatePopupMenu();
                // Disabled info rows.
                let header_w = to_wide(&info.header);
                AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, header_w.as_ptr());
                if !info.outing_str.is_empty() {
                    let outing_w = to_wide(&info.outing_str);
                    AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, outing_w.as_ptr());
                }
                AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
                // Trigger items.
                let wide_labels: Vec<Vec<u16>> =
                    info.target_labels.iter().map(|s| to_wide(s)).collect();
                for (i, w) in wide_labels.iter().enumerate() {
                    AppendMenuW(menu, MF_STRING, IDM_DEBUG_BASE + i, w.as_ptr());
                }
                // Separator + destructive Remove item (only when more than one character).
                if info.can_remove {
                    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
                    let ja = APP.with(|cell| cell.borrow().as_ref().map(|a| a.lang == "ja").unwrap_or(false));
                    let rm_w = to_wide(if ja { "このキャラクターを削除…" } else { "Remove This Character\u{2026}" });
                    AppendMenuW(menu, MF_STRING, IDM_DEBUG_REMOVE, rm_w.as_ptr());
                }
                let mut pt = POINT { x: 0, y: 0 };
                GetCursorPos(&mut pt);
                SetForegroundWindow(hwnd);
                TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, ptr::null());
                DestroyMenu(menu);
                0
            }
            WM_MOUSEMOVE => {
                let dragging = APP.with(|cell| {
                    cell.borrow().as_ref()
                        .and_then(|app| app.chars.iter().find(|c| c.hwnd == hwnd))
                        .map(|c| c.drag_offset.is_some())
                        .unwrap_or(false)
                });
                if dragging {
                    let mut pt = POINT { x: 0, y: 0 };
                    GetCursorPos(&mut pt);
                    APP.with(|cell| {
                        if let Some(app) = cell.borrow_mut().as_mut() {
                            let idx = app.chars.iter().position(|c| c.hwnd == hwnd);
                            if let Some(i) = idx {
                                if let Some((ox, oy)) = app.chars[i].drag_offset {
                                    app.chars[i].char_pos = (pt.x as f64 - ox, pt.y as f64 - oy);
                                }
                            }
                        }
                    });
                    tick_all();
                }
                0
            }
            WM_LBUTTONUP => {
                let was_dragging = APP.with(|cell| {
                    cell.borrow().as_ref()
                        .and_then(|app| app.chars.iter().find(|c| c.hwnd == hwnd))
                        .map(|c| c.drag_offset.is_some())
                        .unwrap_or(false)
                });
                if was_dragging {
                    ReleaseCapture();
                    APP.with(|cell| {
                        if let Some(app) = cell.borrow_mut().as_mut() {
                            let idx = app.chars.iter().position(|c| c.hwnd == hwnd);
                            if let Some(i) = idx {
                                app.chars[i].drag_offset = None;
                                let si   = windows_wm::screen_info();
                                let wins = windows_wm::list_windows(&si);
                                let assets = Rc::clone(&app.chars[i].assets);
                                let sr   = sprite_for_state(&app.chars[i].anim_state, app.chars[i].facing, &app.chars[i].assets.animations);
                                let (sw, sh) = assets.size(&sr.name, sr.mirror);
                                let anchor_cx = app.chars[i].char_pos.0 + sw / 2.0;
                                let anchor_cy = app.chars[i].char_pos.1 + sh;
                                let new_surface = windows_wm::find_surface_for_drop(
                                    anchor_cx, anchor_cy, &wins, &si,
                                ).unwrap_or_else(|| {
                                    Surface::Desktop { x: anchor_cx.clamp(sw / 2.0, si.width - sw / 2.0) }
                                });
                                let cfg = app.chars[i].config.lock().unwrap().current.clone();
                                let new_anim = {
                                    let ctx = BehaviorContext {
                                        state: &State::Grabbed, surface: &new_surface,
                                        elapsed_secs: 0.0, config: &cfg, rng01: 0.0,
                                        surface_progress: 0.5, facing: app.chars[i].facing,
                                        at_edge: false, surface_edge_info: SurfaceEdge::None,
                                        jump_target: None, attract_target: None,
                                    };
                                    app.chars[i].behavior.on_landed(&ctx)
                                };
                                app.chars[i].anim_state = new_anim;
                                app.chars[i].surface    = new_surface;
                            }
                        }
                    });
                    tick_all();
                }
                0
            }
            WM_TIMER if wp == TIMER_TICK => {
                tick_all();
                0
            }
            WM_TRAY => {
                if (lp as u32) & 0xFFFF == WM_RBUTTONUP {
                    let (char_count, ja, weather_info, weather_geo, weather_cfg) = APP.with(|cell| {
                        cell.borrow().as_ref()
                            .map(|app| (
                                app.chars.len(),
                                app.lang == "ja",
                                app.weather.get(),
                                app.weather.geo_status(),
                                app.weather_cfg.clone(),
                            ))
                            .unwrap_or((1, false, None, crate::weather::GeoStatus::Unavailable, Default::default()))
                    });
                    let menu       = CreatePopupMenu();
                    let add_bd_str  = to_wide(if ja { "フトアゴを追加" } else { "Add Bearded Dragon" });
                    let add_pt_str  = to_wide(if ja { "クサガメを追加" } else { "Add Pond Turtle" });
                    let add_lg_str  = to_wide(if ja { "レオパを追加" } else { "Add Leopard Gecko" });
                    let remove_str  = to_wide(if ja { "最後のキャラクターを削除" } else { "Remove Last" });
                    let about_str    = to_wide(if ja { "Petit Mates について" } else { "About Petit Mates" });
                    let alt_held = unsafe {
                        windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(
                            windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_MENU as i32,
                        ) as u16
                            & 0x8000
                            != 0
                    };
                    let settings_str = to_wide(if alt_held {
                        if ja { "設定ファイルを開く" } else { "Open Settings File" }
                    } else if ja {
                        "設定…"
                    } else {
                        "Settings…"
                    });
                    let exit_str     = to_wide(if ja { "終了" } else { "Quit" });
                    AppendMenuW(menu, MF_STRING, IDM_ADD_BD, add_bd_str.as_ptr());
                    AppendMenuW(menu, MF_STRING, IDM_ADD_PT, add_pt_str.as_ptr());
                    AppendMenuW(menu, MF_STRING, IDM_ADD_LG, add_lg_str.as_ptr());
                    let remove_flags = if char_count > 1 { MF_STRING } else { MF_STRING | MF_GRAYED };
                    AppendMenuW(menu, remove_flags, IDM_REMOVE_CHAR, remove_str.as_ptr());
                    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
                    AppendMenuW(menu, MF_STRING,    IDM_SETTINGS, settings_str.as_ptr());
                    // Non-interactive info items: location + weather.
                    if weather_cfg.enabled {
                        use crate::weather::GeoStatus;
                        let loc_text = if let Some(city) = &weather_cfg.city {
                            let suffix = match &weather_geo {
                                GeoStatus::Ok          => " \u{2713}",
                                GeoStatus::Resolving   => if ja { " 解決中..." } else { " resolving..." },
                                GeoStatus::NotFound    => if ja { " 見つかりません" } else { " not found" },
                                GeoStatus::Unavailable => if ja { " 利用不可" } else { " unavailable" },
                            };
                            format!("\u{1f4cd} {}{}", city, suffix)
                        } else if let (Some(lat), Some(lon)) = (weather_cfg.latitude, weather_cfg.longitude) {
                            format!("\u{1f4cd} {:.2}\u{00b0}, {:.2}\u{00b0}", lat, lon)
                        } else {
                            format!("\u{1f4cd} {}", if ja { "未設定" } else { "not configured" })
                        };
                        let wx_text = if let Some(info) = weather_info {
                            let (emoji, cat) = match info.category {
                                crate::weather::WeatherCategory::Sunny  =>
                                    ("\u{2600}\u{fe0f}",  if ja { "晴れ" } else { "Sunny" }),
                                crate::weather::WeatherCategory::Cloudy =>
                                    ("\u{26c5}",           if ja { "曇り" } else { "Cloudy" }),
                                crate::weather::WeatherCategory::Rainy  =>
                                    ("\u{1f327}\u{fe0f}", if ja { "雨"   } else { "Rainy" }),
                                crate::weather::WeatherCategory::Snowy  =>
                                    ("\u{1f328}\u{fe0f}", if ja { "雪"   } else { "Snowy" }),
                            };
                            format!("{} {}, {:.1}\u{00b0}C", emoji, cat, info.temp_c)
                        } else {
                            "\u{2500}".to_string()
                        };
                        let loc_str = to_wide(&loc_text);
                        let wx_str  = to_wide(&wx_text);
                        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
                        AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, loc_str.as_ptr());
                        AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, wx_str.as_ptr());
                    }
                    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
                    AppendMenuW(menu, MF_STRING,    IDM_ABOUT, about_str.as_ptr());
                    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
                    AppendMenuW(menu, MF_STRING,    IDM_EXIT,  exit_str.as_ptr());
                    let mut pt = POINT { x: 0, y: 0 };
                    GetCursorPos(&mut pt);
                    SetForegroundWindow(hwnd);
                    TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, ptr::null());
                    DestroyMenu(menu);
                }
                0
            }
            WM_COMMAND if (wp & 0xFFFF) == IDM_ADD_BD => {
                APP.with(|cell| {
                    if let Some(app) = cell.borrow_mut().as_mut() {
                        let si     = windows_wm::screen_info();
                        let assets = Rc::clone(&app.bd_assets);
                        let config = app.bd_config.clone();
                        let ch     = spawn_char_hwnd(&si, assets, config, "bearded_dragon");
                        app.chars.push(ch);
                    }
                });
                0
            }
            // Debug trigger menu item selected.
            WM_COMMAND if {
                let id = (wp & 0xFFFF) as usize;
                id >= IDM_DEBUG_BASE && id < IDM_DEBUG_BASE + 100
            } => {
                let idx = (wp & 0xFFFF) as usize - IDM_DEBUG_BASE;
                APP.with(|cell| {
                    if let Some(app) = cell.borrow_mut().as_mut() {
                        let char_idx = app.debug_menu_char;
                        if let Some(target) = app.debug_menu_targets.get(idx) {
                            if let Some(ch) = app.chars.get_mut(char_idx) {
                                ch.debug_trigger = Some((
                                    target.clone(),
                                    crate::debug_menu::COUNTDOWN_SECS,
                                ));
                            }
                        }
                    }
                });
                0
            }
            WM_COMMAND if (wp & 0xFFFF) == IDM_ADD_PT => {
                APP.with(|cell| {
                    if let Some(app) = cell.borrow_mut().as_mut() {
                        let si     = windows_wm::screen_info();
                        let assets = Rc::clone(&app.pt_assets);
                        let config = app.pt_config.clone();
                        let ch     = spawn_char_hwnd(&si, assets, config, "pond_turtle");
                        app.chars.push(ch);
                    }
                });
                0
            }
            WM_COMMAND if (wp & 0xFFFF) == IDM_ADD_LG => {
                APP.with(|cell| {
                    if let Some(app) = cell.borrow_mut().as_mut() {
                        let si     = windows_wm::screen_info();
                        let assets = Rc::clone(&app.lg_assets);
                        let config = app.lg_config.clone();
                        let ch     = spawn_char_hwnd(&si, assets, config, "leopard_gecko");
                        app.chars.push(ch);
                    }
                });
                0
            }
            WM_COMMAND if (wp & 0xFFFF) == IDM_REMOVE_CHAR => {
                // Extract hwnd BEFORE releasing the borrow — DestroyWindow triggers
                // WM_DESTROY synchronously, which would conflict with an active borrow_mut.
                let h = APP.with(|cell| {
                    cell.borrow_mut().as_mut().and_then(|app| {
                        if app.chars.len() > 1 { Some(app.chars.pop().unwrap().hwnd) } else { None }
                    })
                });
                if let Some(h) = h { DestroyWindow(h); }
                0
            }
            WM_COMMAND if (wp & 0xFFFF) == IDM_DEBUG_REMOVE => {
                // Collect confirmation info and the survivor hwnd (the window that will
                // still exist after the removal, and that receives WM_APP_REMOVE_CHAR).
                let (char_idx, can, survivor) = APP.with(|cell| {
                    cell.borrow().as_ref()
                        .map(|a| {
                            let can = a.chars.len() > 1;
                            // Pick any surviving hwnd: if removing index 0, use index 1 and vice versa.
                            let survivor = if a.debug_menu_char == 0 {
                                a.chars.get(1).map(|c| c.hwnd).unwrap_or(ptr::null_mut())
                            } else {
                                a.chars.get(0).map(|c| c.hwnd).unwrap_or(ptr::null_mut())
                            };
                            (a.debug_menu_char, can, survivor)
                        })
                        .unwrap_or((0, false, ptr::null_mut()))
                });
                if can && !survivor.is_null() {
                    let ja = APP.with(|cell| cell.borrow().as_ref().map(|a| a.lang == "ja").unwrap_or(false));
                    let msg   = to_wide(if ja { "このキャラクターをデスクトップから削除しますか？" } else { "Remove this character from the desktop?" });
                    let title = to_wide(if ja { "キャラクターの削除" } else { "Remove Character" });
                    let result = MessageBoxW(
                        ptr::null_mut(), msg.as_ptr(), title.as_ptr(),
                        MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
                    );
                    if result == IDYES as i32 {
                        // Defer the actual destruction: post to the surviving window's
                        // queue so it is processed AFTER TrackPopupMenu fully unwinds.
                        PostMessageW(survivor, WM_APP_REMOVE_CHAR, char_idx, 0);
                    }
                }
                0
            }
            WM_APP_REMOVE_CHAR => {
                // Deferred removal posted by IDM_DEBUG_REMOVE.
                // Runs outside any TrackPopupMenu call stack, so DestroyWindow is safe.
                let char_idx = wp as usize;
                struct MigrationInfo {
                    old_hwnd:  HWND,
                    /// Set when chars[0] was removed: (new_host_hwnd, hinstance as isize).
                    new_host:  Option<(HWND, HINSTANCE)>,
                }
                // Mutate Vec and (for host removal) kill old timer + tray inside the borrow.
                let info = APP.with(|cell| -> Option<MigrationInfo> {
                    let mut b = cell.borrow_mut();
                    let app = b.as_mut()?;
                    if app.chars.len() <= 1 || char_idx >= app.chars.len() {
                        return None;
                    }
                    if char_idx == 0 {
                        // Removing the host: kill its timer + tray before we Vec::remove it.
                        let old_hwnd = app.chars[0].hwnd;
                        KillTimer(old_hwnd, TIMER_TICK);
                        remove_tray_icon(old_hwnd);
                        app.chars.remove(0);
                        let new_hwnd  = app.chars[0].hwnd;
                        let hinstance = GetModuleHandleW(ptr::null());
                        Some(MigrationInfo { old_hwnd, new_host: Some((new_hwnd, hinstance)) })
                    } else {
                        let old_hwnd = app.chars.remove(char_idx).hwnd;
                        Some(MigrationInfo { old_hwnd, new_host: None })
                    }
                });
                let Some(info) = info else { return 0; };
                // Re-add tray + timer on the new host BEFORE destroying the old window.
                if let Some((new_hwnd, hinstance)) = info.new_host {
                    add_tray_icon(new_hwnd, hinstance);
                    SetTimer(new_hwnd, TIMER_TICK, 100, None);
                }
                DestroyWindow(info.old_hwnd);
                0
            }
            WM_COMMAND if (wp & 0xFFFF) == IDM_SETTINGS => {
                let alt = unsafe {
                    windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(
                        windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_MENU as i32,
                    ) as u16
                        & 0x8000
                        != 0
                };
                if alt {
                    crate::user_config::open_in_editor();
                } else {
                    crate::user_config::launch_settings_ui();
                }
                0
            }
            WM_COMMAND if (wp & 0xFFFF) == IDM_ABOUT => {
                let text  = to_wide(&format!("Petit Mates\r\nVersion {}\r\n\r\nA desktop accessory by Rino, eMotionGraphics Inc.", env!("CARGO_PKG_VERSION")));
                let title = to_wide("About Petit Mates");
                MessageBoxW(ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONINFORMATION);
                0
            }
            WM_COMMAND if (wp & 0xFFFF) == IDM_EXIT => {
                PostQuitMessage(0);
                0
            }
            WM_DESTROY => {
                // Only quit when the host (first character's) window is destroyed.
                // unwrap_or(false): if APP is unavailable, do NOT quit — avoids
                // spurious exits when a borrow conflict or empty state occurs.
                let is_host = APP.with(|cell| {
                    cell.borrow().as_ref()
                        .and_then(|app| app.chars.first())
                        .map(|ch| ch.hwnd == hwnd)
                        .unwrap_or(false)
                });
                if is_host { PostQuitMessage(0); }
                0
            }
            WM_SETTINGCHANGE => {
                // Only update the tray icon when called on the host window.
                let is_host = APP.with(|cell| {
                    cell.borrow().as_ref()
                        .and_then(|app| app.chars.first())
                        .map(|ch| ch.hwnd == hwnd)
                        .unwrap_or(false)
                });
                if is_host { update_tray_icon(hwnd); }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

// ---- System tray ----

fn add_tray_icon(hwnd: HWND, hinstance: HINSTANCE) {
    unsafe {
        let tip = to_wide("Petit Mates");
        let mut nid: NOTIFYICONDATAW = mem::zeroed();
        nid.cbSize          = mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd            = hwnd;
        nid.uID             = 1;
        nid.uFlags          = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        let icon_id: usize  = if is_dark_mode() { 3 } else { 2 };
        let cx = GetSystemMetrics(SM_CXSMICON).max(32);
        let cy = GetSystemMetrics(SM_CYSMICON).max(32);
        let hicon = LoadImageW(hinstance, icon_id as *const u16, IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR) as HICON;
        nid.hIcon = if !hicon.is_null() { hicon }
                    else { LoadIconW(ptr::null_mut(), IDI_APPLICATION) };
        let n = tip.len().min(nid.szTip.len());
        nid.szTip[..n].copy_from_slice(&tip[..n]);
        Shell_NotifyIconW(NIM_ADD, &nid);
    }
}

fn update_tray_icon(hwnd: HWND) {
    unsafe {
        let hinstance  = GetModuleHandleW(ptr::null());
        let icon_id: usize = if is_dark_mode() { 3 } else { 2 };
        let cx = GetSystemMetrics(SM_CXSMICON).max(32);
        let cy = GetSystemMetrics(SM_CYSMICON).max(32);
        let hicon = LoadImageW(hinstance, icon_id as *const u16, IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR) as HICON;
        if hicon.is_null() { return; }
        let mut nid: NOTIFYICONDATAW = mem::zeroed();
        nid.cbSize  = mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd    = hwnd;
        nid.uID     = 1;
        nid.uFlags  = NIF_ICON;
        nid.hIcon   = hicon;
        Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn remove_tray_icon(hwnd: HWND) {
    unsafe {
        let mut nid: NOTIFYICONDATAW = mem::zeroed();
        nid.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd   = hwnd;
        nid.uID    = 1;
        Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

// ---- Entry point ----

/// Wait until the previous instance releases the single-instance mutex (settings restart).
unsafe fn wait_for_prior_instance_exit(mutex_name: &[u16]) {
    use std::thread;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    for _ in 0..200 {
        let h = CreateMutexW(ptr::null(), 0, mutex_name.as_ptr());
        if h.is_null() {
            thread::sleep(Duration::from_millis(50));
            continue;
        }
        let already = GetLastError() == ERROR_ALREADY_EXISTS;
        CloseHandle(h);
        if !already {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    eprintln!("[petitmates] timed out waiting for prior instance to exit");
}

pub fn run() {
    unsafe {
        let mutex_name = to_wide("Local\\PetitMatesSingleInstance");
        if crate::user_config::is_restarting_instance() {
            wait_for_prior_instance_exit(&mutex_name);
        }
        // Single-instance guard: create a named mutex. If it already exists
        // (ERROR_ALREADY_EXISTS), another instance is running — exit silently.
        let _mutex = windows_sys::Win32::System::Threading::CreateMutexW(
            ptr::null(), 1, mutex_name.as_ptr(),
        );
        if windows_sys::Win32::Foundation::GetLastError()
            == windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS
        {
            return;
        }

        let hinstance  = GetModuleHandleW(ptr::null());
        let class_name = to_wide("PetitMatesOverlay");

        let wc = WNDCLASSEXW {
            cbSize:        mem::size_of::<WNDCLASSEXW>() as u32,
            style:         CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc:   Some(wnd_proc),
            cbClsExtra:    0,
            cbWndExtra:    0,
            hInstance:     hinstance,
            hIcon:         LoadIconW(hinstance, 1usize as *const u16),
            hCursor:       LoadCursorW(ptr::null_mut(), IDC_ARROW),
            hbrBackground: ptr::null_mut(),
            lpszMenuName:  ptr::null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm:       LoadIconW(hinstance, 1usize as *const u16),
        };
        RegisterClassExW(&wc);

        // Load shared assets from embedded bytes.
        let bd_config = make_shared_win_for("bearded_dragon");
        let pt_config = make_shared_win_for("pond_turtle");
        let lg_config = make_shared_win_for("leopard_gecko");
        let user_cfg = crate::user_config::load();
        let sprite_size = user_cfg.display.sprite_size as f64;
        let bd_display_w = sprite_size;
        let pt_display_w = sprite_size;
        let lg_display_w = sprite_size;
        let bd_mf = manifest::load_from_bytes(windows_assets::embedded::bearded_dragon::MANIFEST_TOML)
            .expect("embedded bearded_dragon manifest.toml is invalid");
        let pt_mf = manifest::load_from_bytes(windows_assets::embedded::pond_turtle::MANIFEST_TOML)
            .expect("embedded pond_turtle manifest.toml is invalid");
        let lg_mf = manifest::load_from_bytes(windows_assets::embedded::leopard_gecko::MANIFEST_TOML)
            .expect("embedded leopard_gecko manifest.toml is invalid");
        let bd_assets = Rc::new(
            SpriteAssets::load_embedded(windows_assets::embedded::bearded_dragon::SPRITES, &bd_mf, bd_display_w)
                .expect("failed to decode embedded bearded_dragon sprites"),
        );
        let pt_assets = Rc::new(
            SpriteAssets::load_embedded(windows_assets::embedded::pond_turtle::SPRITES, &pt_mf, pt_display_w)
                .expect("failed to decode embedded pond_turtle sprites"),
        );
        let lg_assets = Rc::new(
            SpriteAssets::load_embedded(windows_assets::embedded::leopard_gecko::SPRITES, &lg_mf, lg_display_w)
                .expect("failed to decode embedded leopard_gecko sprites"),
        );

        // Create character windows. The first serves as the host for timer+tray.
        let si         = windows_wm::screen_info();
        let weather_handle = crate::weather::spawn(&user_cfg.weather);
        let initial_chars: Vec<CharState> = user_cfg
            .characters
            .startup_species_ids()
            .into_iter()
            .map(|species| match species {
                "bearded_dragon" => spawn_char_hwnd(
                    &si,
                    Rc::clone(&bd_assets),
                    bd_config.clone(),
                    species,
                ),
                "pond_turtle" => spawn_char_hwnd(
                    &si,
                    Rc::clone(&pt_assets),
                    pt_config.clone(),
                    species,
                ),
                "leopard_gecko" => spawn_char_hwnd(
                    &si,
                    Rc::clone(&lg_assets),
                    lg_config.clone(),
                    species,
                ),
                _ => unreachable!("startup_species_ids only returns built-in species"),
            })
            .collect();
        let host_hwnd = initial_chars
            .first()
            .expect("startup_species_ids always returns at least one character")
            .hwnd;

        APP.with(|cell| {
            *cell.borrow_mut() = Some(AppState {
                chars:     initial_chars,
                bd_assets,
                pt_assets,
                lg_assets,
                bd_config,
                pt_config,
                lg_config,
                debug_menu_char:    0,
                debug_menu_targets: Vec::new(),
                speech_lock_remaining: 0.0,
                speech_cfg: user_cfg.speech,
                speech_tick: Instant::now(),
                font_size: user_cfg.display.font_size as i32,
                sprite_size: user_cfg.display.sprite_size as f64,
                lang: crate::user_config::resolve_display_language(&user_cfg.display.language),
                weather: weather_handle,
                weather_cfg: user_cfg.weather,
                win_cache: WinListCache::new(),
            });
        });

        add_tray_icon(host_hwnd, hinstance);
        SetTimer(host_hwnd, TIMER_TICK, 100, None);

        // Message loop.
        let mut msg: MSG = mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        remove_tray_icon(host_hwnd);
    }
}
