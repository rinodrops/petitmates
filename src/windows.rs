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
use std::time::Instant;

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::behavior::{BehaviorContext, BehaviorScript, Dir, Side, State, Surface, Transition};
use crate::config::{make_shared_win, SharedConfig};
use crate::engine::advance_anim;
use crate::manifest;
use crate::rust_behavior::RustBehavior;
use crate::sprite_map::{sprite_for_state, sprite_for_turn};
use crate::windows_assets::{self, Anchor, SpriteAssets};
use crate::windows_wm::{self, ScreenInfo, WinInfo};

// ---- Constants ----

const WM_TRAY: u32 = WM_APP + 1;
const IDM_ABOUT: usize = 1;
const IDM_EXIT: usize = 2;
const TIMER_TICK: usize = 1;

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

struct AppState {
    assets: SpriteAssets,
    config: SharedConfig,
    behavior: Box<dyn BehaviorScript>,
    anim_state: State,
    facing: Dir,
    surface: Surface,
    /// Character position in screen coordinates (top-left of sprite bounding
    /// box).  Updated each tick; used for Airborne physics and safeguard checks.
    char_pos: (f64, f64),
    last_tick: Instant,
    visible: bool,
    /// Cursor offset from sprite top-left at drag start (screen coords).
    /// `Some` while Ctrl+dragging, `None` otherwise.
    drag_offset: Option<(f64, f64)>,
    /// Last rendered sprite top-left in screen coords.
    /// Updated every tick so `WM_LBUTTONDOWN` can compute a correct drag offset
    /// even when `char_pos` holds surface-local coordinates.
    last_screen_pos: (i32, i32),
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
    }
}

// ---- Surface context ----

