//! Owned glass-cage Vivarium: layout math, rest behavior, and framebuffer compose.

mod compose;
mod layout;
mod look;

pub use compose::{
    compose, composite_layers, rotate_rgba_about_foot, scale_rgba_triangle, static_layers,
    Framebuffer, MateBlit,
};
#[cfg(windows)]
pub use compose::bgra_premul_to_rgba;
#[allow(unused_imports)]
pub use layout::{AngleId, FloorKind, FloorSpec, LayoutConfig, LayoutLoader, WaterSpec};
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
    /// Dest width of a `canonical_width` sprite at `display_scale` 1.0 (same formula as Display).
    pub sprite_size: u32,
    pub logical_w: u32,
    pub logical_h: u32,
    /// Native window origin. macOS: bottom-left (Y up). Windows: top-left (Y down).
    /// Written by ⌘/Ctrl-drag; omitted until the cage has been moved.
    pub origin_x: Option<f64>,
    pub origin_y: Option<f64>,
    pub grid: u32,
    pub back: BackConfig,
    /// Device pixels per point. Runtime-only (macOS backingScaleFactor; Windows 1.0).
    #[serde(skip, default = "default_backing_scale")]
    pub backing_scale: f64,
}

fn default_backing_scale() -> f64 {
    1.0
}

pub const GRID: u32 = 16;
pub const MIN_LOGICAL_W: u32 = 320;
pub const MIN_LOGICAL_H: u32 = 240;
pub const MAX_LOGICAL_W: u32 = 1920;
pub const MAX_LOGICAL_H: u32 = 1280;

/// Side/bottom glass thickness in logical pixels (wall light loss).
pub const WALL_GLASS_W: u32 = 16;
pub const WALL_GLASS_H: u32 = 16;
pub const TOP_EDGE_H: u32 = 4;

impl Default for VivariumConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            z_order: ZOrder::Normal,
            display_scale: 0.5,
            sprite_size: 150,
            logical_w: 960,
            logical_h: 640,
            origin_x: None,
            origin_y: None,
            grid: GRID,
            back: BackConfig::default(),
            backing_scale: 1.0,
        }
    }
}

impl VivariumConfig {
    pub fn clamp(&mut self) {
        let g = self.grid.max(1);
        self.display_scale = self.display_scale.clamp(0.25, 2.0);
        self.sprite_size = self.sprite_size.clamp(80, 300);
        self.logical_w = snap_dim(self.logical_w, g, MIN_LOGICAL_W, MAX_LOGICAL_W);
        self.logical_h = snap_dim(self.logical_h, g, MIN_LOGICAL_H, MAX_LOGICAL_H);
        self.back.opacity = self.back.opacity.clamp(0.0, 1.0);
        if self.backing_scale < 1.0 {
            self.backing_scale = 1.0;
        }
        self.backing_scale = self.backing_scale.min(3.0);
    }

    /// Window size in points (`logical × display_scale`).
    pub fn point_size(&self) -> (f64, f64) {
        (
            (self.logical_w as f64 * self.display_scale).max(1.0),
            (self.logical_h as f64 * self.display_scale).max(1.0),
        )
    }

    /// Framebuffer size in device pixels (`point_size × backing_scale`).
    pub fn pixel_size(&self) -> (u32, u32) {
        let (w, h) = self.point_size();
        let b = self.backing_scale.max(1.0);
        ((w * b).round().max(1.0) as u32, (h * b).round().max(1.0) as u32)
    }
}

/// Dest size in framebuffer pixels. One scale: `sprite_size / canonical_width`
/// (size at `display_scale` 1.0) times display zoom and backing.
pub fn mate_dest_size(
    src_w: f64,
    src_h: f64,
    sprite_size: u32,
    canonical_width: f64,
    display_scale: f64,
    backing: f64,
) -> (u32, u32) {
    let scale = sprite_size as f64 / canonical_width.max(1.0)
        * display_scale.max(0.05)
        * backing.max(1.0);
    (
        (src_w * scale).round().max(1.0) as u32,
        (src_h * scale).round().max(1.0) as u32,
    )
}

