use std::collections::HashMap;
#[cfg(target_os = "macos")]
use std::path::Path;

// ── Animation definitions ─────────────────────────────────────────────────────

#[derive(serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum AnimMode {
    Loop,
    PingPong,
    Once,
}

impl Default for AnimMode {
    fn default() -> Self { AnimMode::PingPong }
}

fn default_frame_secs() -> f64 { 0.12 }

#[derive(serde::Deserialize, Debug, Clone, Default)]
pub struct VerticalOscillation {
    pub amplitude: f64,
    pub phase_per_frame: f64,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct AnimationDef {
    pub frames: u8,
    #[serde(default)]
    pub mode: AnimMode,
    /// Seconds per frame (used by OneShot and future animations).
    #[serde(default = "default_frame_secs")]
    pub frame_secs: f64,
    #[serde(default)]
    pub vertical_oscillation: Option<VerticalOscillation>,
}

impl Default for AnimationDef {
    fn default() -> Self { AnimationDef { frames: 3, mode: AnimMode::PingPong, frame_secs: 0.12, vertical_oscillation: None } }
}

impl AnimationDef {
    /// Total number of ticks in one full animation cycle.
    pub fn cycle_len(&self) -> u8 {
        match self.mode {
            AnimMode::Loop | AnimMode::Once => self.frames.max(1),
            AnimMode::PingPong => {
                if self.frames <= 1 { 1 } else { 2 * (self.frames - 1) }
            }
        }
    }

    /// Convert a tick counter (0..cycle_len) to a sprite index (0..frames).
    pub fn sprite_index(&self, tick: u8) -> u8 {
        match self.mode {
            AnimMode::Loop => tick % self.frames.max(1),
            AnimMode::Once => tick.min(self.frames.saturating_sub(1)),
            AnimMode::PingPong => {
                let n = self.frames;
                if n <= 1 { return 0; }
                let period = 2 * (n - 1);
                let t = tick % period;
                if t < n { t } else { period - t }
            }
        }
    }
}

// ── Sprite attachment types ───────────────────────────────────────────────────

/// Sprite attachment types for anchor-point calculation.
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum Attachment {
    /// Attached at a single point (x, y) in sprite coordinates.
    Point,
    /// Attached along the right edge at x; hangs on a vertical surface.
    LineX,
    /// Attached along the bottom edge at y; stands on a horizontal surface.
    LineY,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct SpriteInfo {
    pub attachment: Attachment,
    pub x: Option<f64>,
    pub y: Option<f64>,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct SurfaceConfig {
    #[serde(default = "bool_true")]
    pub window_bottom: bool,
}

fn bool_true() -> bool { true }

impl Default for SurfaceConfig {
    fn default() -> Self { SurfaceConfig { window_bottom: true } }
}

#[derive(serde::Deserialize, Debug)]
pub struct Manifest {
    /// Pixel width the sprite was authored at (used for scaling).
    pub canonical_width: f64,
    #[serde(default)]
    pub sprites: HashMap<String, SpriteInfo>,
    #[serde(default)]
    pub animations: HashMap<String, AnimationDef>,
    #[serde(default)]
    pub surfaces: SurfaceConfig,
}

impl Manifest {
    /// Returns the `AnimationDef` for `name`, falling back to the default
    /// (3 frames, ping-pong) when the animation is not defined in the manifest.
    #[allow(dead_code)]
    pub fn anim(&self, name: &str) -> AnimationDef {
        self.animations.get(name).cloned().unwrap_or_default()
    }
}

/// Parse a `Manifest` from filesystem TOML (used for hot-reload on macOS).
#[cfg(target_os = "macos")]
pub fn load(char_dir: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(char_dir.join("manifest.toml")).ok()?;
    toml::from_str(&text).ok()
}

/// Parse a `Manifest` from raw TOML bytes (used for embedded assets on Windows).
#[cfg(target_os = "windows")]
pub fn load_from_bytes(bytes: &[u8]) -> Option<Manifest> {
    let text = std::str::from_utf8(bytes).ok()?;
    toml::from_str(text).ok()
}
