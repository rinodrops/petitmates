//! Recolor a character sprite from a chosen hue.
//!
//! Eyes, mouth, outline, and the bearded dragon's cream belly stay as drawn.
//! Skin beside an open eye follows the body. A pond turtle's shell and skin
//! take separate hues. Walk and vertical-climb poses with a painted mask use
//! that mask for the shell.

use image::{Rgba, RgbaImage};

pub const DRAGON_HUE: f64 = 38.0;
pub const SHELL_HUE: f64 = 42.0;
pub const SKIN_HUE: f64 = 46.0;

#[derive(Clone, Copy, Debug)]
pub enum Coat {
    Dragon { hue: f64 },
    Turtle { shell: f64, skin: f64 },
    DragonColor(RegionColor),
    TurtleColor { shell: RegionColor, skin: RegionColor },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RegionColor {
    Original,
    Legacy { hue: f64, base: f64 },
    Hsv { base: [u16; 3], target: [u16; 3] },
}

impl RegionColor {
    fn is_original(self) -> bool {
        match self {
            Self::Original => true,
            Self::Legacy { hue, base } => (hue - base).abs() < 0.5,
            Self::Hsv { base, target } => base == target,
        }
    }

    fn apply(self, px: Rgba<u8>) -> Rgba<u8> {
        match self {
            Self::Original => px,
            Self::Legacy { hue, base } => repaint(px, hue, base),
            Self::Hsv { base, target } if base == target => px,
            Self::Hsv { base, target } => {
                let (h, s, v) = hsv(px);
                let [h0, s0, v0] = base.map(f64::from);
                let [h1, s1, v1] = target.map(f64::from);
                let (r, g, b) = hsv_to_rgb(h + h1 - h0,
                    map_tone(s, s0 / 100.0, s1 / 100.0),
                    map_tone(v, v0 / 100.0, v1 / 100.0));
                Rgba([r, g, b, px[3]])
            }
        }
    }
}

/// Map the reference tone to the target while keeping the order of shadows
/// and highlights and avoiding clipping elsewhere in the range.
fn map_tone(value: f64, base: f64, target: f64) -> f64 {
    if value <= base {
        if base == 0.0 { target } else { value / base * target }
    } else {
        target + (value - base) / (1.0 - base) * (1.0 - target)
    }
}

impl Coat {
    pub fn is_original(self) -> bool {
        match self {
            Coat::Dragon { hue } => (hue - DRAGON_HUE).abs() < 0.5,
            Coat::Turtle { shell, skin } => {
                (shell - SHELL_HUE).abs() < 0.5 && (skin - SKIN_HUE).abs() < 0.5
            }
            Coat::DragonColor(body) => body.is_original(),
            Coat::TurtleColor { shell, skin } => shell.is_original() && skin.is_original(),
        }
    }
}

/// Tint PNG bytes. Returns the original bytes when the coat is the asset hue.
pub fn tint_png(png: &[u8], sprite: &str, coat: Coat) -> Vec<u8> {
    if coat.is_original() {
        return png.to_vec();
    }
    let Ok(img) = image::load_from_memory(png) else {
        return png.to_vec();
    };
    let mut img = img.to_rgba8();
    tint_image(&mut img, sprite, coat);
    let mut out = Vec::new();
    let _ = image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png);
    if out.is_empty() { png.to_vec() } else { out }
}

pub fn tint_image(img: &mut RgbaImage, sprite: &str, coat: Coat) {
    if coat.is_original() {
        return;
    }
    let regions = tint_regions(img, sprite, is_turtle(coat));
    apply_tint(img, &regions, coat);
}

fn is_turtle(coat: Coat) -> bool {
    matches!(coat, Coat::Turtle { .. } | Coat::TurtleColor { .. })
}

#[derive(Clone, Copy, PartialEq)]
enum TintRegion {
    Protected,
    Primary,
    Secondary,
}

