//! Panorama-group detection over an ordered set of frames.
//!
//! Where [`register`](crate::register) assumes its input frames belong to *one* panorama and solves a
//! single camera bundle, `detect` answers the prior question: given a whole shoot (many unrelated
//! images in capture order), *which* subsets are stitchable panoramas? It is the library-scan gate in
//! front of the merge flow — the user has not hand-picked anything yet.
//!
//! The method is the geometric-verification + grouping half of Brown & Lowe, "Automatic Panoramic
//! Image Stitching using Invariant Features" (IJCV 2007) — the same probabilistic verification that
//! backs OpenCV's stitcher — reusing this crate's existing feature/matcher/RANSAC front end unchanged:
//!
//! 1. **Features once per frame** ([`features::extract`], parallel) — identical to `register`.
//! 2. **Candidate pairs**: all unordered pairs when the set is small (`n <= max_all_pairs`), else a
//!    sliding capture-time window (`j - i <= time_window`) so the O(n²) verification stays bounded on a
//!    large library. A caller may override with `pairs_hint`.
//! 3. **Pairwise verification** ([`matching::match_descriptors`] → [`ransac::verify_pair`]): every
//!    returned pair already satisfies Brown & Lowe's gate `n_inliers > 8 + 0.3·n_matches` (⟺ OpenCV
//!    `conf > 1.0`), so `conf = n_inliers / (8 + 0.3·n_matches)` is `> 1` for every verified edge.
//! 4. **Edge classification** from the homography geometry (the `conf` gate is redundant post-verify;
//!    overlap + centre-shift are the real classifier): `overlap` is the symmetric mean of the
//!    warped-corner-quad ∩ frame-rect area ratio (Sutherland–Hodgman clip + shoelace area), `shift` is
//!    the normalised centre displacement `‖H·cᵢ − cⱼ‖ / diagⱼ`. A **Pano** edge overlaps partially and
//!    has moved; a **Burst** edge (near-duplicate / HDR bracket) overlaps almost fully without moving;
//!    everything else (including any degenerate homography) is **Weak** and never groups.
//! 5. **Grouping**: union-find over Pano edges only → every connected component of ≥ 2 frames is a
//!    group. Its confidence is the *weakest necessary link* — the minimum edge `conf` on the
//!    component's maximum spanning tree over Pano-edge confidence (the max-ST maximises that
//!    bottleneck), so a group is only as trustworthy as the least-confident overlap it depends on.
//!
//! Determinism: features/matching/RANSAC are seeded by a self-contained SplitMix64 (`pair_index`), and
//! `detect_groups` sorts its edges by `(i, j)` and its groups by first member, so two identical
//! invocations produce byte-identical reports regardless of thread scheduling.

use nalgebra::{Matrix3, Point2, Vector3};
use rayon::prelude::*;

use crate::{features, matching, ransac, Frame};

/// Below this many candidate matches a pair cannot clear Brown & Lowe's `n_inliers >= 15` floor, so we
/// skip RANSAC entirely (mirrors `register`'s cheap pre-filter, at the verification floor).
const MIN_MATCHES: usize = 15;

/// Homogeneous-`w` / area denominators below this are treated as the line at infinity (degenerate
/// homography) — matches the perspective-divide guard in [`ransac`].
const EPS: f64 = 1e-9;

/// Thresholds for pair classification and pairing strategy. Defaults follow the Brown & Lowe
/// panorama-recognition regime; geometry (not these knobs) is the real gate, so they are generous.
#[derive(Clone, Copy, Debug)]
pub struct DetectOptions {
    /// Minimum symmetric overlap for a Pano edge (below this the frames barely touch).
    pub overlap_lo: f64,
    /// Maximum symmetric overlap for a Pano edge (above this it is a near-duplicate, not a pan).
    pub overlap_hi: f64,
    /// Minimum normalised centre shift for a Pano edge (the camera actually moved).
    pub shift_min: f64,
    /// Overlap above which a pair is a burst / near-duplicate (also HDR brackets).
    pub burst_overlap: f64,
    /// Centre shift below which a high-overlap pair is a burst rather than a pan.
    pub burst_shift: f64,
    /// `n <= max_all_pairs` ⇒ verify all unordered pairs; larger sets use the sliding window.
    pub max_all_pairs: usize,
    /// Sliding capture-time window: only verify pairs with `j - i <= time_window` on large sets.
    pub time_window: usize,
}

