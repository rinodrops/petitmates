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

struct CharState {
    hwnd: HWND,
    assets: Rc<SpriteAssets>,
    config: SharedConfig,
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
    /// Active speech bubble state; None when no bubble is shown.
    bubble_state: Option<crate::speech::BubbleState>,
    /// HWND for the speech bubble layered window; null when not created yet.
    bubble_hwnd: HWND,
}

struct AppState {
    chars: Vec<CharState>,
    bd_assets: Rc<SpriteAssets>,
    pt_assets: Rc<SpriteAssets>,
    bd_config: SharedConfig,
    pt_config: SharedConfig,
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
    /// Resolved display language: "ja" or "en".
    lang: String,
    /// Shared weather cache updated by the background weather thread.
    weather: crate::weather::WeatherHandle,
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

// ---- Surface context ----

/// Compute `surface_progress`, `at_edge`, `jump_target`, and `attract_target`.
/// Equivalent to `surface_context` in `macos.rs`.
fn surface_context(
    surface: &Surface,
    char_pos: (f64, f64),
    sprite_w: f64,
    facing: Dir,
    jump_max_dist: f64,
    jump_floor_margin: f64,
    attract_dist: f64,
    corner_attract_dist: f64,
    wins: &[WinInfo],
    si: &ScreenInfo,
) -> (f64, bool, Option<(u32, Side)>, Option<(u32, Side, LandingMode)>) {
    let edge_margin = 2.0;
    match surface {
        Surface::WindowTop { win_id, x_local } => {
            let Some(win) = windows_wm::find_win(*win_id, wins) else {
                return (0.5, false, None, None);
            };
            let progress = (x_local / win.w).clamp(0.0, 1.0);
            let at_edge  = *x_local <= edge_margin + sprite_w / 2.0
                        || *x_local >= win.w - edge_margin - sprite_w / 2.0;
            (progress, at_edge, None, None)
        }
        Surface::WindowUpperCorner { win_id, side } => {
            let Some(win) = windows_wm::find_win(*win_id, wins) else {
                return (0.5, false, None, None);
            };
            let corner_cx = match side {
                Side::Left  => win.x,
                Side::Right => win.right(),
            };
            let corner_cy = win.y;
            let attract_target = wins.iter()
                .filter_map(|w| {
                    if w.id == *win_id { return None; }
                    let dist_r = w.x - corner_cx;
                    let dist_l = corner_cx - w.right();
                    let landing_mode = if w.y > corner_cy {
                        LandingMode::TopLanding
                    } else if w.y + w.h > corner_cy {
                        LandingMode::ClimbFromBottom
                    } else {
                        LandingMode::ClimbFromCurrent
                    };
                    if dist_r >= 0.0 && dist_r < corner_attract_dist {
                        Some((w.id, Side::Left, dist_r, landing_mode))
                    } else if dist_l >= 0.0 && dist_l < corner_attract_dist {
                        Some((w.id, Side::Right, dist_l, landing_mode))
                    } else {
                        None
                    }
                })
                .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(id, s, _, lm)| (id, s, lm));
            let progress = if *side == Side::Left { 0.0 } else { 1.0 };
            (progress, false, None, attract_target)
        }
        Surface::Desktop { x } => {
            let progress = (x / si.width).clamp(0.0, 1.0);
            let (cx, _)  = char_pos;
            let floor_y  = si.floor_y();
            // jump_target: current walking direction only, within jump_max_dist.
            let jump_target = wins.iter().find_map(|win| {
                if win.bottom() < floor_y - jump_floor_margin { return None; }
                match facing {
                    Dir::Left => {
                        let dist = cx - win.right();
                        if dist >= 0.0 && dist < jump_max_dist { Some((win.id, Side::Right)) }
                        else { None }
                    }
                    Dir::Right => {
                        let dist = win.x - cx;
                        if dist >= 0.0 && dist < jump_max_dist { Some((win.id, Side::Left)) }
                        else { None }
                    }
                }
            });
            // attract_target: nearest window in either direction within attract_dist.
            let attract_target = wins.iter()
                .filter_map(|win| {
                    if win.bottom() < floor_y - jump_floor_margin { return None; }
                    let dist_r = win.x - cx;
                    let dist_l = cx - win.right();
                    if dist_r >= 0.0 && dist_r < attract_dist {
                        Some((win.id, Side::Left, dist_r))
                    } else if dist_l >= 0.0 && dist_l < attract_dist {
                        Some((win.id, Side::Right, dist_l))
                    } else {
                        None
                    }
                })
                .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(id, side, _)| (id, side, LandingMode::ClimbFromBottom));
            let at_edge = *x <= edge_margin + sprite_w / 2.0
                       || *x >= si.width - edge_margin - sprite_w / 2.0;
            (progress, at_edge, jump_target, attract_target)
        }
        Surface::WindowWall { win_id, y_local, .. } => {
            let Some(win) = windows_wm::find_win(*win_id, wins) else {
                return (0.5, false, None, None);
            };
            let progress = (y_local / win.h).clamp(0.0, 1.0);
            let at_edge  = *y_local <= edge_margin || *y_local >= win.h - edge_margin;
            (progress, at_edge, None, None)
        }
        Surface::WindowBottom { win_id, x_local } => {
            let Some(win) = windows_wm::find_win(*win_id, wins) else {
                return (0.5, false, None, None);
            };
            let progress = (x_local / win.w).clamp(0.0, 1.0);
            let at_edge  = *x_local <= edge_margin + sprite_w / 2.0
                        || *x_local >= win.w - edge_margin - sprite_w / 2.0;
            (progress, at_edge, None, None)
        }
        _ => (0.5, false, None, None),
    }
}

