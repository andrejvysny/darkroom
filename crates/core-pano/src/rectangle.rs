//! Boundary warp / rectangling (Phase P5) — a simplified He, Chang & Sun 2013,
//! "Rectangling Panoramic Images via Warping" (SIGGRAPH 2013).
//!
//! After the multi-band blend, a stitched panorama's valid region is an irregular (often barrel- or
//! parallelogram-shaped) blob inside a rectangular canvas, so [`crate::crop`]'s largest inscribed
//! rectangle throws away a lot of real pixels. This pass warps the composite so the valid region fills
//! more of the bounding rectangle, letting the auto-crop keep far more content.
//!
//! ## Model (inverse mesh warp)
//! A regular grid mesh is laid over the canvas. Its **rest** positions double as the *output* sample
//! lattice (they tile the rectangle exactly, so there are never gaps or holes), and we solve for a
//! per-vertex *source* position — where in the input composite that output vertex should sample from.
//! Applying the warp is then an inverse bilinear mesh warp: every output pixel bilerps the four corner
//! source positions of its cell and samples the composite there.
//!
//! Two quadratic energies, solved once as a linear least-squares (conjugate gradient on the normal
//! equations, `x` and `y` decouple into two SPD systems):
//! - **(a) boundary attraction.** For each mesh vertex on a canvas edge, the source is pulled inward to
//!   the nearest valid-mask pixel along that edge's inward normal (top/bottom edges pull the source's
//!   `y`, left/right edges pull `x`; corners get both). So the rectangle's boundary ends up sampling
//!   real content instead of the empty margin.
//! - **(b) shape preservation.** A first-order membrane energy `Σ_edges |(S_a−S_b) − (R_a−R_b)|²`
//!   keeps every mesh edge close to its rest vector — an ARAP-like/similarity simplification that keeps
//!   the interior locally rigid, so the warp concentrates at the boundary and the centre barely moves.
//! - Line preservation (He et al. §5) is **out of scope for v1** (documented; would need line
//!   detection + per-segment rotation bins).
//!
//! ## Numerical safety
//! The interpolated source field is smooth (membrane term), so large *absolute* displacements at the
//! boundary are fine as long as neighbouring vertices don't cross. We therefore do **not** use the
//! naive "per-vertex displacement < 0.45·cell" clamp (it is only a *sufficient* no-fold bound and, on a
//! strongly sheared boundary like a hand-held pano, it is far too tight — the crop-limiting intrusions
//! are many cells deep, so that clamp would defeat rectangling entirely). Instead we enforce the actual
//! no-fold invariant — every mesh quad keeps a positive Jacobian — by globally capping the applied
//! displacement factor (binary search). This is necessary *and* sufficient, permits the smooth large
//! displacements rectangling needs, and still guarantees no quad ever folds. If the mask is already
//! ≥ 99 % rectangular (or the canvas is too small to mesh) the whole pass is skipped and the input is
//! returned untouched.
//!
//! Determinism: the solve is serial f64 with a fixed iteration budget; the apply is embarrassingly
//! parallel (each output pixel independent), so results are bit-reproducible.

use rayon::prelude::*;