impl Default for DetectOptions {
    fn default() -> Self {
        Self {
            overlap_lo: 0.10,
            overlap_hi: 0.92,
            shift_min: 0.10,
            burst_overlap: 0.92,
            burst_shift: 0.05,
            max_all_pairs: 12,
            time_window: 6,
        }
    }
}

/// How a verified pair relates geometrically.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EdgeClass {
    /// Partial overlap with real camera movement — a stitchable panorama edge.
    Pano,
    /// Near-total overlap with almost no movement — a burst / bracket near-duplicate (not grouped).
    Burst,
    /// Verified but neither Pano nor Burst (or a degenerate homography). Never groups.
    Weak,
}

/// One verified overlapping pair with its geometric relation. `i < j`; indices are into the input
/// `frames` slice (capture order).
#[derive(Clone, Copy, Debug)]
pub struct VerifiedEdge {
    pub i: usize,
    pub j: usize,
    /// Brown & Lowe confidence `n_inliers / (8 + 0.3·n_matches)` (`> 1` for every verified edge).
    pub conf: f64,
    /// Symmetric overlap fraction in `[0, 1]`.
    pub overlap: f64,
    /// Normalised centre displacement `‖H·cᵢ − cⱼ‖ / diagⱼ`.
    pub shift: f64,
    pub class: EdgeClass,
}

/// A detected panorama: a connected component of Pano edges with ≥ 2 members.
#[derive(Clone, Debug)]
pub struct DetectedGroup {
    /// Member frame indices, ascending (input order = capture order).
    pub members: Vec<usize>,
    /// Weakest-necessary-link confidence: the minimum edge `conf` on the group's maximum spanning tree.
    pub confidence: f64,
}

/// The full detection result.
#[derive(Clone, Debug)]
pub struct DetectReport {
    /// Every verified edge (all classes), sorted by `(i, j)`.
    pub edges: Vec<VerifiedEdge>,
    /// Detected groups, sorted by first member index.
    pub groups: Vec<DetectedGroup>,
}

