//! Logical occupancy from posed prop sprites: hole-filled blobs and walk skylines.

use super::layout::{snap_angle_id, Part, PartCatalog, PlacedInstance, Pose};
use super::{Floor, FloorKind, InnerRect};

/// Opaque enough to count as solid (skip AA fringe).
const ALPHA_T: u8 = 32;

#[derive(Debug, Clone)]
pub struct Occupancy {
    pub origin_x: i32,
    pub origin_y: i32,
    pub w: u32,
    pub h: u32,
    /// 1 = hole-filled solid.
    cells: Vec<u8>,
    /// Topmost solid y per column (`h` = empty).
    skyline: Vec<u32>,
}

impl Occupancy {
    #[allow(dead_code)]
    pub fn empty() -> Self {
        Self {
            origin_x: 0,
            origin_y: 0,
            w: 0,
            h: 0,
            cells: Vec::new(),
            skyline: Vec::new(),
        }
    }

    fn idx(&self, x: u32, y: u32) -> usize {
        (y * self.w + x) as usize
    }

    pub fn filled_at(&self, x: f64, y: f64) -> bool {
        if self.w == 0 || self.h == 0 {
            return false;
        }
        let ix = (x.floor() as i32) - self.origin_x;
        let iy = (y.floor() as i32) - self.origin_y;
        if ix < 0 || iy < 0 {
            return false;
        }
        let (ix, iy) = (ix as u32, iy as u32);
        if ix >= self.w || iy >= self.h {
            return false;
        }
        self.cells[self.idx(ix, iy)] != 0
    }

    pub fn skyline_y(&self, x: f64) -> Option<f64> {
        if self.w == 0 {
            return None;
        }
        let ix = (x.floor() as i32) - self.origin_x;
        if ix < 0 {
            return None;
        }
        let ix = ix as u32;
        if ix >= self.w {
            return None;
        }
        let y = self.skyline[ix as usize];
        if y >= self.h {
            None
        } else {
            Some(self.origin_y as f64 + y as f64)
        }
    }

    /// Standing on the ridge is allowed; the interior below it is not.
    pub fn blocks_foot(&self, x: f64, y: f64) -> bool {
        let Some(top) = self.skyline_y(x) else {
            return false;
        };
        y > top + 0.5 && self.filled_at(x, y)
    }
}

pub fn push_out_blocked(x: f64, y: f64, occ: &Occupancy) -> f64 {
    if !occ.blocks_foot(x, y) {
        return x;
    }
    let mut best: Option<f64> = None;
    for delta in 1..400 {
        let d = delta as f64;
        for cand in [x - d, x + d] {
            if !occ.blocks_foot(cand, y) {
                let dist = (cand - x).abs();
                if best.is_none_or(|b| dist < (b - x).abs()) {
                    best = Some(cand);
                }
            }
        }
        if best.is_some() {
            break;
        }
    }
    best.unwrap_or(x)
}

pub fn build_occupancy(
    inner: InnerRect,
    placed: &[PlacedInstance],
    parts: &PartCatalog,
) -> Occupancy {
    let w = inner.w.max(1);
    let h = inner.h.max(1);
    let mut raw = vec![0u8; (w * h) as usize];
    for inst in placed {
        let Some(part) = parts.get(&inst.part_id) else {
            continue;
        };
        blit_part(&mut raw, w, h, inner, &inst.pose, part);
    }
    fill_holes(w, h, &mut raw);
    let mut skyline = vec![h; w as usize];
    for y in 0..h {
        for x in 0..w {
            if raw[(y * w + x) as usize] != 0 && skyline[x as usize] == h {
                skyline[x as usize] = y;
            }
        }
    }
    Occupancy {
        origin_x: inner.x as i32,
        origin_y: inner.y as i32,
        w,
        h,
        cells: raw,
        skyline,
    }
}

