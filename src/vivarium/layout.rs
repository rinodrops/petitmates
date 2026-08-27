//! Hot-reloadable cage assembly (`assets/vivarium/assembly.toml`) and part catalog.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::occupancy;
use super::{InnerRect, MIN_LOGICAL_H, MIN_LOGICAL_W, WALL_GLASS_W, inner_rect};

pub use occupancy::{Occupancy, push_out_blocked};

const BUILTIN_ASSEMBLY: &str = include_str!("../../assets/vivarium/assembly.toml");
const SOIL_TOML: &str = include_str!("../../assets/vivarium/prop/soil01/part.toml");
const SOIL_PNG: &[u8] = include_bytes!("../../assets/vivarium/prop/soil01/sprite.png");
const WOOD_TOML: &str = include_str!("../../assets/vivarium/prop/wood1/part.toml");
const WOOD_PNG: &[u8] = include_bytes!("../../assets/vivarium/prop/wood1/sprite.png");

/// Only these sprite-rotation angles are cached. Arbitrary degrees are forbidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AngleId {
    Flat,
    Ramp30,
}

impl AngleId {
    pub fn deg(self) -> i16 {
        match self {
            Self::Flat => 0,
            Self::Ramp30 => 30,
        }
    }

    /// Clockwise in Y-down cage space (downhill to the right).
    pub fn rad(self) -> f64 {
        (self.deg() as f64).to_radians()
    }
}