/// Classify a shoot into panorama groups.
///
/// `frames` are assumed to be in capture order. `pairs_hint`, if `Some`, is the exact list of unordered
/// pairs to verify (indices into `frames`); if `None`, the pair list is chosen from `opts`
/// (all-pairs for small sets, sliding time-window otherwise). `cancel` is polled at the start of each
/// pair; when it returns `true` the remaining verification is skipped and a partial report is returned.
///
/// The returned [`DetectReport`] is deterministic for a given input.
pub fn detect_groups(
    frames: &[Frame],
    pairs_hint: Option<Vec<(usize, usize)>>,
    opts: &DetectOptions,
    cancel: &(dyn Fn() -> bool + Sync),
) -> DetectReport {
    let n = frames.len();
    if n < 2 {
        return DetectReport {
            edges: Vec::new(),
            groups: Vec::new(),
        };
    }

    // --- Features (parallel over frames), exactly as register() ---
    let pattern = features::fixed_test_pairs();
    let feats: Vec<features::FrameFeatures> = frames
        .par_iter()
        .map(|f| features::extract(f, &pattern))
        .collect();

    // --- Candidate pair list ---
    let pair_list: Vec<(usize, usize)> = match pairs_hint {
        Some(p) => p,
        None if n <= opts.max_all_pairs => (0..n)
            .flat_map(|i| (i + 1..n).map(move |j| (i, j)))
            .collect(),
        None => (0..n)
            .flat_map(|i| {
                (i + 1..n)
                    .filter(move |&j| j - i <= opts.time_window)
                    .map(move |j| (i, j))
            })
            .collect(),
    };

    // --- Pairwise match + RANSAC + classification (parallel over pairs) ---
    let mut edges: Vec<VerifiedEdge> = pair_list
        .par_iter()
        .filter_map(|&(i, j)| {
            if cancel() {
                return None;
            }
            let matches = matching::match_descriptors(&feats[i].descriptors, &feats[j].descriptors);
            if matches.len() < MIN_MATCHES {
                return None;
            }
            let corr: Vec<(Point2<f64>, Point2<f64>)> = matches
                .iter()
                .map(|&(a, b)| {
                    let (px, py) = feats[i].points[a];
                    let (qx, qy) = feats[j].points[b];
                    (Point2::new(px, py), Point2::new(qx, qy))
                })
                .collect();
            let reg_scale = feats[i].reg_scale.min(feats[j].reg_scale);
            let pair_index = (i * n + j) as u64;
            let v = ransac::verify_pair(&corr, reg_scale, pair_index)?;

            let conf = v.n_inliers as f64 / (8.0 + 0.3 * corr.len() as f64);
            let (wi, hi) = (frames[i].width as f64, frames[i].height as f64);
            let (wj, hj) = (frames[j].width as f64, frames[j].height as f64);

            // A degenerate homography (line at infinity / non-invertible) makes no pano/burst claim.
            let overlap = overlap_symmetric(&v.h, wi, hi, wj, hj);
            let shift = shift_ratio(&v.h, wi, hi, wj, hj);
            let (overlap, shift, class) = match (overlap, shift) {
                (Some(o), Some(s)) => {
                    let class = if (opts.overlap_lo..=opts.overlap_hi).contains(&o)
                        && s >= opts.shift_min
                    {
                        EdgeClass::Pano
                    } else if o > opts.burst_overlap && s < opts.burst_shift {
                        EdgeClass::Burst
                    } else {
                        EdgeClass::Weak
                    };
                    (o, s, class)
                }
                (o, s) => (o.unwrap_or(0.0), s.unwrap_or(0.0), EdgeClass::Weak),
            };

            Some(VerifiedEdge {
                i,
                j,
                conf,
                overlap,
                shift,
                class,
            })
        })
        .collect();

    // Determinism: canonical edge order independent of pairing strategy / thread scheduling.
    edges.sort_by(|a, b| a.i.cmp(&b.i).then(a.j.cmp(&b.j)));

    let groups = build_groups(n, &edges);

    DetectReport { edges, groups }
}

/// Union-find over Pano edges → components of ≥ 2 → confidence via max-ST bottleneck.
fn build_groups(n: usize, edges: &[VerifiedEdge]) -> Vec<DetectedGroup> {
    let mut uf = UnionFind::new(n);
    for e in edges {
        if e.class == EdgeClass::Pano {
            uf.union(e.i, e.j);
        }
    }

    // Bucket frames by component root (only Pano edges ever union, so any ≥ 2 bucket is Pano-connected).
    use std::collections::HashMap;
    let mut buckets: HashMap<usize, Vec<usize>> = HashMap::new();
    for v in 0..n {
        buckets.entry(uf.find(v)).or_default().push(v);
    }

    let mut groups: Vec<DetectedGroup> = Vec::new();
    for members in buckets.values() {
        if members.len() < 2 {
            continue;
        }
        let mut m = members.clone();
        m.sort_unstable();
        let confidence = group_confidence(&m, edges, n);
        groups.push(DetectedGroup {
            members: m,
            confidence,
        });
    }
    groups.sort_by_key(|g| g.members[0]);
    groups
}