fn blit_part(raw: &mut [u8], w: u32, h: u32, inner: InnerRect, pose: &Pose, part: &Part) {
    let corners = [
        pose.part_to_cage(0.0, 0.0),
        pose.part_to_cage(part.img_w as f64, 0.0),
        pose.part_to_cage(part.img_w as f64, part.img_h as f64),
        pose.part_to_cage(0.0, part.img_h as f64),
    ];
    let min_x = corners.iter().map(|p| p.0).fold(f64::INFINITY, f64::min).floor() as i32 - 1;
    let max_x = corners.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max).ceil() as i32 + 1;
    let min_y = corners.iter().map(|p| p.1).fold(f64::INFINITY, f64::min).floor() as i32 - 1;
    let max_y = corners.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max).ceil() as i32 + 1;
    let x0 = inner.x as i32;
    let y0 = inner.y as i32;
    let x1 = x0 + w as i32;
    let y1 = y0 + h as i32;
    for cy in min_y.max(y0)..max_y.min(y1) {
        for cx in min_x.max(x0)..max_x.min(x1) {
            let (px, py) = pose.cage_to_part(cx as f64 + 0.5, cy as f64 + 0.5);
            if px < 0.0 || py < 0.0 {
                continue;
            }
            let ix = px.floor() as i64;
            let iy = py.floor() as i64;
            if ix < 0 || iy < 0 || ix >= part.img_w as i64 || iy >= part.img_h as i64 {
                continue;
            }
            let a = part.rgba[((iy as u32 * part.img_w + ix as u32) * 4 + 3) as usize];
            if a > ALPHA_T {
                let lx = (cx - x0) as u32;
                let ly = (cy - y0) as u32;
                raw[(ly * w + lx) as usize] = 1;
            }
        }
    }
}

fn fill_holes(w: u32, h: u32, cells: &mut [u8]) {
    let labels = label_components(w, h, cells);
    let mut max_lab = 0i32;
    for &l in &labels {
        max_lab = max_lab.max(l);
    }
    for lab in 1..=max_lab {
        fill_holes_label(w, h, cells, &labels, lab);
    }
}

fn label_components(w: u32, h: u32, cells: &[u8]) -> Vec<i32> {
    let n = (w * h) as usize;
    let mut labels = vec![0i32; n];
    let mut next = 1i32;
    let mut stack = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            if cells[i] == 0 || labels[i] != 0 {
                continue;
            }
            let lab = next;
            next += 1;
            stack.clear();
            stack.push((x, y));
            labels[i] = lab;
            while let Some((cx, cy)) = stack.pop() {
                for (nx, ny) in four(cx, cy, w, h) {
                    let j = (ny * w + nx) as usize;
                    if cells[j] != 0 && labels[j] == 0 {
                        labels[j] = lab;
                        stack.push((nx, ny));
                    }
                }
            }
        }
    }
    labels
}

fn four(x: u32, y: u32, w: u32, h: u32) -> impl Iterator<Item = (u32, u32)> {
    let mut n = [(0u32, 0u32); 4];
    let mut k = 0;
    if x > 0 {
        n[k] = (x - 1, y);
        k += 1;
    }
    if x + 1 < w {
        n[k] = (x + 1, y);
        k += 1;
    }
    if y > 0 {
        n[k] = (x, y - 1);
        k += 1;
    }
    if y + 1 < h {
        n[k] = (x, y + 1);
        k += 1;
    }
    n.into_iter().take(k)
}

fn fill_holes_label(w: u32, h: u32, cells: &mut [u8], labels: &[i32], lab: i32) {
    let mut min_x = w;
    let mut max_x = 0u32;
    let mut min_y = h;
    let mut max_y = 0u32;
    let mut any = false;
    for y in 0..h {
        for x in 0..w {
            if labels[(y * w + x) as usize] != lab {
                continue;
            }
            any = true;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
    }
    if !any {
        return;
    }
    let bw = max_x - min_x + 3;
    let bh = max_y - min_y + 3;
    let mut pad = vec![0u8; (bw * bh) as usize];
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            if labels[(y * w + x) as usize] == lab {
                let px = x - min_x + 1;
                let py = y - min_y + 1;
                pad[(py * bw + px) as usize] = 1;
            }
        }
    }
    let mut seen = vec![false; pad.len()];
    let mut stack = vec![(0u32, 0u32)];
    seen[0] = true;
    while let Some((x, y)) = stack.pop() {
        for (nx, ny) in four(x, y, bw, bh) {
            let j = (ny * bw + nx) as usize;
            if seen[j] || pad[j] != 0 {
                continue;
            }
            seen[j] = true;
            stack.push((nx, ny));
        }
    }
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let px = x - min_x + 1;
            let py = y - min_y + 1;
            let j = (py * bw + px) as usize;
            if pad[j] == 0 && !seen[j] {
                cells[(y * w + x) as usize] = 1;
            }
        }
    }
}

