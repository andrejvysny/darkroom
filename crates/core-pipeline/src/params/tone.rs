// --- Scene-referred base tone operator (ACR-matched) ---------------------------------------------
// Replaces the old fixed `exp()` highlight shoulder. A monotone curve maps scene-linear ProPhoto
// [0,∞) → display-linear [0,1), applied per-channel before the ProPhoto→sRGB matrix (Codex review:
// per-channel on Rec.709 is unsafe for wide-gamut colors). Sampled into a LUT over the log-exposure
// domain (so headroom is covered) and uploaded to the GPU.
//
// The `amount=1` shape (default) is now FIT to Adobe Camera Raw's real default tone response
// (`base_curve_ref::ADOBE_DEFAULT_TONE_CURVE`, the universal Adobe-default curve the R7's
// Adobe-Standard profile renders through — it carries no embedded ProfileToneCurve). It maps
// mid-grey 0.18 → ~0.388 display-linear (≈65% sRGB), the canonical ACR mid-grey, so unedited imports
// match the Lightroom default look (paired with the scene-linear `baseline_gain`, see DevelopParams).
// `tone_amount` interpolates a flat low-contrast neutral (amount=0, mid-grey→0.18) → this ACR fit.
//
// Above the reference domain (scene-linear x > X_JOIN) we keep highlight headroom with a
// tangent-continuous asymptotic shoulder (Codex: `1−k/(x+k)` cannot pass through (1,1); anchor the
// asymptote-to-1 tail at a point where y<1 instead). The shoulder is C¹ at the joint and rolls off
// smoothly to 1.0 instead of hard-clipping at x=1 (avoids a slope-corner → highlight banding).

/// Base-curve LUT length (entries over the log-exposure domain). Matches `BASE_LUT_N` in develop.wgsl.
pub const BASE_LUT_SIZE: usize = 512;
/// Log-exposure domain (stops relative to mid-grey) the LUT spans. ~21 stops covers deep shadows to
/// far highlight headroom; all golden test inputs (x ≤ 8) map inside it.
pub(super) const BASE_U_MIN: f32 = -13.0;
pub(super) const BASE_U_MAX: f32 = 8.0;
/// Scene-linear mid-grey anchor.
const MID_GREY: f32 = 0.18;
/// Hue-protection strength: how strongly saturated highlights are pulled toward the (hue-preserving)
/// luminance-ratio variant vs the per-channel curve. Auto (not user-controlled) for now.
pub(super) const BASE_HUE_PROTECT: f32 = 0.6;

/// Scene-linear exposure-normalization gain applied once (before color-balance / develop / the base
/// tone curve) so a correctly-exposed mid-grey reaches the ACR curve's 0.18 input → renders at ACR
/// brightness. Our `develop_linear` buffer is already white-normalized (white≈1.0, like ACR), so the
/// 0.18→0.388 curve carries the brightness match and this defaults to a no-op **1.0**. It is the
/// single visual-QA knob to make unedited imports brighter/darker to taste vs Lightroom. Rides the
/// otherwise-unused `ExtraUniform.texel.z` (no new GPU binding).
pub const BASELINE_GAIN: f32 = 1.0;

/// Lens-correction slider→coefficient scales (the single tuning knob for how strong the modules feel
/// at full ±100). `K1`/`K2` scale the radial displacement (`r2`, `r2²`); `CA` is the per-channel
/// radial magnitude for lateral chromatic-aberration correction.
pub const LENS_K1_SCALE: f32 = 0.15;
pub const LENS_K2_SCALE: f32 = 0.05;
pub const LENS_CA_SCALE: f32 = 0.02;

// Highlight-shoulder joint: above this scene-linear input we leave the reference table and follow an
// asymptotic shoulder. (X0,Y0) is the Adobe reference value there; A makes the shoulder C¹ (its slope
// at X0 equals the reference's central-difference slope 0.2406, so A = 0.2406/(1−Y0)).
const TONE_X_JOIN: f32 = 0.875;
const TONE_Y_JOIN: f32 = 0.97702;
const TONE_SHOULDER_A: f32 = 0.2406 / (1.0 - TONE_Y_JOIN); // ≈ 10.47

/// The ACR-default tone curve `f_acr(x)` for one scalar channel (scene-linear → display-linear).
/// Reference table on `[0, X_JOIN]`, asymptotic headroom shoulder above. Monotone; f(0)=0;
/// f(0.18)≈0.388; asymptotes to 1.0 as x→∞ (never hard-clips).
fn acr_curve(x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    if x <= TONE_X_JOIN {
        crate::base_curve_ref::adobe_default_curve(x)
    } else {
        1.0 - (1.0 - TONE_Y_JOIN) / (1.0 + TONE_SHOULDER_A * (x - TONE_X_JOIN))
    }
}