/// Minimum edge `conf` on the maximum spanning tree of the group's Pano edges (weakest necessary link).
/// The max-ST maximises the bottleneck, so this is the best achievable weakest link. For a two-member
/// group it is simply that single edge's `conf`. `members` must be sorted ascending.
fn group_confidence(members: &[usize], edges: &[VerifiedEdge], n: usize) -> f64 {
    let in_group = |x: usize| members.binary_search(&x).is_ok();
    let mut es: Vec<&VerifiedEdge> = edges
        .iter()
        .filter(|e| e.class == EdgeClass::Pano && in_group(e.i) && in_group(e.j))
        .collect();
    // Kruskal, heaviest first; ties broken by (i, j) for determinism.
    es.sort_by(|a, b| {
        b.conf
            .partial_cmp(&a.conf)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.i.cmp(&b.i))
            .then(a.j.cmp(&b.j))
    });

    let mut uf = UnionFind::new(n);
    let mut min_conf = f64::INFINITY;
    let mut added = 0usize;
    for e in es {
        if uf.find(e.i) != uf.find(e.j) {
            uf.union(e.i, e.j);
            min_conf = min_conf.min(e.conf);
            added += 1;
            if added == members.len() - 1 {
                break;
            }
        }
    }
    min_conf
}

/// Minimal union-find (private to `detect`; `graph.rs`'s tree keeps only the largest component, so it
/// is not reusable here where every component is a candidate group).
struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        UnionFind {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }
    fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        let mut cur = x;
        while self.parent[cur] != root {
            let next = self.parent[cur];
            self.parent[cur] = root;
            cur = next;
        }
        root
    }
    fn union(&mut self, x: usize, y: usize) {
        let (rx, ry) = (self.find(x), self.find(y));
        if rx == ry {
            return;
        }
        match self.rank[rx].cmp(&self.rank[ry]) {
            std::cmp::Ordering::Less => self.parent[rx] = ry,
            std::cmp::Ordering::Greater => self.parent[ry] = rx,
            std::cmp::Ordering::Equal => {
                self.parent[ry] = rx;
                self.rank[rx] += 1;
            }
        }
    }
}

// --- Geometry helpers (pure; unit-tested below) ---------------------------------------------------

/// Map the corners `(0,0),(w,0),(w,hh),(0,hh)` of a `w×hh` frame through `h` with perspective divide.
/// Returns `None` if any corner maps to the line at infinity (`|w| < EPS`), i.e. `h` is degenerate.
fn warp_quad(h: &Matrix3<f64>, w: f64, hh: f64) -> Option<[Point2<f64>; 4]> {
    let corners = [(0.0, 0.0), (w, 0.0), (w, hh), (0.0, hh)];
    let mut out = [Point2::new(0.0, 0.0); 4];
    for (k, &(cx, cy)) in corners.iter().enumerate() {
        let v = h * Vector3::new(cx, cy, 1.0);
        if v.z.abs() < EPS {
            return None;
        }
        out[k] = Point2::new(v.x / v.z, v.y / v.z);
    }
    Some(out)
}

/// Sutherland–Hodgman clip of `poly` against the axis-aligned rectangle `[0,w] × [0,h]`. The input is a
/// convex quad (the image of a rectangle under a homography), for which SH against a convex window is
/// exact. Returns the clipped polygon vertices (possibly empty if there is no overlap).
fn clip_poly_rect(poly: &[Point2<f64>], w: f64, h: f64) -> Vec<Point2<f64>> {
    let p = clip_half(poly, |v| v.x >= 0.0, |a, b| intersect_x(a, b, 0.0));
    let p = clip_half(&p, |v| v.x <= w, |a, b| intersect_x(a, b, w));
    let p = clip_half(&p, |v| v.y >= 0.0, |a, b| intersect_y(a, b, 0.0));
    clip_half(&p, |v| v.y <= h, |a, b| intersect_y(a, b, h))
}

/// One Sutherland–Hodgman pass against a single half-plane defined by `inside`, using `intersect` for
/// the crossing point between an inside and an outside vertex.
fn clip_half<F, G>(poly: &[Point2<f64>], inside: F, intersect: G) -> Vec<Point2<f64>>
where
    F: Fn(&Point2<f64>) -> bool,
    G: Fn(&Point2<f64>, &Point2<f64>) -> Point2<f64>,
{
    let n = poly.len();
    let mut out = Vec::with_capacity(n + 1);
    if n == 0 {
        return out;
    }
    for i in 0..n {
        let cur = poly[i];
        let prev = poly[(i + n - 1) % n];
        let cur_in = inside(&cur);
        let prev_in = inside(&prev);
        if cur_in {
            if !prev_in {
                out.push(intersect(&prev, &cur));
            }
            out.push(cur);
        } else if prev_in {
            out.push(intersect(&prev, &cur));
        }
    }
    out
}