/// Compute `surface_progress` [0..1], `at_edge`, and `jump_target` for the
/// current surface.  Equivalent to `surface_context` in `macos.rs`.
fn surface_context(
    surface: &Surface,
    char_pos: (f64, f64),
    sprite_w: f64,
    facing: Dir,
    jump_max_dist: f64,
    wins: &[WinInfo],
    si: &ScreenInfo,
) -> (f64, bool, Option<(u32, Side)>) {
    let edge_margin = 2.0;
    match surface {
        Surface::WindowTop { win_id, x_local } => {
            let Some(win) = windows_wm::find_win(*win_id, wins) else {
                return (0.5, false, None);
            };
            let progress = (x_local / win.w).clamp(0.0, 1.0);
            let at_edge  = *x_local <= edge_margin + sprite_w / 2.0
                        || *x_local >= win.w - edge_margin - sprite_w / 2.0;
            (progress, at_edge, None)
        }
        Surface::Desktop { x } => {
            let progress = (x / si.width).clamp(0.0, 1.0);
            let (cx, _)  = char_pos;
            let floor_y  = si.floor_y();
            let jump_target = wins.iter().find_map(|win| {
                if win.bottom() < floor_y - 150.0 { return None; }
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
            let at_edge = *x <= edge_margin + sprite_w / 2.0
                       || *x >= si.width - edge_margin - sprite_w / 2.0;
            (progress, at_edge, jump_target)
        }
        Surface::WindowWall { win_id, y_local, .. } => {
            let Some(win) = windows_wm::find_win(*win_id, wins) else {
                return (0.5, false, None);
            };
            let progress = (y_local / win.h).clamp(0.0, 1.0);
            let at_edge  = *y_local <= edge_margin || *y_local >= win.h - edge_margin;
            (progress, at_edge, None)
        }
        _ => (0.5, false, None),
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

// ---- Tick (10 Hz) ----

fn tick(hwnd: HWND) {
    APP.with(|cell| {
        let mut b = cell.borrow_mut();
        let Some(s) = b.as_mut() else { return };

        // While being dragged, skip the state machine and just render at the
        // position set by WM_MOUSEMOVE.
        if s.drag_offset.is_some() {
            s.last_tick = Instant::now(); // keep dt fresh so release doesn't jump
            let sr = sprite_for_state(&s.anim_state, s.facing);
            let Some(sprite) = s.assets.sprite(sr.name, sr.mirror) else { return };
            let (px, py) = (s.char_pos.0 as i32, s.char_pos.1 as i32);
            let bgra = sprite.bgra.clone();
            unsafe { set_layered_content(hwnd, &bgra, sprite.w, sprite.h, px, py, 200); }
            return;
        }

        // Compute dt, capped to avoid large jumps after pauses.
        let now = Instant::now();
        let dt  = now.duration_since(s.last_tick).as_secs_f64().min(0.1);
        s.last_tick = now;

        // Hot-reload config.
        s.config.lock().unwrap().reload_if_changed();
        let cfg = s.config.lock().unwrap().current.clone();

        // Screen info and window list.
        let si   = windows_wm::screen_info();
        let wins = windows_wm::list_windows(&si);

        // Surface validity check.
        if !windows_wm::surface_still_valid(&s.surface, &wins) {
            let ctx = BehaviorContext {
                state: &s.anim_state, surface: &s.surface,
                elapsed_secs: 0.0, config: &cfg, rng01: 0.0,
                surface_progress: 0.5, facing: s.facing,
                at_edge: false, jump_target: None,
            };
            s.anim_state = s.behavior.on_surface_lost(&ctx);
            s.surface = Surface::Airborne;
        }

        // Advance per-state animation timers.
        let elapsed = advance_anim(&mut s.anim_state, dt, &cfg);

        // Save CG y before position update for swept landing detection.
        let prev_cy = s.char_pos.1;

        // Update char_pos for Airborne / Walking states.
        match &s.anim_state {
            State::Falling { vx, vy } => {
                let (vx, vy) = (*vx, *vy);
                let (cx, cy) = s.char_pos;
                s.char_pos = (cx + vx * dt, cy + vy * dt);
            }
            State::Walking { dir, .. } => {
                let speed = cfg.floor.walk_speed;
                let delta = speed * dt;
                match &mut s.surface {
                    Surface::Desktop { x } => {
                        *x += match dir { Dir::Left => -delta, Dir::Right => delta };
                        let half_w = cfg.display.display_width / 2.0;
                        *x = x.clamp(half_w, si.width - half_w);
                        s.char_pos.0 = *x;
                    }
                    Surface::WindowTop { x_local, .. } => {
                        *x_local += match dir { Dir::Left => -delta, Dir::Right => delta };
                    }
                    _ => {}
                }
            }
            State::ClimbingUp { .. } => {
                if let Surface::WindowWall { y_local, .. } = &mut s.surface {
                    *y_local -= cfg.wall.climb_speed * dt;
                    *y_local = y_local.max(0.0);
                }
            }
            _ => {}
        }
        // ClimbingDown: separate re-borrow required by borrow checker.
        if matches!(&s.anim_state, State::ClimbingDown { .. }) {
            if let Surface::WindowWall { win_id, y_local, .. } = &mut s.surface {
                if let Some(win) = windows_wm::find_win(*win_id, &wins) {
                    *y_local += cfg.wall.climb_speed * dt;
                    *y_local = y_local.min(win.h);
                }
            }
        }

        // Gravity.
        if let State::Falling { vy, .. } = &mut s.anim_state {
            *vy = (*vy + cfg.jump.gravity * 60.0 * dt).min(600.0);
        }

        // Off-screen safeguard.
        {
            let (fw, fh) = s.assets.size("s-stand", false);
            let (cx, cy) = s.char_pos;
            let below = cy > si.height + fh;
            let sides  = cx < -(fw * 3.0) || cx > si.width + fw * 3.0;
            if below || sides {
                let (dx, dy) = startup_drop(&si, &s.assets);
                s.char_pos   = (dx, dy);
                s.surface    = Surface::Airborne;
                s.anim_state = State::Falling { vx: 0.0, vy: 0.0 };
            }
        }

        // Landing detection (swept check).
        if let State::Falling { vy, .. } = &s.anim_state {
            if *vy >= 0.0 {
                let (fw, fh) = s.assets.size("s-jump", false);
                let foot_x    = s.char_pos.0 + fw / 2.0;
                let foot_y    = s.char_pos.1 + fh;
                let floor_y   = si.floor_y();
                let cy_prev   = prev_cy;
                let cy_now    = s.char_pos.1;

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
                            state: &s.anim_state, surface: &new_surface,
                            elapsed_secs: 0.0, config: &cfg, rng01: 0.0,
                            surface_progress: 0.5, facing: s.facing,
                            at_edge: false, jump_target: None,
                        };
                        s.behavior.on_landed(&ctx)
                    };
                    // Snap char_pos so the foot sits exactly on the surface.
                    let (_, stand_h) = s.assets.size("s-stand", false);
                    let stand_anchor = s.assets.anchor("s-stand").unwrap_or(Anchor { x: 0.0, y: 0.0 });
                    let snap_y = match &new_surface {
                        Surface::WindowTop { win_id, .. } =>
                            windows_wm::find_win(*win_id, &wins).map(|w| w.y),
                        Surface::Desktop { .. } => Some(floor_y),
                        _ => None,
                    };
                    if let Some(sy) = snap_y {
                        s.char_pos = (foot_x - fw / 2.0, sy - stand_h + stand_anchor.y);
                    }
                    s.surface    = new_surface;
                    s.anim_state = new_anim;
                }
            }
        }

        // Compute surface_progress, at_edge, jump_target.
        let sr_for_ctx = match &s.anim_state {
            State::TurningAround { elapsed, .. } => {
                let p = (*elapsed / cfg.floor.turn_duration).clamp(0.0, 1.0);
                sprite_for_turn(p, s.facing)
            }
            other => sprite_for_state(other, s.facing),
        };
        let sprite_w = s.assets.size(sr_for_ctx.name, sr_for_ctx.mirror).0;
        let (surface_progress, at_edge, jump_target) = surface_context(
            &s.surface, s.char_pos, sprite_w, s.facing,
            cfg.jump.wall_jump_max_dist, &wins, &si,
        );

        // Save to_dir if TurningAround completes this tick.
        let turn_to_dir = if let State::TurningAround { to_dir, .. } = &s.anim_state {
            Some(*to_dir)
        } else { None };

        // Run behavior state machine.
        let transition = {
            let ctx = BehaviorContext {
                state: &s.anim_state, surface: &s.surface,
                elapsed_secs: elapsed, config: &cfg, rng01: 0.0,
                surface_progress, facing: s.facing, at_edge, jump_target,
            };
            s.behavior.next_state(&ctx)
        };

        match transition {
            Transition::Stay => {}
            Transition::To(new_state) => {
                if let Some(dir) = turn_to_dir {
                    s.facing = dir;
                }
                // When dropping off a wall, seed char_pos from wall position.
                if matches!(&new_state, State::Falling { .. }) {
                    let fall_pos: Option<(f64, f64)> = (|| {
                        let (sw, sh) = s.assets.size("s-jump", false);
                        match &s.surface {
                            Surface::WindowWall { win_id, side, y_local } => {
                                let win = windows_wm::find_win(*win_id, &wins)?;
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
                    if let Some(pos) = fall_pos { s.char_pos = pos; }
                }

                // Keep surface in sync when the new state implies a surface change.
                let new_surface: Option<Surface> = match (&new_state, &s.surface) {
                    (State::Falling { .. }, _) => Some(Surface::Airborne),
                    (State::CornerTransitionSide { side, .. }, Surface::WindowTop { win_id, .. }) =>
                        Some(Surface::WindowUpperCorner { win_id: *win_id, side: *side }),
                    (State::CornerTransitionSide { side, .. }, Surface::WindowWall { win_id, .. }) =>
                        Some(Surface::WindowUpperCorner { win_id: *win_id, side: *side }),
                    (State::ClimbingDown { .. }, Surface::WindowUpperCorner { win_id, side }) => {
                        let y_local = s.assets.size("s-hang-wall-0", false).1 / 2.0;
                        Some(Surface::WindowWall { win_id: *win_id, side: *side, y_local })
                    }
                    (State::Walking { .. }, Surface::WindowUpperCorner { win_id, side }) => {
                        let walk_w  = s.assets.size("s-walk-0", false).0;
                        let x_offset = walk_w / 2.0 + 3.0;
                        let x = match side {
                            Side::Left  => x_offset,
                            Side::Right => windows_wm::find_win(*win_id, &wins)
                                .map(|w| w.w - x_offset)
                                .unwrap_or(400.0 - x_offset),
                        };
                        Some(Surface::WindowTop { win_id: *win_id, x_local: x })
                    }
                    (State::WallEntry { .. }, Surface::Desktop { .. })
                    | (State::WallEntry { .. }, Surface::WindowTop { .. }) => {
                        if let State::JumpRunup { target_win_id, target_side, .. } = &s.anim_state {
                            if let Some(win) = windows_wm::find_win(*target_win_id, &wins) {
                                let side = *target_side;
                                let hang_h = s.assets.size("s-hang-wall-0", false).1;
                                let y_local = (win.h - hang_h / 2.0).clamp(hang_h / 2.0, win.h - 4.0);
                                let stand_w = s.assets.size("s-stand", false).0;
                                s.char_pos.0 = match side {
                                    Side::Right => win.right() - stand_w,
                                    Side::Left  => win.x,
                                };
                                s.char_pos.1 = win.y + y_local;
                                s.facing = match side {
                                    Side::Left  => Dir::Right,
                                    Side::Right => Dir::Left,
                                };
                                Some(Surface::WindowWall { win_id: win.id, side, y_local })
                            } else { None }
                        } else { None }
                    }
                    _ => None,
                };
                if let Some(ns) = new_surface { s.surface = ns; }

                // Sync facing for wall / corner-transition states.
                if matches!(&new_state, State::ClimbingUp { .. } | State::ClimbingDown { .. }) {
                    if let Surface::WindowWall { side, .. }
                         | Surface::WindowUpperCorner { side, .. } = &s.surface
                    {
                        s.facing = match side {
                            Side::Left  => Dir::Right,
                            Side::Right => Dir::Left,
                        };
                    }
                }
                if let State::CornerTransitionSide { side, .. } = &new_state {
                    s.facing = match side {
                        Side::Left  => Dir::Right,
                        Side::Right => Dir::Left,
                    };
                }
                s.anim_state = new_state;
            }
        }

        // Keep facing in sync with Walking direction.
        if let State::Walking { dir, .. } = &s.anim_state {
            s.facing = *dir;
        }

        // Select sprite.
        let sr = match &s.anim_state {
            State::TurningAround { elapsed, .. } => {
                let p = (*elapsed / cfg.floor.turn_duration).clamp(0.0, 1.0);
                sprite_for_turn(p, s.facing)
            }
            other => sprite_for_state(other, s.facing),
        };

        let Some(sprite) = s.assets.sprite(sr.name, sr.mirror) else { return };
        let (sw, sh) = (sprite.w as f64, sprite.h as f64);

        // Compute screen position.
        let anchor       = s.assets.anchor(sr.name).unwrap_or(Anchor { x: 0.0, y: 0.0 });
        let stand_anchor_y = s.assets.anchor("s-stand").map(|a| a.y).unwrap_or(0.0);
        let (px, py) = surface_to_screen_pos(
            &s.surface, s.char_pos, (sw, sh), anchor, stand_anchor_y, &wins, &si,
        );

        // Hover: check whether cursor is over the sprite.
        let alpha: u8 = unsafe {
            let mut pt = POINT { x: 0, y: 0 };
            let over = GetCursorPos(&mut pt) != 0
                && pt.x >= px && pt.x < px + sprite.w
                && pt.y >= py && pt.y < py + sprite.h;
            if over { cfg.display.hover_alpha.clamp(0.0, 1.0).mul_add(254.0, 1.0) as u8 }
            else    { 255 }
        };

        // Render.
        let bgra = sprite.bgra.clone();
        unsafe {
            set_layered_content(hwnd, &bgra, sprite.w, sprite.h, px, py, alpha);
            if !s.visible {
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                s.visible = true;
            }
        }
        s.last_screen_pos = (px, py);
    });
}

// ---- Window procedure ----

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            // Pass-through by default; capture when Ctrl is held.
            // This replaces WS_EX_TRANSPARENT: we return HTTRANSPARENT normally
            // so clicks reach the window behind us, and HTCLIENT when Ctrl is
            // pressed so our window receives the click for dragging.
            WM_NCHITTEST => {
                let ctrl = GetAsyncKeyState(VK_CONTROL as i32) as u16 & 0x8000 != 0;
                if ctrl { HTCLIENT as LRESULT } else { HTTRANSPARENT as LRESULT }
            }
            WM_LBUTTONDOWN => {
                let mut pt = POINT { x: 0, y: 0 };
                GetCursorPos(&mut pt);
                APP.with(|cell| {
                    if let Some(s) = cell.borrow_mut().as_mut() {
                        // Use last_screen_pos (sprite top-left in screen coords)
                        // rather than char_pos, which may be in surface-local coords
                        // (e.g. x_local on a WindowTop surface).
                        let (lx, ly) = s.last_screen_pos;
                        s.drag_offset = Some((pt.x as f64 - lx as f64, pt.y as f64 - ly as f64));
                        // Seed char_pos with the actual screen top-left so that
                        // the drag render and WM_MOUSEMOVE position math are consistent.
                        s.char_pos   = (lx as f64, ly as f64);
                        s.anim_state  = State::Grabbed;
                        s.surface     = Surface::Airborne;
                    }
                });
                SetCapture(hwnd);
                0
            }
            WM_MOUSEMOVE => {
                let dragging = APP.with(|cell| cell.borrow().as_ref().map(|s| s.drag_offset.is_some()).unwrap_or(false));
                if dragging {
                    let mut pt = POINT { x: 0, y: 0 };
                    GetCursorPos(&mut pt);
                    APP.with(|cell| {
                        if let Some(s) = cell.borrow_mut().as_mut() {
                            if let Some((ox, oy)) = s.drag_offset {
                                s.char_pos = (pt.x as f64 - ox, pt.y as f64 - oy);
                            }
                        }
                    });
                    tick(hwnd);
                }
                0
            }
            WM_LBUTTONUP => {
                let was_dragging = APP.with(|cell| cell.borrow().as_ref().map(|s| s.drag_offset.is_some()).unwrap_or(false));
                if was_dragging {
                    ReleaseCapture();
                    APP.with(|cell| {
                        if let Some(s) = cell.borrow_mut().as_mut() {
                            s.drag_offset = None;
                            let si   = windows_wm::screen_info();
                            let wins = windows_wm::list_windows(&si);
                            let sr   = sprite_for_state(&s.anim_state, s.facing);
                            let (sw, sh) = s.assets.size(sr.name, sr.mirror);
                            let anchor_cx = s.char_pos.0 + sw / 2.0;
                            let anchor_cy = s.char_pos.1 + sh;
                            let new_surface = windows_wm::find_surface_for_drop(
                                anchor_cx, anchor_cy, &wins, &si,
                            ).unwrap_or_else(|| {
                                Surface::Desktop { x: anchor_cx.clamp(sw / 2.0, si.width - sw / 2.0) }
                            });
                            let cfg = s.config.lock().unwrap().current.clone();
                            let ctx = BehaviorContext {
                                state: &State::Grabbed, surface: &new_surface,
                                elapsed_secs: 0.0, config: &cfg, rng01: 0.0,
                                surface_progress: 0.5, facing: s.facing,
                                at_edge: false, jump_target: None,
                            };
                            s.anim_state = s.behavior.on_landed(&ctx);
                            s.surface    = new_surface;
                        }
                    });
                    tick(hwnd);
                }
                0
            }
            WM_TIMER if wp == TIMER_TICK => {
                tick(hwnd);
                0
            }
            WM_TRAY => {
                if (lp as u32) & 0xFFFF == WM_RBUTTONUP {
                    let menu       = CreatePopupMenu();
                    let about_str  = to_wide("About Petit Mates");
                    let exit_str   = to_wide("Quit");
                    AppendMenuW(menu, MF_STRING,    IDM_ABOUT, about_str.as_ptr());
                    AppendMenuW(menu, MF_SEPARATOR, 0,         ptr::null());
                    AppendMenuW(menu, MF_STRING,    IDM_EXIT,  exit_str.as_ptr());
                    let mut pt = POINT { x: 0, y: 0 };
                    GetCursorPos(&mut pt);
                    SetForegroundWindow(hwnd);
                    TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, ptr::null());
                    DestroyMenu(menu);
                }
                0
            }
            WM_COMMAND if (wp & 0xFFFF) == IDM_ABOUT => {
                let text  = to_wide("Petit Mates\r\nVersion 0.1.0\r\n\r\nA desktop accessory by eMotionGraphics.");
                let title = to_wide("About Petit Mates");
                MessageBoxW(ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONINFORMATION);
                0
            }
            WM_COMMAND if (wp & 0xFFFF) == IDM_EXIT => {
                PostQuitMessage(0);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            WM_SETTINGCHANGE => {
                update_tray_icon(hwnd);
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
        let hicon = LoadImageW(hinstance, icon_id as *const u16, IMAGE_ICON, 16, 16, LR_SHARED) as HICON;
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
        let hicon = LoadImageW(hinstance, icon_id as *const u16, IMAGE_ICON, 16, 16, LR_SHARED) as HICON;
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
        let hinstance   = GetModuleHandleW(ptr::null());
        let class_name  = to_wide("PetitMatesOverlay");

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

        let hwnd = CreateWindowExW(
            // No WS_EX_TRANSPARENT: we handle pass-through dynamically in WM_NCHITTEST.
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            class_name.as_ptr(),
            ptr::null(),
            WS_POPUP,
            0, 0, 1, 1,
            ptr::null_mut(), ptr::null_mut(), hinstance, ptr::null(),
        );
        assert!(!hwnd.is_null(), "CreateWindowExW failed");

        // Load assets from embedded bytes (sprite PNGs + manifest.toml are
        // compiled into the exe by build.rs via include_bytes!).
        let config    = make_shared_win();
        let display_w = config.lock().unwrap().current.display.display_width;
        let mf        = manifest::load_from_bytes(windows_assets::embedded::MANIFEST_TOML)
            .expect("embedded manifest.toml is invalid");
        let assets    = SpriteAssets::load_embedded(&mf, display_w)
            .expect("failed to decode embedded sprites");

        // Startup drop position.
        let si            = windows_wm::screen_info();
        let (sx, sy)      = startup_drop(&si, &assets);

        // Place the window off-screen initially; tick() will position it.
        let init_sprite = assets.sprite("s-stand", false).expect("s-stand.png missing");
        set_layered_content(hwnd, &init_sprite.bgra, init_sprite.w, init_sprite.h, -4096, -4096, 255);

        APP.with(|cell| {
            *cell.borrow_mut() = Some(AppState {
                assets,
                config,
                behavior:    Box::new(RustBehavior::new()),
                anim_state:  State::Falling { vx: 0.0, vy: 0.0 },
                facing:      Dir::Left,
                surface:     Surface::Airborne,
                char_pos:    (sx, sy),
                last_tick:   Instant::now(),
                visible:     false,
                drag_offset: None,
                last_screen_pos: (-4096, -4096),
            });
        });

        add_tray_icon(hwnd, hinstance);
        SetTimer(hwnd, TIMER_TICK, 100, None);

        // Message loop.
        let mut msg: MSG = mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        remove_tray_icon(hwnd);
    }
}
