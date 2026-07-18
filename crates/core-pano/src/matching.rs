//! Descriptor matching: brute-force Hamming k=2 nearest neighbours in both directions, Lowe's ratio
//! test, and a mutual-best (cross-check) filter.
//!
//! WHY brute force (not imageproc's `match_binary_descriptors`): imageproc matches via LSH, which is
//! approximate and seed-dependent. At ≤1500 descriptors/frame an exact O(n·m) scan is cheap
//! (parallelised over queries with rayon) and fully deterministic, and it lets us apply the exact
//! Lowe-ratio + mutual-best gate that panorama registration relies on to keep the correspondence set
//! clean before RANSAC.

use rayon::prelude::*;

use crate::features::Descriptor;

/// Lowe's ratio: keep a match only if the best distance is clearly better than the second best.
const LOWE_RATIO: f32 = 0.8;

/// Find the nearest and second-nearest `train` descriptor for `query` by Hamming distance.
/// Returns `(best_index, best_distance, second_distance)`.
fn best_two(query: &Descriptor, train: &[Descriptor]) -> (usize, u32, u32) {
    let mut best = u32::MAX;
    let mut second = u32::MAX;
    let mut best_idx = 0usize;
    for (idx, t) in train.iter().enumerate() {
        let d = query.hamming(t);
        if d < best {
            second = best;
            best = d;
            best_idx = idx;
        } else if d < second {
            second = d;
        }
    }
    (best_idx, best, second)
}

/// For every descriptor in `from`, its ratio-passing best index in `to` (or `None`).
fn directional(from: &[Descriptor], to: &[Descriptor]) -> Vec<Option<usize>> {
    if to.len() < 2 {
        return vec![None; from.len()];
    }
    from.par_iter()
        .map(|q| {
            let (idx, best, second) = best_two(q, to);
            // second == u32::MAX only if to.len() < 2, already handled; ratio guards ambiguous matches.
            if (best as f32) < LOWE_RATIO * second as f32 {
                Some(idx)
            } else {
                None
            }
        })
        .collect()
}

/// Return mutually-consistent, ratio-passing matches as `(i_index, j_index)` pairs into the two
/// descriptor arrays.
pub(crate) fn match_descriptors(di: &[Descriptor], dj: &[Descriptor]) -> Vec<(usize, usize)> {
    let fwd = directional(di, dj); // i -> j
    let bwd = directional(dj, di); // j -> i
    let mut matches = Vec::new();
    for (i, &fj) in fwd.iter().enumerate() {
        if let Some(j) = fj {
            if bwd[j] == Some(i) {
                matches.push((i, j));
            }
        }
    }
    matches
}