/// Crossing point of segment `a→b` with the vertical line `x = xc` (callers guarantee `a`,`b` straddle
/// it, so `b.x != a.x`).
fn intersect_x(a: &Point2<f64>, b: &Point2<f64>, xc: f64) -> Point2<f64> {
    let t = (xc - a.x) / (b.x - a.x);
    Point2::new(xc, a.y + t * (b.y - a.y))
}

/// Crossing point of segment `a→b` with the horizontal line `y = yc`.
fn intersect_y(a: &Point2<f64>, b: &Point2<f64>, yc: f64) -> Point2<f64> {
    let t = (yc - a.y) / (b.y - a.y);
    Point2::new(a.x + t * (b.x - a.x), yc)
}

/// Shoelace area of a polygon (absolute value; 0 for fewer than 3 vertices).
fn poly_area(poly: &[Point2<f64>]) -> f64 {
    let n = poly.len();
    if n < 3 {
        return 0.0;
    }
    let mut acc = 0.0;
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        acc += a.x * b.y - b.x * a.y;
    }
    acc.abs() * 0.5
}

/// Symmetric overlap fraction of frame `i` (`wi×hi`) and frame `j` (`wj×hj`) under homography `h`
/// (`i → j`): the mean of each frame's warped-quad ∩ other-frame-rect area, normalised by the target
/// frame area. Returns `None` if `h` is non-invertible or degenerate on either direction.
fn overlap_symmetric(h: &Matrix3<f64>, wi: f64, hi: f64, wj: f64, hj: f64) -> Option<f64> {
    let hinv = h.try_inverse()?;
    let quad_ij = warp_quad(h, wi, hi)?; // frame i corners in frame j space
    let area_ij = poly_area(&clip_poly_rect(&quad_ij, wj, hj));
    let quad_ji = warp_quad(&hinv, wj, hj)?; // frame j corners in frame i space
    let area_ji = poly_area(&clip_poly_rect(&quad_ji, wi, hi));
    Some(0.5 * (area_ij / (wj * hj) + area_ji / (wi * hi)))
}

