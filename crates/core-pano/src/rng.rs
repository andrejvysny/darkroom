//! Self-contained deterministic RNG (SplitMix64).
//!
//! WHY not `rand`: imageproc pulls in `rand 0.8`, but panorama determinism must never depend on a
//! transitive rand version bump (a different `StdRng` stream would silently change every feature
//! descriptor and every RANSAC sample). SplitMix64 is a tiny, fully specified generator, so the
//! fixed BRIEF test-pair pattern (seed `0x5EED`) and per-pair RANSAC sampling
//! (seed `0xC0FFEE + pair_index`) are reproducible forever, on any platform, with no external dep.

/// SplitMix64 — Vigna's public-domain finalizer. Deterministic given the seed.
#[derive(Clone)]
pub(crate) struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub(crate) fn new(seed: u64) -> Self {
        SplitMix64 { state: seed }
    }

    #[inline]
    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)` with 53 bits of mantissa.
    #[inline]
    pub(crate) fn next_f64(&mut self) -> f64 {
        // Top 53 bits -> [0,1), the standard construction.
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Uniform integer in `[0, n)` (rejection-free modulo; bias negligible for our small `n`).
    #[inline]
    pub(crate) fn gen_range(&mut self, n: usize) -> usize {
        debug_assert!(n > 0);
        (self.next_u64() % n as u64) as usize
    }

    /// Standard-normal sample via Box–Muller (returns one of the pair; adequate here).
    #[inline]
    pub(crate) fn next_gaussian(&mut self) -> f64 {
        // Guard u1 away from 0 so ln() stays finite.
        let u1 = (self.next_f64()).max(1e-12);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}