pub fn floors_from_occupancy(occ: &Occupancy) -> Vec<Floor> {
    let mut floors = Vec::new();
    if occ.w == 0 {
        return floors;
    }
    let mut x = 0u32;
    let mut blob = 0u32;
    while x < occ.w {
        while x < occ.w && occ.skyline[x as usize] >= occ.h {
            x += 1;
        }
        if x >= occ.w {
            break;
        }
        let start = x;
        while x < occ.w && occ.skyline[x as usize] < occ.h {
            x += 1;
        }
        let pts = skyline_points(occ, start, x);
        let simple = rdp(&pts, 1.25);
        for (si, pair) in simple.windows(2).enumerate() {
            let (x0, y0) = (pair[0][0], pair[0][1]);
            let (x1, y1) = (pair[1][0], pair[1][1]);
            let (left, y_left, right, y_right) = if x1 >= x0 {
                (x0, y0, x1, y1)
            } else {
                (x1, y1, x0, y0)
            };
            let w = right - left;
            if w < 2.0 {
                continue;
            }
            let angle_id = snap_angle_id(y_left, y_right, w);
            floors.push(Floor {
                id: format!("blob:{blob}:{si}"),
                kind: FloorKind::from_angle(angle_id),
                x: left,
                w,
                y_left,
                y_right,
                vis_h: 8.0,
                fill: String::new(),
                alpha: 0,
                angle_id,
            });
        }
        blob += 1;
    }
    floors
}

fn skyline_points(occ: &Occupancy, x0: u32, x1: u32) -> Vec<[f64; 2]> {
    let mut pts = Vec::new();
    for x in x0..x1 {
        let y = occ.skyline[x as usize];
        pts.push([
            occ.origin_x as f64 + x as f64 + 0.5,
            occ.origin_y as f64 + y as f64,
        ]);
    }
    if let Some(last) = pts.last().copied() {
        pts.push([last[0] + 0.5, last[1]]);
    }
    pts
}

fn rdp(pts: &[[f64; 2]], eps: f64) -> Vec<[f64; 2]> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let (first, last) = (pts[0], pts[pts.len() - 1]);
    let mut max_d = 0.0;
    let mut idx = 0;
    for (i, p) in pts.iter().enumerate().skip(1).take(pts.len() - 2) {
        let d = perp_dist(*p, first, last);
        if d > max_d {
            max_d = d;
            idx = i;
        }
    }
    if max_d > eps {
        let mut left = rdp(&pts[..=idx], eps);
        let right = rdp(&pts[idx..], eps);
        left.pop();
        left.extend(right);
        left
    } else {
        vec![first, last]
    }
}

