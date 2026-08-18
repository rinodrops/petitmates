//! Hot-reloadable cage floors and visual water (`assets/vivarium/layout.toml`).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

const BUILTIN: &str = include_str!("../../assets/vivarium/layout.toml");

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FloorKind {
    Flat,
    Ramp,
}

impl Default for FloorKind {
    fn default() -> Self {
        Self::Flat
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct FloorSpec {
    pub id: String,
    #[serde(default)]
    pub kind: FloorKind,
    pub x: f64,
    pub w: f64,
    /// Walkable surface as a fraction of inner (0 = top, 1 = bottom).
    pub y: f64,
    /// Visual thickness downward, as a fraction of inner height.
    #[serde(default = "default_floor_h")]
    pub h: f64,
    #[serde(default = "default_floor_fill")]
    pub fill: String,
    #[serde(default = "default_floor_alpha")]
    pub alpha: u8,
}

fn default_floor_h() -> f64 {
    0.08
}
fn default_floor_fill() -> String {
    "#c4baa0".into()
}
fn default_floor_alpha() -> u8 {
    180
}

impl FloorSpec {
    fn clamp(&mut self) {
        self.x = self.x.clamp(0.0, 1.0);
        self.y = self.y.clamp(0.0, 1.0);
        self.w = self.w.clamp(0.02, 1.0);
        self.h = self.h.clamp(0.0, 1.0);
        if self.x + self.w > 1.0 {
            self.w = 1.0 - self.x;
        }
        if self.id.is_empty() {
            self.id = "floor".into();
        }
    }

    pub fn angle_id(&self) -> AngleId {
        match self.kind {
            FloorKind::Flat => AngleId::Flat,
            FloorKind::Ramp => AngleId::Ramp30,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct WaterSpec {
    /// Waterline as a fraction of inner from the top.
    pub y: f64,
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
        self.y = self.y.clamp(0.05, 0.98);
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct LayoutConfig {
    pub water: WaterSpec,
    pub floors: Vec<FloorSpec>,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        parse_layout(BUILTIN).unwrap_or_else(hardcoded_layout)
    }
}

fn hardcoded_layout() -> LayoutConfig {
    LayoutConfig {
        water: WaterSpec {
            y: 0.62,
            fill: default_water_fill(),
            alpha: default_water_alpha(),
        },
        floors: vec![
            FloorSpec {
                id: "bank".into(),
                kind: FloorKind::Flat,
                x: 0.00,
                w: 0.38,
                y: 0.58,
                h: 0.42,
                fill: "#c4baa0".into(),
                alpha: 200,
            },
            FloorSpec {
                id: "ramp".into(),
                kind: FloorKind::Ramp,
                x: 0.34,
                w: 0.45,
                y: 0.58,
                h: 0.08,
                fill: "#b8a888".into(),
                alpha: 180,
            },
            FloorSpec {
                id: "basin".into(),
                kind: FloorKind::Flat,
                x: 0.70,
                w: 0.30,
                y: 0.97,
                h: 0.03,
                fill: "#9a8b6e".into(),
                alpha: 160,
            },
        ],
    }
}

impl LayoutConfig {
    pub fn clamp(&mut self) {
        self.water.clamp();
        if self.floors.is_empty() {
            *self = hardcoded_layout();
            return;
        }
        for f in &mut self.floors {
            f.clamp();
        }
    }
}

fn parse_layout(text: &str) -> Option<LayoutConfig> {
    let mut cfg: LayoutConfig = toml::from_str(text).ok()?;
    cfg.clamp();
    Some(cfg)
}

pub struct LayoutLoader {
    path: PathBuf,
    last_modified: Option<SystemTime>,
    pub current: LayoutConfig,
}

impl LayoutLoader {
    pub fn load() -> Self {
        let path = layout_toml_path();
        let mut loader = Self {
            path,
            last_modified: None,
            current: LayoutConfig::default(),
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
            if let Some(cfg) = parse_layout(&text) {
                self.current = cfg;
                return true;
            }
        }
        false
    }
}

fn layout_toml_path() -> PathBuf {
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/vivarium/layout.toml");
    if dev.exists() {
        return dev;
    }
    if let Some(exe) = std::env::current_exe().ok() {
        if let Some(dir) = exe.parent() {
            let bundled = dir
                .join("../Resources/assets/vivarium/layout.toml")
                .canonicalize()
                .ok();
            if let Some(p) = bundled.filter(|p| p.exists()) {
                return p;
            }
            let beside = dir.join("vivarium_layout.toml");
            if beside.exists() {
                return beside;
            }
        }
    }
    dev
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_layout_toml_parses() {
        let cfg = parse_layout(BUILTIN).expect("assets/vivarium/layout.toml");
        assert!(cfg.water.alpha <= 80, "water should stay almost transparent");
        assert_eq!(cfg.floors.len(), 3);
        assert!(cfg.floors.iter().any(|f| f.id == "bank"));
        assert!(cfg.floors.iter().any(|f| f.kind == FloorKind::Ramp));
        assert!(cfg.floors.iter().any(|f| f.id == "basin"));
    }
}