/// Sprite width in logical cage coordinates (physics / clamp).
/// Independent of `display_scale`: zoom scales the whole cage together.
pub fn mate_logical_width(src_w: f64, sprite_size: u32, canonical_width: f64) -> f64 {
    src_w * (sprite_size as f64 / canonical_width.max(1.0))
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

/// Walkable floor in cage-logical pixels (top-left origin, Y down).
#[derive(Debug, Clone, PartialEq)]
pub struct Floor {
    pub id: String,
    pub kind: FloorKind,
    pub x: f64,
    pub w: f64,
    pub y_left: f64,
    pub y_right: f64,
    pub vis_h: f64,
    pub fill: String,
    pub alpha: u8,
    pub angle_id: AngleId,
}

impl Floor {
    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn walk_y(&self, x: f64) -> f64 {
        if self.w < 1.0 {
            return self.y_left;
        }
        let t = ((x - self.x) / self.w).clamp(0.0, 1.0);
        self.y_left + t * (self.y_right - self.y_left)
    }

    pub fn submerged_at(&self, x: f64, waterline: f64) -> bool {
        self.walk_y(x) > waterline + 1.0
    }
}

pub struct CageLayout {
    pub floors: Vec<Floor>,
    pub waterline: f64,
    pub water_fill: String,
    pub water_alpha: u8,
    #[allow(dead_code)]
    pub inner: InnerRect,
}

/// Fixed ramp slope (downhill to the right). Matches `AngleId::Ramp30`.
const RAMP_TAN: f64 = 0.57735026919;

pub fn resolve_layout(inner: InnerRect, layout: &LayoutConfig) -> CageLayout {
    let waterline = inner.y as f64 + layout.water.y.clamp(0.05, 0.98) * inner.h as f64;
    let mut floors = Vec::new();
    for spec in &layout.floors {
        let x = inner.x as f64 + spec.x.clamp(0.0, 1.0) * inner.w as f64;
        let w = (spec.w.clamp(0.02, 1.0) * inner.w as f64)
            .min((inner.x + inner.w) as f64 - x)
            .max(1.0);
        let y_left = inner.y as f64 + spec.y.clamp(0.0, 1.0) * inner.h as f64;
        let y_right = match spec.kind {
            FloorKind::Flat => y_left,
            FloorKind::Ramp => (y_left + w * RAMP_TAN).min((inner.y + inner.h) as f64),
        };
        let vis_h = (spec.h.clamp(0.0, 1.0) * inner.h as f64).max(2.0);
        floors.push(Floor {
            id: spec.id.clone(),
            kind: spec.kind,
            x,
            w,
            y_left,
            y_right,
            vis_h,
            fill: spec.fill.clone(),
            alpha: spec.alpha,
            angle_id: spec.angle_id(),
        });
    }
    if floors.is_empty() {
        let y = (inner.y + inner.h) as f64;
        floors.push(Floor {
            id: "basin".into(),
            kind: FloorKind::Flat,
            x: inner.x as f64,
            w: inner.w as f64,
            y_left: y,
            y_right: y,
            vis_h: 8.0,
            fill: "#9a8b6e".into(),
            alpha: 160,
            angle_id: AngleId::Flat,
        });
    }
    CageLayout {
        floors,
        waterline,
        water_fill: layout.water.fill.clone(),
        water_alpha: layout.water.alpha,
        inner,
    }
}

/// Allowed x-range on `floor` given `water_affinity`. `None` if the floor is unusable.
pub fn walk_range(
    floor: &Floor,
    water_affinity: f64,
    waterline: f64,
    sprite_w: f64,
) -> Option<(f64, f64)> {
    let half = sprite_w / 2.0;
    let mut lo = floor.x + half;
    let mut hi = floor.right() - half;
    if water_affinity <= 0.0 {
        if floor.y_left.min(floor.y_right) > waterline + 1.0 {
            return None;
        }
        if (floor.y_right - floor.y_left).abs() > 1.0 {
            let t = ((waterline - floor.y_left) / (floor.y_right - floor.y_left)).clamp(0.0, 1.0);
            let xw = floor.x + t * floor.w;
            if floor.y_right > floor.y_left {
                hi = hi.min(xw - 2.0);
            } else {
                lo = lo.max(xw + 2.0);
            }
        }
    }
    if hi < lo {
        return None;
    }
    Some((lo, hi))
}

pub fn default_floor_id(floors: &[Floor], water_affinity: f64, waterline: f64, sprite_w: f64) -> String {
    let prefer = if water_affinity > 0.0 { "basin" } else { "bank" };
    if floors.iter().any(|f| {
        f.id == prefer && walk_range(f, water_affinity, waterline, sprite_w).is_some()
    }) {
        return prefer.into();
    }
    floors
        .iter()
        .find(|f| walk_range(f, water_affinity, waterline, sprite_w).is_some())
        .map(|f| f.id.clone())
        .unwrap_or_else(|| floors[0].id.clone())
}

pub fn place_on_floor(
    floor: &Floor,
    t: f64,
    water_affinity: f64,
    waterline: f64,
    sprite_w: f64,
) -> (f64, f64) {
    let (lo, hi) = walk_range(floor, water_affinity, waterline, sprite_w)
        .unwrap_or((floor.x + sprite_w / 2.0, floor.right() - sprite_w / 2.0));
    let x = lo + (hi - lo) * t.clamp(0.0, 1.0);
    (x, floor.walk_y(x))
}

/// Default floor plus a foot position along it (`t` in 0–1).
pub fn spawn_on_floor(
    floors: &[Floor],
    water_affinity: f64,
    waterline: f64,
    sprite_w: f64,
    t: f64,
) -> (String, f64, f64) {
    let id = default_floor_id(floors, water_affinity, waterline, sprite_w);
    let floor = floors
        .iter()
        .find(|f| f.id == id)
        .unwrap_or(&floors[0]);
    let (x, y) = place_on_floor(floor, t, water_affinity, waterline, sprite_w);
    (floor.id.clone(), x, y)
}

pub fn floor_by_id<'a>(floors: &'a [Floor], id: &str) -> Option<&'a Floor> {
    floors.iter().find(|f| f.id == id)
}