/// Target mesh resolution (cells); actual counts shrink so a cell never goes below [`MIN_CELL`] px.
const TARGET_COLS: usize = 40;
const TARGET_ROWS: usize = 25;
/// Minimum cell size in pixels (clamps the mesh on small canvases).
const MIN_CELL: usize = 16;
/// Soft weight of the boundary-attraction constraints relative to the per-edge membrane term (=1).
const BOUNDARY_WEIGHT: f64 = 6.0;
/// Floor anchor of every vertex to its rest position: removes the membrane null space (global
/// translation) and keeps the system strictly PD.
const ANCHOR_EPS: f64 = 0.01;
/// Strong anchor added to vertices deep inside the valid region (see [`STRONG_ANCHOR`] ramp). This is
/// the "classify inside vs near-boundary" step: interior vertices are pinned rigidly to rest so the
/// warp has *compact support* near the boundary — the far interior (and its high-frequency detail)
/// does not move, while the boundary band is free to ramp out and fill. Uniform screening cannot do
/// both at once (it either folds at the boundary or leaks to the centre); a distance-gated anchor can.
const STRONG_ANCHOR: f64 = 4.0;
/// A vertex is fully free within this many cells of the nearest empty pixel, then ramps to fully
/// pinned over [`RAMP_CELLS`] more cells. The free band must be a few cells deep so a deep boundary
/// intrusion's displacement can spread across enough cells to never fold.
const FREE_BAND_CELLS: f64 = 1.5;
const RAMP_CELLS: f64 = 4.0;
/// Cap a boundary source displacement to this many cells, so acute canvas corners (whole empty
/// columns/rows) can't demand a grotesque stretch that would swamp the centre.
const MAX_TARGET_CELLS: f64 = 6.0;
/// Conjugate-gradient budget and relative residual tolerance.
const CG_MAX_ITERS: usize = 500;
const CG_TOL: f64 = 1e-6;
/// Skip the whole pass if the valid mask is already at least this fraction rectangular.
const NEAR_RECT_COVERAGE: f64 = 0.99;
/// Minimum normalized per-quad Jacobian (relative to a rest cell's area) tolerated as "no fold".
const JAC_MIN: f64 = 0.05;

/// The solved rectangling mesh: rest (= output) lattice + solved source displacement per vertex.
pub(crate) struct Mesh {
    pub cols: usize,
    pub rows: usize,
    pub cell_w: f64,
    pub cell_h: f64,
    /// `(rows+1)*(cols+1)` rest positions, row-major, vertex `(i,j)` at `i*(cols+1)+j`.
    pub rest: Vec<[f64; 2]>,
    /// Solved source displacement `S − R` at each vertex (pre-lerp, pre-no-fold-cap).
    pub disp: Vec<[f64; 2]>,
}

impl Mesh {
    #[inline]
    fn vid(&self, i: usize, j: usize) -> usize {
        i * (self.cols + 1) + j
    }

    /// Source positions at applied factor `f` (`f` already folds in the user lerp and no-fold cap):
    /// `rest + f·disp`.
    pub(crate) fn source_at(&self, f: f64) -> Vec<[f64; 2]> {
        self.rest
            .iter()
            .zip(self.disp.iter())
            .map(|(r, d)| [r[0] + f * d[0], r[1] + f * d[1]])
            .collect()
    }
}

/// Warp `rgb`/`valid` (interleaved RGB + coverage, `w×h`) so the valid region fills more of the
/// rectangle. `t ∈ [0,1]` is the user boundary-warp strength (slider/100). Returns the warped
/// `(rgb, valid)`, or `None` if the pass was skipped (already rectangular, or canvas too small).
pub(crate) fn rectangle_warp(
    rgb: &[f32],
    valid: &[u8],
    w: usize,
    h: usize,
    t: f64,
) -> Option<(Vec<f32>, Vec<u8>)> {
    let mesh = build_and_solve(valid, w, h)?;
    let f_max = nofold_factor(&mesh);
    let factor = t.clamp(0.0, 1.0).min(f_max);
    let src = mesh.source_at(factor);
    Some(apply_warp(rgb, valid, w, h, &mesh, &src))
}

