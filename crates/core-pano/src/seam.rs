//! Seam finding: per-frame ownership masks via dynamic-programming min-cost seams (at the seam scale).
//!
//! Strategy (deliberately simple and robust — the multi-band blend hides residual seam error, so
//! chasing a globally optimal seam network buys little):
//!
//! 1. **Voronoi init.** Every lowres canvas pixel is initially owned by the used frame whose feather
//!    weight is highest there ("best weight wins"); uncovered pixels are marked empty (255).
//! 2. **Pairwise DP seams.** For each verified pair (from `reg.pairwise`, processed in descending
//!    inlier order) whose canvas rects overlap, a monotone min-cost seam is cut across the overlap
//!    band. The cost per pixel is the classic COLOR_GRAD term — the colour difference between the two
//!    warps plus the gradient magnitude of that difference — so the seam prefers to run where the two
//!    images already agree and through low-texture regions. The seam runs along the overlap rect's
//!    longer axis (a vertical seam for a tall overlap, horizontal for a wide one). Ownership on each
//!    side of the seam is reassigned to `i` or `j`, keeping frame `i` on whichever side already holds
//!    more of its pixels (a light topology-preserving rule). Frames with no pairwise entry keep their
//!    Voronoi mask.
//!
//! The output is a canvas-sized id map at the seam scale; blend upsamples it with nearest-neighbour.

use crate::project::Warped;
use crate::PairMatch;

/// A canvas-sized frame-ownership id map at the seam (lowres) scale.
pub(crate) struct IdMap {
    /// One byte per lowres canvas pixel: the owning slot (position in `used_indices`), or 255 = empty.
    pub ids: Vec<u8>,
    pub w: usize,
    pub h: usize,
}

impl IdMap {
    /// Owning slot (or 255 = empty) at a FULL-res canvas pixel, sampled nearest from the lowres map.
    #[inline]
    pub(crate) fn owner_at(&self, cx: usize, cy: usize, canvas_w: usize, canvas_h: usize) -> u8 {
        let lx = if canvas_w <= 1 {
            0
        } else {
            (cx * self.w) / canvas_w
        }
        .min(self.w - 1);
        let ly = if canvas_h <= 1 {
            0
        } else {
            (cy * self.h) / canvas_h
        }
        .min(self.h - 1);
        self.ids[ly * self.w + lx]
    }
}

/// Marker for an unowned canvas pixel.
const EMPTY: u8 = 255;
/// Feather-weight floor for a frame to be eligible to own a pixel.
const WEIGHT_FLOOR: f32 = 0.05;
/// Cost assigned to band pixels where the two frames do not both have data (keeps the seam inside the
/// mutual-overlap strip).
const OUT_OF_BAND_COST: f32 = 1.0e6;

/// Build the seam id map. `warps` are index-aligned with `used_indices`; `used_indices` maps a slot to
/// its original frame index; `pairwise` are the verified P1 pairs (in original frame indices).
pub(crate) fn build_id_map(
    warps: &[Option<Warped>],
    used_indices: &[usize],
    pairwise: &[PairMatch],
    canvas_w: usize,
    canvas_h: usize,
) -> IdMap {
    let mut ids = vec![EMPTY; canvas_w * canvas_h];

    // --- 1. Voronoi by feather weight. ---
    for cy in 0..canvas_h {
        for cx in 0..canvas_w {
            let mut best_w = WEIGHT_FLOOR;
            let mut best_slot = EMPTY;
            for (slot, warp) in warps.iter().enumerate() {
                if let Some(w) = warp {
                    if let Some((_, wt)) = w.sample(cx, cy) {
                        if wt > best_w {
                            best_w = wt;
                            best_slot = slot as u8;
                        }
                    }
                }
            }
            ids[cy * canvas_w + cx] = best_slot;
        }
    }

    // Original frame index -> slot, so pairwise (in frame indices) can address the warps.
    let mut slot_of = std::collections::HashMap::new();
    for (slot, &fidx) in used_indices.iter().enumerate() {
        slot_of.insert(fidx, slot);
    }

    // --- 2. Pairwise DP seams, strongest overlaps first. ---
    let mut order: Vec<&PairMatch> = pairwise.iter().collect();
    order.sort_by(|a, b| b.n_inliers.cmp(&a.n_inliers));

    for pm in order {
        let (Some(&si), Some(&sj)) = (slot_of.get(&pm.i), slot_of.get(&pm.j)) else {
            continue;
        };
        let (Some(wi), Some(wj)) = (&warps[si], &warps[sj]) else {
            continue;
        };
        let Some(rect) = rect_overlap(wi.rect, wj.rect) else {
            continue;
        };
        carve_seam(&mut ids, canvas_w, wi, wj, si as u8, sj as u8, rect);
    }

    IdMap {
        ids,
        w: canvas_w,
        h: canvas_h,
    }
}

