//! Platform-independent terrestrial physics.
//!
//! Shared geometry types (`WinInfo`, `PhysicsScreen`), window-query helpers,
//! surface-context computation, and physics-tick step functions used by both
//! `macos.rs` and `windows.rs`.  No platform-specific imports here.

use rand::{Rng, SeedableRng};

use crate::behavior::{AquaticState, Dir, LandingMode, Side, State, Surface};
use crate::config::Config;

// ---- Shared geometry ----

/// A platform window's bounding rectangle in screen coordinates
/// (origin = top-left, Y increases downward on both platforms).
///
/// Structurally identical on macOS (`wm::WinInfo`) and Windows
/// (`windows_wm::WinInfo`); both modules re-export this type.
#[derive(Debug, Clone, Copy)]
pub struct WinInfo {
    pub id: u32,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl WinInfo {
    pub fn right(&self) -> f64 { self.x + self.w }
    pub fn bottom(&self) -> f64 { self.y + self.h }
}

/// Screen geometry as seen by the physics layer.
/// Each platform derives this from its own `ScreenInfo`.
#[derive(Debug, Clone, Copy)]
pub struct PhysicsScreen {
    pub width: f64,
    pub height: f64,
    /// Y coordinate of the desktop floor (top of Dock / taskbar).
    pub floor_y: f64,
}

// ---- Window queries ----

pub fn find_win(id: u32, wins: &[WinInfo]) -> Option<&WinInfo> {
    wins.iter().find(|w| w.id == id)
}

pub fn surface_still_valid(surface: &Surface, wins: &[WinInfo]) -> bool {
    match surface {
        Surface::Desktop { .. } | Surface::Airborne => true,
        Surface::WindowTop { win_id, .. }
        | Surface::WindowWall { win_id, .. }
        | Surface::WindowUpperCorner { win_id, .. }
        | Surface::WindowBottom { win_id, .. }
        | Surface::WindowInterior { win_id, .. } => find_win(*win_id, wins).is_some(),
    }
}

// ---- Surface context ----

/// Compute `surface_progress`, `at_edge`, `jump_target`, and `attract_target`
/// for the current surface.
pub fn surface_context(
    surface: &Surface,
    sprite_w: f64,
    facing: Dir,
    jump_max_dist: f64,
    jump_floor_margin: f64,
    attract_dist: f64,
    corner_attract_dist: f64,
    wins: &[WinInfo],
    si: &PhysicsScreen,
) -> (f64, bool, Option<(u32, Side)>, Option<(u32, Side, LandingMode)>) {
    let edge_margin = 2.0;
    match surface {
        Surface::WindowTop { win_id, x_local } => {
            let Some(win) = find_win(*win_id, wins) else {
                return (0.5, false, None, None);
            };
            let progress = (x_local / win.w).clamp(0.0, 1.0);
            let at_edge = *x_local <= edge_margin + sprite_w / 2.0
                || *x_local >= win.w - edge_margin - sprite_w / 2.0;
            (progress, at_edge, None, None)
        }
        Surface::WindowUpperCorner { win_id, side } => {
            let Some(_win) = find_win(*win_id, wins) else {
                return (0.5, false, None, None);
            };
            let win = _win;
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
            let cx      = *x;
            let floor_y = si.floor_y;
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
            let Some(win) = find_win(*win_id, wins) else {
                return (0.5, false, None, None);
            };
            let progress = (y_local / win.h).clamp(0.0, 1.0);
            let at_edge = *y_local <= edge_margin || *y_local >= win.h - edge_margin;
            (progress, at_edge, None, None)
        }
        Surface::WindowBottom { win_id, x_local } => {
            let Some(win) = find_win(*win_id, wins) else {
                return (0.5, false, None, None);
            };
            let progress = (x_local / win.w).clamp(0.0, 1.0);
            let at_edge = *x_local <= edge_margin + sprite_w / 2.0
                || *x_local >= win.w - edge_margin - sprite_w / 2.0;
            (progress, at_edge, None, None)
        }
        _ => (0.5, false, None, None),
    }
}

// ---- Spawn / jump helpers ----

/// Random spawn X within the center 80% of the screen, at `start_y`.
///
/// `start_y` is platform-specific:
/// - macOS: `si.menu_bar_height` (just below the menu bar)
/// - Windows: `-stand_h` (completely above the screen top)
pub fn startup_drop(si: &PhysicsScreen, stand_w: f64, start_y: f64) -> (f64, f64) {
    let margin = si.width * 0.10;
    let usable = (si.width - margin * 2.0 - stand_w).max(0.0);
    let offset = rand::rngs::SmallRng::from_os_rng().random::<f64>() * usable;
    (margin + offset, start_y)
}

/// Precompute the horizontal snap X and approximate target Y for an
/// `Airborne` jump.  Sizes are passed explicitly to keep this function
/// platform-independent.
pub fn airborne_snap_target(
    win: &WinInfo,
    side: Side,
    landing_mode: LandingMode,
    hang_h: f64,
    stand_w: f64,
    current_cy: f64,
) -> (f64, f64) {
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

fn compute_jump_physics(
    launch: (f64, f64),
    target: (f64, f64),
    air_speed: f64,
    gravity: f64,
    min_jump_vy: f64,
) -> (f64, f64) {
    let dx = target.0 - launch.0;
    let dy = target.1 - launch.1;
    let g  = gravity * 60.0;
    let t  = dx.abs().max(1.0) / air_speed;
    let vx = dx / t;
    let vy = ((dy - 0.5 * g * t * t) / t).min(-min_jump_vy);
    (vx, vy)
}

// ---- Physics tick steps ----

/// Advance surface-local positions and `pos` (the free-flight CG coordinate)
/// for the current animation state.
///
/// For Desktop walking, `pos.0` is kept in sync with `Surface::Desktop { x }`
/// so both platforms see the same value.
pub fn integrate_velocity(
    anim_state: &State,
    surface: &mut Surface,
    pos: &mut (f64, f64),
    cfg: &Config,
    si: &PhysicsScreen,
    sprite_size: f64,
    dt: f64,
    wins: &[WinInfo],
) {
    match anim_state {
        State::Falling { vx, vy, .. } | State::Airborne { vx, vy, .. } => {
            pos.0 += vx * dt;
            pos.1 += vy * dt;
        }
        State::Walking { dir, .. } | State::Running { dir, .. } => {
            let speed = if matches!(anim_state, State::Running { .. }) {
                cfg.floor.run_speed
            } else {
                cfg.floor.walk_speed
            };
            let delta = speed * dt;
            match surface {
                Surface::Desktop { x } => {
                    *x += match dir { Dir::Left => -delta, Dir::Right => delta };
                    let half_w = sprite_size / 2.0;
                    *x = x.clamp(half_w, si.width - half_w);
                    pos.0 = *x;
                }
                Surface::WindowTop { x_local, .. }
                | Surface::WindowBottom { x_local, .. } => {
                    *x_local += match dir { Dir::Left => -delta, Dir::Right => delta };
                }
                _ => {}
            }
        }
        State::ClimbingUp { .. } => {
            if let Surface::WindowWall { y_local, .. } = surface {
                *y_local -= cfg.wall.climb_speed * dt;
                *y_local = y_local.max(0.0);
            }
        }
        _ => {}
    }
    // ClimbingDown: separate re-borrow required by the borrow checker.
    if matches!(anim_state, State::ClimbingDown { .. }) {
        if let Surface::WindowWall { win_id, y_local, .. } = surface {
            if let Some(win) = find_win(*win_id, wins) {
                *y_local += cfg.wall.climb_speed * dt;
                *y_local = y_local.min(win.h);
            }
        }
    }
}

/// Apply gravity to `Falling` / `Airborne` states (capped to prevent tunneling).
pub fn apply_gravity(anim_state: &mut State, gravity: f64, dt: f64) {
    if let State::Falling { vy, .. } | State::Airborne { vy, .. } = anim_state {
        *vy = (*vy + gravity * 60.0 * dt).min(600.0);
    }
}

/// When airborne and the character reaches its target X, snap to the wall.
pub fn check_airborne_arrival(
    anim_state: &mut State,
    surface: &mut Surface,
    pos: &mut (f64, f64),
    facing: &mut Dir,
    wins: &[WinInfo],
    hang_h: f64,
    stand_w: f64,
    jump_h: f64,
) {
    let (vx, vy, target_win_id, target_side, landing_mode, target_cx) =
        if let State::Airborne { vx, vy, target_win_id, target_side, landing_mode, target_cx, .. }
            = anim_state
        {
            (*vx, *vy, *target_win_id, *target_side, *landing_mode, *target_cx)
        } else {
            return;
        };

    let arrived = if vx >= 0.0 { pos.0 >= target_cx } else { pos.0 <= target_cx };
    if !arrived { return; }

    if let Some(win) = find_win(target_win_id, wins) {
        match landing_mode {
            LandingMode::TopLanding => {
                let x_local = match target_side {
                    Side::Left  => stand_w / 2.0 + 4.0,
                    Side::Right => win.w - stand_w / 2.0 - 4.0,
                };
                *pos     = (win.x + x_local, win.y);
                *facing  = match target_side { Side::Left => Dir::Right, Side::Right => Dir::Left };
                *surface    = Surface::WindowTop { win_id: win.id, x_local };
                *anim_state = State::Observing { elapsed: 0.0, duration: 3.0 };
            }
            LandingMode::ClimbFromCurrent | LandingMode::ClimbFromBottom => {
                let y_local = (pos.1 + jump_h / 2.0 - win.y).clamp(hang_h / 2.0, win.h - 4.0);
                pos.0 = match target_side {
                    Side::Right => win.right() - stand_w,
                    Side::Left  => win.x,
                };
                pos.1 = win.y + y_local;
                *facing  = match target_side { Side::Left => Dir::Right, Side::Right => Dir::Left };
                *surface    = Surface::WindowWall { win_id: win.id, side: target_side, y_local };
                *anim_state = State::WallEntry { elapsed: 0.0 };
            }
        }
    } else {
        *anim_state = State::Falling { vx, vy: vy.max(0.0), shocked: 0.5 };
    }
}

/// If `pos` is outside screen bounds, reset to a startup-drop position.
/// Returns `true` when a reset was performed.
///
/// `start_y` is platform-specific (see `startup_drop`).
pub fn check_off_screen(
    anim_state: &mut State,
    surface: &mut Surface,
    pos: &mut (f64, f64),
    si: &PhysicsScreen,
    stand_size: (f64, f64),
    start_y: f64,
) -> bool {
    let (fw, fh) = stand_size;
    let (cx, cy) = *pos;
    if cy > si.height + fh || cx < -(fw * 3.0) || cx > si.width + fw * 3.0 {
        *pos        = startup_drop(si, fw, start_y);
        *surface    = Surface::Airborne;
        *anim_state = State::Falling { vx: 0.0, vy: 0.0, shocked: 0.0 };
        true
    } else {
        false
    }
}

/// Information returned by a successful landing detection.
pub struct LandingInfo {
    pub new_surface: Surface,
    /// Horizontal foot center: `pos.0 = foot_x - fall_w / 2.0`
    pub foot_x: f64,
    /// Surface Y for snapping: `pos.1 = surface_y - stand_h + stand_anchor_y`
    pub surface_y: f64,
}

/// Swept-check landing detection for falling characters.
/// Returns `None` when no landing occurred this tick.
pub fn check_landing(
    anim_state: &State,
    prev_cy: f64,
    pos: (f64, f64),
    si: &PhysicsScreen,
    wins: &[WinInfo],
    sprite_size: f64,
    fall_size: (f64, f64),
) -> Option<LandingInfo> {
    let State::Falling { vy, .. } = anim_state else { return None; };
    if *vy < 0.0 { return None; }

    let (fw, fh) = fall_size;
    let foot_x     = pos.0 + fw / 2.0;
    let foot_y_now = pos.1 + fh;
    let floor_y    = si.floor_y;
    let cy_now     = pos.1;

    let landed_win = wins.iter()
        .filter(|win| {
            win.y < floor_y
                && foot_x >= win.x
                && foot_x <= win.right()
                && prev_cy < win.y
                && cy_now + fh >= win.y
        })
        .min_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));
    let landed_floor = landed_win.is_none() && foot_y_now >= floor_y;