fn tint_regions(img: &RgbaImage, sprite: &str, turtle: bool) -> Vec<TintRegion> {
    let eyes = eye_protect(img);
    let shell = turtle.then(|| shell_mask(img, mask_for(sprite).as_ref()));
    img.pixels().enumerate().map(|(i, px)| {
        if eyes[i] || px[3] == 0 || is_outline(px) || is_mouth(px) {
            return TintRegion::Protected;
        }
        if let Some(shell) = &shell {
            if is_stripe(px) {
                TintRegion::Protected
            } else if shell[i] {
                TintRegion::Primary
            } else if is_olive(px) {
                TintRegion::Secondary
            } else {
                TintRegion::Protected
            }
        } else if !is_cream(px) && is_dragon_body(px) {
            TintRegion::Primary
        } else {
            TintRegion::Protected
        }
    }).collect()
}

fn apply_tint(img: &mut RgbaImage, regions: &[TintRegion], coat: Coat) {
    let (primary, secondary) = match coat {
        Coat::Dragon { hue } => (RegionColor::Legacy { hue, base: DRAGON_HUE }, RegionColor::Original),
        Coat::Turtle { shell, skin } => (
            RegionColor::Legacy { hue: shell, base: SHELL_HUE },
            RegionColor::Legacy { hue: skin, base: SKIN_HUE },
        ),
        Coat::DragonColor(body) => (body, RegionColor::Original),
        Coat::TurtleColor { shell, skin } => (shell, skin),
    };
    for (px, region) in img.pixels_mut().zip(regions) {
        match region {
            TintRegion::Primary => *px = primary.apply(*px),
            TintRegion::Secondary => *px = secondary.apply(*px),
            TintRegion::Protected => {}
        }
    }
}

/// Prepared sprite for repeated preview edits.
#[allow(dead_code)]
pub struct TintPreview {
    image: RgbaImage,
    regions: Vec<TintRegion>,
}

#[allow(dead_code)]
impl TintPreview {
    pub fn new(image: RgbaImage, sprite: &str, turtle: bool) -> Self {
        let regions = tint_regions(&image, sprite, turtle);
        Self { image, regions }
    }

    pub fn render(&self, coat: Option<Coat>) -> RgbaImage {
        let mut image = self.image.clone();
        if let Some(coat) = coat.filter(|coat| !coat.is_original()) {
            apply_tint(&mut image, &self.regions, coat);
        }
        image
    }
}

/// Hue is absolute. Saturation and value are multipliers on the pixel (1.0 keeps them).
/// The local coat tuner uses this. [`tint_image`] is the shipping tint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegionAdjust {
    pub hue: f64,
    pub sat: f64,
    pub val: f64,
}

#[derive(Clone, Copy, Debug)]
pub enum AdjustCoat {
    Dragon(RegionAdjust),
    Turtle { shell: RegionAdjust, skin: RegionAdjust },
}

/// Recolor protected regions with independent hue, saturation, and value.
/// Eyes, mouth, outline, the dragon's cream belly, and the turtle's yellow stripes stay.
#[allow(dead_code)]
pub fn tint_adjust(img: &mut RgbaImage, sprite: &str, coat: AdjustCoat) {
    let mask = mask_for(sprite);
    let eye = eye_protect(img);
    let shell = shell_mask(img, mask.as_ref());
    for (i, px) in img.pixels_mut().enumerate() {
        if eye[i] || is_outline(px) || is_mouth(px) {
            continue;
        }
        match coat {
            AdjustCoat::Dragon(adj) => {
                if is_cream(px) || !is_dragon_body(px) {
                    continue;
                }
                *px = apply_region(*px, adj);
            }
            AdjustCoat::Turtle { shell: shell_adj, skin } => {
                if is_stripe(px) {
                    continue;
                }
                if shell[i] {
                    *px = apply_region(*px, shell_adj);
                } else if is_olive(px) {
                    *px = apply_region(*px, skin);
                }
            }
        }
    }
}

fn apply_region(px: Rgba<u8>, adj: RegionAdjust) -> Rgba<u8> {
    let (_h, s, v) = hsv(px);
    let s2 = (s * adj.sat).clamp(0.0, 1.0);
    let v2 = (v * adj.val).clamp(0.0, 1.0);
    let (r, g, b) = hsv_to_rgb(adj.hue, s2, v2);
    Rgba([r, g, b, px[3]])
}