/// Cut one min-cost seam through the overlap of two frames and reassign ownership on each side.
fn carve_seam(
    ids: &mut [u8],
    canvas_w: usize,
    wi: &Warped,
    wj: &Warped,
    si: u8,
    sj: u8,
    rect: (usize, usize, usize, usize),
) {
    let (rx, ry, ow, oh) = rect;
    if ow < 2 || oh < 2 {
        return;
    }

    // Colour-difference + gradient-magnitude cost surface over the overlap rect. Pixels where the two
    // frames do not both have data get a large cost, pinning the seam inside the mutual band.
    let mut diff = vec![OUT_OF_BAND_COST; ow * oh];
    let mut band_count = 0usize;
    for ly in 0..oh {
        for lx in 0..ow {
            let (cx, cy) = (rx + lx, ry + ly);
            if let (Some((ci, wgi)), Some((cj, wgj))) = (wi.sample(cx, cy), wj.sample(cx, cy)) {
                if wgi > WEIGHT_FLOOR && wgj > WEIGHT_FLOOR {
                    let d = (ci[0] - cj[0]).abs() + (ci[1] - cj[1]).abs() + (ci[2] - cj[2]).abs();
                    diff[ly * ow + lx] = d;
                    band_count += 1;
                }
            }
        }
    }
    if band_count < oh.min(ow) {
        return; // band too thin to carve a meaningful seam; keep the Voronoi assignment
    }

    // cost = colour diff + gradient magnitude of the diff (central differences, clamped edges).
    let mut cost = vec![0.0f32; ow * oh];
    for ly in 0..oh {
        for lx in 0..ow {
            let c = diff[ly * ow + lx];
            let left = diff[ly * ow + lx.saturating_sub(1)];
            let right = diff[ly * ow + (lx + 1).min(ow - 1)];
            let up = diff[ly.saturating_sub(1) * ow + lx];
            let down = diff[(ly + 1).min(oh - 1) * ow + lx];
            let gx = (right - left).abs();
            let gy = (down - up).abs();
            cost[ly * ow + lx] = c + (gx * gx + gy * gy).sqrt();
        }
    }

    // The seam runs along the longer axis of the overlap: a vertical seam (one column per row) splits
    // a tall overlap left/right; a horizontal seam (one row per column) splits a wide one top/bottom.
    if oh >= ow {
        let seam = dp_seam_vertical(&cost, ow, oh);
        reassign_vertical(ids, canvas_w, wi, wj, si, sj, rect, &seam);
    } else {
        let seam = dp_seam_horizontal(&cost, ow, oh);
        reassign_horizontal(ids, canvas_w, wi, wj, si, sj, rect, &seam);
    }
}

/// Min-cost top-to-bottom seam: `seam[y]` = chosen column for row `y`.
fn dp_seam_vertical(cost: &[f32], ow: usize, oh: usize) -> Vec<usize> {
    let mut dp = cost[0..ow].to_vec();
    let mut next = vec![0.0f32; ow];
    let mut back = vec![0usize; ow * oh];
    for y in 1..oh {
        let cur = y * ow;
        for x in 0..ow {
            let mut best = dp[x];
            let mut bx = x;
            if x > 0 && dp[x - 1] < best {
                best = dp[x - 1];
                bx = x - 1;
            }
            if x + 1 < ow && dp[x + 1] < best {
                best = dp[x + 1];
                bx = x + 1;
            }
            back[cur + x] = bx;
            next[x] = cost[cur + x] + best;
        }
        dp.copy_from_slice(&next);
    }
    let mut x = (0..ow)
        .min_by(|&a, &b| dp[a].partial_cmp(&dp[b]).unwrap())
        .unwrap();
    let mut seam = vec![0usize; oh];
    for y in (0..oh).rev() {
        seam[y] = x;
        x = back[y * ow + x];
    }
    seam
}

/// Min-cost left-to-right seam: `seam[x]` = chosen row for column `x`.
fn dp_seam_horizontal(cost: &[f32], ow: usize, oh: usize) -> Vec<usize> {
    let mut dp = vec![0.0f32; oh];
    for (y, d) in dp.iter_mut().enumerate() {
        *d = cost[y * ow];
    }
    let mut back = vec![0usize; ow * oh];
    let mut next = vec![0.0f32; oh];
    for x in 1..ow {
        for y in 0..oh {
            let mut best = dp[y];
            let mut by = y;
            if y > 0 && dp[y - 1] < best {
                best = dp[y - 1];
                by = y - 1;
            }
            if y + 1 < oh && dp[y + 1] < best {
                best = dp[y + 1];
                by = y + 1;
            }
            back[x * oh + y] = by;
            next[y] = cost[y * ow + x] + best;
        }
        dp.copy_from_slice(&next);
    }
    let mut y = (0..oh)
        .min_by(|&a, &b| dp[a].partial_cmp(&dp[b]).unwrap())
        .unwrap();
    let mut seam = vec![0usize; ow];
    for x in (0..ow).rev() {
        seam[x] = y;
        y = back[x * oh + y];
    }
    seam
}

