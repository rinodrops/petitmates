#![cfg(target_os = "macos")]
#![allow(deprecated, non_snake_case, unused_unsafe)]

//! Loads and scales all character sprites; computes anchor points.
//! Also pre-generates horizontally-mirrored copies for right-facing sprites.
//!
//! ## Coordinate conventions
//!
//! Manifest x/y values are in **original (unscaled) sprite pixels**, with the
//! sprite origin at the **top-left** corner (matching how image editors report
//! pixel coordinates).
//!
//! The `Anchor` struct stores distances in **scaled display pixels** from the
//! sprite's own origin corners, making panel-position math straightforward:
//!
//! | attachment | anchor.x                              | anchor.y                              |
//! |------------|---------------------------------------|---------------------------------------|
//! | `line_y`   | 0 (unused)                            | distance from **bottom** to foot line |
//! | `line_x`   | distance from **right edge** to wall  | 0 (unused)                            |
//! | `point`    | distance from **left** to attach pt   | distance from **bottom** to attach pt |
//!
//! ## Mirroring
//!
//! Sprite images are stored unmirrored.  The renderer flips the graphics
//! context horizontally before drawing when `SpriteRef::mirror` is `true`
//! (see `sprite_map.rs`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{AnyThread, MainThreadOnly};
use objc2_app_kit::{NSImage, NSImageView};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

use crate::manifest::{Attachment, AnimationDef, Manifest, SurfaceConfig};



// ---- Anchor ----

/// Attachment point for a sprite, in scaled display pixels.
/// See module-level docs for the coordinate conventions.
#[derive(Debug, Clone, Copy)]
pub struct Anchor {
    /// Horizontal component (meaning depends on attachment type).
    pub x: f64,
    /// Vertical component (meaning depends on attachment type).
    pub y: f64,
}

// ---- SpriteAssets ----

/// All loaded and scaled sprites for one character.
/// Both normal and horizontally-mirrored copies are pre-generated at load time.
pub struct SpriteAssets {
    images: HashMap<String, Retained<NSImage>>,
    mirrored: HashMap<String, Retained<NSImage>>,
    anchors: HashMap<String, Anchor>,
    native: HashMap<String, (u32, u32, Vec<u8>)>,
    native_mirrored: HashMap<String, (u32, u32, Vec<u8>)>,
    cage: RefCell<HashMap<(String, bool, u32, u32, i16), (u32, u32, Vec<u8>, f64, f64)>>,
    pub canonical_width: f64,
    pub animations: HashMap<String, AnimationDef>,
    pub surfaces: SurfaceConfig,
}

impl SpriteAssets {
    /// Load every sprite listed in `manifest` from `char_dir/sprite/`.
    /// Returns `None` if any listed sprite file is missing or unreadable.
    pub fn load(
        char_dir: &Path,
        manifest: &Manifest,
        display_width: f64,
    ) -> Option<Self> {
        let scale = display_width / manifest.canonical_width.max(1.0);
        let sprite_dir = char_dir.join("sprite");

        let mut images = HashMap::new();
        let mut mirrored = HashMap::new();
        let mut anchors = HashMap::new();
        let mut native = HashMap::new();
        let mut native_mirrored = HashMap::new();

        for (name, info) in &manifest.sprites {
            // Reject names that could escape the sprite directory (groundwork
            // for untrusted `.pmate` manifests).
            if !crate::manifest::is_safe_asset_name(name) {
                eprintln!("[assets] rejecting unsafe sprite name: {name:?}");
                return None;
            }
            let path = sprite_dir.join(format!("{name}.png"));
            let ns_path = NSString::from_str(path.to_str()?);

            let orig =
                unsafe { NSImage::initWithContentsOfFile(NSImage::alloc(), &ns_path) }?;
            let orig_h = unsafe { orig.size() }.height;
            let (nw, nh, native_rgba) = nsimage_to_rgba(&orig)?;
            let native_flip = mirror_rgba(&native_rgba, nw, nh);

            let scaled = scale_image(&orig, scale);
            let anchor = compute_anchor(info.x, info.y, orig_h, scale, &info.attachment);
            let mirror = unsafe { make_mirrored_image(&scaled) };

            native.insert(name.clone(), (nw, nh, native_rgba));
            native_mirrored.insert(name.clone(), (nw, nh, native_flip));
            mirrored.insert(name.clone(), mirror);
            images.insert(name.clone(), scaled);
            anchors.insert(name.clone(), anchor);
        }

        Some(SpriteAssets {
            images,
            mirrored,
            anchors,
            native,
            native_mirrored,
            cage: RefCell::new(HashMap::new()),
            canonical_width: manifest.canonical_width,
            animations: manifest.animations.clone(),
            surfaces: manifest.surfaces.clone(),
        })
    }

    /// Returns the (optionally mirrored) NSImage for `name`.
    pub fn image(&self, name: &str, mirror: bool) -> Option<&Retained<NSImage>> {
        if mirror {
            self.mirrored.get(name)
        } else {
            self.images.get(name)
        }
    }

    /// Returns the anchor for `name`, or `None` if not loaded.
    pub fn anchor(&self, name: &str) -> Option<Anchor> {
        self.anchors.get(name).copied()
    }

    /// Native PNG size in source pixels.
    pub fn native_size(&self, name: &str) -> (f64, f64) {
        self.native
            .get(name)
            .map(|(w, h, _)| (*w as f64, *h as f64))
            .unwrap_or((512.0, 512.0))
    }

