//! Seam finding: per-frame ownership masks via dynamic-programming min-cost seams (at the seam scale).
//!
//! Strategy (deliberately simple and robust — the multi-band blend hides residual seam error, so
//! chasing a globally optimal seam network buys little):
//!
//! 1. **Voronoi init.** Every lowres canvas pixel is initially owned by the used frame whose feather
//!    weight is highest there ("best weight wins"); uncovered pixels are marked empty (255).
//! 2. **Pairwise DP seams.** For each verified pair (from `reg.pairwise`, processed in descending
//!    inlier order) whose canvas rects overlap, a monotone min-cost seam is cut across the overlap
//!    band. The cost per pixel is the classic COLOR_GRAD term — the *gain-corrected* colour difference
//!    between the two warps plus the gradient magnitude of that difference — so the seam prefers to run
//!    where the two images already agree and through low-texture regions. A light **deghosting** term
//!    hard-penalizes locally anomalous disagreement (moving objects / parallax) so the seam routes
//!    around them (see [`ghost_mask`]). The seam runs along the overlap rect's longer axis (vertical
//!    for a tall overlap, horizontal for a wide one). Ownership on each side is reassigned to `i` or
//!    `j`, keeping frame `i` on whichever side already holds more of its pixels (a light
//!    topology-preserving rule). Frames with no pairwise entry keep their Voronoi mask.
//!
//! An s-t min-cut alternative to the DP seam exists behind [`SEAM_METHOD`] (see [`SeamMethod`] for the
//! Priority-2 benchmark verdict — DP stays the default). The output is a canvas-sized id map at the
//! seam scale; blend upsamples it with nearest-neighbour.

use pathfinding::directed::edmonds_karp::edmonds_karp_sparse;

use crate::project::Warped;
use crate::PairMatch;

/// Seam-finding strategy. DP is the wired default (see [`SEAM_METHOD`]); [`SeamMethod::GraphCut`] is a
/// benchmark-gated alternative (Priority-2 investigation — see the `graph_cut_vs_dp_seam_benchmark`
/// test, run with `--ignored`).
///
/// **Verdict — DP stays default.** A true s-t min-cut *can* in principle route the seam in 2-D around a
/// moving object where a monotone DP seam cannot, so the upgrade is implemented and available behind
/// this constant. But the benchmark (a ~72k-node overlap band at seam scale) found it does not pay off
/// here on two counts:
/// - **No T1 win.** On both a static overlap and a moving-object overlap, DP and graph-cut produced the
///   *same* max seam-crossing colour disagreement (T1 = 0 — the overlap band is wide enough that the
///   monotone DP seam already routes around the object), and the multi-band blend hides the residual
///   either way. Graph-cut was not *measurably* better.
/// - **Far too slow.** `pathfinding` only exposes Edmonds-Karp (no Dinic). On the 72k-node band it took
///   ~40 s versus DP's ~35 ms (3 orders of magnitude, versus the ~2 s budget), and it scales
///   super-linearly in node count, so the full ~1400 px seam scale is worse still.
///
/// So DP remains the default; graph-cut is correct and tested, and can be enabled by flipping
/// [`SEAM_METHOD`] should a future workload (heavy parallax / moving subjects) justify the cost.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeamMethod {
    Dp,
    // Constructed only when [`SEAM_METHOD`] is flipped or by the benchmark test; kept available behind
    // the constant per the Priority-2 verdict (DP stays default).
    #[allow(dead_code)]
    GraphCut,
}

/// The active seam method. DP by design (see [`SeamMethod`]); this is the "StitchOptions-independent
/// internal constant" the graph-cut upgrade lives behind.
pub(crate) const SEAM_METHOD: SeamMethod = SeamMethod::Dp;

/// Node cap for the graph-cut overlap graph; a larger band is coarsened by an integer stride to fit.
const GC_MAX_NODES: usize = 150_000;
/// Fixed-point scale for quantizing the (float) seam cost into the integer capacities Edmonds-Karp
/// needs. `+1` on every n-link keeps capacities strictly positive.
const GC_COST_SCALE: f64 = 256.0;