/// Reassign ownership across a vertical seam (columns `< seam[y]` = one side, `>=` the other).
#[allow(clippy::too_many_arguments)]
fn reassign_vertical(
    ids: &mut [u8],
    canvas_w: usize,
    wi: &Warped,
    wj: &Warped,
    si: u8,
    sj: u8,
    rect: (usize, usize, usize, usize),
    seam: &[usize],
) {
    let (rx, ry, ow, _oh) = rect;
    // Which frame keeps the left side: whoever currently owns more of it (topology-preserving).
    let (mut li, mut lj) = (0usize, 0usize);
    for (ly, &sx) in seam.iter().enumerate() {
        for lx in 0..sx {
            match ids[(ry + ly) * canvas_w + (rx + lx)] {
                v if v == si => li += 1,
                v if v == sj => lj += 1,
                _ => {}
            }
        }
    }
    let (left, right) = if li >= lj { (si, sj) } else { (sj, si) };
    for (ly, &sx) in seam.iter().enumerate() {
        for lx in 0..ow {
            let desired = if lx < sx { left } else { right };
            assign(ids, canvas_w, wi, wj, si, sj, rx + lx, ry + ly, desired);
        }
    }
}

/// Reassign ownership across a horizontal seam (rows `< seam[x]` = one side, `>=` the other).
#[allow(clippy::too_many_arguments)]
fn reassign_horizontal(
    ids: &mut [u8],
    canvas_w: usize,
    wi: &Warped,
    wj: &Warped,
    si: u8,
    sj: u8,
    rect: (usize, usize, usize, usize),
    seam: &[usize],
) {
    let (rx, ry, ow, oh) = rect;
    let (mut ti, mut tj) = (0usize, 0usize);
    for (lx, &sy) in seam.iter().enumerate() {
        for ly in 0..sy {
            match ids[(ry + ly) * canvas_w + (rx + lx)] {
                v if v == si => ti += 1,
                v if v == sj => tj += 1,
                _ => {}
            }
        }
    }
    let (top, bottom) = if ti >= tj { (si, sj) } else { (sj, si) };
    for (lx, &sy) in seam.iter().enumerate().take(ow) {
        for ly in 0..oh {
            let desired = if ly < sy { top } else { bottom };
            assign(ids, canvas_w, wi, wj, si, sj, rx + lx, ry + ly, desired);
        }
    }
}

/// Assign a pixel to `desired`, but only among frames that actually have data there, and only if it is
/// currently owned by one of the two seam frames (never steal a third frame's pixel).
#[allow(clippy::too_many_arguments)]
#[inline]
fn assign(
    ids: &mut [u8],
    canvas_w: usize,
    wi: &Warped,
    wj: &Warped,
    si: u8,
    sj: u8,
    cx: usize,
    cy: usize,
    desired: u8,
) {
    let idx = cy * canvas_w + cx;
    let cur = ids[idx];
    if cur != si && cur != sj {
        return;
    }
    let has_i = wi
        .sample(cx, cy)
        .map(|(_, w)| w > WEIGHT_FLOOR)
        .unwrap_or(false);
    let has_j = wj
        .sample(cx, cy)
        .map(|(_, w)| w > WEIGHT_FLOOR)
        .unwrap_or(false);
    let (other, has_desired, has_other) = if desired == si {
        (sj, has_i, has_j)
    } else {
        (si, has_j, has_i)
    };
    if has_desired {
        ids[idx] = desired;
    } else if has_other {
        ids[idx] = other;
    }
}

/// Intersection of two `(x0,y0,w,h)` rects, or `None` if disjoint.
fn rect_overlap(
    a: (usize, usize, usize, usize),
    b: (usize, usize, usize, usize),
) -> Option<(usize, usize, usize, usize)> {
    let x0 = a.0.max(b.0);
    let y0 = a.1.max(b.1);
    let x1 = (a.0 + a.2).min(b.0 + b.2);
    let y1 = (a.1 + a.3).min(b.1 + b.3);
    if x1 > x0 && y1 > y0 {
        Some((x0, y0, x1 - x0, y1 - y0))
    } else {
        None
    }
}