/// The base tone operator value `f(x)` for one scalar channel, `amount` in [0,1].
/// `amount=0` is a flat low-contrast neutral (Reinhard, mid-grey→0.18); `amount=1` is the ACR fit
/// (`acr_curve`, mid-grey→0.388). Monotone, `f(0)=0`, asymptotes to 1.
pub fn base_curve_value(x: f32, amount: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    // Neutral soft-clip (Reinhard anchored at 0.18): f0(0.18) = 0.18/(0.18+0.82) = 0.18.
    let f0 = x / (x + (1.0 - MID_GREY)); // = x / (x + 0.82)
    let f1 = acr_curve(x);
    let y = f0 + (f1 - f0) * amount.clamp(0.0, 1.0);
    y.max(0.0)
}

/// Sample the base tone operator into a LUT over the log-exposure domain (`BASE_U_MIN..BASE_U_MAX`).
pub fn build_base_curve_lut(amount: f32) -> Vec<f32> {
    let n = BASE_LUT_SIZE;
    let mut lut = vec![0f32; n];
    for (i, v) in lut.iter_mut().enumerate() {
        let u = BASE_U_MIN + (i as f32 / (n - 1) as f32) * (BASE_U_MAX - BASE_U_MIN);
        let x = MID_GREY * 2f32.powf(u);
        *v = base_curve_value(x, amount);
    }
    lut
}

#[cfg(test)]
mod acr_fit_tests {
    use super::*;
    use crate::base_curve_ref::ADOBE_DEFAULT_TONE_CURVE;

    // Mirror develop.wgsl::base_curve_lookup so the test exercises the FULL path that the GPU does:
    // build the LUT → resample it over the log-exposure domain. Catches LUT-resolution / log-domain
    // / build bugs, not just the analytic `base_curve_value`.
    fn lut_lookup(lut: &[f32], x: f32) -> f32 {
        if x <= 0.0 {
            return 0.0;
        }
        let u = (x / MID_GREY).log2();
        let t = ((u - BASE_U_MIN) / (BASE_U_MAX - BASE_U_MIN)).clamp(0.0, 1.0)
            * (BASE_LUT_SIZE - 1) as f32;
        let i0 = t.floor() as usize;
        let i1 = (i0 + 1).min(BASE_LUT_SIZE - 1);
        let f = t - i0 as f32;
        lut[i0] * (1.0 - f) + lut[i1] * f
    }

    // CIE L* from display-linear Y (Y already in [0,1]).
    fn lstar(y: f32) -> f32 {
        let y = y.max(0.0);
        if y > 0.008856 {
            116.0 * y.cbrt() - 16.0
        } else {
            903.3 * y
        }
    }

    /// The default (amount=1) base curve, resampled through the real LUT path, must reproduce the
    /// Adobe Camera Raw default tone response within RMS L* < 2.0 ("matches ACR"). The 16 control
    /// points are the verified Adobe-default samples (RawTherapee `adobe_camera_raw_default_curve`).
    #[test]
    fn default_curve_matches_acr_reference() {
        let lut = build_base_curve_lut(1.0);
        // (scene-linear input, expected display-linear output) — Adobe default curve.
        let refpts: [(f32, f32); 16] = [
            (0.0, 0.0),
            (0.000977, 0.000780),
            (0.003906, 0.003140),
            (0.007812, 0.006230),
            (0.015625, 0.014850),
            (0.031250, 0.040020),
            (0.0625, 0.104330),
            (0.125, 0.259610),
            (0.18, 0.388086),
            (0.25, 0.520690),
            (0.375, 0.690350),
            (0.5, 0.804860),
            (0.625, 0.884530),
            (0.75, 0.939860),
            (0.875, 0.977020),
            (1.0, 1.0),
        ];
        let mut sum_sq = 0.0f32;
        let mut max_d = 0.0f32;
        for (x, y_ref) in refpts {
            let got = lut_lookup(&lut, x);
            let d = (lstar(got) - lstar(y_ref)).abs();
            max_d = max_d.max(d);
            sum_sq += d * d;
        }
        let rms = (sum_sq / refpts.len() as f32).sqrt();
        assert!(rms < 2.0, "RMS L* = {rms} (want < 2.0)");
        // The only intentional deviation is the headroom shoulder near white (x≥0.875), which
        // asymptotes to 1 instead of hitting it — a fraction of an L* unit.
        assert!(max_d < 2.5, "max L* deviation = {max_d}");

        // Mid-grey anchor: 0.18 → ~0.388 (the ACR brightness), NOT the old 0.18→0.18.
        let mg = lut_lookup(&lut, 0.18);
        assert!((mg - 0.388).abs() < 0.01, "f(0.18) = {mg} (want ≈0.388)");
    }

