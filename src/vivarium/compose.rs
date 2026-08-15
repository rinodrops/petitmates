//! Software compose of the glass cage into a straight RGBA framebuffer (top-left origin).
//!
//! Glass is a CPU stand-in for a liquid-glass look: thin panes dim a few
//! percent with a faint turquoise absorption; thick wall slabs go deeper teal.
//! White is only Fresnel / specular on the lips. Backdrop blur is CGS on the
//! window (see `LookConfig::blur`), not a CPU frost of this framebuffer.

use super::{inner_rect, BackKind, LookConfig, VivariumConfig, WALL_GLASS_H, WALL_GLASS_W};

#[derive(Clone)]
pub struct Framebuffer {
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

/// Sprite blit: `rgba` is `src_w * src_h` straight RGBA; drawn into `w * h`.
pub struct MateBlit<'a> {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub src_w: u32,
    pub src_h: u32,
    /// Straight RGBA, row-major, top-left.
    pub rgba: &'a [u8],
}

pub fn compose(cfg: &VivariumConfig, look: &LookConfig, mates: &[MateBlit<'_>]) -> Framebuffer {
    let (back, glass) = static_layers(cfg, look);
    composite_layers(cfg, look, &back, &glass, mates)
}

/// Back + land, and glass-only. Rebuild only when look or cage size changes.
pub fn static_layers(cfg: &VivariumConfig, look: &LookConfig) -> (Framebuffer, Framebuffer) {
    let (pw, ph) = cfg.pixel_size();
    let sx = pw as f64 / cfg.logical_w.max(1) as f64;
    let sy = ph as f64 / cfg.logical_h.max(1) as f64;
    let mut back = Framebuffer {
        rgba: vec![0u8; (pw * ph * 4) as usize],
        w: pw,
        h: ph,
    };
    fill_back(&mut back, cfg);
    draw_back_pane(&mut back, cfg, look, sx, sy);
    draw_land_hint(&mut back, cfg, sx, sy);

    let mut glass = Framebuffer {
        rgba: vec![0u8; (pw * ph * 4) as usize],
        w: pw,
        h: ph,
    };
    draw_glass(&mut glass, cfg, look, sx, sy);
    (back, glass)
}

pub fn composite_layers(
    cfg: &VivariumConfig,
    look: &LookConfig,
    back: &Framebuffer,
    glass: &Framebuffer,
    mates: &[MateBlit<'_>],
) -> Framebuffer {
    let sx = back.w as f64 / cfg.logical_w.max(1) as f64;
    let sy = back.h as f64 / cfg.logical_h.max(1) as f64;
    let mut fb = back.clone();
    for m in mates {
        blit_mate(&mut fb, m);
    }
    over_layer(&mut fb, glass);
    let radius = (7.0 * sx.min(sy)).max(3.0);
    apply_round_corners(&mut fb, radius);
    stroke_silhouette(&mut fb, radius, look.outline_width, look.outline);
    fb
}

fn over_layer(fb: &mut Framebuffer, src: &Framebuffer) {
    let n = fb.rgba.len().min(src.rgba.len()) / 4;
    for i in 0..n {
        let o = i * 4;
        let sa = src.rgba[o + 3];
        if sa == 0 {
            continue;
        }
        let x = (i as u32) % fb.w;
        let y = (i as u32) / fb.w;
        over(fb, x, y, src.rgba[o], src.rgba[o + 1], src.rgba[o + 2], sa);
    }
}

fn fill_back(fb: &mut Framebuffer, cfg: &VivariumConfig) {
    let (r1, g1, b1, a1) = parse_hex(&cfg.back.color).unwrap_or((216, 224, 228, 255));
    let (r2, g2, b2, a2) = parse_hex(&cfg.back.color2).unwrap_or((185, 198, 204, 255));
    let op = cfg.back.opacity.clamp(0.0, 1.0);
    if op < 0.01 {
        return;
    }
    let kind = match cfg.back.kind {
        BackKind::Image => BackKind::Gradient, // file wallpaper: later
        k => k,
    };
    for y in 0..fb.h {
        let t = if fb.h <= 1 { 0.0 } else { y as f64 / (fb.h - 1) as f64 };
        let (r, g, b, a) = match kind {
            BackKind::Solid => (r1, g1, b1, a1),
            BackKind::Gradient | BackKind::Image => (
                lerp_u8(r1, r2, t),
                lerp_u8(g1, g2, t),
                lerp_u8(b1, b2, t),
                lerp_u8(a1, a2, t),
            ),
        };
        let a = (a as f64 * op).round() as u8;
        for x in 0..fb.w {
            put(fb, x, y, r, g, b, a);
        }
    }
}

fn draw_back_pane(fb: &mut Framebuffer, cfg: &VivariumConfig, look: &LookConfig, sx: f64, sy: f64) {
    if look.back.dim < 0.002 {
        return;
    }
    let inner = inner_rect(cfg.logical_w, cfg.logical_h);
    let x0 = (inner.x as f64 * sx).floor() as u32;
    let y0 = (inner.y as f64 * sy).floor() as u32;
    let x1 = ((inner.x + inner.w) as f64 * sx).ceil().min(fb.w as f64) as u32;
    let y1 = ((inner.y + inner.h) as f64 * sy).ceil().min(fb.h as f64) as u32;
    let tint = look.back.tint();
    for y in y0..y1 {
        for x in x0..x1 {
            overlay_glass(fb, x, y, look.back.dim, 0.0, tint);
        }
    }
}

fn draw_land_hint(fb: &mut Framebuffer, cfg: &VivariumConfig, sx: f64, sy: f64) {
    let inner = inner_rect(cfg.logical_w, cfg.logical_h);
    let land_h = (inner.h as f64 * 0.18).max(8.0);
    let x0 = (inner.x as f64 * sx).floor() as u32;
    let x1 = ((inner.x + inner.w) as f64 * sx).ceil().min(fb.w as f64) as u32;
    let y1 = ((inner.y + inner.h) as f64 * sy).ceil().min(fb.h as f64) as u32;
    let y0 = ((inner.y + inner.h) as f64 * sy - land_h * sy).floor() as u32;
    for y in y0..y1 {
        for x in x0..x1 {
            over(fb, x, y, 196, 186, 160, 12);
        }
    }
}

/// Thin face: ~5% turquoise absorption. Thick walls: deeper teal.
/// White is lips / specular only, encoded so premul-over-desktop lightens.
fn draw_glass(fb: &mut Framebuffer, cfg: &VivariumConfig, look: &LookConfig, sx: f64, sy: f64) {
    let inner = inner_rect(cfg.logical_w, cfg.logical_h);
    let ix0 = (inner.x as f64 * sx).floor();
    let iy0 = (inner.y as f64 * sy).floor();
    let ix1 = ((inner.x + inner.w) as f64 * sx).ceil();
    let iy1 = ((inner.y + inner.h) as f64 * sy).ceil();
    let wall_w = (WALL_GLASS_W as f64 * sx).max(2.0);
    let wall_h = (WALL_GLASS_H as f64 * sy).max(2.0);
    let top_h = (super::TOP_EDGE_H as f64 * sy).max(1.0);
    let face_falloff = (22.0 * sx).max(8.0);
    let spec_x = ix0 + (ix1 - ix0) * look.front.spec_x;
    let spec_sigma = ((ix1 - ix0) * look.front.spec_width).max(4.0);
    let face_tint = look.front.tint();
    let wall_tint = look.side.tint();
    let face_dim = look.front.dim;
    let side_dim = look.side.dim;
    let bottom_dim = (look.side.dim * 1.25).min(1.0);
    let top_dim = look.side.dim * 0.55;

    for y in 0..fb.h {
        let yf = y as f64 + 0.5;
        for x in 0..fb.w {
            let xf = x as f64 + 0.5;
            // Sides run full height; floor/lid sit between them (real tank join).
            let in_left = xf < wall_w;
            let in_right = xf >= fb.w as f64 - wall_w;
            let in_side = in_left || in_right;
            let in_bottom = !in_side && yf >= fb.h as f64 - wall_h;
            let in_top = !in_side && yf < top_h;
            let in_face = !in_side && !in_bottom && !in_top;

            let mut dim = 0.0;
            let mut hl = 0.0;
            let tint = if in_face { face_tint } else { wall_tint };

            if in_face {
                let d = (xf - ix0)
                    .min(ix1 - xf)
                    .min(yf - iy0)
                    .min(iy1 - yf)
                    .max(0.0);
                let t = (1.0 - (d / face_falloff).clamp(0.0, 1.0)).powf(2.6);
                dim = face_dim;
                hl = t * 0.18;
            } else if in_side {
                let across = if in_left {
                    xf / wall_w
                } else {
                    (fb.w as f64 - xf) / wall_w
                }
                .clamp(0.0, 1.0);
                let v_lip = (1.0 - across).powf(5.0) + across.powf(5.0);
                dim = side_dim;
                hl = v_lip.min(1.0) * 0.45;
            } else if in_bottom || in_top {
                let across = if in_bottom {
                    (fb.h as f64 - yf) / wall_h
                } else {
                    yf / top_h
                }
                .clamp(0.0, 1.0);
                let h_lip = (1.0 - across).powf(5.0) + across.powf(5.0);
                dim = if in_bottom { bottom_dim } else { top_dim };
                hl = h_lip.min(1.0) * 0.45;
            }

            if in_face && look.front.spec_strength > 0.001 {
                let dx = (xf - spec_x) / spec_sigma;
                hl += (-0.5 * dx * dx).exp() * look.front.spec_strength;
            }

            overlay_glass(fb, x, y, dim, hl, tint);
        }
    }

    let y0 = iy0.round().clamp(0.0, fb.h as f64 - 1.0) as u32;
    let y1 = (iy1.round() - 1.0).clamp(0.0, fb.h as f64 - 1.0) as u32;
    let x0 = ix0.round().clamp(0.0, fb.w as f64 - 1.0) as u32;
    let x1 = (ix1.round() - 1.0).clamp(0.0, fb.w as f64 - 1.0) as u32;
    let y_bot = fb.h.saturating_sub(1);
    // Top rim and ground contact: full width (sides reach the floor).
    // Floor-plate inner lip: only between the side panes.
    for x in 0..fb.w {
        overlay_glass(fb, x, 0, 0.0, 0.50, face_tint);
        if fb.h > 1 {
            overlay_glass(fb, x, 1, 0.0, 0.22, face_tint);
        }
        overlay_glass(fb, x, y0, 0.0, 0.42, face_tint);
        if y0 + 1 < fb.h {
            overlay_glass(fb, x, y0 + 1, 0.0, 0.20, face_tint);
        }
        overlay_glass(fb, x, y_bot, 0.0, 0.28, face_tint);
        if y_bot > 0 {
            overlay_glass(fb, x, y_bot - 1, 0.0, 0.14, face_tint);
        }
    }
    for x in x0..=x1 {
        overlay_glass(fb, x, y1, 0.0, 0.38, face_tint);
        if y1 > 0 {
            overlay_glass(fb, x, y1 - 1, 0.0, 0.18, face_tint);
        }
    }
    // Inner faces of the side panes, full height (past the floor plate).
    for y in 0..fb.h {
        overlay_glass(fb, x0, y, 0.0, 0.40, face_tint);
        if x0 + 1 < fb.w {
            overlay_glass(fb, x0 + 1, y, 0.0, 0.20, face_tint);
        }
        overlay_glass(fb, x1, y, 0.0, 0.40, face_tint);
        if x1 > 0 {
            overlay_glass(fb, x1 - 1, y, 0.0, 0.20, face_tint);
        }
    }
}

fn apply_round_corners(fb: &mut Framebuffer, radius: f64) {
    let r = radius.min(fb.w.min(fb.h) as f64 * 0.2).max(1.0);
    let w = fb.w as f64;
    let h = fb.h as f64;
    for y in 0..fb.h {
        let yf = y as f64 + 0.5;
        for x in 0..fb.w {
            let xf = x as f64 + 0.5;
            let cx = if xf < r {
                r
            } else if xf > w - r {
                w - r
            } else {
                continue;
            };
            let cy = if yf < r {
                r
            } else if yf > h - r {
                h - r
            } else {
                continue;
            };
            let d = ((xf - cx) * (xf - cx) + (yf - cy) * (yf - cy)).sqrt();
            let cover = (r - d + 0.5).clamp(0.0, 1.0);
            if cover >= 0.999 {
                continue;
            }
            if cover < 0.01 {
                put(fb, x, y, 0, 0, 0, 0);
            } else {
                let i = idx(fb, x, y);
                fb.rgba[i + 3] = ((fb.rgba[i + 3] as f64) * cover).round() as u8;
            }
        }
    }
}

/// Dark stroke on the rounded-rect silhouette. Native `NSWindow` shadows stay
/// off (opaque backing). Outside coverage only exists at the cut corners.
fn stroke_silhouette(fb: &mut Framebuffer, radius: f64, width: f64, strength: f64) {
    if strength < 0.01 || width < 0.05 {
        return;
    }
    let r = radius.min(fb.w.min(fb.h) as f64 * 0.2).max(1.0);
    let w = fb.w as f64;
    let h = fb.h as f64;
    let ow = width.max(0.5);
    for y in 0..fb.h {
        let py = y as f64 + 0.5;
        for x in 0..fb.w {
            let px = x as f64 + 0.5;
            let sd = sd_rounded_box(px, py, w, h, r);
            let t = 1.0 - (sd.abs() / ow).clamp(0.0, 1.0);
            if t <= 0.0 {
                continue;
            }
            let a = if sd >= 0.0 { t * t } else { t };
            let alpha = (strength * a * 255.0).round().clamp(0.0, 255.0) as u8;
            if alpha > 0 {
                over(fb, x, y, 8, 14, 16, alpha);
            }
        }
    }
}

fn sd_rounded_box(px: f64, py: f64, w: f64, h: f64, r: f64) -> f64 {
    let hw = w * 0.5;
    let hh = h * 0.5;
    let dx = (px - hw).abs() - (hw - r);
    let dy = (py - hh).abs() - (hh - r);
    let ax = dx.max(0.0);
    let ay = dy.max(0.0);
    ax.hypot(ay) + dx.max(dy).min(0.0) - r
}

/// `dim`/`hl` are 0..1. Result = dest*(1-d)*(1-h) + tint*d*(1-h) + white*h.
fn overlay_glass(fb: &mut Framebuffer, x: u32, y: u32, dim: f64, hl: f64, tint: (u8, u8, u8)) {
    let d = dim.clamp(0.0, 1.0);
    let h = hl.clamp(0.0, 1.0);
    if d < 0.002 && h < 0.002 {
        return;
    }
    let a = 1.0 - (1.0 - d) * (1.0 - h);
    if a < 1e-6 {
        return;
    }
    let ih = 1.0 - h;
    let pr = (tint.0 as f64 / 255.0) * d * ih + h;
    let pg = (tint.1 as f64 / 255.0) * d * ih + h;
    let pb = (tint.2 as f64 / 255.0) * d * ih + h;
    over(
        fb,
        x,
        y,
        ((pr / a) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((pg / a) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((pb / a) * 255.0).round().clamp(0.0, 255.0) as u8,
        (a * 255.0).round().clamp(0.0, 255.0) as u8,
    );
}

fn blit_mate(fb: &mut Framebuffer, m: &MateBlit<'_>) {
    let sw = m.src_w.max(1);
    let sh = m.src_h.max(1);
    for row in 0..m.h {
        let dy = m.y + row as i32;
        if dy < 0 || dy >= fb.h as i32 {
            continue;
        }
        let sy = row as u64 * sh as u64 / m.h.max(1) as u64;
        for col in 0..m.w {
            let dx = m.x + col as i32;
            if dx < 0 || dx >= fb.w as i32 {
                continue;
            }
            let sx = col as u64 * sw as u64 / m.w.max(1) as u64;
            let i = ((sy * sw as u64 + sx) * 4) as usize;
            if i + 3 >= m.rgba.len() {
                continue;
            }
            over(
                fb,
                dx as u32,
                dy as u32,
                m.rgba[i],
                m.rgba[i + 1],
                m.rgba[i + 2],
                m.rgba[i + 3],
            );
        }
    }
}

/// Premul BGRA → straight RGBA (Windows sprites).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn bgra_premul_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; bgra.len()];
    for (src, dst) in bgra.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
        let a = src[3];
        if a == 0 {
            continue;
        }
        dst[0] = ((src[2] as u16 * 255) / a as u16).min(255) as u8;
        dst[1] = ((src[1] as u16 * 255) / a as u16).min(255) as u8;
        dst[2] = ((src[0] as u16 * 255) / a as u16).min(255) as u8;
        dst[3] = a;
    }
    out
}

fn put(fb: &mut Framebuffer, x: u32, y: u32, r: u8, g: u8, b: u8, a: u8) {
    let i = idx(fb, x, y);
    fb.rgba[i] = r;
    fb.rgba[i + 1] = g;
    fb.rgba[i + 2] = b;
    fb.rgba[i + 3] = a;
}

fn over(fb: &mut Framebuffer, x: u32, y: u32, sr: u8, sg: u8, sb: u8, sa: u8) {
    if sa == 0 {
        return;
    }
    let i = idx(fb, x, y);
    let sa = sa as f64 / 255.0;
    let da = fb.rgba[i + 3] as f64 / 255.0;
    let out_a = sa + da * (1.0 - sa);
    if out_a < 1e-6 {
        return;
    }
    let ia = 1.0 - sa;
    fb.rgba[i] = ((sr as f64 * sa + fb.rgba[i] as f64 * da * ia) / out_a).round() as u8;
    fb.rgba[i + 1] = ((sg as f64 * sa + fb.rgba[i + 1] as f64 * da * ia) / out_a).round() as u8;
    fb.rgba[i + 2] = ((sb as f64 * sa + fb.rgba[i + 2] as f64 * da * ia) / out_a).round() as u8;
    fb.rgba[i + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}

fn idx(fb: &Framebuffer, x: u32, y: u32) -> usize {
    ((y * fb.w + x) * 4) as usize
}

fn lerp_u8(a: u8, b: u8, t: f64) -> u8 {
    (a as f64 + (b as f64 - a as f64) * t).round() as u8
}

pub fn parse_hex(s: &str) -> Option<(u8, u8, u8, u8)> {
    let t = s.trim().trim_start_matches('#');
    match t.len() {
        6 => {
            let n = u32::from_str_radix(t, 16).ok()?;
            Some((((n >> 16) & 0xff) as u8, ((n >> 8) & 0xff) as u8, (n & 0xff) as u8, 255))
        }
        8 => {
            let n = u32::from_str_radix(t, 16).ok()?;
            Some((
                ((n >> 24) & 0xff) as u8,
                ((n >> 16) & 0xff) as u8,
                ((n >> 8) & 0xff) as u8,
                (n & 0xff) as u8,
            ))
        }
        _ => None,
    }
}

/// Convert straight RGBA to premultiplied BGRA for `UpdateLayeredWindow`.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn rgba_to_bgra_premul(rgba: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for (src, dst) in rgba.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
        let a = src[3] as u16;
        dst[0] = ((src[2] as u16 * a) / 255) as u8;
        dst[1] = ((src[1] as u16 * a) / 255) as u8;
        dst[2] = ((src[0] as u16 * a) / 255) as u8;
        dst[3] = src[3];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vivarium::{LookConfig, VivariumConfig};

    fn alpha_at(fb: &Framebuffer, x: u32, y: u32) -> u8 {
        fb.rgba[idx(fb, x, y) + 3]
    }

    /// Premul-over a mid gray, matching Cocoa/Win window compositing.
    fn over_gray(fb: &Framebuffer, x: u32, y: u32, dst: u8) -> u8 {
        let i = idx(fb, x, y);
        let a = fb.rgba[i + 3] as u16;
        let pr = (fb.rgba[i] as u16 * a) / 255;
        (pr + dst as u16 * (255 - a) / 255) as u8
    }

    #[test]
    fn compose_fills_pixels() {
        let cfg = VivariumConfig::default();
        let fb = compose(&cfg, &LookConfig::default(), &[]);
        assert!(fb.w > 0 && fb.h > 0);
        assert_eq!(fb.rgba.len(), (fb.w * fb.h * 4) as usize);
        let opaque = fb.rgba.chunks(4).filter(|p| p[3] > 0).count();
        assert!(opaque > 100);
    }

    #[test]
    fn glass_center_dims_rim_brightens() {
        let cfg = VivariumConfig::default();
        let fb = compose(&cfg, &LookConfig::default(), &[]);
        let cx = fb.w / 2;
        let cy = fb.h / 2;
        let dst = 180u8;
        let center = over_gray(&fb, cx, cy, dst);
        let inner = inner_rect(cfg.logical_w, cfg.logical_h);
        let sx = fb.w as f64 / cfg.logical_w as f64;
        let wall_mid_x = ((inner.x as f64 * sx) * 0.5).round() as u32;
        let lip_x = (inner.x as f64 * sx).round() as u32;
        let wx = wall_mid_x.min(fb.w - 1);
        let wall_mid = over_gray(&fb, wx, cy, dst);
        let lip = over_gray(&fb, lip_x.min(fb.w - 1), cy, dst);
        let top_lip = over_gray(&fb, cx, 3, dst);
        let wi = idx(&fb, wx, cy);
        assert!(center < dst, "center should dim ({center} vs {dst})");
        assert!(alpha_at(&fb, cx, cy) < 40);
        assert!(wall_mid < dst, "wall mid should dim ({wall_mid} vs {dst})");
        assert!(fb.rgba[wi + 1] > fb.rgba[wi], "wall mid should be teal");
        assert!(lip > dst, "inner lip should brighten ({lip} vs {dst})");
        assert!(lip > wall_mid, "lip {lip} vs wall mid {wall_mid}");
        assert!(top_lip > dst, "top inner lip should brighten ({top_lip} vs {dst})");
        let side_foot = over_gray(&fb, wx, fb.h.saturating_sub(4), dst);
        assert!(
            side_foot < dst + 8,
            "side pane should reach the floor without a white square ({side_foot})"
        );
        assert!(
            alpha_at(&fb, 0, 0) < 50,
            "outer corners stay mostly clear ({})",
            alpha_at(&fb, 0, 0)
        );
        let edge = over_gray(&fb, cx, 0, 220);
        let inset = over_gray(&fb, cx, 3, 220);
        assert!(edge < 220, "outer edge should darken ({edge})");
        assert!(edge < inset, "outline sits outside the white lip ({edge} vs {inset})");
    }

    #[test]
    fn front_glass_tints_mates() {
        let cfg = VivariumConfig::default();
        let look = LookConfig::default();
        let inner = inner_rect(cfg.logical_w, cfg.logical_h);
        let sx = cfg.pixel_size().0 as f64 / cfg.logical_w as f64;
        let sy = cfg.pixel_size().1 as f64 / cfg.logical_h as f64;
        let x = ((inner.x + inner.w / 2) as f64 * sx).round() as i32;
        let y = ((inner.y + inner.h / 2) as f64 * sy).round() as i32;
        let pix = [255u8, 220, 40, 255];
        let mates = [MateBlit {
            x,
            y,
            w: 1,
            h: 1,
            src_w: 1,
            src_h: 1,
            rgba: &pix,
        }];
        let fb = compose(&cfg, &look, &mates);
        let i = idx(&fb, x as u32, y as u32);
        assert_ne!(&fb.rgba[i..i + 4], &pix, "front glass should cover the mate");
    }
}
