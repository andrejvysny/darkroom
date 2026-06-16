//! CPU tone-curve evaluation → 256-entry RGBA8 LUT for the GPU.
//!
//! Uses monotone cubic Hermite (Fritsch–Carlson) interpolation so curves never overshoot
//! their control points (no haloing/clipping artifacts). Empty / single-point channels are
//! treated as identity. The master (`rgb`) curve is composed first, then the per-channel curve.

use crate::params::{CurvePoint, ToneCurve};

/// LUT resolution. 256 entries == one per 8-bit output level; the shader lerps between entries.
pub const LUT_SIZE: usize = 256;

/// A channel is identity when it has fewer than two control points, or its points already
/// describe the line y=x (within a small tolerance).
pub fn is_identity(points: &[CurvePoint]) -> bool {
    if points.len() < 2 {
        return true;
    }
    points.iter().all(|p| (p.y - p.x).abs() < 1e-4)
}

/// Sort by x, clamp to [0,1], and drop points with (near-)duplicate x to keep secants finite.
fn clean(points: &[CurvePoint]) -> Vec<(f32, f32)> {
    let mut p: Vec<(f32, f32)> = points
        .iter()
        .map(|c| (c.x.clamp(0.0, 1.0), c.y.clamp(0.0, 1.0)))
        .collect();
    p.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    p.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-6);
    p
}

/// Evaluate a monotone cubic spline through `pts` (already cleaned/sorted) at `x` in [0,1].
/// Flat extrapolation outside the control-point range. Returns a value clamped to [0,1].
fn eval(pts: &[(f32, f32)], x: f32) -> f32 {
    let n = pts.len();
    if n == 0 {
        return x.clamp(0.0, 1.0);
    }
    if n == 1 {
        return pts[0].1;
    }
    if x <= pts[0].0 {
        return pts[0].1;
    }
    if x >= pts[n - 1].0 {
        return pts[n - 1].1;
    }

    // Secant slopes.
    let d: Vec<f32> = (0..n - 1)
        .map(|i| (pts[i + 1].1 - pts[i].1) / (pts[i + 1].0 - pts[i].0))
        .collect();

    // Initial tangents (average of adjacent secants; endpoints use the one-sided secant).
    let mut m = vec![0f32; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        m[i] = (d[i - 1] + d[i]) * 0.5;
    }

    // Fritsch–Carlson monotonicity clamp.
    for i in 0..n - 1 {
        if d[i].abs() < 1e-9 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
        } else {
            let a = m[i] / d[i];
            let b = m[i + 1] / d[i];
            let s = a * a + b * b;
            if s > 9.0 {
                let t = 3.0 / s.sqrt();
                m[i] = t * a * d[i];
                m[i + 1] = t * b * d[i];
            }
        }
    }

    // Locate the interval and evaluate the Hermite basis.
    let mut k = 0;
    while k < n - 1 && x > pts[k + 1].0 {
        k += 1;
    }
    let h = pts[k + 1].0 - pts[k].0;
    let t = (x - pts[k].0) / h;
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    (h00 * pts[k].1 + h10 * h * m[k] + h01 * pts[k + 1].1 + h11 * h * m[k + 1]).clamp(0.0, 1.0)
}

/// Build a `LUT_SIZE`-entry RGBA8 LUT. For input index `i`, the output for channel `c` is
/// `channel_c(master(i/255))`. Alpha is unused (set to 255). Returns `LUT_SIZE*4` bytes.
pub fn build_lut(tc: &ToneCurve) -> Vec<u8> {
    let master = clean(&tc.rgb);
    let cr = clean(&tc.r);
    let cg = clean(&tc.g);
    let cb = clean(&tc.b);

    let q = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let mut lut = vec![0u8; LUT_SIZE * 4];
    for i in 0..LUT_SIZE {
        let x = i as f32 / (LUT_SIZE as f32 - 1.0);
        let m = eval(&master, x);
        lut[i * 4] = q(eval(&cr, m));
        lut[i * 4 + 1] = q(eval(&cg, m));
        lut[i * 4 + 2] = q(eval(&cb, m));
        lut[i * 4 + 3] = 255;
    }
    lut
}

/// The LUT for an identity (no-op) curve — a straight ramp. Used as the initial upload.
pub fn identity_lut() -> Vec<u8> {
    build_lut(&ToneCurve::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f32, y: f32) -> CurvePoint {
        CurvePoint { x, y }
    }

    #[test]
    fn identity_is_a_straight_ramp() {
        let lut = identity_lut();
        for i in 0..LUT_SIZE {
            assert_eq!(lut[i * 4], i as u8, "identity LUT must map i -> i");
        }
    }

    #[test]
    fn empty_curve_is_identity() {
        assert!(ToneCurve::default().is_identity());
        assert_eq!(eval(&clean(&[]), 0.3), 0.3);
    }

    #[test]
    fn brightening_curve_lifts_midtones() {
        // A curve that pushes mid-gray (0.5) up to 0.7.
        let tc = ToneCurve {
            rgb: vec![pt(0.0, 0.0), pt(0.5, 0.7), pt(1.0, 1.0)],
            ..Default::default()
        };
        let lut = build_lut(&tc);
        let mid = lut[128 * 4]; // input ~0.5
        assert!(mid > 150, "mid-gray should lift well above 128, got {mid}");
    }

    #[test]
    fn monotone_no_overshoot() {
        // Steep then flat — monotone interpolation must never exceed [0,1] or go non-monotone.
        let tc = ToneCurve {
            rgb: vec![pt(0.0, 0.0), pt(0.2, 0.8), pt(0.8, 0.85), pt(1.0, 1.0)],
            ..Default::default()
        };
        let lut = build_lut(&tc);
        let mut prev = 0u8;
        for i in 0..LUT_SIZE {
            let v = lut[i * 4];
            assert!(
                v >= prev,
                "LUT must be non-decreasing at {i}: {prev} -> {v}"
            );
            prev = v;
        }
    }

    #[test]
    fn per_channel_after_master() {
        // Master identity, red channel lifts everything.
        let tc = ToneCurve {
            r: vec![pt(0.0, 0.2), pt(1.0, 1.0)],
            ..Default::default()
        };
        let lut = build_lut(&tc);
        assert!(
            lut[0] > 0,
            "red at input 0 should be lifted by per-channel curve"
        );
        assert_eq!(lut[1], 0, "green untouched at input 0");
    }
}
