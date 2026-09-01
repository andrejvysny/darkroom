//! Deterministic fixtures shared by the unit tests. No `rand` dependency: a fixed LCG keeps every
//! test byte-reproducible across machines and runs (a flaky numeric test is worse than no test).

use crate::features::HandFeatures;
use crate::sample::{LabelProvenance, Sample};

/// Numerical-Recipes 64-bit LCG, top bits only (the low bits of an LCG are famously non-random).
pub(crate) struct Lcg(u64);

impl Lcg {
    pub(crate) fn new(seed: u64) -> Self {
        Lcg(seed.wrapping_mul(2_862_933_555_777_941_757).wrapping_add(1))
    }

    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    /// Uniform in `[0, 1]`.
    pub(crate) fn unit(&mut self) -> f32 {
        self.next_u32() as f32 / (u32::MAX >> 1) as f32
    }

    /// Uniform in `[-1, 1]`.
    pub(crate) fn sym(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }
}

pub(crate) fn sample_x(x: Vec<f32>, y: bool, provenance: LabelProvenance, group: u64) -> Sample {
    Sample {
        x,
        y,
        provenance,
        group,
    }
}

pub(crate) fn sample(y: bool, provenance: LabelProvenance, group: u64) -> Sample {
    sample_x(vec![0.0, 0.0], y, provenance, group)
}

/// `n_groups` bursts of 3 (one pick, two rejects), linearly separable on `signal_col`.
///
/// Noise is confined to the first 8 columns; every other column is constant 0, which standardizes to
/// exactly 0 (std floor) and therefore contributes nothing. That keeps the effective dimensionality —
/// and so the amount of overfitting — independent of `d`, while still exercising the full-width
/// assemble/impute/fold plumbing at `d = DIM`.
pub(crate) fn burst_samples(
    rng: &mut Lcg,
    n_groups: u64,
    d: usize,
    signal_col: usize,
) -> Vec<Sample> {
    let mut out = Vec::new();
    for g in 0..n_groups {
        for i in 0..3 {
            let mut x = vec![0f32; d];
            for v in x.iter_mut().take(8.min(d)) {
                *v = rng.sym() * 0.4;
            }
            let pick = i == 0;
            x[signal_col] = if pick { 1.0 } else { -1.0 } + rng.sym() * 0.15;
            out.push(sample_x(x, pick, LabelProvenance::Unprompted, g));
        }
    }
    out
}

/// Every hand feature present (no NaNs) — the baseline for imputation tests.
pub(crate) fn full_hand() -> HandFeatures {
    HandFeatures {
        sharpness_log: 0.42,
        clip_hi: 0.01,
        clip_lo: 0.02,
        dynamic_range_ev: 9.5,
        mean_log_luma: -2.3,
        face_count: 2.0,
        face_max_det: 0.93,
        face_max_quality: 0.71,
        has_face: 1.0,
        log_iso: 6.4,
        log_shutter: -5.0,
        aperture: 2.8,
        focal: 85.0,
        rank_sharpness: 0.8,
        rank_face_quality: 0.6,
        rank_iso: 0.5,
    }
}