/// Snap a walk-segment slope to the two cached sprite angles.
pub fn snap_angle_id(y_left: f64, y_right: f64, w: f64) -> AngleId {
    if w < 1.0 {
        return AngleId::Flat;
    }
    let deg = (y_right - y_left).atan2(w).to_degrees().abs();
    if deg < 15.0 {
        AngleId::Flat
    } else {
        AngleId::Ramp30
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorKind {
    Flat,
    Ramp,
}

impl FloorKind {
    pub(super) fn from_angle(id: AngleId) -> Self {
        match id {
            AngleId::Flat => Self::Flat,
            AngleId::Ramp30 => Self::Ramp,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HAnchor {
    Left,
    Right,
    #[serde(rename = "none")]
    Free,
}

impl Default for HAnchor {
    fn default() -> Self {
        Self::Free
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    Back,
    Front,
}

impl Default for Layer {
    fn default() -> Self {
        Self::Back
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
struct PartToml {
    #[serde(default)]
    name: String,
    #[serde(default = "default_true")]
    allow_back: bool,
    #[serde(default = "default_true")]
    allow_front: bool,
    /// Cage-logical px at instance scale 1.0. 0 = PNG size (1x legacy).
    #[serde(default)]
    dest_w: u32,
    #[serde(default)]
    dest_h: u32,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone)]
pub struct Part {
    #[allow(dead_code)]
    pub id: String,
    #[allow(dead_code)]
    pub name: String,
    pub allow_back: bool,
    pub allow_front: bool,
    /// Cage-logical size at instance scale 1.0.
    pub dest_w: u32,
    pub dest_h: u32,
    pub img_w: u32,
    pub img_h: u32,
    pub rgba: Vec<u8>,
}

pub type PartCatalog = HashMap<String, Part>;

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct Instance {
    pub part: String,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    #[serde(default)]
    pub angle_deg: f64,
    #[serde(default = "default_scale")]
    pub scale: f64,
    #[serde(default)]
    pub h_anchor: HAnchor,
    #[serde(default)]
    pub layer: Layer,
}

fn default_scale() -> f64 {
    1.0
}

impl Instance {
    fn clamp(&mut self) {
        self.x = self.x.max(0.0);
        self.y = self.y.max(0.0);
        self.scale = self.scale.clamp(0.5, 1.5);
        self.angle_deg = self.angle_deg.clamp(-180.0, 180.0);
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct WaterSpec {
    /// Water column height from the inner bottom, in logical px.
    pub depth_px: f64,
    #[serde(default = "default_water_fill")]
    pub fill: String,
    #[serde(default = "default_water_alpha")]
    pub alpha: u8,
}

fn default_water_fill() -> String {
    "#5aa0aa".into()
}
fn default_water_alpha() -> u8 {
    40
}

impl WaterSpec {
    fn clamp(&mut self) {
        self.depth_px = self.depth_px.clamp(0.0, 4000.0);
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct AssemblyConfig {
    pub water: WaterSpec,
    #[serde(default)]
    pub instances: Vec<Instance>,
}

impl Default for AssemblyConfig {
    fn default() -> Self {
        parse_assembly(BUILTIN_ASSEMBLY).unwrap_or_else(hardcoded_assembly)
    }
}

fn hardcoded_assembly() -> AssemblyConfig {
    AssemblyConfig {
        water: WaterSpec {
            depth_px: 80.0,
            fill: default_water_fill(),
            alpha: 60,
        },
        instances: vec![
            Instance {
                part: "soil01".into(),
                x: 0.0,
                y: 0.0,
                angle_deg: 0.0,
                scale: 1.0,
                h_anchor: HAnchor::Left,
                layer: Layer::Back,
            },
            Instance {
                part: "wood1".into(),
                x: 0.0,
                y: 0.0,
                angle_deg: 0.0,
                scale: 1.0,
                h_anchor: HAnchor::Right,
                layer: Layer::Back,
            },
        ],
    }
}

impl AssemblyConfig {
    pub fn clamp(&mut self) {
        self.water.clamp();
        for inst in &mut self.instances {
            inst.clamp();
        }
    }

    /// Smallest logical width that keeps a non-negative gap between left- and
    /// right-anchored parts. If they already overlap at `current_w`, refuse shrink.
    pub fn min_logical_w(&self, parts: &PartCatalog, current_w: u32) -> u32 {
        let inner = inner_rect(current_w.max(1), MIN_LOGICAL_H);
        let mut left_span = 0.0_f64;
        let mut right_span = 0.0_f64;
        for inst in &self.instances {
            let Some(part) = parts.get(&inst.part) else {
                continue;
            };
            let sw = part.dest_w as f64 * inst.scale;
            match inst.h_anchor {
                HAnchor::Left => left_span = left_span.max(inst.x + sw),
                HAnchor::Right => right_span = right_span.max(inst.x + sw),
                HAnchor::Free => {}
            }
        }
        let needed_inner = left_span + right_span;
        let walls = (WALL_GLASS_W * 2) as f64;
        if (inner.w as f64) + 0.5 < needed_inner {
            return current_w.max(MIN_LOGICAL_W);
        }
        let needed = (needed_inner + walls).ceil() as u32;
        needed.max(MIN_LOGICAL_W)
    }
}

/// Compatibility alias used by the rest of the crate.
pub type LayoutConfig = AssemblyConfig;

fn parse_assembly(text: &str) -> Option<AssemblyConfig> {
    let mut cfg: AssemblyConfig = toml::from_str(text).ok()?;
    cfg.clamp();
    Some(cfg)
}

fn decode_rgba(png: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(png).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some((w, h, img.into_raw()))
}

fn parse_part(id: &str, toml_text: &str, png: &[u8]) -> Option<Part> {
    let spec: PartToml = toml::from_str(toml_text).ok()?;
    let (img_w, img_h, rgba) = decode_rgba(png)?;
    let dest_w = if spec.dest_w > 0 { spec.dest_w } else { img_w };
    let dest_h = if spec.dest_h > 0 { spec.dest_h } else { img_h };
    Some(Part {
        id: id.to_string(),
        name: if spec.name.is_empty() {
            id.to_string()
        } else {
            spec.name
        },
        allow_back: spec.allow_back,
        allow_front: spec.allow_front,
        dest_w,
        dest_h,
        img_w,
        img_h,
        rgba,
    })
}

fn load_part_from_dir(dir: &Path, id: &str) -> Option<Part> {
    let toml_text = std::fs::read_to_string(dir.join("part.toml")).ok()?;
    let png = std::fs::read(dir.join("sprite.png")).ok()?;
    parse_part(id, &toml_text, &png)
}

pub fn load_part_catalog() -> PartCatalog {
    let mut parts = PartCatalog::new();
    if let Some(p) = parse_part("soil01", SOIL_TOML, SOIL_PNG) {
        parts.insert("soil01".into(), p);
    }
    if let Some(p) = parse_part("wood1", WOOD_TOML, WOOD_PNG) {
        parts.insert("wood1".into(), p);
    }
    let root = prop_root();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for ent in entries.flatten() {
            let path = ent.path();
            if !path.is_dir() {
                continue;
            }
            let Some(id) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Some(p) = load_part_from_dir(&path, id) {
                parts.insert(id.to_string(), p);
            }
        }
    }
    parts
}

#[derive(Debug, Clone, Copy)]
pub struct Pose {
    pub left: f64,
    pub top: f64,
    pub sw: f64,
    pub sh: f64,
    pub angle_deg: f64,
    pub scale: f64,
}

impl Pose {
    pub fn part_to_cage(self, px: f64, py: f64) -> (f64, f64) {
        let sx = px * self.scale;
        let sy = py * self.scale;
        let ang = self.angle_deg.to_radians();
        let (c, s) = (ang.cos(), ang.sin());
        let dx = sx - self.sw / 2.0;
        let dy = sy - self.sh;
        let rx = dx * c - dy * s;
        let ry = dx * s + dy * c;
        (self.left + self.sw / 2.0 + rx, self.top + self.sh + ry)
    }

    pub fn cage_to_part(self, cx: f64, cy: f64) -> (f64, f64) {
        let ang = self.angle_deg.to_radians();
        let (c, s) = (ang.cos(), ang.sin());
        let rx = cx - (self.left + self.sw / 2.0);
        let ry = cy - (self.top + self.sh);
        let dx = rx * c + ry * s;
        let dy = -rx * s + ry * c;
        let sx = dx + self.sw / 2.0;
        let sy = dy + self.sh;
        let scale = self.scale.max(1e-6);
        (sx / scale, sy / scale)
    }
}

#[derive(Debug, Clone)]
pub struct PlacedInstance {
    #[allow(dead_code)]
    pub index: usize,
    pub part_id: String,
    pub layer: Layer,
    pub pose: Pose,
}

pub fn instance_pose(inner: InnerRect, inst: &Instance, part: &Part) -> Pose {
    let sw = part.dest_w as f64 * inst.scale;
    let sh = part.dest_h as f64 * inst.scale;
    let bottom = inner.y as f64 + inner.h as f64 - inst.y;
    let top = bottom - sh;
    let left = match inst.h_anchor {
        HAnchor::Left => inner.x as f64 + inst.x,
        HAnchor::Right => inner.x as f64 + inner.w as f64 - inst.x - sw,
        HAnchor::Free => inner.x as f64 + inst.x,
    };
    Pose {
        left,
        top,
        sw,
        sh,
        angle_deg: inst.angle_deg,
        scale: inst.scale,
    }
}

fn layer_allowed(inst: &Instance, part: &Part) -> bool {
    match inst.layer {
        Layer::Back => part.allow_back,
        Layer::Front => part.allow_front,
    }
}

pub struct LayoutLoader {
    path: PathBuf,
    last_modified: Option<SystemTime>,
    pub current: AssemblyConfig,
    pub parts: PartCatalog,
}

impl LayoutLoader {
    pub fn load() -> Self {
        let mut loader = Self {
            path: assembly_toml_path(),
            last_modified: None,
            current: AssemblyConfig::default(),
            parts: load_part_catalog(),
        };
        loader.reload_if_changed();
        loader
    }

    /// Call each tick. `true` if values changed.
    pub fn reload_if_changed(&mut self) -> bool {
        let mtime = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();
        if mtime == self.last_modified {
            return false;
        }
        self.last_modified = mtime;
        if let Ok(text) = std::fs::read_to_string(&self.path) {
            if let Some(cfg) = parse_assembly(&text) {
                self.current = cfg;
                self.parts = load_part_catalog();
                return true;
            }
        }
        false
    }
}

fn vivarium_dir() -> PathBuf {
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/vivarium");
    if dev.exists() {
        return dev;
    }
    if let Some(exe) = std::env::current_exe().ok() {
        if let Some(dir) = exe.parent() {
            let bundled = dir.join("../Resources/assets/vivarium").canonicalize().ok();
            if let Some(p) = bundled.filter(|p| p.exists()) {
                return p;
            }
        }
    }
    dev
}

fn assembly_toml_path() -> PathBuf {
    let dir = vivarium_dir();
    let p = dir.join("assembly.toml");
    if p.exists() {
        return p;
    }
    if let Some(exe) = std::env::current_exe().ok() {
        if let Some(parent) = exe.parent() {
            let beside = parent.join("vivarium_assembly.toml");
            if beside.exists() {
                return beside;
            }
        }
    }
    p
}

fn prop_root() -> PathBuf {
    vivarium_dir().join("prop")
}

/// Place instances, derive occupancy blobs, and walk skylines.
pub fn resolve_layout(
    inner: InnerRect,
    assembly: &AssemblyConfig,
    parts: &PartCatalog,
) -> super::CageLayout {
    let inner_bottom = inner.y as f64 + inner.h as f64;
    let waterline = (inner_bottom - assembly.water.depth_px).max(inner.y as f64);
    let mut placed = Vec::new();
    for (index, inst) in assembly.instances.iter().enumerate() {
        let Some(part) = parts.get(&inst.part) else {
            continue;
        };
        if !layer_allowed(inst, part) {
            continue;
        }
        let pose = instance_pose(inner, inst, part);
        placed.push(PlacedInstance {
            index,
            part_id: inst.part.clone(),
            layer: inst.layer,
            pose,
        });
    }
    let occ = occupancy::build_occupancy(inner, &placed, parts);
    let mut floors = occupancy::floors_from_occupancy(&occ);
    if floors.is_empty() {
        let y = inner_bottom;
        floors.push(super::Floor {
            id: "basin".into(),
            kind: FloorKind::Flat,
            x: inner.x as f64,
            w: inner.w as f64,
            y_left: y,
            y_right: y,
            vis_h: 8.0,
            fill: String::new(),
            alpha: 0,
            angle_id: AngleId::Flat,
        });
    }
    super::CageLayout {
        floors,
        waterline,
        water_fill: assembly.water.fill.clone(),
        water_alpha: assembly.water.alpha,
        inner,
        placed,
        occupancy: occ,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_assembly_toml_parses() {
        let cfg = parse_assembly(BUILTIN_ASSEMBLY).expect("assets/vivarium/assembly.toml");
        assert!(
            cfg.water.alpha <= 80,
            "water should stay almost transparent"
        );
        assert_eq!(cfg.instances.len(), 2);
        assert!(
            cfg.instances
                .iter()
                .any(|i| i.part == "soil01" && i.h_anchor == HAnchor::Left)
        );
        assert!(
            cfg.instances
                .iter()
                .any(|i| i.part == "wood1" && i.h_anchor == HAnchor::Right)
        );
    }

    #[test]
    fn builtin_parts_parse() {
        let parts = load_part_catalog();
        let soil = parts.get("soil01").expect("missing soil01");
        assert_eq!(soil.dest_w, 837);
        assert_eq!(soil.dest_h, 94);
        assert_eq!(soil.img_w, soil.dest_w * 2);
        assert_eq!(soil.img_h, soil.dest_h * 2);
        let wood = parts.get("wood1").expect("missing wood1");
        assert_eq!(wood.dest_w, 669);
        assert_eq!(wood.dest_h, 133);
        assert_eq!(wood.img_w, wood.dest_w * 2);
        assert_eq!(wood.img_h, wood.dest_h * 2);
    }

    #[test]
    fn omitted_dest_defaults_to_png_size() {
        let part = parse_part("x", "name = \"x\"\n", SOIL_PNG).unwrap();
        assert_eq!(part.dest_w, part.img_w);
        assert_eq!(part.dest_h, part.img_h);
        let part = parse_part("x", "name = \"x\"\ndest_w = 40\ndest_h = 20\n", SOIL_PNG).unwrap();
        assert_eq!(part.dest_w, 40);
        assert_eq!(part.dest_h, 20);
        assert!(part.img_w > 40);
    }
}
