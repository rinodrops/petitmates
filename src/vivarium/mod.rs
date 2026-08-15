//! Owned glass-cage Vivarium: layout math, rest behavior, and framebuffer compose.

mod compose;
mod look;

pub use compose::{compose, composite_layers, static_layers, Framebuffer, MateBlit};
#[cfg(windows)]
pub use compose::bgra_premul_to_rgba;
pub use look::{LookConfig, LookLoader};
#[cfg(windows)]
pub use compose::rgba_to_bgra_premul;

/// Character belongs to the desktop (Free Roam) or the cage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Affiliation {
    #[default]
    Desktop,
    Vivarium,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ZOrder {
    Desktop,
    #[default]
    Normal,
    Front,
}

impl ZOrder {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Normal => "normal",
            Self::Front => "front",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackKind {
    Solid,
    #[default]
    Gradient,
    Image,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct BackConfig {
    pub kind: BackKind,
    /// CSS-like hex `#rrggbb` or `#rrggbbaa`.
    pub color: String,
    pub color2: String,
    pub image: String,
    pub opacity: f64,
}

impl Default for BackConfig {
    fn default() -> Self {
        Self {
            kind: BackKind::Gradient,
            color: "#000000".into(),
            color2: "#000000".into(),
            image: String::new(),
            // Extra back dim (black). 0 = glass-only; raise to darken further.
            opacity: 0.0,
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct VivariumConfig {
    /// Show the owned cage window.
    pub enabled: bool,
    pub z_order: ZOrder,
    pub display_scale: f64,
    pub character_height: u32,
    pub logical_w: u32,
    pub logical_h: u32,
    /// Native window origin. macOS: bottom-left (Y up). Windows: top-left (Y down).
    /// Written by ⌘/Ctrl-drag; omitted until the cage has been moved.
    pub origin_x: Option<f64>,
    pub origin_y: Option<f64>,
    pub grid: u32,
    pub back: BackConfig,
}

pub const GRID: u32 = 16;
pub const MIN_LOGICAL_W: u32 = 320;
pub const MIN_LOGICAL_H: u32 = 240;
pub const MAX_LOGICAL_W: u32 = 1920;
pub const MAX_LOGICAL_H: u32 = 1280;

/// Side/bottom glass thickness in logical pixels (wall light loss).
pub const WALL_GLASS_W: u32 = 32;
pub const WALL_GLASS_H: u32 = 32;
pub const TOP_EDGE_H: u32 = 4;

impl Default for VivariumConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            z_order: ZOrder::Normal,
            display_scale: 0.5,
            character_height: 72,
            logical_w: 960,
            logical_h: 640,
            origin_x: None,
            origin_y: None,
            grid: GRID,
            back: BackConfig::default(),
        }
    }
}

impl VivariumConfig {
    pub fn clamp(&mut self) {
        let g = self.grid.max(1);
        self.display_scale = self.display_scale.clamp(0.25, 2.0);
        self.character_height = self.character_height.clamp(32, 150);
        self.logical_w = snap_dim(self.logical_w, g, MIN_LOGICAL_W, MAX_LOGICAL_W);
        self.logical_h = snap_dim(self.logical_h, g, MIN_LOGICAL_H, MAX_LOGICAL_H);
        self.back.opacity = self.back.opacity.clamp(0.0, 1.0);
    }

    pub fn pixel_size(&self) -> (u32, u32) {
        let s = self.display_scale;
        (
            ((self.logical_w as f64) * s).round().max(1.0) as u32,
            ((self.logical_h as f64) * s).round().max(1.0) as u32,
        )
    }
}

pub fn snap_dim(v: u32, grid: u32, min: u32, max: u32) -> u32 {
    let g = grid.max(1);
    let clamped = v.clamp(min, max);
    let snapped = (clamped / g) * g;
    snapped.max(min)
}

/// Interior rectangle in logical pixels (inside the thick glass bands).
#[derive(Debug, Clone, Copy)]
pub struct InnerRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

pub const EDGE_HIT_PX: f64 = 12.0;
pub const EDGE_LEFT: u8 = 1;
pub const EDGE_RIGHT: u8 = 2;
pub const EDGE_TOP: u8 = 4;
pub const EDGE_BOTTOM: u8 = 8;

/// Window-local hit test. `y_up` is true for macOS (origin at bottom).
pub fn hit_resize_edges(lx: f64, ly: f64, w: f64, h: f64, y_up: bool) -> u8 {
    let mut e = 0u8;
    if lx <= EDGE_HIT_PX {
        e |= EDGE_LEFT;
    }
    if lx >= w - EDGE_HIT_PX {
        e |= EDGE_RIGHT;
    }
    if y_up {
        if ly <= EDGE_HIT_PX {
            e |= EDGE_BOTTOM;
        }
        if ly >= h - EDGE_HIT_PX {
            e |= EDGE_TOP;
        }
    } else if ly <= EDGE_HIT_PX {
        e |= EDGE_TOP;
    } else if ly >= h - EDGE_HIT_PX {
        e |= EDGE_BOTTOM;
    }
    e
}

/// Apply an edge drag in screen points. `scale` is points per logical pixel.
/// `y_up`: macOS window origin is bottom-left.
pub fn resize_from_drag(
    edge: u8,
    dx: f64,
    dy: f64,
    orig_w: u32,
    orig_h: u32,
    orig_ox: f64,
    orig_oy: f64,
    scale: f64,
    y_up: bool,
    grid: u32,
) -> (u32, u32, f64, f64) {
    let s = scale.max(0.05);
    let mut w = orig_w as f64;
    let mut h = orig_h as f64;
    if edge & EDGE_RIGHT != 0 {
        w += dx / s;
    }
    if edge & EDGE_LEFT != 0 {
        w -= dx / s;
    }
    if y_up {
        if edge & EDGE_TOP != 0 {
            h += dy / s;
        }
        if edge & EDGE_BOTTOM != 0 {
            h -= dy / s;
        }
    } else {
        if edge & EDGE_BOTTOM != 0 {
            h += dy / s;
        }
        if edge & EDGE_TOP != 0 {
            h -= dy / s;
        }
    }
    let new_w = snap_dim(w.round().max(0.0) as u32, grid, MIN_LOGICAL_W, MAX_LOGICAL_W);
    let new_h = snap_dim(h.round().max(0.0) as u32, grid, MIN_LOGICAL_H, MAX_LOGICAL_H);
    let mut ox = orig_ox;
    let mut oy = orig_oy;
    if edge & EDGE_LEFT != 0 {
        ox = orig_ox + (orig_w as f64 - new_w as f64) * s;
    }
    if y_up {
        if edge & EDGE_BOTTOM != 0 {
            oy = orig_oy + (orig_h as f64 - new_h as f64) * s;
        }
    } else if edge & EDGE_TOP != 0 {
        oy = orig_oy + (orig_h as f64 - new_h as f64) * s;
    }
    (new_w, new_h, ox, oy)
}

pub fn clamp_vivi_x(x: f64, inner: InnerRect, sprite_w: f64) -> f64 {
    let min_x = inner.x as f64 + sprite_w / 2.0;
    let max_x = (inner.x + inner.w) as f64 - sprite_w / 2.0;
    x.clamp(min_x.min(max_x), min_x.max(max_x))
}

pub fn inner_rect(logical_w: u32, logical_h: u32) -> InnerRect {
    let left = WALL_GLASS_W;
    let right = WALL_GLASS_W;
    let top = TOP_EDGE_H;
    let bottom = WALL_GLASS_H;
    let w = logical_w.saturating_sub(left + right).max(1);
    let h = logical_h.saturating_sub(top + bottom).max(1);
    InnerRect { x: left, y: top, w, h }
}

/// macOS `NSWindowLevel` / CG window level for the cage.
pub fn ns_window_level(z: ZOrder) -> isize {
    // kCGDesktopWindowLevel = INT32_MIN + 20 (behind desktop icons)
    // NSNormalWindowLevel = 0
    // NSFloatingWindowLevel = 3
    match z {
        ZOrder::Desktop => (i32::MIN as isize) + 20,
        ZOrder::Normal => 0,
        ZOrder::Front => 3,
    }
}

/// Rest-bias tick for a character inside the cage (short moves, mostly idle).
pub fn tick_rest(
    state: &mut crate::behavior::State,
    facing: &mut crate::behavior::Dir,
    x: &mut f64,
    dt: f64,
    inner: InnerRect,
    sprite_w: f64,
    rng01: f64,
    rng_walk: f64,
) {
    use crate::behavior::{Dir, State};

    let min_x = inner.x as f64 + sprite_w / 2.0;
    let max_x = (inner.x + inner.w) as f64 - sprite_w / 2.0;
    let min_x = min_x.min(max_x);

    let resting = matches!(
        state,
        State::SitIdle { .. } | State::LieIdle { .. } | State::Sleeping { .. }
    );

    if let State::Walking { dir, frame, frame_elapsed } = state {
        let speed = 18.0;
        let sign = match dir {
            Dir::Left => -1.0,
            Dir::Right => 1.0,
        };
        *x = (*x + sign * speed * dt).clamp(min_x, max_x);
        *frame_elapsed += dt;
        if *frame_elapsed > 0.18 {
            *frame_elapsed = 0.0;
            *frame = (*frame + 1) % 4;
        }
        if *x <= min_x + 0.5 {
            *dir = Dir::Right;
            *facing = Dir::Right;
        } else if *x >= max_x - 0.5 {
            *dir = Dir::Left;
            *facing = Dir::Left;
        }
        // Mostly stop after a short stroll.
        if rng_walk < 0.015 {
            *state = pick_rest(rng01);
        }
        return;
    }

    if resting {
        if let State::SitIdle { elapsed, duration, .. }
        | State::LieIdle { elapsed, duration, .. }
        | State::Sleeping { elapsed, duration, .. } = state
        {
            *elapsed += dt;
            if *elapsed > *duration && rng_walk < 0.4 {
                let go_right = rng01 > 0.5;
                *facing = if go_right { Dir::Right } else { Dir::Left };
                *state = State::Walking {
                    dir: *facing,
                    frame: 0,
                    frame_elapsed: 0.0,
                };
            }
        }
        return;
    }

    *state = pick_rest(rng01);
    *x = (*x).clamp(min_x, max_x);
}

fn pick_rest(rng01: f64) -> crate::behavior::State {
    use crate::behavior::State;
    if rng01 < 0.45 {
        State::LieIdle {
            head_front: false,
            elapsed: 0.0,
            duration: 8.0 + rng01 * 14.0,
            head_timer: 0.0,
        }
    } else if rng01 < 0.8 {
        State::SitIdle {
            head_front: false,
            elapsed: 0.0,
            duration: 6.0 + rng01 * 10.0,
            head_timer: 0.0,
        }
    } else {
        State::Sleeping {
            elapsed: 0.0,
            duration: 12.0 + rng01 * 18.0,
            head_front: false,
            head_timer: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_aligns_to_grid() {
        assert_eq!(snap_dim(970, 16, MIN_LOGICAL_W, MAX_LOGICAL_W), 960);
        assert_eq!(snap_dim(10, 16, MIN_LOGICAL_W, MAX_LOGICAL_W), MIN_LOGICAL_W);
    }

    #[test]
    fn inner_fits_inside_walls() {
        let r = inner_rect(960, 640);
        assert_eq!(r.x, WALL_GLASS_W);
        assert_eq!(r.w, 960 - WALL_GLASS_W * 2);
        assert_eq!(r.h, 640 - TOP_EDGE_H - WALL_GLASS_H);
    }

    #[test]
    fn resize_right_grows_width_keeps_origin() {
        let (w, h, ox, oy) = resize_from_drag(
            EDGE_RIGHT, 8.0, 0.0, 960, 640, 100.0, 50.0, 0.5, true, 16,
        );
        assert_eq!(w, 976);
        assert_eq!(h, 640);
        assert_eq!(ox, 100.0);
        assert_eq!(oy, 50.0);
    }

    #[test]
    fn resize_left_keeps_right_edge() {
        let (w, _h, ox, _oy) = resize_from_drag(
            EDGE_LEFT, -8.0, 0.0, 960, 640, 100.0, 50.0, 0.5, true, 16,
        );
        assert_eq!(w, 976);
        assert!((ox - 92.0).abs() < 0.01);
    }
}
