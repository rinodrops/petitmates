#![cfg(target_os = "windows")]

//! Windows sprite asset loader.
//!
//! Loads PNG sprites with the `image` crate, scales them, converts to
//! pre-multiplied BGRA (required by `UpdateLayeredWindow`), and generates
//! horizontally-mirrored copies at load time.
//!
//! The `Anchor` type and `compute_anchor` logic are identical to the macOS
//! `assets.rs` so that `surface_to_screen_pos` in `windows.rs` can use the
//! same formulas as `surface_to_ns_origin` in `macos.rs`.

use std::collections::HashMap;
use std::path::Path;

use image::GenericImageView;

use crate::manifest::{Attachment, Manifest};

// ---- Anchor ----

/// Attachment point for a sprite, in scaled display pixels.
///
/// | attachment | anchor.x                             | anchor.y                             |
/// |------------|--------------------------------------|--------------------------------------|
/// | `line_y`   | 0 (unused)                           | distance from **bottom** to foot line |
/// | `line_x`   | distance from **left** to grip line  | 0 (unused)                           |
/// | `point`    | distance from **left** to attach pt  | distance from **bottom** to attach pt |
#[derive(Debug, Clone, Copy)]
pub struct Anchor {
    pub x: f64,
    pub y: f64,
}

// ---- Sprite ----

/// A loaded, scaled, pre-multiplied BGRA sprite ready for `UpdateLayeredWindow`.
pub struct Sprite {
    /// Row-major BGRA pixels, pre-multiplied alpha.
    pub bgra: Vec<u8>,
    pub w: i32,
    pub h: i32,
}

// ---- SpriteAssets ----

pub struct SpriteAssets {
    images:   HashMap<String, Sprite>,
    mirrored: HashMap<String, Sprite>,
    anchors:  HashMap<String, Anchor>,
}

impl SpriteAssets {
    /// Load every sprite listed in `manifest` from `char_dir/sprite/`.
    /// Returns `None` if any listed sprite is missing or unreadable.
    pub fn load(char_dir: &Path, manifest: &Manifest, display_width: f64) -> Option<Self> {
        let scale = display_width / manifest.canonical_width;
        let sprite_dir = char_dir.join("sprite");

        let mut images   = HashMap::new();
        let mut mirrored = HashMap::new();
        let mut anchors  = HashMap::new();

        for (name, info) in &manifest.sprites {
            let path = sprite_dir.join(format!("{name}.png"));
            let img  = image::open(&path).ok()?;
            let (ow, oh) = img.dimensions();
            let nw = ((ow as f64 * scale).round() as u32).max(1);
            let nh = ((oh as f64 * scale).round() as u32).max(1);

            let scaled = img
                .resize_exact(nw, nh, image::imageops::FilterType::Triangle)
                .to_rgba8();

            let anchor = compute_anchor(info.x, info.y, oh as f64, scale, &info.attachment);
            let bgra   = rgba_to_bgra_premul(&scaled);
            let mirror = mirror_bgra(&bgra, nw as usize, nh as usize);

            images.insert(name.clone(),   Sprite { bgra,   w: nw as i32, h: nh as i32 });
            mirrored.insert(name.clone(), Sprite { bgra: mirror, w: nw as i32, h: nh as i32 });
            anchors.insert(name.clone(), anchor);
        }

        Some(SpriteAssets { images, mirrored, anchors })
    }

    /// Returns the (optionally mirrored) sprite for `name`.
    pub fn sprite(&self, name: &str, mirror: bool) -> Option<&Sprite> {
        if mirror { self.mirrored.get(name) } else { self.images.get(name) }
    }

    /// Returns the anchor for `name`, or `None` if not loaded.
    pub fn anchor(&self, name: &str) -> Option<Anchor> {
        self.anchors.get(name).copied()
    }

    /// Returns the (w, h) of `name` in display pixels, or `(150, 150)` as
    /// a safe fallback.
    pub fn size(&self, name: &str, mirror: bool) -> (f64, f64) {
        self.sprite(name, mirror)
            .map(|s| (s.w as f64, s.h as f64))
            .unwrap_or((150.0, 150.0))
    }
}

// ---- Helpers ----

/// Build an `Anchor` from raw manifest values.
/// Matches `compute_anchor` in `assets.rs` exactly.
fn compute_anchor(
    x_orig: Option<f64>,
    y_orig: Option<f64>,
    sprite_h_orig: f64,
    scale: f64,
    attachment: &Attachment,
) -> Anchor {
    match attachment {
        Attachment::LineY => {
            let y = (sprite_h_orig - y_orig.unwrap_or(0.0)) * scale;
            Anchor { x: 0.0, y }
        }
        Attachment::LineX => {
            let x = x_orig.unwrap_or(0.0) * scale;
            Anchor { x, y: 0.0 }
        }
        Attachment::Point => {
            let x = x_orig.unwrap_or(0.0) * scale;
            let y = (sprite_h_orig - y_orig.unwrap_or(0.0)) * scale;
            Anchor { x, y }
        }
    }
}

/// Convert RGBA straight-alpha to BGRA pre-multiplied alpha.
fn rgba_to_bgra_premul(img: &image::RgbaImage) -> Vec<u8> {
    img.pixels()
        .flat_map(|p| {
            let a = p[3] as u32;
            [
                (p[2] as u32 * a / 255) as u8, // B
                (p[1] as u32 * a / 255) as u8, // G
                (p[0] as u32 * a / 255) as u8, // R
                p[3],                           // A (unchanged)
            ]
        })
        .collect()
}

/// Flip a BGRA buffer horizontally (row by row).
fn mirror_bgra(bgra: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; bgra.len()];
    for row in 0..h {
        for col in 0..w {
            let src = (row * w + col) * 4;
            let dst = (row * w + (w - 1 - col)) * 4;
            out[dst..dst + 4].copy_from_slice(&bgra[src..src + 4]);
        }
    }
    out
}