/// Build the mesh and solve the boundary-attraction + membrane least-squares. `None` when the pass
/// should be skipped.
pub(crate) fn build_and_solve(valid: &[u8], w: usize, h: usize) -> Option<Mesh> {
    if w < 2 * MIN_CELL || h < 2 * MIN_CELL {
        return None;
    }
    // Already (nearly) rectangular → nothing to gain.
    let covered: usize = valid.iter().map(|&v| (v != 0) as usize).sum();
    if covered as f64 >= NEAR_RECT_COVERAGE * (w * h) as f64 {
        return None;
    }

    let cols = TARGET_COLS.min(w / MIN_CELL).max(1);
    let rows = TARGET_ROWS.min(h / MIN_CELL).max(1);
    let cell_w = w as f64 / cols as f64;
    let cell_h = h as f64 / rows as f64;

    let nv = (rows + 1) * (cols + 1);
    let mut rest = vec![[0.0f64; 2]; nv];
    for i in 0..=rows {
        for j in 0..=cols {
            rest[i * (cols + 1) + j] = [j as f64 * cell_w, i as f64 * cell_h];
        }
    }
    let mut mesh = Mesh {
        cols,
        rows,
        cell_w,
        cell_h,
        rest,
        disp: vec![[0.0; 2]; nv],
    };

    // --- Adjacency (4-connected grid) ---
    let mut adj: Vec<Vec<usize>> = vec![Vec::with_capacity(4); nv];
    for i in 0..=rows {
        for j in 0..=cols {
            let v = mesh.vid(i, j);
            if i > 0 {
                adj[v].push(mesh.vid(i - 1, j));
            }
            if i < rows {
                adj[v].push(mesh.vid(i + 1, j));
            }
            if j > 0 {
                adj[v].push(mesh.vid(i, j - 1));
            }
            if j < cols {
                adj[v].push(mesh.vid(i, j + 1));
            }
        }
    }

    // --- Boundary source targets (per axis; x and y decouple). ---
    // For a canvas-edge vertex, the source coordinate normal to that edge is pulled to the nearest
    // valid-mask pixel; the tangential coordinate is left free (membrane holds it near rest).
    let max_disp_x = MAX_TARGET_CELLS * cell_w;
    let max_disp_y = MAX_TARGET_CELLS * cell_h;

    // Per-vertex constraint (target coordinate, weight) for the x- and y-systems.
    let mut cx_tgt = vec![0.0f64; nv];
    let mut cx_w = vec![0.0f64; nv];
    let mut cy_tgt = vec![0.0f64; nv];
    let mut cy_w = vec![0.0f64; nv];

    for i in 0..=rows {
        for j in 0..=cols {
            let v = mesh.vid(i, j);
            let [rx, ry] = mesh.rest[v];
            // Left / right edges pull source x.
            if j == 0 {
                if let Some(xl) = scan_left(valid, w, h, ry) {
                    cx_tgt[v] = clamp_target(xl as f64, rx, max_disp_x);
                    cx_w[v] = BOUNDARY_WEIGHT;
                }
            } else if j == cols {
                if let Some(xr) = scan_right(valid, w, h, ry) {
                    cx_tgt[v] = clamp_target(xr as f64, rx, max_disp_x);
                    cx_w[v] = BOUNDARY_WEIGHT;
                }
            }
            // Top / bottom edges pull source y.
            if i == 0 {
                if let Some(yt) = scan_top(valid, w, h, rx) {
                    cy_tgt[v] = clamp_target(yt as f64, ry, max_disp_y);
                    cy_w[v] = BOUNDARY_WEIGHT;
                }
            } else if i == rows {
                if let Some(yb) = scan_bottom(valid, w, h, rx) {
                    cy_tgt[v] = clamp_target(yb as f64, ry, max_disp_y);
                    cy_w[v] = BOUNDARY_WEIGHT;
                }
            }
        }
    }

    // --- Per-vertex anchor: pin deep-interior vertices rigidly to rest (compact support). ---
    let dist = dist_to_invalid(valid, w, h);
    let free_lo = FREE_BAND_CELLS * cell_w.min(cell_h);
    let free_hi = free_lo + RAMP_CELLS * cell_w.min(cell_h);
    let anchor: Vec<f64> = (0..nv)
        .map(|v| {
            let [rx, ry] = mesh.rest[v];
            let x = (rx.round() as isize).clamp(0, w as isize - 1) as usize;
            let y = (ry.round() as isize).clamp(0, h as isize - 1) as usize;
            let d = dist[y * w + x] as f64;
            let ramp = smoothstep(free_lo, free_hi, d); // 0 near boundary, 1 deep inside
            ANCHOR_EPS + STRONG_ANCHOR * ramp
        })
        .collect();

    // --- Solve the two SPD systems: H = L(membrane) + diag(weight + anchor). ---
    let rest_x: Vec<f64> = mesh.rest.iter().map(|p| p[0]).collect();
    let rest_y: Vec<f64> = mesh.rest.iter().map(|p| p[1]).collect();
    let sx = solve_axis(&adj, &rest_x, &cx_tgt, &cx_w, &anchor);
    let sy = solve_axis(&adj, &rest_y, &cy_tgt, &cy_w, &anchor);

    for v in 0..nv {
        mesh.disp[v] = [sx[v] - mesh.rest[v][0], sy[v] - mesh.rest[v][1]];
    }
    Some(mesh)
}