    /// Headroom shoulder: scene-linear values above 1.0 stay monotone and asymptote to (not exceed)
    /// 1.0 — no hard clip corner.
    #[test]
    fn headroom_shoulder_is_monotone_below_one() {
        let lut = build_base_curve_lut(1.0);
        let mut prev = lut_lookup(&lut, 1.0);
        for k in 1..=64 {
            let x = 1.0 + k as f32 * 0.5; // 1.5 .. 33
            let y = lut_lookup(&lut, x);
            assert!(y >= prev - 1e-4, "non-monotone at x={x}: {y} < {prev}");
            assert!(y <= 1.0 + 1e-4, "exceeds 1.0 at x={x}: {y}");
            prev = y;
        }
    }

    /// The amount slider interpolates flat-neutral (0.18→0.18) → ACR fit (0.18→0.388) monotonically.
    #[test]
    fn amount_blends_flat_to_acr() {
        let flat = build_base_curve_lut(0.0);
        let acr = build_base_curve_lut(1.0);
        assert!(
            (lut_lookup(&flat, 0.18) - 0.18).abs() < 0.01,
            "flat mid-grey ≠ 0.18"
        );
        assert!(
            lut_lookup(&acr, 0.18) > lut_lookup(&flat, 0.18),
            "ACR must brighten mid-grey"
        );
        // Sanity: the embedded reference is monotone.
        let mut prev = -1.0;
        for &v in ADOBE_DEFAULT_TONE_CURVE.iter() {
            assert!(v >= prev - 1e-6);
            prev = v;
        }
    }
}
#[cfg(test)]
mod base_curve_tests {
    use super::*;

    #[test]
    fn anchors_zero_and_mid_grey_brightens_with_amount() {
        // f(0)=0 for all amounts; mid-grey rises monotonically from the flat-neutral 0.18 (amount=0)
        // to the ACR brightness ~0.388 (amount=1) as the Base-curve slider engages the ACR fit.
        let mut prev_mg = -1.0;
        for amount in [0.0, 0.5, 1.0] {
            assert_eq!(base_curve_value(0.0, amount), 0.0, "f(0) must be 0");
            let m = base_curve_value(MID_GREY, amount);
            assert!(m >= prev_mg, "mid-grey must not darken as amount rises");
            prev_mg = m;
        }
        assert!(
            (base_curve_value(MID_GREY, 0.0) - 0.18).abs() < 1e-3,
            "flat (amount=0) mid-grey must be 0.18"
        );
        assert!(
            (base_curve_value(MID_GREY, 1.0) - 0.388).abs() < 0.01,
            "ACR (amount=1) mid-grey must be ≈0.388"
        );
    }

    #[test]
    fn monotone_bounded_and_compresses_highlights() {
        let xs = [0.001, 0.01, 0.05, 0.18, 0.5, 1.0, 2.0, 4.0, 8.0, 32.0];
        let mut prev = -1.0;
        for x in xs {
            let y = base_curve_value(x, 1.0);
            assert!((0.0..1.0).contains(&y), "f({x})={y} out of [0,1)");
            assert!(y > prev, "f must increase ({prev} -> {y} at x={x})");
            prev = y;
        }
        assert!(base_curve_value(4.0, 1.0) < base_curve_value(8.0, 1.0));
        assert!(base_curve_value(8.0, 1.0) < 1.0);
    }

    #[test]
    fn full_amount_is_more_contrasty_than_neutral() {
        assert!(base_curve_value(1.0, 1.0) > base_curve_value(1.0, 0.0));
    }

    #[test]
    fn lut_matches_curve_at_sample_points() {
        let lut = build_base_curve_lut(1.0);
        assert_eq!(lut.len(), BASE_LUT_SIZE);
        let u_at = |i: usize| {
            BASE_U_MIN + (i as f32 / (BASE_LUT_SIZE - 1) as f32) * (BASE_U_MAX - BASE_U_MIN)
        };
        for i in [0usize, BASE_LUT_SIZE / 2, BASE_LUT_SIZE - 1] {
            let x = MID_GREY * 2f32.powf(u_at(i));
            assert!((lut[i] - base_curve_value(x, 1.0)).abs() < 1e-6);
        }
    }
}
