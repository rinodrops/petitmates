//! Hot-reloadable glass look (`assets/vivarium/look.toml`).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

const BUILTIN: &str = include_str!("../../assets/vivarium/look.toml");

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct PaneLook {
    /// 0..1 absorption (0.05 ≈ 5% darker).
    pub dim: f64,
    /// 0 = black ND, 1 = saturated tank-edge teal.
    pub turquoise: f64,
    /// Front-pane specular: 0..1 from the left of the opening.
    #[serde(default = "default_spec_x")]
    pub spec_x: f64,
    /// Front-pane specular width as a fraction of the opening.
    #[serde(default = "default_spec_width")]
    pub spec_width: f64,
    /// Front-pane specular peak (0..1).
    #[serde(default = "default_spec_strength")]
    pub spec_strength: f64,
}

fn default_spec_x() -> f64 {
    0.20
}
fn default_spec_width() -> f64 {
    0.11
}
fn default_spec_strength() -> f64 {
    0.08
}

impl Default for PaneLook {
    fn default() -> Self {
        Self {
            dim: 0.05,
            turquoise: 0.45,
            spec_x: default_spec_x(),
            spec_width: default_spec_width(),
            spec_strength: default_spec_strength(),
        }
    }
}

impl PaneLook {
    pub fn clamp(&mut self) {
        self.dim = self.dim.clamp(0.0, 1.0);
        self.turquoise = self.turquoise.clamp(0.0, 1.0);
        self.spec_x = self.spec_x.clamp(0.0, 1.0);
        self.spec_width = self.spec_width.clamp(0.02, 0.6);
        self.spec_strength = self.spec_strength.clamp(0.0, 1.0);
    }

    pub fn tint(&self) -> (u8, u8, u8) {
        const TEAL: (f64, f64, f64) = (6.0, 140.0, 148.0);
        let t = self.turquoise.clamp(0.0, 1.0);
        (
            (TEAL.0 * t).round() as u8,
            (TEAL.1 * t).round() as u8,
            (TEAL.2 * t).round() as u8,
        )
    }
}

fn default_blur() -> f64 {
    20.0
}
fn default_outline() -> f64 {
    0.55
}
fn default_outline_width() -> f64 {
    1.35
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct LookConfig {
    #[serde(default)]
    pub back: PaneLook,
    #[serde(default)]
    pub front: PaneLook,
    #[serde(default)]
    pub side: PaneLook,
    /// Window-server backdrop blur radius in points (macOS CGS). 0 = off.
    #[serde(default = "default_blur")]
    pub blur: f64,
    /// Dark silhouette stroke (0..1). Native window shadows stay off.
    #[serde(default = "default_outline")]
    pub outline: f64,
    /// Stroke width in cage framebuffer pixels.
    #[serde(default = "default_outline_width")]
    pub outline_width: f64,
}

impl Default for LookConfig {
    fn default() -> Self {
        parse_look(BUILTIN).unwrap_or_else(hardcoded_look)
    }
}

fn hardcoded_look() -> LookConfig {
    LookConfig {
        back: PaneLook {
            dim: 0.04,
            turquoise: 0.35,
            ..PaneLook::default()
        },
        front: PaneLook {
            dim: 0.05,
            turquoise: 0.45,
            ..PaneLook::default()
        },
        side: PaneLook {
            dim: 0.24,
            turquoise: 1.0,
            ..PaneLook::default()
        },
        blur: default_blur(),
        outline: default_outline(),
        outline_width: default_outline_width(),
    }
}

impl LookConfig {
    pub fn clamp(&mut self) {
        self.back.clamp();
        self.front.clamp();
        self.side.clamp();
        self.blur = self.blur.clamp(0.0, 100.0);
        self.outline = self.outline.clamp(0.0, 1.0);
        self.outline_width = self.outline_width.clamp(0.0, 4.0);
    }

    /// True when the CPU glass layers can be reused (backdrop blur is CGS).
    pub fn glass_eq(&self, other: &Self) -> bool {
        self.back == other.back && self.front == other.front && self.side == other.side
    }

    /// True when the composed cage pixels can be reused (not CGS blur).
    pub fn raster_eq(&self, other: &Self) -> bool {
        self.glass_eq(other)
            && self.outline == other.outline
            && self.outline_width == other.outline_width
    }
}

fn parse_look(text: &str) -> Option<LookConfig> {
    let mut cfg: LookConfig = toml::from_str(text).ok()?;
    cfg.clamp();
    Some(cfg)
}

pub struct LookLoader {
    path: PathBuf,
    last_modified: Option<SystemTime>,
    pub current: LookConfig,
}

impl LookLoader {
    pub fn load() -> Self {
        let path = look_toml_path();
        let mut loader = Self {
            path,
            last_modified: None,
            current: LookConfig::default(),
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
            if let Some(cfg) = parse_look(&text) {
                self.current = cfg;
                return true;
            }
        }
        false
    }
}

fn look_toml_path() -> PathBuf {
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/vivarium/look.toml");
    if dev.exists() {
        return dev;
    }
    if let Some(exe) = std::env::current_exe().ok() {
        if let Some(dir) = exe.parent() {
            let bundled = dir
                .join("../Resources/assets/vivarium/look.toml")
                .canonicalize()
                .ok();
            if let Some(p) = bundled.filter(|p| p.exists()) {
                return p;
            }
            let beside = dir.join("vivarium_look.toml");
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
    fn builtin_look_toml_parses() {
        let cfg = parse_look(BUILTIN).expect("assets/vivarium/look.toml");
        assert!(cfg.front.dim > 0.0);
        assert!(cfg.side.turquoise >= cfg.front.turquoise);
        assert!(cfg.front.spec_width > 0.0);
        assert!(cfg.blur >= 0.0);
        assert!(cfg.outline > 0.0);
        assert!(cfg.outline_width > 0.0);
    }
}