/// Clamp a source target so its displacement from `rest` never exceeds `max_disp`.
#[inline]
fn clamp_target(target: f64, rest: f64, max_disp: f64) -> f64 {
    (target - rest).clamp(-max_disp, max_disp) + rest
}

/// Solve one coordinate axis: minimise
/// `Σ_edges (x_a−x_b−(r_a−r_b))² + Σ w_v (x_v−T_v)² + Σ a_v (x_v−r_v)²`
/// via conjugate gradient on the normal equations. `rest`/`tgt`/`cw`/`anchor` are index-aligned
/// with `adj`.
fn solve_axis(
    adj: &[Vec<usize>],
    rest: &[f64],
    tgt: &[f64],
    cw: &[f64],
    anchor: &[f64],
) -> Vec<f64> {
    let n = rest.len();
    // Diagonal augmentation and RHS.
    let diag_extra: Vec<f64> = (0..n).map(|i| cw[i] + anchor[i]).collect();
    let rhs: Vec<f64> = (0..n)
        .map(|i| {
            // membrane RHS term: Σ_neighbours (r_i − r_n)
            let mut b = 0.0;
            for &nb in &adj[i] {
                b += rest[i] - rest[nb];
            }
            b + cw[i] * tgt[i] + anchor[i] * rest[i]
        })
        .collect();

    // H·p : Laplacian + diagonal.
    let apply = |x: &[f64], out: &mut [f64]| {
        for i in 0..n {
            let mut s = diag_extra[i] * x[i];
            for &nb in &adj[i] {
                s += x[i] - x[nb];
            }
            out[i] = s;
        }
    };

    let mut x = rest.to_vec(); // warm start at the rest positions
    let mut ax = vec![0.0; n];
    apply(&x, &mut ax);
    let mut r: Vec<f64> = (0..n).map(|i| rhs[i] - ax[i]).collect();
    let mut p = r.clone();
    let mut rs = dot(&r, &r);
    let r0 = rs.sqrt().max(1e-30);
    let mut ap = vec![0.0; n];
    for _ in 0..CG_MAX_ITERS {
        if rs.sqrt() <= CG_TOL * r0 {
            break;
        }
        apply(&p, &mut ap);
        let pap = dot(&p, &ap);
        if pap <= 0.0 {
            break;
        }
        let alpha = rs / pap;
        for i in 0..n {
            x[i] += alpha * p[i];
            r[i] -= alpha * ap[i];
        }
        let rs_new = dot(&r, &r);
        let beta = rs_new / rs;
        for i in 0..n {
            p[i] = r[i] + beta * p[i];
        }
        rs = rs_new;
    }
    x
}

#[inline]
fn dot(a: &[f64], b: &[f64]) -> f64 {
    let mut s = 0.0;
    for i in 0..a.len() {
        s += a[i] * b[i];
    }
    s
}

