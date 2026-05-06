use std::collections::HashMap;
use std::path::Path;

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

#[derive(serde::Deserialize, Debug)]
pub struct Manifest {
    /// Pixel width the sprite was authored at (used for scaling).
    pub canonical_width: f64,
    #[serde(default)]
    pub sprites: HashMap<String, SpriteInfo>,
}

pub fn load(char_dir: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(char_dir.join("manifest.toml")).ok()?;
    toml::from_str(&text).ok()
}

/// Parse a `Manifest` from raw TOML bytes (used for embedded assets on Windows).
pub fn load_from_bytes(bytes: &[u8]) -> Option<Manifest> {
    let text = std::str::from_utf8(bytes).ok()?;
    toml::from_str(text).ok()
}