    let new_surface = landed_win
        .map(|win| Surface::WindowTop {
            win_id: win.id,
            x_local: (foot_x - win.x).clamp(0.0, win.w),
        })
        .or_else(|| landed_floor.then(|| {
            let half_w = sprite_size / 2.0;
            Surface::Desktop { x: foot_x.clamp(half_w, si.width - half_w) }
        }))?;

    let surface_y = match &new_surface {
        Surface::WindowTop { win_id, .. } => find_win(*win_id, wins).map(|w| w.y)?,
        Surface::Desktop { .. } => floor_y,
        _ => return None,
    };

    Some(LandingInfo { new_surface, foot_x, surface_y })
}

// ---- Transition resolver ----

/// Sprite sizes required for `resolve_transition`.
pub struct TerrestrialSizes {
    pub hang_h:   f64,
    pub stand_w:  f64,
    pub walk_w:   f64,
    pub jump_w:   f64,
    pub jump_h:   f64,
    /// Width of the sprite currently displayed (for wall-bottom corner offset).
    pub sprite_w: f64,
}

/// Resolve the surface / position / facing side-effects of a `Transition::To`
/// (behavior state-machine output).
///
/// Modifies `new_state`, `surface`, `pos`, and `facing` in place.
///
/// `launch_pos` is "where the character currently appears on screen":
/// - macOS: pass `last_screen_pos` (CG top-left of last rendered sprite)
/// - Windows: pass `char_pos`
pub fn resolve_transition(
    new_state: &mut State,
    cur_state: &State,
    surface: &mut Surface,
    pos: &mut (f64, f64),
    facing: &mut Dir,
    cfg: &Config,
    wins: &[WinInfo],
    sz: &TerrestrialSizes,
    water_affinity: f64,
    launch_pos: (f64, f64),
) {
    // Seed pos from wall position when transitioning to Falling from a wall.
    if matches!(&*new_state, State::Falling { .. }) {
        if let Surface::WindowWall { win_id, side, y_local } = &*surface {
            if let Some(win) = find_win(*win_id, wins) {
                *pos = (
                    match side { Side::Right => win.right() - sz.jump_w, Side::Left => win.x },
                    win.y + y_local - sz.jump_h / 2.0,
                );
            }
        }
    }

    let new_surface: Option<Surface> = match (&*new_state, &*surface) {
        (State::Falling { .. }, _) => Some(Surface::Airborne),

        (State::Airborne { .. }, _) => {
            if let State::JumpRunup { target_win_id, target_side, landing_mode, .. } = cur_state {
                if let Some(win) = find_win(*target_win_id, wins) {
                    let side = *target_side;
                    let lm   = *landing_mode;
                    let (target_cx, target_cy) = airborne_snap_target(
                        win, side, lm, sz.hang_h, sz.stand_w, launch_pos.1,
                    );
                    let (vx, vy) = compute_jump_physics(
                        launch_pos, (target_cx, target_cy),
                        cfg.jump.air_speed, cfg.jump.gravity, cfg.jump.min_jump_vy,
                    );
                    *new_state = State::Airborne {
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

        (State::CornerTransitionSide { side, .. }, Surface::WindowTop { win_id, .. })
        | (State::CornerTransitionSide { side, .. }, Surface::WindowWall { win_id, .. }) =>
            Some(Surface::WindowUpperCorner { win_id: *win_id, side: *side }),

        (State::ClimbingDown { .. }, Surface::WindowUpperCorner { win_id, side }) =>
            Some(Surface::WindowWall { win_id: *win_id, side: *side, y_local: sz.hang_h / 2.0 }),

        (State::Walking { .. } | State::Running { .. }, Surface::WindowUpperCorner { win_id, side }) => {
            let x_offset = sz.walk_w / 2.0 + 3.0;
            let x = match side {
                Side::Left  => x_offset,
                Side::Right => find_win(*win_id, wins)
                    .map(|w| w.w - x_offset)
                    .unwrap_or(400.0 - x_offset),
            };
            Some(Surface::WindowTop { win_id: *win_id, x_local: x })
        }

        (State::WallEntry { .. }, Surface::Desktop { .. })
        | (State::WallEntry { .. }, Surface::WindowTop { .. })
        | (State::WallEntry { .. }, Surface::WindowUpperCorner { .. }) => {
            if let State::JumpRunup { target_win_id, target_side, landing_mode, .. } = cur_state {
                resolve_wall_entry(
                    new_state, pos, facing, wins, *target_win_id, *target_side, *landing_mode,
                    sz.hang_h, sz.stand_w, launch_pos.1,
                )
            } else {
                None
            }
        }

        (State::Walking { dir, .. } | State::Running { dir, .. },
         Surface::WindowWall { win_id, side, y_local }) => {
            resolve_wall_bottom(
                new_state, pos, facing, wins, *win_id, *side, *y_local, *dir,
                sz.jump_w, sz.jump_h, sz.sprite_w, water_affinity,
            )
        }

        _ => None,
    };

    if let Some(ns) = new_surface {
        *surface = ns;
    }

    // Sync facing for climbing and corner-transition states.
    if matches!(&*new_state, State::ClimbingUp { .. } | State::ClimbingDown { .. }) {
        if let Surface::WindowWall { side, .. } | Surface::WindowUpperCorner { side, .. } = &*surface {
            *facing = match side { Side::Left => Dir::Right, Side::Right => Dir::Left };
        }
    }
    if let State::CornerTransitionSide { side, .. } = &*new_state {
        *facing = match side { Side::Left => Dir::Right, Side::Right => Dir::Left };
    }
}

fn resolve_wall_entry(
    new_state: &mut State,
    pos: &mut (f64, f64),
    facing: &mut Dir,
    wins: &[WinInfo],
    target_win_id: u32,
    target_side: Side,
    landing_mode: LandingMode,
    hang_h: f64,
    stand_w: f64,
    launch_cy: f64,
) -> Option<Surface> {
    let win  = find_win(target_win_id, wins)?;
    let side = target_side;
    match landing_mode {
        LandingMode::TopLanding => {
            let x_local = match side {
                Side::Left  => stand_w / 2.0 + 4.0,
                Side::Right => win.w - stand_w / 2.0 - 4.0,
            };
            pos.0 = win.x + x_local;
            pos.1 = win.y;
            *facing    = match side { Side::Left => Dir::Right, Side::Right => Dir::Left };
            *new_state = State::Observing { elapsed: 0.0, duration: 3.0 };
            Some(Surface::WindowTop { win_id: win.id, x_local })
        }
        LandingMode::ClimbFromCurrent => {
            let y_local = (launch_cy - win.y).clamp(hang_h / 2.0, win.h - 4.0);
            pos.0 = match side { Side::Right => win.right() - stand_w, Side::Left => win.x };
            pos.1 = win.y + y_local;
            *facing = match side { Side::Left => Dir::Right, Side::Right => Dir::Left };
            Some(Surface::WindowWall { win_id: win.id, side, y_local })
        }
        LandingMode::ClimbFromBottom => {
            let y_local = (win.h - hang_h / 2.0).clamp(hang_h / 2.0, win.h - 4.0);
            pos.0 = match side { Side::Right => win.right() - stand_w, Side::Left => win.x };
            pos.1 = win.y + y_local;
            *facing = match side { Side::Left => Dir::Right, Side::Right => Dir::Left };
            Some(Surface::WindowWall { win_id: win.id, side, y_local })
        }
    }
}

fn resolve_wall_bottom(
    new_state: &mut State,
    pos: &mut (f64, f64),
    facing: &mut Dir,
    wins: &[WinInfo],
    win_id: u32,
    side: Side,
    y_local: f64,
    dir: Dir,
    jump_w: f64,
    jump_h: f64,
    sprite_w: f64,
    water_affinity: f64,
) -> Option<Surface> {
    let win = find_win(win_id, wins)?;
    if y_local < win.h - 4.0 { return None; }
    if water_affinity > 0.0 {
        let corner_offset = sprite_w / 2.0 + 4.0;
        let x_local = match side {
            Side::Left  => corner_offset,
            Side::Right => win.w - corner_offset,
        };
        pos.0 = win.x + x_local;
        pos.1 = win.bottom();
        *facing = dir;
        Some(Surface::WindowBottom { win_id, x_local })
    } else {
        pos.0 = match side { Side::Left => win.x, Side::Right => win.right() - jump_w };
        pos.1 = win.y + y_local - jump_h / 2.0;
        *new_state = State::Falling { vx: 0.0, vy: 0.0, shocked: 0.0 };
        Some(Surface::Airborne)
    }
}

// ---- Water physics ----

/// Per-tick physics for a character in `PhysicsWorld::Water`.
///
/// `surface` must be `Surface::WindowInterior`; other variants are a no-op.
/// `sprite_half` is half the sprite's display width, used as the wall margin.
pub fn tick_water(
    surface: &mut Surface,
    aq: &mut AquaticState,
    win: &WinInfo,
    cfg: &crate::config::WaterConfig,
    sprite_half: f64,
    dt: f64,
    rng: &mut impl rand::Rng,
) {
    let Surface::WindowInterior { ref mut x_local, ref mut y_local, .. } = *surface else { return };

    // Integrate velocity.
    *x_local += aq.vx * dt;
    *y_local += aq.vy * dt;

    let m = sprite_half.max(4.0);

    // Bounce off walls with a slight random angle perturbation.
    if *x_local < m         { *x_local = m;           aq.vx =  aq.vx.abs()  + rng.gen_range(-8.0..8.0); }
    if *x_local > win.w - m { *x_local = win.w - m;   aq.vx = -aq.vx.abs()  + rng.gen_range(-8.0..8.0); }
    if *y_local < m         { *y_local = m;            aq.vy =  aq.vy.abs()  + rng.gen_range(-8.0..8.0); }
    if *y_local > win.h - m { *y_local = win.h - m;   aq.vy = -aq.vy.abs()  + rng.gen_range(-8.0..8.0); }

    // Thigmotaxis: pull toward the nearest wall when too far from all walls.
    let dist_l = *x_local - m;
    let dist_r = (win.w - m) - *x_local;
    let dist_t = *y_local - m;
    let dist_b = (win.h - m) - *y_local;
    let min_dist = dist_l.min(dist_r).min(dist_t).min(dist_b);

    if min_dist > cfg.thigmotaxis_margin {
        let pull = (min_dist - cfg.thigmotaxis_margin) * 30.0;
        if dist_l <= dist_r && dist_l <= dist_t && dist_l <= dist_b {
            aq.vx -= pull * dt;
        } else if dist_r <= dist_t && dist_r <= dist_b {
            aq.vx += pull * dt;
        } else if dist_t <= dist_b {
            aq.vy -= pull * dt;
        } else {
            aq.vy += pull * dt;
        }
    }

    // Clamp to max speed.
    let speed_sq = aq.vx * aq.vx + aq.vy * aq.vy;
    let max_sp   = cfg.swim_speed;
    if speed_sq > max_sp * max_sp {
        let s = speed_sq.sqrt();
        aq.vx = aq.vx / s * max_sp;
        aq.vy = aq.vy / s * max_sp;
    }

    // Ensure minimum speed so the character never becomes stationary.
    let min_sp = max_sp * 0.25;
    if speed_sq < min_sp * min_sp {
        let angle: f64 = rng.gen_range(0.0..std::f64::consts::TAU);
        aq.vx = angle.cos() * min_sp;
        aq.vy = angle.sin() * min_sp;
    }
}