/// Hermite smoothstep: 0 for `d ≤ lo`, 1 for `d ≥ hi`, smooth in between.
#[inline]
fn smoothstep(lo: f64, hi: f64, d: f64) -> f64 {
    if hi <= lo {
        return if d >= hi { 1.0 } else { 0.0 };
    }
    let t = ((d - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Approximate Euclidean distance (in pixels) from every pixel to the nearest *invalid* (empty) pixel,
/// via a two-pass chamfer transform (invalid pixels are seeds at distance 0). Used to gate the
/// interior anchor: pixels far from any empty region are the rigid interior.
fn dist_to_invalid(valid: &[u8], w: usize, h: usize) -> Vec<f32> {
    const ORTHO: f32 = 1.0;
    const DIAG: f32 = std::f32::consts::SQRT_2;
    let big = (w + h) as f32;
    let mut d: Vec<f32> = valid
        .iter()
        .map(|&v| if v == 0 { 0.0 } else { big })
        .collect();
    // Forward pass (top-left → bottom-right).
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut m = d[i];
            if x > 0 {
                m = m.min(d[i - 1] + ORTHO);
            }
            if y > 0 {
                m = m.min(d[i - w] + ORTHO);
                if x > 0 {
                    m = m.min(d[i - w - 1] + DIAG);
                }
                if x + 1 < w {
                    m = m.min(d[i - w + 1] + DIAG);
                }
            }
            d[i] = m;
        }
    }
    // Backward pass (bottom-right → top-left).
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            let mut m = d[i];
            if x + 1 < w {
                m = m.min(d[i + 1] + ORTHO);
            }
            if y + 1 < h {
                m = m.min(d[i + w] + ORTHO);
                if x + 1 < w {
                    m = m.min(d[i + w + 1] + DIAG);
                }
                if x > 0 {
                    m = m.min(d[i + w - 1] + DIAG);
                }
            }
            d[i] = m;
        }
    }
    d
}

// --- Nearest valid pixel scans (used to build boundary targets). ---

/// Topmost valid row in the column nearest `xf` (scanning down), or `None` if the column is empty.
fn scan_top(valid: &[u8], w: usize, h: usize, xf: f64) -> Option<usize> {
    let x = (xf.round() as isize).clamp(0, w as isize - 1) as usize;
    (0..h).find(|&y| valid[y * w + x] != 0)
}

/// Bottommost valid row in the column nearest `xf` (scanning up), or `None` if empty.
fn scan_bottom(valid: &[u8], w: usize, h: usize, xf: f64) -> Option<usize> {
    let x = (xf.round() as isize).clamp(0, w as isize - 1) as usize;
    (0..h).rev().find(|&y| valid[y * w + x] != 0)
}

/// Leftmost valid column in the row nearest `yf` (scanning right), or `None` if empty.
fn scan_left(valid: &[u8], w: usize, h: usize, yf: f64) -> Option<usize> {
    let y = (yf.round() as isize).clamp(0, h as isize - 1) as usize;
    (0..w).find(|&x| valid[y * w + x] != 0)
}

/// Rightmost valid column in the row nearest `yf` (scanning left), or `None` if empty.
fn scan_right(valid: &[u8], w: usize, h: usize, yf: f64) -> Option<usize> {
    let y = (yf.round() as isize).clamp(0, h as isize - 1) as usize;
    (0..w).rev().find(|&x| valid[y * w + x] != 0)
}

// --- No-fold safety ---