// ---- Startup drop ----

fn startup_drop(si: &ScreenInfo, assets: &SpriteAssets) -> (f64, f64) {
    use rand::{Rng, SeedableRng};
    let (stand_w, stand_h) = assets.size("s-stand", false);
    let margin  = si.width * 0.10;
    let usable  = (si.width - margin * 2.0 - stand_w).max(0.0);
    let offset  = rand::rngs::SmallRng::from_os_rng().random::<f64>() * usable;
    // Start completely above the screen (sprite top at y = -stand_h, feet at y = 0)
    // so the character falls into view without immediately landing on system windows
    // near the top of the screen.
    (margin + offset, -stand_h)
}

/// Precompute the horizontal snap X and approximate target Y for an `Airborne` jump.
fn airborne_snap_target(
    win: &windows_wm::WinInfo,
    side: Side,
    landing_mode: LandingMode,
    assets: &SpriteAssets,
    current_cy: f64,
) -> (f64, f64) {
    let hang_h  = assets.size("s-hang-wall-0", false).1;
    let stand_w = assets.size("s-stand", false).0;
    let target_cx = match side {
        Side::Right => win.right() - stand_w,
        Side::Left  => win.x,
    };
    let target_cy = match landing_mode {
        LandingMode::ClimbFromBottom  => (win.y + win.h - hang_h / 2.0).clamp(win.y, win.y + win.h),
        LandingMode::ClimbFromCurrent => current_cy,
        LandingMode::TopLanding       => win.y,
    };
    (target_cx, target_cy)
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
    let (sx, sy) = startup_drop(si, &assets);
    if let Some(init) = assets.sprite("s-stand", false) {
        unsafe { set_layered_content(hwnd, &init.bgra, init.w, init.h, -4096, -4096, 255) };
    }
    CharState {
        hwnd,
        assets,
        config,
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
        speech_engine: crate::speech::SpeechEngine::new(crate::speech::load(char_name)),
        bubble_state: None,
        bubble_hwnd: ptr::null_mut(),
    }
}

// ---- Per-character tick ----