fn repaint(px: Rgba<u8>, target: f64, base: f64) -> Rgba<u8> {
    let (h, s, v) = hsv(px);
    let (h, s, v) = legacy_hsv(h, s, v, target, base);
    let (r, g, b) = hsv_to_rgb(h, s, v);
    Rgba([r, g, b, px[3]])
}

fn legacy_hsv(h: f64, s: f64, v: f64, target: f64, base: f64) -> (f64, f64, f64) {
    let delta = wrap_deg(target - base);
    if delta.abs() <= 20.0 {
        (wrap_deg(h + delta), s, v)
    } else {
        let s2 = (s * (0.62 / 0.77)).min(0.78);
        let t = (v - 0.85) / 0.15;
        let v2 = (0.27 + t.clamp(-0.4, 1.2) * 0.26).clamp(0.12, 0.62);
        (wrap_deg(target), s2, v2)
    }
}

/// Show the legacy transform's representative color when editing a saved hue.
#[allow(dead_code)]
pub fn legacy_editor_color(_path: &str, hue: f64, base: [u16; 3]) -> [u16; 3] {
    let [h, s, v] = base.map(f64::from);
    let (h, s, v) = legacy_hsv(h, s / 100.0, v / 100.0, hue, h);
    [h.round().rem_euclid(360.0) as u16, (s * 100.0).round() as u16, (v * 100.0).round() as u16]
}

fn wrap_deg(d: f64) -> f64 {
    let mut x = d % 360.0;
    if x < 0.0 {
        x += 360.0;
    }
    if x > 180.0 {
        x -= 360.0;
    }
    x
}

fn mask_for(sprite: &str) -> Option<RgbaImage> {
    let bytes: &[u8] = match sprite {
        "s-walk-0" => include_bytes!("../assets/pond_turtle/mask/s-walk-0.png"),
        "s-walk-1" => include_bytes!("../assets/pond_turtle/mask/s-walk-1.png"),
        "s-walk-2" => include_bytes!("../assets/pond_turtle/mask/s-walk-2.png"),
        "s-walk-3" => include_bytes!("../assets/pond_turtle/mask/s-walk-3.png"),
        "s-hang-wall-0" => include_bytes!("../assets/pond_turtle/mask/s-hang-wall-0.png"),
        "s-hang-wall-1" => include_bytes!("../assets/pond_turtle/mask/s-hang-wall-1.png"),
        "s-hang-wall-2" => include_bytes!("../assets/pond_turtle/mask/s-hang-wall-2.png"),
        _ => return None,
    };
    image::load_from_memory(bytes)
        .ok()
        .map(|img| img.to_rgba8())
}