/// Normalised centre displacement: `‖H·cᵢ − cⱼ‖ / hypot(wj, hj)`, with `cᵢ`,`cⱼ` the frame centres.
/// Returns `None` if `H·cᵢ` maps to the line at infinity.
fn shift_ratio(h: &Matrix3<f64>, wi: f64, hi: f64, wj: f64, hj: f64) -> Option<f64> {
    let v = h * Vector3::new(wi * 0.5, hi * 0.5, 1.0);
    if v.z.abs() < EPS {
        return None;
    }
    let mapped = Point2::new(v.x / v.z, v.y / v.z);
    let (cx, cy) = (wj * 0.5, hj * 0.5);
    let d = ((mapped.x - cx).powi(2) + (mapped.y - cy).powi(2)).sqrt();
    Some(d / (wj * wj + hj * hj).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f64, y: f64) -> Point2<f64> {
        Point2::new(x, y)
    }

    #[test]
    fn poly_area_unit_square() {
        let sq = [pt(0.0, 0.0), pt(2.0, 0.0), pt(2.0, 3.0), pt(0.0, 3.0)];
        assert!((poly_area(&sq) - 6.0).abs() < 1e-9);
        // Winding direction must not matter (shoelace is taken in absolute value).
        let rev = [pt(0.0, 0.0), pt(0.0, 3.0), pt(2.0, 3.0), pt(2.0, 0.0)];
        assert!((poly_area(&rev) - 6.0).abs() < 1e-9);
        // Degenerate polygons have no area.
        assert!(poly_area(&[pt(0.0, 0.0), pt(1.0, 1.0)]) < 1e-12);
    }

    #[test]
    fn clip_identity_keeps_interior_rect() {
        // A quad fully inside [0,10]x[0,10] is returned unchanged in area.
        let quad = [pt(2.0, 2.0), pt(6.0, 2.0), pt(6.0, 8.0), pt(2.0, 8.0)];
        let clipped = clip_poly_rect(&quad, 10.0, 10.0);
        assert!((poly_area(&clipped) - 24.0).abs() < 1e-9);
    }

    #[test]
    fn clip_half_overlap_rect() {
        // A 4-wide quad spanning x in [-2, 2], y in [0, 4] against [0,10]x[0,10]:
        // clipped to x in [0, 2] -> area 2*4 = 8 (hand-computed).
        let quad = [pt(-2.0, 0.0), pt(2.0, 0.0), pt(2.0, 4.0), pt(-2.0, 4.0)];
        let clipped = clip_poly_rect(&quad, 10.0, 10.0);
        assert!(
            (poly_area(&clipped) - 8.0).abs() < 1e-9,
            "area = {}",
            poly_area(&clipped)
        );
    }

    #[test]
    fn clip_no_overlap_is_empty() {
        let quad = [
            pt(20.0, 20.0),
            pt(30.0, 20.0),
            pt(30.0, 30.0),
            pt(20.0, 30.0),
        ];
        let clipped = clip_poly_rect(&quad, 10.0, 10.0);
        assert!(clipped.is_empty());
        assert!(poly_area(&clipped) < 1e-12);
    }

    #[test]
    fn shift_ratio_identity_is_zero() {
        let h = Matrix3::identity();
        let s = shift_ratio(&h, 640.0, 480.0, 640.0, 480.0).unwrap();
        assert!(s.abs() < 1e-12, "identity shift = {s}");
    }

    #[test]
    fn shift_ratio_pure_translation() {
        // Translate the centre by (tx, ty); shift = ‖(tx,ty)‖ / diag.
        let (w, h) = (640.0, 480.0);
        let (tx, ty) = (100.0, 50.0);
        let hmat = Matrix3::new(1.0, 0.0, tx, 0.0, 1.0, ty, 0.0, 0.0, 1.0);
        let s = shift_ratio(&hmat, w, h, w, h).unwrap();
        let expect = (tx * tx + ty * ty).sqrt() / (w * w + h * h).sqrt();
        assert!((s - expect).abs() < 1e-12, "shift {s} vs {expect}");
    }

    #[test]
    fn overlap_symmetric_half_x_shift() {
        // Two identical WxH frames; H shifts frame i by +W/2 in x. The overlap region is exactly half
        // of each frame in both directions, so the symmetric mean is 0.5.
        let (w, h) = (600.0, 400.0);
        let hmat = Matrix3::new(1.0, 0.0, w * 0.5, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0);
        let o = overlap_symmetric(&hmat, w, h, w, h).unwrap();
        assert!((o - 0.5).abs() < 1e-9, "overlap = {o}");
    }

    #[test]
    fn overlap_symmetric_identity_is_full() {
        let (w, h) = (600.0, 400.0);
        let o = overlap_symmetric(&Matrix3::identity(), w, h, w, h).unwrap();
        assert!((o - 1.0).abs() < 1e-9, "overlap = {o}");
    }

    #[test]
    fn degenerate_homography_yields_none() {
        // A rank-deficient (non-invertible) matrix has no symmetric overlap.
        let singular = Matrix3::new(1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0);
        assert!(overlap_symmetric(&singular, 100.0, 100.0, 100.0, 100.0).is_none());
        assert!(shift_ratio(&singular, 100.0, 100.0, 100.0, 100.0).is_none());
    }

    #[test]
    fn warp_quad_translation_moves_corners() {
        let hmat = Matrix3::new(1.0, 0.0, 10.0, 0.0, 1.0, 20.0, 0.0, 0.0, 1.0);
        let q = warp_quad(&hmat, 100.0, 50.0).unwrap();
        assert!((q[0].x - 10.0).abs() < 1e-9 && (q[0].y - 20.0).abs() < 1e-9);
        assert!((q[2].x - 110.0).abs() < 1e-9 && (q[2].y - 70.0).abs() < 1e-9);
    }
}