fn tick_char(ch: &mut CharState, cfg: &crate::config::Config, si: &ScreenInfo, wins: &[WinInfo]) {
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

    // Update char_pos for Airborne / Walking states.
    match &ch.anim_state {
        State::Falling { vx, vy, .. } | State::Airborne { vx, vy, .. } => {
            let (vx, vy) = (*vx, *vy);
            let (cx, cy) = ch.char_pos;
            ch.char_pos = (cx + vx * dt, cy + vy * dt);
        }
        State::Walking { dir, .. } | State::Running { dir, .. } => {
            let speed = if matches!(&ch.anim_state, State::Running { .. }) {
                cfg.floor.run_speed
            } else {
                cfg.floor.walk_speed
            };
            let delta = speed * dt;
            match &mut ch.surface {
                Surface::Desktop { x } => {
                    *x += match dir { Dir::Left => -delta, Dir::Right => delta };
                    let half_w = cfg.display.display_width / 2.0;
                    *x = x.clamp(half_w, si.width - half_w);
                    ch.char_pos.0 = *x;
                }
                Surface::WindowTop { x_local, .. }
                | Surface::WindowBottom { x_local, .. } => {
                    *x_local += match dir { Dir::Left => -delta, Dir::Right => delta };
                }
                _ => {}
            }
        }
        State::ClimbingUp { .. } => {
            if let Surface::WindowWall { y_local, .. } = &mut ch.surface {
                *y_local -= cfg.wall.climb_speed * dt;
                *y_local = y_local.max(0.0);
            }
        }
        _ => {}
    }
    // ClimbingDown: separate re-borrow required by borrow checker.
    if matches!(&ch.anim_state, State::ClimbingDown { .. }) {
        if let Surface::WindowWall { win_id, y_local, .. } = &mut ch.surface {
            if let Some(win) = windows_wm::find_win(*win_id, wins) {
                *y_local += cfg.wall.climb_speed * dt;
                *y_local = y_local.min(win.h);
            }
        }
    }

    // Gravity.
    if let State::Falling { vy, .. } | State::Airborne { vy, .. } = &mut ch.anim_state {
        *vy = (*vy + cfg.jump.gravity * 60.0 * dt).min(600.0);
    }

    // Airborne arrival: snap to wall when character reaches the target X.
    if let State::Airborne { vx, vy, target_win_id, target_side, landing_mode, target_cx, .. }
        = ch.anim_state.clone()
    {
        let arrived = if vx >= 0.0 { ch.char_pos.0 >= target_cx }
                      else         { ch.char_pos.0 <= target_cx };
        if arrived {
            if let Some(win) = windows_wm::find_win(target_win_id, wins) {
                let hang_h  = assets.size("s-hang-wall-0", false).1;
                let stand_w = assets.size("s-stand", false).0;
                match landing_mode {
                    LandingMode::TopLanding => {
                        let x_local = match target_side {
                            Side::Left  => stand_w / 2.0 + 4.0,
                            Side::Right => win.w - stand_w / 2.0 - 4.0,
                        };
                        ch.char_pos = (win.x + x_local, win.y);
                        ch.facing   = match target_side {
                            Side::Left  => Dir::Right,
                            Side::Right => Dir::Left,
                        };
                        ch.surface    = Surface::WindowTop { win_id: win.id, x_local };
                        ch.anim_state = State::Observing { elapsed: 0.0, duration: 3.0 };
                    }
                    LandingMode::ClimbFromCurrent | LandingMode::ClimbFromBottom => {
                        let jump_h  = assets.size("s-jump", false).1;
                        let y_local = (ch.char_pos.1 + jump_h / 2.0 - win.y).clamp(hang_h / 2.0, win.h - 4.0);
                        ch.char_pos.0 = match target_side {
                            Side::Right => win.right() - stand_w,
                            Side::Left  => win.x,
                        };
                        ch.char_pos.1 = win.y + y_local;
                        ch.facing     = match target_side {
                            Side::Left  => Dir::Right,
                            Side::Right => Dir::Left,
                        };
                        ch.surface    = Surface::WindowWall { win_id: win.id, side: target_side, y_local };
                        ch.anim_state = State::WallEntry { elapsed: 0.0 };
                    }
                }
            } else {
                ch.anim_state = State::Falling { vx, vy: vy.max(0.0), shocked: 0.5 };
            }
        }
    }

    // Off-screen safeguard.
    {
        let (fw, fh) = assets.size("s-stand", false);
        let (cx, cy) = ch.char_pos;
        let below = cy > si.height + fh;
        let sides  = cx < -(fw * 3.0) || cx > si.width + fw * 3.0;
        if below || sides {
            let (dx, dy) = startup_drop(si, assets);
            ch.char_pos   = (dx, dy);
            ch.surface    = Surface::Airborne;
            ch.anim_state = State::Falling { vx: 0.0, vy: 0.0, shocked: 0.0 };
        }
    }

    // Landing detection (swept check).
    if let State::Falling { vy, .. } = &ch.anim_state {
        if *vy >= 0.0 {
            let (fw, fh) = assets.size("s-jump", false);
            let foot_x    = ch.char_pos.0 + fw / 2.0;
            let foot_y    = ch.char_pos.1 + fh;
            let floor_y   = si.floor_y();
            let cy_prev   = prev_cy;
            let cy_now    = ch.char_pos.1;

            let landed_win = wins.iter()
                .filter(|win| {
                    win.y < floor_y
                        && foot_x >= win.x
                        && foot_x <= win.right()
                        && cy_prev < win.y
                        && cy_now + fh >= win.y
                })
                .min_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));
            let landed_floor = landed_win.is_none() && foot_y >= floor_y;

            let new_surface = landed_win
                .map(|win| Surface::WindowTop {
                    win_id: win.id,
                    x_local: (foot_x - win.x).clamp(0.0, win.w),
                })
                .or_else(|| landed_floor.then(|| {
                    let half_w = cfg.display.display_width / 2.0;
                    Surface::Desktop { x: foot_x.clamp(half_w, si.width - half_w) }
                }));

            if let Some(new_surface) = new_surface {
                let new_anim = {
                    let ctx = BehaviorContext {
                        state: &ch.anim_state, surface: &new_surface,
                        elapsed_secs: 0.0, config: cfg, rng01: 0.0,
                        surface_progress: 0.5, facing: ch.facing,
                        at_edge: false, surface_edge_info: SurfaceEdge::None,
                        jump_target: None, attract_target: None,
                    };
                    ch.behavior.on_landed(&ctx)
                };
                let (_, stand_h) = assets.size("s-stand", false);
                let stand_anchor = assets.anchor("s-stand").unwrap_or(Anchor { x: 0.0, y: 0.0 });
                let snap_y = match &new_surface {
                    Surface::WindowTop { win_id, .. } =>
                        windows_wm::find_win(*win_id, wins).map(|w| w.y),
                    Surface::Desktop { .. } => Some(floor_y),
                    _ => None,
                };
                if let Some(sy) = snap_y {
                    ch.char_pos = (foot_x - fw / 2.0, sy - stand_h + stand_anchor.y);
                }
                ch.surface    = new_surface;
                ch.anim_state = new_anim;
            }
        }
    }

    // Compute surface_progress, at_edge, jump_target.
    let sr_for_ctx = match &ch.anim_state {
        State::TurningAround { elapsed, .. } => {
            let p = (*elapsed / cfg.floor.turn_duration).clamp(0.0, 1.0);
            sprite_for_turn(p, ch.facing)
        }
        other => sprite_for_state(other, ch.facing, &ch.assets.animations),
    };
    let sprite_w = assets.size(&sr_for_ctx.name, sr_for_ctx.mirror).0;
    let (surface_progress, at_edge, jump_target, attract_target) = surface_context(
        &ch.surface, ch.char_pos, sprite_w, ch.facing,
        cfg.jump.wall_jump_max_dist, cfg.jump.wall_jump_floor_margin,
        cfg.jump.climb_attract_dist, cfg.corner.corner_jump_dist, wins, si,
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
        Transition::To(mut new_state) => {
            if let Some(dir) = turn_to_dir {
                ch.facing = dir;
            }
            // When dropping off a wall, seed char_pos from wall position.
            if matches!(&new_state, State::Falling { .. }) {
                let fall_pos: Option<(f64, f64)> = (|| {
                    let (sw, sh) = assets.size("s-jump", false);
                    match &ch.surface {
                        Surface::WindowWall { win_id, side, y_local } => {
                            let win = windows_wm::find_win(*win_id, wins)?;
                            let cy = win.y + y_local - sh / 2.0;
                            let cx = match side {
                                Side::Right => win.right() - sw,
                                Side::Left  => win.x,
                            };
                            Some((cx, cy))
                        }
                        _ => None,
                    }
                })();
                if let Some(pos) = fall_pos { ch.char_pos = pos; }
            }

            // Keep surface in sync when the new state implies a surface change.
            let new_surface: Option<Surface> = match (&new_state, &ch.surface) {
                (State::Falling { .. }, _) => Some(Surface::Airborne),
                // Airborne: fill in physics from JumpRunup state, then go airborne.
                (State::Airborne { .. }, _) => {
                    if let State::JumpRunup { target_win_id, target_side, landing_mode, .. } = &ch.anim_state {
                        if let Some(win) = windows_wm::find_win(*target_win_id, wins) {
                            let side = *target_side;
                            let lm   = *landing_mode;
                            let (target_cx, target_cy) = airborne_snap_target(
                                &win, side, lm, &ch.assets, ch.char_pos.1,
                            );
                            let dx = target_cx - ch.char_pos.0;
                            let dy = target_cy - ch.char_pos.1;
                            let g  = cfg.jump.gravity * 60.0;
                            let t  = dx.abs().max(1.0) / cfg.jump.air_speed;
                            let vx = dx / t;
                            let vy = ((dy - 0.5 * g * t * t) / t).min(-cfg.jump.min_jump_vy);
                            new_state = State::Airborne {
                                vx, vy,
                                target_win_id: *target_win_id,
                                target_side: side,
                                landing_mode: lm,
                                target_cx,
                                target_cy,
                            };
                        }
                    }
                    Some(Surface::Airborne)
                }
                (State::CornerTransitionSide { side, .. }, Surface::WindowTop { win_id, .. }) =>
                    Some(Surface::WindowUpperCorner { win_id: *win_id, side: *side }),
                (State::CornerTransitionSide { side, .. }, Surface::WindowWall { win_id, .. }) =>
                    Some(Surface::WindowUpperCorner { win_id: *win_id, side: *side }),
                (State::ClimbingDown { .. }, Surface::WindowUpperCorner { win_id, side }) => {
                    let y_local = assets.size("s-hang-wall-0", false).1 / 2.0;
                    Some(Surface::WindowWall { win_id: *win_id, side: *side, y_local })
                }
                (State::Walking { .. } | State::Running { .. }, Surface::WindowUpperCorner { win_id, side }) => {
                    let walk_w   = assets.size("s-walk-0", false).0;
                    let x_offset = walk_w / 2.0 + 3.0;
                    let x = match side {
                        Side::Left  => x_offset,
                        Side::Right => windows_wm::find_win(*win_id, wins)
                            .map(|w| w.w - x_offset)
                            .unwrap_or(400.0 - x_offset),
                    };
                    Some(Surface::WindowTop { win_id: *win_id, x_local: x })
                }
                (State::WallEntry { .. }, Surface::Desktop { .. })
                | (State::WallEntry { .. }, Surface::WindowTop { .. })
                | (State::WallEntry { .. }, Surface::WindowUpperCorner { .. }) => {
                    if let State::JumpRunup { target_win_id, target_side, landing_mode, .. } = &ch.anim_state {
                        if let Some(win) = windows_wm::find_win(*target_win_id, wins) {
                            let side   = *target_side;
                            let hang_h = assets.size("s-hang-wall-0", false).1;
                            let stand_w = assets.size("s-stand", false).0;
                            match landing_mode {
                                LandingMode::TopLanding => {
                                    let x_local = match side {
                                        Side::Left  => stand_w / 2.0 + 4.0,
                                        Side::Right => win.w - stand_w / 2.0 - 4.0,
                                    };
                                    ch.char_pos.0 = win.x + x_local;
                                    ch.char_pos.1 = win.y;
                                    ch.facing = match side {
                                        Side::Left  => Dir::Right,
                                        Side::Right => Dir::Left,
                                    };
                                    new_state = State::Observing { elapsed: 0.0, duration: 3.0 };
                                    Some(Surface::WindowTop { win_id: win.id, x_local })
                                }
                                LandingMode::ClimbFromCurrent => {
                                    let cur_y = ch.char_pos.1;
                                    let y_local = (cur_y - win.y).clamp(hang_h / 2.0, win.h - 4.0);
                                    ch.char_pos.0 = match side {
                                        Side::Right => win.right() - stand_w,
                                        Side::Left  => win.x,
                                    };
                                    ch.char_pos.1 = win.y + y_local;
                                    ch.facing = match side {
                                        Side::Left  => Dir::Right,
                                        Side::Right => Dir::Left,
                                    };
                                    Some(Surface::WindowWall { win_id: win.id, side, y_local })
                                }
                                LandingMode::ClimbFromBottom => {
                                    let y_local = (win.h - hang_h / 2.0).clamp(hang_h / 2.0, win.h - 4.0);
                                    ch.char_pos.0 = match side {
                                        Side::Right => win.right() - stand_w,
                                        Side::Left  => win.x,
                                    };
                                    ch.char_pos.1 = win.y + y_local;
                                    ch.facing = match side {
                                        Side::Left  => Dir::Right,
                                        Side::Right => Dir::Left,
                                    };
                                    Some(Surface::WindowWall { win_id: win.id, side, y_local })
                                }
                            }
                        } else { None }
                    } else { None }
                }
                // ClimbingDown reached the wall bottom: step onto WindowBottom.
                (State::Walking { dir, .. } | State::Running { dir, .. }, Surface::WindowWall { win_id, side, y_local }) => {
                    if let Some(win) = windows_wm::find_win(*win_id, wins) {
                        if *y_local >= win.h - 4.0 {
                            let corner_offset = sprite_w / 2.0 + 4.0;
                            let x_local = match side {
                                Side::Left  => corner_offset,
                                Side::Right => win.w - corner_offset,
                            };
                            ch.char_pos.0 = win.x + x_local;
                            ch.char_pos.1 = win.bottom();
                            ch.facing = *dir;
                            Some(Surface::WindowBottom { win_id: *win_id, x_local })
                        } else { None }
                    } else { None }
                }
                _ => None,
            };
            if let Some(ns) = new_surface { ch.surface = ns; }

            // Sync facing for wall / corner-transition states.
            if matches!(&new_state, State::ClimbingUp { .. } | State::ClimbingDown { .. }) {
                if let Surface::WindowWall { side, .. }
                     | Surface::WindowUpperCorner { side, .. } = &ch.surface
                {
                    ch.facing = match side {
                        Side::Left  => Dir::Right,
                        Side::Right => Dir::Left,
                    };
                }
            }
            if let State::CornerTransitionSide { side, .. } = &new_state {
                ch.facing = match side {
                    Side::Left  => Dir::Right,
                    Side::Right => Dir::Left,
                };
            }
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

// ---- Language detection ----

/// Detect the OS preferred UI language, returning `"ja"` or `"en"`.
///
/// Uses `GetUserPreferredUILanguages` and returns `"ja"` if the first
/// language whose BCP-47 tag starts with `"ja"` appears before any English
/// tag.  Falls back to `"en"`.
fn detect_system_language() -> String {
    use windows_sys::Win32::Globalization::GetUserPreferredUILanguages;
    use windows_sys::Win32::Foundation::FALSE;
    const MUI_LANGUAGE_NAME: u32 = 0x08;
    unsafe {
        let mut num_langs: u32 = 0;
        let mut buf_size: u32  = 0;
        // First call: get required buffer size.
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME, &mut num_langs, std::ptr::null_mut(), &mut buf_size,
        );
        if buf_size == 0 {
            return "en".to_owned();
        }
        let mut buf: Vec<u16> = vec![0u16; buf_size as usize];
        let ok = GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME, &mut num_langs, buf.as_mut_ptr(), &mut buf_size,
        );
        if ok == FALSE || buf_size == 0 {
            return "en".to_owned();
        }
        // Buffer is a double-null-terminated list of null-separated strings.
        for segment in buf.split(|&c| c == 0) {
            if segment.is_empty() { continue; }
            let tag = String::from_utf16_lossy(segment);
            if tag.starts_with("ja") { return "ja".to_owned(); }
            if tag.starts_with("en") { return "en".to_owned(); }
        }
    }
    "en".to_owned()
}