fn shell_mask(img: &RgbaImage, painted: Option<&RgbaImage>) -> Vec<bool> {
    let (w, h) = img.dimensions();
    let n = (w * h) as usize;
    if let Some(mask) = painted {
        if mask.dimensions() == (w, h) {
            return mask
                .pixels()
                .map(|p| p[0] > p[2] && p[0] > 160 && p[3] > 16)
                .collect();
        }
    }
    let mut olive = vec![false; n];
    for (i, px) in img.pixels().enumerate() {
        olive[i] = is_olive(px) && !is_outline(px);
    }
    let mut seen = vec![false; n];
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let w = w as i32;
    let h = h as i32;
    for start in 0..n {
        if !olive[start] || seen[start] {
            continue;
        }
        let mut q = vec![start];
        seen[start] = true;
        let mut cells = Vec::new();
        while let Some(i) = q.pop() {
            cells.push(i);
            let x = (i as i32) % w;
            let y = (i as i32) / w;
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                let nx = x + dx;
                let ny = y + dy;
                if nx < 0 || ny < 0 || nx >= w || ny >= h {
                    continue;
                }
                let k = (ny * w + nx) as usize;
                if olive[k] && !seen[k] {
                    seen[k] = true;
                    q.push(k);
                }
            }
        }
        if cells.len() >= 200 {
            groups.push(cells);
        }
    }
    let eye = eye_center(img);
    let Some((ex, ey)) = eye else {
        let mut out = vec![false; n];
        if let Some(big) = groups.iter().max_by_key(|g| g.len()) {
            if big.len() * 2 >= olive.iter().filter(|p| **p).count() {
                for i in big {
                    out[*i] = true;
                }
            }
        }
        return out;
    };
    let dist = |g: &[usize]| {
        let (mut sx, mut sy) = (0.0, 0.0);
        for i in g {
            sx += (*i as i32 % w) as f64;
            sy += (*i as i32 / w) as f64;
        }
        let c = g.len() as f64;
        let dx = sx / c - ex;
        let dy = sy / c - ey;
        dx * dx + dy * dy
    };
    let Some(head) = groups.iter().min_by(|a, b| dist(a).total_cmp(&dist(b))) else {
        return vec![false; n];
    };
    let far_min = 0.36 * w.max(h) as f64;
    let mut shell = vec![false; n];
    let mut best: Option<&Vec<usize>> = None;
    for g in &groups {
        if std::ptr::eq(g, head) {
            continue;
        }
        if dist(g).sqrt() >= far_min && best.map(|b| g.len() > b.len()).unwrap_or(true) {
            best = Some(g);
        }
    }
    if let Some(g) = best {
        if g.len() * 100 / olive.iter().filter(|p| **p).count().max(1) >= 35 {
            for i in g {
                shell[*i] = true;
            }
            return shell;
        }
    }
    // Gather dorsal scutes on the far side of the eye.
    let mut dorsal: Vec<&Vec<usize>> = Vec::new();
    for g in &groups {
        if std::ptr::eq(g, head) || dist(g).sqrt() < far_min {
            continue;
        }
        let cy = g.iter().map(|i| *i as i32 / w).sum::<i32>() as f64 / g.len() as f64;
        if cy <= h as f64 * 0.72 {
            dorsal.push(g);
        }
    }
    for g in dorsal {
        for i in g {
            shell[*i] = true;
        }
    }
    shell
}

fn eye_protect(img: &RgbaImage) -> Vec<bool> {
    let (w, h) = img.dimensions();
    let n = (w * h) as usize;
    let mut white = vec![false; n];
    for (i, px) in img.pixels().enumerate() {
        white[i] = is_near_white(*px);
    }
    let w = w as i32;
    let h = h as i32;
    let mut seen = vec![false; n];
    let mut keep = vec![false; n];
    for start in 0..n {
        if !white[start] || seen[start] {
            continue;
        }
        let mut q = vec![start];
        seen[start] = true;
        let mut cells = Vec::new();
        while let Some(i) = q.pop() {
            cells.push(i);
            let x = (i as i32) % w;
            let y = (i as i32) / w;
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                let nx = x + dx;
                let ny = y + dy;
                if nx < 0 || ny < 0 || nx >= w || ny >= h {
                    continue;
                }
                let k = (ny * w + nx) as usize;
                if white[k] && !seen[k] {
                    seen[k] = true;
                    q.push(k);
                }
            }
        }
        if cells.len() >= 40 {
            for i in cells {
                keep[i] = true;
            }
        }
    }
    expand_eye_protection(img, keep)
}

fn expand_eye_protection(img: &RgbaImage, keep: Vec<bool>) -> Vec<bool> {
    let mut protect = keep.clone();
    let (w, h) = (img.width() as usize, img.height() as usize);
    const REACH: [usize; 6] = [5, 4, 4, 4, 3, 0];
    let rgba = img.as_raw();
    for (i, &is_eye) in keep.iter().enumerate() {
        if !is_eye {
            continue;
        }
        let (x, y) = (i % w, i / w);
        for ny in y.saturating_sub(5)..=(y + 5).min(h - 1) {
            let reach = REACH[y.abs_diff(ny)];
            let row = ny * w;
            for nx in x.saturating_sub(reach)..=(x + reach).min(w - 1) {
                let j = row + nx;
                let offset = 4 * j;
                // HSV value < 0.75 is equivalent to every RGB byte being < 192.
                if !protect[j] && rgba[offset..offset + 3].iter().all(|&c| c < 192) {
                    protect[j] = true;
                }
            }
        }
    }
    protect
}

fn eye_center(img: &RgbaImage) -> Option<(f64, f64)> {
    let mut n = 0.0;
    let mut sx = 0.0;
    let mut sy = 0.0;
    for (x, y, px) in img.enumerate_pixels() {
        if is_near_white(*px) {
            n += 1.0;
            sx += x as f64;
            sy += y as f64;
        }
    }
    (n > 30.0).then_some((sx / n, sy / n))
}