pub fn clamp_vivi_pos(
    x: f64,
    floor_id: &mut String,
    floors: &[Floor],
    water_affinity: f64,
    waterline: f64,
    sprite_w: f64,
) -> (f64, f64) {
    let idx = floors.iter().position(|f| f.id == *floor_id).or_else(|| {
        floors
            .iter()
            .position(|f| walk_range(f, water_affinity, waterline, sprite_w).is_some())
    });
    let Some(idx) = idx else {
        return (x, waterline);
    };
    let floor = &floors[idx];
    *floor_id = floor.id.clone();
    if let Some((lo, hi)) = walk_range(floor, water_affinity, waterline, sprite_w) {
        let nx = x.clamp(lo, hi);
        return (nx, floor.walk_y(nx));
    }
    *floor_id = default_floor_id(floors, water_affinity, waterline, sprite_w);
    if let Some(f) = floors.iter().find(|f| f.id == *floor_id) {
        return place_on_floor(f, 0.5, water_affinity, waterline, sprite_w);
    }
    (x, waterline)
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

#[allow(dead_code)]
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

/// Rest-bias tick on a floor. Turns only at edges where `turn_min` / `turn_max` are set.
pub fn tick_rest(
    state: &mut crate::behavior::State,
    facing: &mut crate::behavior::Dir,
    x: &mut f64,
    dt: f64,
    min_x: f64,
    max_x: f64,
    turn_min: bool,
    turn_max: bool,
    rng01: f64,
    rng_walk: f64,
) {
    use crate::behavior::{Dir, State};

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
        if turn_min && *x <= min_x + 0.5 {
            *dir = Dir::Right;
            *facing = Dir::Right;
        } else if turn_max && *x >= max_x - 0.5 {
            *dir = Dir::Left;
            *facing = Dir::Left;
        }
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

fn neighbor_idx(floors: &[Floor], idx: usize, toward_right: bool) -> Option<usize> {
    let cur = &floors[idx];
    let edge = if toward_right { cur.right() } else { cur.x };
    let mut best: Option<(f64, usize)> = None;
    for (i, f) in floors.iter().enumerate() {
        if i == idx {
            continue;
        }
        let other = if toward_right { f.x } else { f.right() };
        let dist = (other - edge).abs();
        if dist < 48.0 && f.x.max(cur.x) < f.right().min(cur.right()) + 8.0 {
            if best.is_none_or(|(d, _)| dist < d) {
                best = Some((dist, i));
            }
        }
    }
    best.map(|(_, i)| i)
}

/// One cage-mate tick: rest/walk on allowed floors. No swimming.
pub fn tick_cage_mate(
    state: &mut crate::behavior::State,
    facing: &mut crate::behavior::Dir,
    x: &mut f64,
    y: &mut f64,
    floor_id: &mut String,
    water_affinity: f64,
    floors: &[Floor],
    waterline: f64,
    sprite_w: f64,
    dt: f64,
    rng: &mut impl rand::Rng,
) {
    use crate::behavior::Dir;

    if floors.is_empty() {
        return;
    }
    let mut idx = floors
        .iter()
        .position(|f| f.id == *floor_id)
        .unwrap_or_else(|| {
            let id = default_floor_id(floors, water_affinity, waterline, sprite_w);
            floors.iter().position(|f| f.id == id).unwrap_or(0)
        });
    let Some((min_x, max_x)) = walk_range(&floors[idx], water_affinity, waterline, sprite_w) else {
        *floor_id = default_floor_id(floors, water_affinity, waterline, sprite_w);
        idx = floors.iter().position(|f| f.id == *floor_id).unwrap_or(0);
        let (nx, ny) = place_on_floor(&floors[idx], 0.5, water_affinity, waterline, sprite_w);
        *x = nx;
        *y = ny;
        return;
    };
    *floor_id = floors[idx].id.clone();

    let left_n = neighbor_idx(floors, idx, false)
        .filter(|&i| walk_range(&floors[i], water_affinity, waterline, sprite_w).is_some());
    let right_n = neighbor_idx(floors, idx, true)
        .filter(|&i| walk_range(&floors[i], water_affinity, waterline, sprite_w).is_some());
    let turn_min = left_n.is_none();
    let turn_max = right_n.is_none();

    let r1: f64 = rng.random();
    let r2: f64 = rng.random();
    tick_rest(state, facing, x, dt, min_x, max_x, turn_min, turn_max, r1, r2);

    if *x <= min_x + 0.5 {
        if let Some(n) = left_n {
            *floor_id = floors[n].id.clone();
            idx = n;
            if let Some((lo, hi)) = walk_range(&floors[idx], water_affinity, waterline, sprite_w) {
                *x = hi.min(lo + 4.0).max(lo);
            }
            *facing = Dir::Left;
        }
    } else if *x >= max_x - 0.5 {
        if let Some(n) = right_n {
            *floor_id = floors[n].id.clone();
            idx = n;
            if let Some((lo, hi)) = walk_range(&floors[idx], water_affinity, waterline, sprite_w) {
                *x = lo.max(hi - 4.0).min(hi);
            }
            *facing = Dir::Right;
        }
    }
    *y = floors[idx].walk_y(*x);
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

    #[test]
    fn mate_scale_is_canonical_not_per_frame_height() {
        let (sit_w, sit_h) = mate_dest_size(467.0, 329.0, 150, 512.0, 1.0, 1.0);
        let (lie_w, lie_h) = mate_dest_size(549.0, 179.0, 150, 512.0, 1.0, 1.0);
        assert_eq!(sit_w, 137);
        assert_eq!(sit_h, 96);
        assert_eq!(lie_w, 161);
        assert_eq!(lie_h, 52);
        let sit_body = sit_w as f64 / 467.0;
        let lie_body = lie_w as f64 / 549.0;
        assert!((sit_body - lie_body).abs() < 0.002);
    }

    #[test]
    fn mate_dest_includes_display_scale() {
        let (w1, h1) = mate_dest_size(512.0, 329.0, 150, 512.0, 1.0, 1.0);
        let (w2, h2) = mate_dest_size(512.0, 329.0, 150, 512.0, 0.5, 1.0);
        assert_eq!(w1, 150);
        assert_eq!(w2, 75);
        assert_eq!(h2 * 2, h1);
    }

    #[test]
    fn mate_logical_width_ignores_pose_height() {
        let sit = mate_logical_width(467.0, 150, 512.0);
        let lie = mate_logical_width(549.0, 150, 512.0);
        assert!((sit - 467.0 * 150.0 / 512.0).abs() < 0.01);
        assert!(lie > sit);
    }

    fn default_cage() -> CageLayout {
        resolve_layout(inner_rect(960, 640), &LayoutConfig::default())
    }

    #[test]
    fn default_layout_has_three_floors_and_ramp() {
        let cage = default_cage();
        assert_eq!(cage.floors.len(), 3);
        assert!(cage.floors.iter().any(|f| f.id == "bank"));
        assert!(cage.floors.iter().any(|f| f.id == "basin"));
        let ramp = cage.floors.iter().find(|f| f.id == "ramp").expect("ramp");
        assert_eq!(ramp.kind, FloorKind::Ramp);
        assert_eq!(ramp.angle_id, AngleId::Ramp30);
        assert!(ramp.y_right > ramp.y_left + 10.0);
    }

    #[test]
    fn floors_stay_inside_inner_after_resize() {
        let inner = inner_rect(480, 320);
        let cage = resolve_layout(inner, &LayoutConfig::default());
        let x1 = (inner.x + inner.w) as f64;
        let y1 = (inner.y + inner.h) as f64;
        for f in &cage.floors {
            assert!(f.x >= inner.x as f64 - 0.01);
            assert!(f.right() <= x1 + 0.01);
            assert!(f.y_left.min(f.y_right) >= inner.y as f64 - 0.01);
            assert!(f.y_left.max(f.y_right) <= y1 + 0.01);
        }
        assert!(cage.waterline > inner.y as f64);
        assert!(cage.waterline < y1);
    }

    #[test]
    fn zero_affinity_cannot_use_basin_or_past_waterline() {
        let cage = default_cage();
        let basin = cage.floors.iter().find(|f| f.id == "basin").unwrap();
        assert!(walk_range(basin, 0.0, cage.waterline, 40.0).is_none());
        let ramp = cage.floors.iter().find(|f| f.id == "ramp").unwrap();
        let (lo, hi) = walk_range(ramp, 0.0, cage.waterline, 40.0).expect("emerged ramp");
        assert!(ramp.walk_y(hi) <= cage.waterline + 4.0);
        assert!(ramp.walk_y(lo) < cage.waterline);
        let bank = cage.floors.iter().find(|f| f.id == "bank").unwrap();
        assert!(walk_range(bank, 0.0, cage.waterline, 40.0).is_some());
    }

    #[test]
    fn turtle_can_use_basin() {
        let cage = default_cage();
        let basin = cage.floors.iter().find(|f| f.id == "basin").unwrap();
        assert!(walk_range(basin, 0.5, cage.waterline, 40.0).is_some());
        let id = default_floor_id(&cage.floors, 0.5, cage.waterline, 40.0);
        assert_eq!(id, "basin");
        let land_id = default_floor_id(&cage.floors, 0.0, cage.waterline, 40.0);
        assert_eq!(land_id, "bank");
    }

    #[test]
    fn clamp_pins_y_to_floor_surface() {
        let cage = default_cage();
        let mut floor_id = "bank".to_string();
        let (x, y) = clamp_vivi_pos(100.0, &mut floor_id, &cage.floors, 0.0, cage.waterline, 40.0);
        let bank = cage.floors.iter().find(|f| f.id == "bank").unwrap();
        assert_eq!(floor_id, "bank");
        assert!((y - bank.walk_y(x)).abs() < 0.01);
        let (lo, hi) = walk_range(bank, 0.0, cage.waterline, 40.0).unwrap();
        assert!(x >= lo - 0.01 && x <= hi + 0.01);
    }

    #[test]
    fn ramp_walk_y_interpolates() {
        let cage = default_cage();
        let ramp = cage.floors.iter().find(|f| f.id == "ramp").unwrap();
        let mid = ramp.x + ramp.w * 0.5;
        let y = ramp.walk_y(mid);
        let expected = (ramp.y_left + ramp.y_right) * 0.5;
        assert!((y - expected).abs() < 0.5);
    }

    #[test]
    fn tick_rest_turns_at_bank_edge() {
        use crate::behavior::{Dir, State};
        let mut state = State::Walking {
            dir: Dir::Left,
            frame: 0,
            frame_elapsed: 0.0,
        };
        let mut facing = Dir::Left;
        let mut x = 90.0;
        tick_rest(
            &mut state,
            &mut facing,
            &mut x,
            1.0,
            90.0,
            260.0,
            true,
            true,
            0.2,
            0.9,
        );
        if let State::Walking { dir, .. } = state {
            assert_eq!(dir, Dir::Right);
        } else {
            panic!("expected walking");
        }
        assert_eq!(facing, Dir::Right);
    }
}