/// Deghosting: a band pixel is a "ghost" (a local moving-object / parallax disagreement) if its
/// gain-corrected `|I_i − I_j|` exceeds this multiple of the band's median disagreement…
const GHOST_DIFF_MULT: f32 = 8.0;
/// …and also clears this absolute floor (so a near-perfectly-agreeing band, whose median ≈ 0, does not
/// flag every resampling wobble as a ghost).
const GHOST_ABS_FLOOR: f32 = 0.05;
/// Ghost regions are dilated by this radius (px, at seam scale) before being penalized, so the seam is
/// pushed clear of the object's edge, not just its interior.
const GHOST_DILATE: usize = 3;
/// Hard penalty added to the seam cost inside a (dilated) ghost region — large enough to route the seam
/// around it, but below [`OUT_OF_BAND_COST`] so it never beats the "stay in the mutual band" pin.
const GHOST_PENALTY: f32 = 1.0e4;

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
    gains: &[[f32; 3]],
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
        let (gi, gj) = (gains[si], gains[sj]);
        match SEAM_METHOD {
            SeamMethod::Dp => {
                carve_seam(&mut ids, canvas_w, wi, wj, si as u8, sj as u8, rect, gi, gj)
            }
            SeamMethod::GraphCut => graph_cut(&mut ids, canvas_w, wi, wj, si as u8, sj as u8, rect),
        }
    }

    IdMap {
        ids,
        w: canvas_w,
        h: canvas_h,
    }
}