fn hsv(px: Rgba<u8>) -> (f64, f64, f64) {
    let r = px[0] as f64 / 255.0;
    let g = px[1] as f64 / 255.0;
    let b = px[2] as f64 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let s = if max == 0.0 { 0.0 } else { d / max };
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, max)
}

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (u8, u8, u8) {
    let h = h.rem_euclid(360.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (rp, gp, bp) = match h {
        h if h < 60.0 => (c, x, 0.0),
        h if h < 120.0 => (x, c, 0.0),
        h if h < 180.0 => (0.0, c, x),
        h if h < 240.0 => (0.0, x, c),
        h if h < 300.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((rp + m) * 255.0).round() as u8,
        ((gp + m) * 255.0).round() as u8,
        ((bp + m) * 255.0).round() as u8,
    )
}

fn is_outline(px: &Rgba<u8>) -> bool {
    if px[3] < 16 {
        return false;
    }
    let (_h, s, v) = hsv(*px);
    (px[0].max(px[1]).max(px[2]) < 60 && s < 0.5) || (v < 0.16 && s < 0.55)
}

fn is_mouth(px: &Rgba<u8>) -> bool {
    let (h, s, v) = hsv(*px);
    (h < 16.0 || h > 345.0) && s > 0.45 && v > 0.35
}

fn is_near_white(px: Rgba<u8>) -> bool {
    let (_h, s, v) = hsv(px);
    px[3] > 16 && s < 0.22 && v > 0.78
}

fn is_cream(px: &Rgba<u8>) -> bool {
    let (h, s, v) = hsv(*px);
    (10.0..=60.0).contains(&h) && s < 0.40 && v > 0.58
}

fn is_dragon_body(px: &Rgba<u8>) -> bool {
    let (h, s, v) = hsv(*px);
    (6.0..=58.0).contains(&h) && s >= 0.28 && v >= 0.20
}

fn is_stripe(px: &Rgba<u8>) -> bool {
    let (h, s, v) = hsv(*px);
    (30.0..=75.0).contains(&h) && s >= 0.42 && v >= 0.58
}

fn is_olive(px: &Rgba<u8>) -> bool {
    let (h, s, v) = hsv(*px);
    (18.0..=72.0).contains(&h) && s >= 0.22 && (0.16..0.62).contains(&v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_shell_preserves_original_skin_on_climbs() {
        let sprite = "s-hang-wall-0";
        let bytes = include_bytes!("../assets/pond_turtle/sprite/s-hang-wall-0.png");
        let original = image::load_from_memory(bytes).unwrap().to_rgba8();
        let shell = RegionColor::Hsv { base: [42, 48, 42], target: [200, 68, 60] };
        let coat = Coat::TurtleColor { shell, skin: RegionColor::Original };
        let preview = TintPreview::new(original.clone(), sprite, true).render(Some(coat));
        let mut runtime = original.clone();
        tint_image(&mut runtime, sprite, coat);
        assert_eq!(preview, runtime);
        assert_eq!(runtime.get_pixel(144, 236), original.get_pixel(144, 236));
        assert_ne!(runtime.get_pixel(305, 337), original.get_pixel(305, 337));
    }

    #[test]
    fn climb_shell_keeps_its_own_hue() {
        let png = include_bytes!("../assets/pond_turtle/sprite/s-hang-wall-0.png");
        let tinted = tint_png(
            png,
            "s-hang-wall-0",
            Coat::Turtle {
                shell: 200.0,
                skin: SKIN_HUE,
            },
        );
        let img = image::load_from_memory(&tinted).unwrap().to_rgba8();
        let shell = hsv(*img.get_pixel(305, 337));
        let skin = hsv(*img.get_pixel(144, 236));
        assert!(
            (shell.0 - 200.0).abs() < 8.0,
            "shell hue {}",
            shell.0
        );
        assert!(
            (skin.0 - SKIN_HUE).abs() < 20.0,
            "skin hue {}",
            skin.0
        );
    }
}