fn perp_dist(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-9 {
        return ((p[0] - a[0]).hypot(p[1] - a[1])).abs();
    }
    ((dy * p[0] - dx * p[1] + b[0] * a[1] - b[1] * a[0]).abs()) / len
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vivarium::layout::{HAnchor, Instance, Layer, Part};
    use crate::vivarium::{inner_rect, resolve_layout, AssemblyConfig, WaterSpec};
    use std::collections::HashMap;

    fn solid(w: u32, h: u32) -> Vec<u8> {
        let mut p = vec![0u8; (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            p[i * 4 + 3] = 255;
        }
        p
    }

    fn donut(w: u32, h: u32, hole: (u32, u32, u32, u32)) -> Vec<u8> {
        let mut p = solid(w, h);
        for y in hole.1..hole.3 {
            for x in hole.0..hole.2 {
                let i = (y * w + x) as usize;
                p[i * 4 + 3] = 0;
            }
        }
        p
    }

    fn part(id: &str, w: u32, h: u32, rgba: Vec<u8>) -> Part {
        Part {
            id: id.into(),
            name: id.into(),
            allow_back: true,
            allow_front: true,
            img_w: w,
            img_h: h,
            rgba,
        }
    }

    fn placed(id: &str, pose: Pose) -> PlacedInstance {
        PlacedInstance {
            index: 0,
            part_id: id.into(),
            layer: Layer::Back,
            pose,
        }
    }

    #[test]
    fn donut_hole_is_blocked_after_fill() {
        let inner = InnerRect {
            x: 0,
            y: 0,
            w: 20,
            h: 20,
        };
        let mut parts = PartCatalog::new();
        parts.insert("ring".into(), part("ring", 12, 12, donut(12, 12, (4, 4, 8, 8))));
        let pose = Pose {
            left: 4.0,
            top: 4.0,
            sw: 12.0,
            sh: 12.0,
            angle_deg: 0.0,
            scale: 1.0,
        };
        let occ = build_occupancy(inner, &[placed("ring", pose)], &parts);
        assert!(occ.filled_at(10.0, 10.0), "hole fill should occupy the donut interior");
        assert!(occ.blocks_foot(10.0, 10.0));
        let top = occ.skyline_y(10.0).unwrap();
        assert!(!occ.blocks_foot(10.0, top));
    }

    #[test]
    fn separated_rects_leave_a_gap() {
        let inner = InnerRect {
            x: 0,
            y: 0,
            w: 40,
            h: 16,
        };
        let mut parts = PartCatalog::new();
        parts.insert("a".into(), part("a", 8, 8, solid(8, 8)));
        parts.insert("b".into(), part("b", 8, 8, solid(8, 8)));
        let a = Pose {
            left: 2.0,
            top: 4.0,
            sw: 8.0,
            sh: 8.0,
            angle_deg: 0.0,
            scale: 1.0,
        };
        let b = Pose {
            left: 24.0,
            top: 4.0,
            sw: 8.0,
            sh: 8.0,
            angle_deg: 0.0,
            scale: 1.0,
        };
        let occ = build_occupancy(
            inner,
            &[placed("a", a), placed("b", b)],
            &parts,
        );
        assert!(occ.filled_at(6.0, 8.0));
        assert!(occ.filled_at(28.0, 8.0));
        assert!(!occ.filled_at(16.0, 8.0), "gap between blobs stays open");
        let floors = floors_from_occupancy(&occ);
        assert!(floors.len() >= 2);
        let right0 = floors[0].right();
        let left1 = floors.iter().map(|f| f.x).fold(f64::INFINITY, f64::min);
        let max_left = floors.iter().map(|f| f.x).fold(f64::NEG_INFINITY, f64::max);
        assert!(max_left > right0 + 4.0 || floors.iter().any(|f| f.x > right0 + 4.0));
        let _ = left1;
    }

    #[test]
    fn overlapping_rects_are_one_blob() {
        let inner = InnerRect {
            x: 0,
            y: 0,
            w: 30,
            h: 16,
        };
        let mut parts = PartCatalog::new();
        parts.insert("a".into(), part("a", 10, 8, solid(10, 8)));
        parts.insert("b".into(), part("b", 10, 8, solid(10, 8)));
        let a = Pose {
            left: 4.0,
            top: 4.0,
            sw: 10.0,
            sh: 8.0,
            angle_deg: 0.0,
            scale: 1.0,
        };
        let b = Pose {
            left: 10.0,
            top: 4.0,
            sw: 10.0,
            sh: 8.0,
            angle_deg: 0.0,
            scale: 1.0,
        };
        let occ = build_occupancy(
            inner,
            &[placed("a", a), placed("b", b)],
            &parts,
        );
        assert!(occ.filled_at(12.0, 8.0));
        let floors = floors_from_occupancy(&occ);
        let ids: Vec<_> = floors.iter().map(|f| f.id.split(':').nth(1).unwrap()).collect();
        assert!(ids.iter().all(|b| *b == "0"), "overlap is one connected blob");
    }

    #[test]
    fn resolve_layout_overlapping_instances_block_the_union() {
        let mut parts = HashMap::new();
        parts.insert("a".into(), part("a", 40, 20, solid(40, 20)));
        parts.insert("b".into(), part("b", 40, 20, solid(40, 20)));
        let assembly = AssemblyConfig {
            water: WaterSpec {
                depth_px: 0.0,
                fill: "#000000".into(),
                alpha: 0,
            },
            instances: vec![
                Instance {
                    part: "a".into(),
                    x: 0.0,
                    y: 0.0,
                    angle_deg: 0.0,
                    scale: 1.0,
                    h_anchor: HAnchor::Left,
                    layer: Layer::Back,
                },
                Instance {
                    part: "b".into(),
                    x: 20.0,
                    y: 0.0,
                    angle_deg: 0.0,
                    scale: 1.0,
                    h_anchor: HAnchor::Left,
                    layer: Layer::Back,
                },
            ],
        };
        let inner = inner_rect(200, 240);
        let cage = resolve_layout(inner, &assembly, &parts);
        assert!(!cage.floors.is_empty());
        let mid_x = inner.x as f64 + 30.0;
        let foot_y = cage.floors[0].walk_y(mid_x) + 4.0;
        assert!(cage.occupancy.blocks_foot(mid_x, foot_y));
    }
}