/// Largest applied factor `f ∈ [0,1]` for which every mesh quad keeps a positive Jacobian.
pub(crate) fn nofold_factor(mesh: &Mesh) -> f64 {
    if min_norm_jacobian(mesh, 1.0) >= JAC_MIN {
        return 1.0;
    }
    // `f = 0` is the identity map (normalized Jacobian ≡ 1 ≥ JAC_MIN); bisect for the threshold.
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if min_norm_jacobian(mesh, mid) >= JAC_MIN {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Minimum over all quads of the per-quad source Jacobian at applied factor `f`, normalized by a rest
/// cell's area (so the identity map reads 1.0). Evaluated at the four cell corners (a bilinear map's
/// Jacobian is linear in each of `u,v`, so the extrema live at the corners).
pub(crate) fn min_norm_jacobian(mesh: &Mesh, f: f64) -> f64 {
    let src = mesh.source_at(f);
    let (cols, rows) = (mesh.cols, mesh.rows);
    let inv_area = 1.0 / (mesh.cell_w * mesh.cell_h);
    let mut m = f64::INFINITY;
    for i in 0..rows {
        for j in 0..cols {
            let p00 = src[i * (cols + 1) + j];
            let p10 = src[i * (cols + 1) + j + 1];
            let p01 = src[(i + 1) * (cols + 1) + j];
            let p11 = src[(i + 1) * (cols + 1) + j + 1];
            // S(u,v) = P00 + u(P10−P00) + v(P01−P00) + uv(P00−P10−P01+P11)
            let du0 = [p10[0] - p00[0], p10[1] - p00[1]];
            let dv0 = [p01[0] - p00[0], p01[1] - p00[1]];
            let cross = [
                p00[0] - p10[0] - p01[0] + p11[0],
                p00[1] - p10[1] - p01[1] + p11[1],
            ];
            for &(u, v) in &[(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                let su = [du0[0] + v * cross[0], du0[1] + v * cross[1]];
                let sv = [dv0[0] + u * cross[0], dv0[1] + u * cross[1]];
                let det = su[0] * sv[1] - su[1] * sv[0];
                m = m.min(det * inv_area);
            }
        }
    }
    m
}

// --- Apply ---

/// Inverse bilinear mesh warp: for every output pixel, bilerp its cell's four corner source positions
/// and sample the composite there. Rest positions tile the rectangle exactly, so the output has no
/// gaps. Parallel over output rows.
fn apply_warp(
    rgb: &[f32],
    valid: &[u8],
    w: usize,
    h: usize,
    mesh: &Mesh,
    src: &[[f64; 2]],
) -> (Vec<f32>, Vec<u8>) {
    let (cols, rows) = (mesh.cols, mesh.rows);
    let (cell_w, cell_h) = (mesh.cell_w, mesh.cell_h);
    let mut out_rgb = vec![0.0f32; w * h * 3];
    let mut out_valid = vec![0u8; w * h];

    out_rgb
        .par_chunks_mut(w * 3)
        .zip(out_valid.par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, (row_rgb, row_valid))| {
            let i = ((y as f64 / cell_h).floor() as usize).min(rows - 1);
            let v = (y as f64 - i as f64 * cell_h) / cell_h;
            for x in 0..w {
                let j = ((x as f64 / cell_w).floor() as usize).min(cols - 1);
                let u = (x as f64 - j as f64 * cell_w) / cell_w;
                let p00 = src[i * (cols + 1) + j];
                let p10 = src[i * (cols + 1) + j + 1];
                let p01 = src[(i + 1) * (cols + 1) + j];
                let p11 = src[(i + 1) * (cols + 1) + j + 1];
                let a = 1.0 - u;
                let b = 1.0 - v;
                let sx = a * b * p00[0] + u * b * p10[0] + a * v * p01[0] + u * v * p11[0];
                let sy = a * b * p00[1] + u * b * p10[1] + a * v * p01[1] + u * v * p11[1];
                if sx < 0.0 || sy < 0.0 || sx > (w - 1) as f64 || sy > (h - 1) as f64 {
                    continue;
                }
                // Coverage from the nearest source pixel's valid flag.
                let nx = sx.round() as usize;
                let ny = sy.round() as usize;
                if valid[ny * w + nx] == 0 {
                    continue;
                }
                let c = sample_composite(rgb, w, h, sx, sy);
                row_rgb[x * 3] = c[0];
                row_rgb[x * 3 + 1] = c[1];
                row_rgb[x * 3 + 2] = c[2];
                row_valid[x] = 1;
            }
        });

    (out_rgb, out_valid)
}

/// Bilinear sample of the composite RGB at `(sx,sy)` (caller guarantees `0 ≤ sx ≤ w−1`, likewise `sy`).
#[inline]
fn sample_composite(rgb: &[f32], w: usize, h: usize, sx: f64, sy: f64) -> [f32; 3] {
    let x0 = sx.floor() as usize;
    let y0 = sy.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = (sx - x0 as f64) as f32;
    let fy = (sy - y0 as f64) as f32;
    let mut out = [0.0f32; 3];
    for (ch, o) in out.iter_mut().enumerate() {
        let p00 = rgb[(y0 * w + x0) * 3 + ch];
        let p10 = rgb[(y0 * w + x1) * 3 + ch];
        let p01 = rgb[(y1 * w + x0) * 3 + ch];
        let p11 = rgb[(y1 * w + x1) * 3 + ch];
        let top = p00 + (p10 - p00) * fx;
        let bot = p01 + (p11 - p01) * fx;
        *o = top + (bot - top) * fy;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L-shaped mask: a `w×h` rectangle with the top-left quadrant (`x<w/2 && y<h/2`) empty.
    fn l_shape(w: usize, h: usize) -> Vec<u8> {
        let mut v = vec![1u8; w * h];
        for y in 0..h / 2 {
            for x in 0..w / 2 {
                v[y * w + x] = 0;
            }
        }
        v
    }

    /// A curved + sheared "barrel" mask resembling a real single-row cylindrical pano: slanted, bowed
    /// top/bottom edges and small empty triangles at the left/right, inside a `w×h` canvas.
    fn barrel(w: usize, h: usize) -> Vec<u8> {
        let mut v = vec![0u8; w * h];
        for x in 0..w {
            let cxn = x as f64 / (w - 1) as f64; // 0..1
                                                 // Top intrusion deep on the left, bows up in the middle; bottom deep on the right.
            let top = 15.0 + 55.0 * (1.0 - cxn) - 25.0 * (4.0 * cxn * (1.0 - cxn));
            let bot = (h - 1) as f64 - (15.0 + 45.0 * cxn - 20.0 * (4.0 * cxn * (1.0 - cxn)));
            let y0 = top.round().max(0.0) as usize;
            let y1 = (bot.round() as usize).min(h - 1);
            for y in y0..=y1 {
                v[y * w + x] = 1;
            }
        }
        v
    }

    #[test]
    fn mesh_dims_clamp_to_min_cell() {
        // Small canvas: cell must not go below MIN_CELL, so counts shrink from the 40x25 target.
        let (w, h) = (200usize, 120usize);
        let mesh = build_and_solve(&l_shape(w, h), w, h).expect("mesh");
        assert!(mesh.cols <= 200 / MIN_CELL && mesh.rows <= 120 / MIN_CELL);
        assert!(mesh.cell_w >= MIN_CELL as f64 - 1e-9 && mesh.cell_h >= MIN_CELL as f64 - 1e-9);
    }

    #[test]
    fn near_rectangular_mask_is_skipped() {
        let (w, h) = (320usize, 256usize);
        let full = vec![1u8; w * h];
        assert!(
            build_and_solve(&full, w, h).is_none(),
            "full mask must skip"
        );
        assert!(
            rectangle_warp(&vec![0.5f32; w * h * 3], &full, w, h, 1.0).is_none(),
            "full mask warp must be a no-op (None)"
        );
    }

    #[test]
    fn notch_boundary_moves_outward_monotonically_with_t() {
        // (Priority-1 test 3) The solved mesh pulls a notch-edge vertex's *source* into the valid
        // content (outward fill), and the lerp is strictly monotone: |disp(0.5)| lies strictly
        // between |disp(0)|=0 and |disp(1)|.
        let (w, h) = (320usize, 256usize);
        let mesh = build_and_solve(&l_shape(w, h), w, h).expect("mesh");

        // A top-edge vertex sitting deep inside the empty top-left notch.
        let v = mesh.vid(0, 3);
        assert!(
            mesh.rest[v][0] < (w / 2) as f64,
            "vertex should be inside the notch"
        );

        // Source y is pulled downward (positive) toward the content boundary at y = h/2 — i.e. the
        // notch's top boundary is moved outward so the empty rows above it get filled from content.
        let dy = mesh.disp[v][1];
        assert!(
            dy > 5.0,
            "notch source should be pulled outward into content, got dy={dy}"
        );

        // Strict monotonicity of the displacement magnitude with t: |D(0)| = 0 < |D(0.5)| < |D(1)|,
        // and the lerp is exactly linear (D(0.5) is the midpoint of D(0) and D(1)).
        let mag = |f: f64| -> f64 {
            let p = mesh.source_at(f)[v];
            ((p[0] - mesh.rest[v][0]).powi(2) + (p[1] - mesh.rest[v][1]).powi(2)).sqrt()
        };
        let (m0, mh, m1) = (mag(0.0), mag(0.5), mag(1.0));
        assert_eq!(m0, 0.0, "t=0 must be the rest position");
        assert!(
            m0 < mh && mh < m1,
            "displacement not strictly monotone in t: {m0} {mh} {m1}"
        );
        assert!(
            (mh - 0.5 * m1).abs() < 1e-6,
            "lerp should be exactly linear in t"
        );
    }

    fn assert_no_fold(mask: &[u8], w: usize, h: usize, name: &str) {
        let mesh = build_and_solve(mask, w, h).expect("mesh");
        // Identity sanity: zero displacement is the undistorted grid (normalized Jacobian ≡ 1).
        assert!((min_norm_jacobian(&mesh, 0.0) - 1.0).abs() < 1e-9);
        let f = nofold_factor(&mesh);
        assert!(
            f > 0.0 && f <= 1.0,
            "{name}: nofold factor out of range: {f}"
        );
        // (Priority-1 test 4) Every quad keeps a positive Jacobian at the applied factor.
        let jmin = min_norm_jacobian(&mesh, f);
        assert!(
            jmin > 0.0,
            "{name}: a quad folded (min Jacobian {jmin} at factor {f})"
        );
    }

    #[test]
    fn no_fold_on_l_shape_and_barrel() {
        // (Priority-1 test 4) No quad Jacobian goes non-positive at the applied (no-fold-capped) factor.
        assert_no_fold(&l_shape(320, 256), 320, 256, "l_shape");
        assert_no_fold(&barrel(480, 320), 480, 320, "barrel");
    }

    #[test]
    fn warp_is_deterministic() {
        let (w, h) = (480usize, 320usize);
        let mask = barrel(w, h);
        // A cheap deterministic texture so the RGB compare is meaningful.
        let mut rgb = vec![0.0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                if mask[y * w + x] != 0 {
                    let i = (y * w + x) * 3;
                    rgb[i] = ((x * 7 + y * 3) % 251) as f32 / 251.0;
                    rgb[i + 1] = ((x * 5 + y * 11) % 251) as f32 / 251.0;
                    rgb[i + 2] = ((x + y) % 251) as f32 / 251.0;
                }
            }
        }
        let a = rectangle_warp(&rgb, &mask, w, h, 1.0).expect("warp");
        let b = rectangle_warp(&rgb, &mask, w, h, 1.0).expect("warp");
        assert_eq!(a.0, b.0, "rgb not deterministic");
        assert_eq!(a.1, b.1, "mask not deterministic");
    }
}