    /// Cage blit source: native PNG scaled once to `dw×dh`, then rotated by a
    /// permitted `angle_id` about the foot. Cache key includes the angle.
    pub fn cage_rgba(
        &self,
        name: &str,
        mirror: bool,
        dw: u32,
        dh: u32,
        angle: crate::vivarium::AngleId,
    ) -> Option<(u32, u32, Vec<u8>, f64, f64)> {
        let key = (name.to_string(), mirror, dw, dh, angle.deg());
        {
            let cache = self.cage.borrow();
            if let Some(v) = cache.get(&key) {
                return Some(v.clone());
            }
        }
        let (sw, sh, src) = if mirror {
            self.native_mirrored.get(name)?
        } else {
            self.native.get(name)?
        };
        let scaled = crate::vivarium::scale_rgba_triangle(src, *sw, *sh, dw, dh);
        let out = crate::vivarium::rotate_rgba_about_foot(&scaled, dw, dh, angle.rad());
        self.cage.borrow_mut().insert(key, out.clone());
        Some(out)
    }

    /// Returns `(width, height)` of `name` in display pixels, or `(150, 150)` as a fallback.
    pub fn size(&self, name: &str, mirror: bool) -> (f64, f64) {
        self.image(name, mirror)
            .map(|img| { let s = unsafe { img.size() }; (s.width, s.height) })
            .unwrap_or((150.0, 150.0))
    }
}

fn nsimage_to_rgba(img: &NSImage) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let tiff: *mut AnyObject = msg_send![img, TIFFRepresentation];
        if tiff.is_null() {
            return None;
        }
        let tiff = Retained::retain(tiff)?;
        let cls = AnyClass::get(c"NSBitmapImageRep")?;
        let alloc: *mut AnyObject = msg_send![cls, alloc];
        let rep: *mut AnyObject = msg_send![alloc, initWithData: &*tiff];
        let rep = Retained::from_raw(rep)?;
        let w: isize = msg_send![&*rep, pixelsWide];
        let h: isize = msg_send![&*rep, pixelsHigh];
        let spp: isize = msg_send![&*rep, samplesPerPixel];
        if w < 1 || h < 1 || spp < 3 {
            return None;
        }
        let data: *mut u8 = msg_send![&*rep, bitmapData];
        if data.is_null() {
            return None;
        }
        let n = (w * h) as usize;
        let src = std::slice::from_raw_parts(data, n * spp as usize);
        let mut out = vec![0u8; n * 4];
        for i in 0..n {
            let s = i * spp as usize;
            let d = i * 4;
            out[d] = src[s];
            out[d + 1] = src[s + 1];
            out[d + 2] = src[s + 2];
            out[d + 3] = if spp >= 4 { src[s + 3] } else { 255 };
        }
        Some((w as u32, h as u32, out))
    }
}

fn mirror_rgba(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let w = w as usize;
    let h = h as usize;
    let mut out = vec![0u8; rgba.len()];
    for row in 0..h {
        for col in 0..w {
            let src = (row * w + col) * 4;
            let dst = (row * w + (w - 1 - col)) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    out
}

// ---- Image helpers ----

/// Return a new NSImage scaled uniformly by `scale`.
pub fn scale_image(src: &NSImage, scale: f64) -> Retained<NSImage> {
    let orig = unsafe { src.size() };
    let sz = NSSize::new(orig.width * scale, orig.height * scale);
    unsafe {
        let dst = NSImage::initWithSize(NSImage::alloc(), sz);
        dst.lockFocus();
        src.drawInRect(NSRect::new(NSPoint::ZERO, sz));
        dst.unlockFocus();
        dst
    }
}

/// Create a horizontally-flipped copy of `src` using NSAffineTransform.
unsafe fn make_mirrored_image(src: &NSImage) -> Retained<NSImage> {
    let sz = src.size();
    let dst = NSImage::initWithSize(NSImage::alloc(), sz);
    dst.lockFocus();

    // Apply horizontal-flip transform via NSAffineTransform
    let tf_cls = AnyClass::get(c"NSAffineTransform").expect("NSAffineTransform not found");
    let tf: *mut AnyObject = msg_send![tf_cls, transform];
    let _: () = msg_send![tf, translateXBy: sz.width yBy: 0.0f64];
    let _: () = msg_send![tf, scaleXBy: -1.0f64 yBy: 1.0f64];
    let _: () = msg_send![tf, concat];

    src.drawInRect(NSRect::new(NSPoint::ZERO, sz));
    dst.unlockFocus();
    dst
}

/// Build a single-image `NSImageView` sized to `image`.
/// The `mt` token is required by AppKit's main-thread checks.
pub fn make_image_view(image: &NSImage, mt: MainThreadMarker) -> Retained<NSImageView> {
    let sz = unsafe { image.size() };
    unsafe {
        let iv = NSImageView::initWithFrame(
            NSImageView::alloc(mt),
            NSRect::new(NSPoint::ZERO, sz),
        );
        iv.setImage(Some(image));
        // Keep per-pixel alpha (default bezel/background would look opaque).
        let _: () = msg_send![&*iv, setImageFrameStyle: 0u64];
        iv.setWantsLayer(true);
        iv
    }
}

// ---- Anchor helpers ----

fn compute_anchor(
    x_orig: Option<f64>,
    y_orig: Option<f64>,
    sprite_h_orig: f64,
    scale: f64,
    attachment: &Attachment,
) -> Anchor {
    match attachment {
        Attachment::LineY => {
            // foot line: y_orig pixels from the top → convert to distance from bottom
            let y = (sprite_h_orig - y_orig.unwrap_or(0.0)) * scale;
            Anchor { x: 0.0, y }
        }
        Attachment::LineX => {
            // wall grip: x_orig pixels from the left
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