// ---- Tick all characters (10 Hz timer callback) ----

fn tick_all() {
    APP.with(|cell| {
        let mut b = cell.borrow_mut();
        let Some(app) = b.as_mut() else { return };

        let si   = windows_wm::screen_info();
        let wins = windows_wm::list_windows(&si);

        let n = app.chars.len();
        for i in 0..n {
            app.chars[i].config.lock().unwrap().reload_if_changed();
            let cfg = app.chars[i].config.lock().unwrap().current.clone();
            tick_char(&mut app.chars[i], &cfg, &si, &wins);
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
                    break;
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
                    let (char_count, ja) = APP.with(|cell| {
                        cell.borrow().as_ref()
                            .map(|app| (app.chars.len(), app.lang == "ja"))
                            .unwrap_or((1, false))
                    });
                    let menu       = CreatePopupMenu();
                    let add_bd_str  = to_wide(if ja { "フトアゴヒゲトカゲを追加" } else { "Add Bearded Dragon" });
                    let add_pt_str  = to_wide(if ja { "クサガメを追加" } else { "Add Pond Turtle" });
                    let remove_str  = to_wide(if ja { "最後のキャラクターを削除" } else { "Remove Last" });
                    let about_str    = to_wide(if ja { "Petit Mates について" } else { "About Petit Mates" });
                    let settings_str = to_wide(if ja { "設定ファイルを開く" } else { "Open Settings File" });
                    let exit_str     = to_wide(if ja { "終了" } else { "Quit" });
                    AppendMenuW(menu, MF_STRING, IDM_ADD_BD, add_bd_str.as_ptr());
                    AppendMenuW(menu, MF_STRING, IDM_ADD_PT, add_pt_str.as_ptr());
                    let remove_flags = if char_count > 1 { MF_STRING } else { MF_STRING | MF_GRAYED };
                    AppendMenuW(menu, remove_flags, IDM_REMOVE_CHAR, remove_str.as_ptr());
                    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
                    AppendMenuW(menu, MF_STRING,    IDM_SETTINGS, settings_str.as_ptr());
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
                crate::user_config::open_in_editor();
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

pub fn run() {
    unsafe {
        // Single-instance guard: create a named mutex. If it already exists
        // (ERROR_ALREADY_EXISTS), another instance is running — exit silently.
        let mutex_name = to_wide("Local\\PetitMatesSingleInstance");
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
        let user_cfg = crate::user_config::load();
        let sprite_size = user_cfg.display.sprite_size as f64;
        let bd_display_w = sprite_size;
        let pt_display_w = sprite_size;
        let bd_mf = manifest::load_from_bytes(windows_assets::embedded::bearded_dragon::MANIFEST_TOML)
            .expect("embedded bearded_dragon manifest.toml is invalid");
        let pt_mf = manifest::load_from_bytes(windows_assets::embedded::pond_turtle::MANIFEST_TOML)
            .expect("embedded pond_turtle manifest.toml is invalid");
        let bd_assets = Rc::new(
            SpriteAssets::load_embedded(windows_assets::embedded::bearded_dragon::SPRITES, &bd_mf, bd_display_w)
                .expect("failed to decode embedded bearded_dragon sprites"),
        );
        let pt_assets = Rc::new(
            SpriteAssets::load_embedded(windows_assets::embedded::pond_turtle::SPRITES, &pt_mf, pt_display_w)
                .expect("failed to decode embedded pond_turtle sprites"),
        );

        // Create both character windows. The first serves as the host for timer+tray.
        let si         = windows_wm::screen_info();
        let weather_handle = crate::weather::spawn(&user_cfg.weather);
        let bd_char    = spawn_char_hwnd(&si, Rc::clone(&bd_assets), bd_config.clone(), "bearded_dragon");
        let pt_char    = spawn_char_hwnd(&si, Rc::clone(&pt_assets), pt_config.clone(), "pond_turtle");
        let host_hwnd  = bd_char.hwnd;

        APP.with(|cell| {
            *cell.borrow_mut() = Some(AppState {
                chars:     vec![bd_char, pt_char],
                bd_assets,
                pt_assets,
                bd_config,
                pt_config,
                debug_menu_char:    0,
                debug_menu_targets: Vec::new(),
                speech_lock_remaining: 0.0,
                speech_cfg: user_cfg.speech,
                speech_tick: Instant::now(),
                font_size: user_cfg.display.font_size as i32,
                lang: user_cfg.display.language.clone()
                    .unwrap_or_else(detect_system_language),
                weather: weather_handle,
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