/// Cut one min-cost seam through the overlap of two frames and reassign ownership on each side.
///
/// `gain_i`/`gain_j` are the per-frame exposure gains (slot-aligned); the seam cost uses the
/// gain-corrected colour disagreement so exposure offsets don't masquerade as content differences, and
/// a light **deghosting** term adds a hard penalty over locally anomalous disagreement (moving objects
/// / parallax) so the seam routes around them instead of blending a ghost.
#[allow(clippy::too_many_arguments)]
fn carve_seam(
    ids: &mut [u8],
    canvas_w: usize,
    wi: &Warped,
    wj: &Warped,
    si: u8,
    sj: u8,
    rect: (usize, usize, usize, usize),
    gain_i: [f32; 3],
    gain_j: [f32; 3],
) {
    let (rx, ry, ow, oh) = rect;
    if ow < 2 || oh < 2 {
        return;
    }

    // Gain-corrected colour-difference surface over the overlap rect. Pixels where the two frames do
    // not both have data get a large cost, pinning the seam inside the mutual band.
    let mut diff = vec![OUT_OF_BAND_COST; ow * oh];
    let mut band_count = 0usize;
    for ly in 0..oh {
        for lx in 0..ow {
            let (cx, cy) = (rx + lx, ry + ly);
            if let (Some((ci, wgi)), Some((cj, wgj))) = (wi.sample(cx, cy), wj.sample(cx, cy)) {
                if wgi > WEIGHT_FLOOR && wgj > WEIGHT_FLOOR {
                    let d = (ci[0] * gain_i[0] - cj[0] * gain_j[0]).abs()
                        + (ci[1] * gain_i[1] - cj[1] * gain_j[1]).abs()
                        + (ci[2] * gain_i[2] - cj[2] * gain_j[2]).abs();
                    diff[ly * ow + lx] = d;
                    band_count += 1;
                }
            }
        }
    }
    if band_count < oh.min(ow) {
        return; // band too thin to carve a meaningful seam; keep the Voronoi assignment
    }

    // Deghosting: mark band pixels whose disagreement is anomalously high (moving object / parallax),
    // dilate the mark, and later hard-penalize the seam there. Threshold = max(k·median, floor).
    let ghost = ghost_mask(&diff, ow, oh, band_count);

    // cost = colour diff + gradient magnitude of the diff (central differences, clamped edges)
    //      + hard ghost penalty.
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
            let ghost_pen = if ghost[ly * ow + lx] {
                GHOST_PENALTY
            } else {
                0.0
            };
            cost[ly * ow + lx] = c + (gx * gx + gy * gy).sqrt() + ghost_pen;
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

/// Build the (dilated) deghosting mask over the overlap band: `true` where the disagreement is a local
/// anomaly (moving object / parallax). `band_count` is the number of in-band (both-covered) pixels.
fn ghost_mask(diff: &[f32], ow: usize, oh: usize, band_count: usize) -> Vec<bool> {
    // Median of the in-band disagreements (values below OUT_OF_BAND_COST).
    let mut vals: Vec<f32> = diff
        .iter()
        .copied()
        .filter(|&d| d < OUT_OF_BAND_COST)
        .collect();
    if vals.is_empty() {
        return vec![false; ow * oh];
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = vals[band_count / 2];
    let threshold = (GHOST_DIFF_MULT * median).max(GHOST_ABS_FLOOR);

    let raw: Vec<bool> = diff
        .iter()
        .map(|&d| d < OUT_OF_BAND_COST && d > threshold)
        .collect();
    // Cheap square dilation by GHOST_DILATE (separable max over a (2r+1) window, both axes).
    if !raw.iter().any(|&g| g) {
        return raw;
    }
    let r = GHOST_DILATE;
    let mut tmp = vec![false; ow * oh];
    for ly in 0..oh {
        for lx in 0..ow {
            let mut hit = false;
            for k in lx.saturating_sub(r)..=(lx + r).min(ow - 1) {
                if raw[ly * ow + k] {
                    hit = true;
                    break;
                }
            }
            tmp[ly * ow + lx] = hit;
        }
    }
    let mut out = vec![false; ow * oh];
    for ly in 0..oh {
        for lx in 0..ow {
            let mut hit = false;
            for k in ly.saturating_sub(r)..=(ly + r).min(oh - 1) {
                if tmp[k * ow + lx] {
                    hit = true;
                    break;
                }
            }
            out[ly * ow + lx] = hit;
        }
    }
    out
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

/// Cell classification for the graph-cut overlap grid.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cell {
    /// Only frame `i` has data here → forced to the source (`A`) side.
    IOnly,
    /// Only frame `j` has data here → forced to the sink (`B`) side.
    JOnly,
    /// Both frames overlap here → a free graph node the min-cut may assign to either side.
    Both,
    /// Neither frame → not part of this pair.
    Neither,
}

/// Cut one seam through the overlap via an s-t **min-cut** (max-flow) instead of a monotone DP seam.
///
/// Nodes are the overlap cells (coarsened by an integer stride so the graph never exceeds
/// [`GC_MAX_NODES`]). Cells covered by only one frame are folded into that frame's terminal (source =
/// frame `i`, sink = frame `j`) with infinite capacity; between two overlapping cells the n-link
/// capacity is the classic Kwatra matching cost (the colour disagreement of the two warps at the two
/// endpoints), so the min-cut runs where the images already agree. The cut partitions the overlap into
/// an `i`-side and a `j`-side (recovered by a residual-graph BFS from the source), and ownership is
/// reassigned accordingly. Unlike the DP seam this is free to route in 2-D (e.g. around a moving
/// object), at the cost of a max-flow solve.
fn graph_cut(
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
    // Coarsen so the node count stays under the cap.
    let stride = (((ow * oh) as f64 / GC_MAX_NODES as f64).sqrt().ceil() as usize).max(1);
    let gw = ow.div_ceil(stride);
    let gh = oh.div_ceil(stride);

    // Classifier for an arbitrary canvas pixel: which frame(s) cover it.
    let classify = |cx: isize, cy: isize| -> (Cell, [f32; 3], [f32; 3]) {
        if cx < 0 || cy < 0 || cx >= canvas_w as isize {
            return (Cell::Neither, [0.0; 3], [0.0; 3]);
        }
        let (cx, cy) = (cx as usize, cy as usize);
        let hi = wi.sample(cx, cy).filter(|&(_, w)| w > WEIGHT_FLOOR);
        let hj = wj.sample(cx, cy).filter(|&(_, w)| w > WEIGHT_FLOOR);
        match (hi, hj) {
            (Some((ci, _)), Some((cj, _))) => (Cell::Both, ci, cj),
            (Some(_), None) => (Cell::IOnly, [0.0; 3], [0.0; 3]),
            (None, Some(_)) => (Cell::JOnly, [0.0; 3], [0.0; 3]),
            (None, None) => (Cell::Neither, [0.0; 3], [0.0; 3]),
        }
    };
    // Grid-cell centre pixel.
    let centre = |gx: usize, gy: usize| -> (isize, isize) {
        (
            (rx + (gx * stride + stride / 2).min(ow - 1)) as isize,
            (ry + (gy * stride + stride / 2).min(oh - 1)) as isize,
        )
    };

    // Classify each grid cell (sampled at its centre pixel) and cache its two colours.
    let mut cells = vec![Cell::Neither; gw * gh];
    let mut col_i = vec![[0.0f32; 3]; gw * gh];
    let mut col_j = vec![[0.0f32; 3]; gw * gh];
    for gy in 0..gh {
        for gx in 0..gw {
            let (cx, cy) = centre(gx, gy);
            let (cell, ci, cj) = classify(cx, cy);
            let g = gy * gw + gx;
            cells[g] = cell;
            col_i[g] = ci;
            col_j[g] = cj;
        }
    }

    // Node ids: 0 = source (frame i), 1 = sink (frame j), each `Both` cell = 2 + grid index.
    const SRC: usize = 0;
    const SNK: usize = 1;
    let node = |g: usize| 2 + g;
    let match_cost = |g: usize| -> f64 {
        let (a, b) = (col_i[g], col_j[g]);
        ((a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs()) as f64
    };
    let inf: i64 = i64::MAX / 4;

    let mut vertices: Vec<usize> = vec![SRC, SNK];
    let mut caps: Vec<((usize, usize), i64)> = Vec::new();
    // cap map + adjacency for the residual BFS afterwards.
    let mut cap: std::collections::HashMap<(usize, usize), i64> = std::collections::HashMap::new();
    let mut adj: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    let add = |u: usize,
               v: usize,
               c: i64,
               cap: &mut std::collections::HashMap<(usize, usize), i64>,
               adj: &mut std::collections::HashMap<usize, Vec<usize>>,
               caps: &mut Vec<((usize, usize), i64)>| {
        caps.push(((u, v), c));
        *cap.entry((u, v)).or_insert(0) += c;
        adj.entry(u).or_default().push(v);
        adj.entry(v).or_default().push(u);
    };

    let mut any_both = false;
    for gy in 0..gh {
        for gx in 0..gw {
            let g = gy * gw + gx;
            if cells[g] != Cell::Both {
                continue;
            }
            any_both = true;
            vertices.push(node(g));
            // Terminal links: a cell is pinned to the source (frame `i`) if it borders an `i`-only
            // region, to the sink (frame `j`) if it borders a `j`-only region. In-band overlap rects
            // are all `Both`, so those exclusive regions live just OUTSIDE the rect — sample the ring
            // one pixel beyond the rect edge for boundary cells, and any in-grid single-coverage cells
            // for the non-rectangular case.
            let mut to_src = false;
            let mut to_snk = false;
            for (nx, ny) in neigh(gx, gy, gw, gh) {
                match cells[ny * gw + nx] {
                    Cell::IOnly => to_src = true,
                    Cell::JOnly => to_snk = true,
                    _ => {}
                }
            }
            let (ccx, ccy) = centre(gx, gy);
            let probe = |dx: isize, dy: isize, to_src: &mut bool, to_snk: &mut bool| match classify(
                ccx + dx,
                ccy + dy,
            )
            .0
            {
                Cell::IOnly => *to_src = true,
                Cell::JOnly => *to_snk = true,
                _ => {}
            };
            if gx == 0 {
                probe(-(stride as isize), 0, &mut to_src, &mut to_snk);
            }
            if gx == gw - 1 {
                probe(stride as isize, 0, &mut to_src, &mut to_snk);
            }
            if gy == 0 {
                probe(0, -(stride as isize), &mut to_src, &mut to_snk);
            }
            if gy == gh - 1 {
                probe(0, stride as isize, &mut to_src, &mut to_snk);
            }
            if to_src {
                add(SRC, node(g), inf, &mut cap, &mut adj, &mut caps);
            }
            if to_snk {
                add(node(g), SNK, inf, &mut cap, &mut adj, &mut caps);
            }
            // n-links to the right/down neighbour (each undirected edge once, added both directions).
            for (nx, ny) in [(gx + 1, gy), (gx, gy + 1)] {
                if nx < gw && ny < gh {
                    let g2 = ny * gw + nx;
                    if cells[g2] == Cell::Both {
                        let c = ((match_cost(g) + match_cost(g2)) * GC_COST_SCALE) as i64 + 1;
                        add(node(g), node(g2), c, &mut cap, &mut adj, &mut caps);
                        add(node(g2), node(g), c, &mut cap, &mut adj, &mut caps);
                    }
                }
            }
        }
    }
    if !any_both {
        return;
    }

    let (flows, _max, _cut) = edmonds_karp_sparse(&vertices, &SRC, &SNK, caps);
    // Residual BFS from the source: reachable `Both` cells are the frame-`i` (source) side.
    let mut flow: std::collections::HashMap<(usize, usize), i64> = std::collections::HashMap::new();
    for ((u, v), f) in flows {
        *flow.entry((u, v)).or_insert(0) += f;
    }
    let residual = |u: usize, v: usize| -> i64 {
        cap.get(&(u, v)).copied().unwrap_or(0) - flow.get(&(u, v)).copied().unwrap_or(0)
            + flow.get(&(v, u)).copied().unwrap_or(0)
    };
    let mut src_side = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(SRC);
    src_side.insert(SRC);
    while let Some(u) = queue.pop_front() {
        if let Some(ns) = adj.get(&u) {
            for &v in ns {
                if !src_side.contains(&v) && residual(u, v) > 0 {
                    src_side.insert(v);
                    queue.push_back(v);
                }
            }
        }
    }

    // Per grid cell → desired frame: source side / I-only ⇒ `si`; sink side / J-only ⇒ `sj`.
    let side_is_i = |g: usize| -> Option<bool> {
        match cells[g] {
            Cell::IOnly => Some(true),
            Cell::JOnly => Some(false),
            Cell::Both => Some(src_side.contains(&node(g))),
            Cell::Neither => None,
        }
    };

    for ly in 0..oh {
        for lx in 0..ow {
            let g = (ly / stride) * gw + (lx / stride);
            if let Some(is_i) = side_is_i(g) {
                let desired = if is_i { si } else { sj };
                assign(ids, canvas_w, wi, wj, si, sj, rx + lx, ry + ly, desired);
            }
        }
    }
}

/// 4-neighbours of a grid cell inside `gw×gh`.
fn neigh(gx: usize, gy: usize, gw: usize, gh: usize) -> impl Iterator<Item = (usize, usize)> {
    let mut out = Vec::with_capacity(4);
    if gx > 0 {
        out.push((gx - 1, gy));
    }
    if gx + 1 < gw {
        out.push((gx + 1, gy));
    }
    if gy > 0 {
        out.push((gx, gy - 1));
    }
    if gy + 1 < gh {
        out.push((gx, gy + 1));
    }
    out.into_iter()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Warped;
    use std::time::Instant;

    /// Build two overlapping warps on a `canvas_w×canvas_h` canvas: frame `i` covers the left super-rect
    /// `[0,split+ov)`, frame `j` the right `[split,canvas_w)`, overlapping in `[split, split+ov)`. Both
    /// sample the same deterministic static background so their overlap agrees (near-zero seam cost),
    /// except pixels inside `object` (a rect in canvas coords) which are painted bright in frame `i`
    /// only — a "moving object" that a good seam must route around.
    fn overlap_pair(
        canvas_w: usize,
        canvas_h: usize,
        split: usize,
        ov: usize,
        object: Option<(usize, usize, usize, usize)>,
    ) -> (Warped, Warped) {
        let bg = |x: usize, y: usize, c: usize| ((x * 3 + y * 7 + c * 13) % 97) as f32 / 97.0;
        let make = |x0: usize, wsub: usize, paint_obj: bool| -> Warped {
            let mut rgb = vec![0.0f32; wsub * canvas_h * 3];
            let mut weight = vec![0.0f32; wsub * canvas_h];
            for y in 0..canvas_h {
                for lx in 0..wsub {
                    let x = x0 + lx;
                    let i = y * wsub + lx;
                    for c in 0..3 {
                        rgb[i * 3 + c] = bg(x, y, c);
                    }
                    if paint_obj {
                        if let Some((ox, oy, ow, oh)) = object {
                            if x >= ox && x < ox + ow && y >= oy && y < oy + oh {
                                rgb[i * 3] = 1.0;
                                rgb[i * 3 + 1] = 1.0;
                                rgb[i * 3 + 2] = 1.0;
                            }
                        }
                    }
                    weight[i] = 1.0;
                }
            }
            Warped {
                rect: (x0, 0, wsub, canvas_h),
                rgb,
                weight,
            }
        };
        let wi = make(0, split + ov, true); // left frame carries the object
        let wj = make(split, canvas_w - split, false);
        (wi, wj)
    }

    /// Max colour disagreement `|I_i − I_j|` (L1) spanned by the ownership boundary — the T1-style
    /// "max seam-crossing gradient" the blend must bridge. Lower is a better seam.
    fn seam_crossing_max(
        ids: &[u8],
        canvas_w: usize,
        canvas_h: usize,
        wi: &Warped,
        wj: &Warped,
        si: u8,
        sj: u8,
    ) -> f32 {
        let mut m = 0.0f32;
        for y in 0..canvas_h {
            for x in 0..canvas_w {
                let o = ids[y * canvas_w + x];
                if o != si && o != sj {
                    continue;
                }
                // Is this a boundary pixel (a 4-neighbour owned by the other frame)?
                let mut boundary = false;
                for (nx, ny) in [
                    (x.wrapping_sub(1), y),
                    (x + 1, y),
                    (x, y.wrapping_sub(1)),
                    (x, y + 1),
                ] {
                    if nx < canvas_w && ny < canvas_h {
                        let no = ids[ny * canvas_w + nx];
                        if (no == si || no == sj) && no != o {
                            boundary = true;
                        }
                    }
                }
                if !boundary {
                    continue;
                }
                if let (Some((ci, _)), Some((cj, _))) = (wi.sample(x, y), wj.sample(x, y)) {
                    let d = (ci[0] - cj[0]).abs() + (ci[1] - cj[1]).abs() + (ci[2] - cj[2]).abs();
                    m = m.max(d);
                }
            }
        }
        m
    }

    fn voronoi(ids: &mut [u8], canvas_w: usize, canvas_h: usize, wi: &Warped, wj: &Warped) {
        for y in 0..canvas_h {
            for x in 0..canvas_w {
                let hi = wi.sample(x, y).map(|(_, w)| w).unwrap_or(0.0);
                let hj = wj.sample(x, y).map(|(_, w)| w).unwrap_or(0.0);
                ids[y * canvas_w + x] = if hi <= WEIGHT_FLOOR && hj <= WEIGHT_FLOOR {
                    EMPTY
                } else if hi >= hj {
                    0
                } else {
                    1
                };
            }
        }
    }

    /// Priority-2 benchmark: DP vs graph-cut on (a) a static overlap (like the real pano) and (b) a
    /// moving-object overlap. Reports the T1 metric (max seam-crossing colour disagreement) and the
    /// runtime for each, and records the verdict. Not an assertion of superiority — it documents the
    /// trade-off that keeps DP the default.
    ///
    /// `#[ignore]`d because Edmonds-Karp on a ~72k-node band takes tens of seconds (that runtime is
    /// precisely the finding); run explicitly with `cargo test -p core-pano -- --ignored --nocapture`.
    #[test]
    #[ignore = "graph-cut max-flow is ~40s at this band size; run manually for the P2 numbers"]
    fn graph_cut_vs_dp_seam_benchmark() {
        let (cw, ch) = (520usize, 360usize);
        let (split, ov) = (180usize, 200usize); // overlap [180,380) × 360 = 72k px band

        for (label, object) in [
            ("static", None),
            // A bright bar across most of the band with a gap only on the far left, positioned mid-
            // height: a monotone DP seam can't detour to the gap fast enough and is forced through it.
            ("moving_object", Some((230, 150, 140, 70))),
        ] {
            let (wi, wj) = overlap_pair(cw, ch, split, ov, object);
            let rect = rect_overlap(wi.rect, wj.rect).expect("overlap");

            let mut dp = vec![EMPTY; cw * ch];
            voronoi(&mut dp, cw, ch, &wi, &wj);
            let t = Instant::now();
            carve_seam(&mut dp, cw, &wi, &wj, 0, 1, rect, [1.0; 3], [1.0; 3]);
            let dp_ms = t.elapsed().as_secs_f64() * 1e3;
            let dp_t1 = seam_crossing_max(&dp, cw, ch, &wi, &wj, 0, 1);

            let mut gc = vec![EMPTY; cw * ch];
            voronoi(&mut gc, cw, ch, &wi, &wj);
            let t = Instant::now();
            graph_cut(&mut gc, cw, &wi, &wj, 0, 1, rect);
            let gc_ms = t.elapsed().as_secs_f64() * 1e3;
            let gc_t1 = seam_crossing_max(&gc, cw, ch, &wi, &wj, 0, 1);

            println!(
                "[{label}] band {}x{}  DP: T1={dp_t1:.3} {dp_ms:.1}ms   GraphCut: T1={gc_t1:.3} {gc_ms:.1}ms",
                rect.2, rect.3
            );

            // Both must produce a valid partition covering the whole band with one of the two frames.
            let band_owned = (0..ch)
                .flat_map(|y| (rect.0..rect.0 + rect.2).map(move |x| (x, y)))
                .all(|(x, y)| matches!(gc[y * cw + x], 0 | 1));
            assert!(band_owned, "[{label}] graph-cut left band pixels unowned");
        }
    }

    /// Sanity: the graph-cut partition splits the static overlap into a left (frame i) and right
    /// (frame j) region — the source/sink terminals actually pin the two sides.
    #[test]
    fn graph_cut_partitions_overlap() {
        // Small band so Edmonds-Karp stays fast enough for the regular suite.
        let (cw, ch) = (110usize, 70usize);
        let (wi, wj) = overlap_pair(cw, ch, 40, 40, None);
        let rect = rect_overlap(wi.rect, wj.rect).unwrap();
        let mut ids = vec![EMPTY; cw * ch];
        voronoi(&mut ids, cw, ch, &wi, &wj);
        graph_cut(&mut ids, cw, &wi, &wj, 0, 1, rect);
        // Far-left band column owned by i, far-right by j.
        assert_eq!(
            ids[(ch / 2) * cw + rect.0],
            0,
            "left band edge should be frame i"
        );
        assert_eq!(
            ids[(ch / 2) * cw + rect.0 + rect.2 - 1],
            1,
            "right band edge should be frame j"
        );
    }
}
