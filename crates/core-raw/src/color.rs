//! Self-contained color/transfer math for the HDR decode paths (HEIF PQ → linear ProPhoto).
//!
//! Duplicates a handful of tiny f64 mat3 helpers from `core-pipeline/src/params.rs` — they cannot
//! be shared: core-pipeline depends on core-raw, so importing them here would create a dependency
//! cycle. Both copies mirror rawler's `XYZ_TO_PROFOTORGB_D50`, so the chain composes exactly with
//! the linear-ProPhoto working buffer produced by `develop`/`display`.

use std::collections::HashMap;

use rawler::imgop::xyz::{FlatColorMatrix, Illuminant};

pub(crate) type M3 = [[f64; 3]; 3];

/// XYZ → linear-ProPhoto (D50), identical to rawler's `XYZ_TO_PROFOTORGB_D50` and to the copy in
/// `core-pipeline/src/params.rs`.
pub(crate) const XYZ_TO_PROPHOTO_D50: M3 = [
    [1.3459433, -0.2556075, -0.0511118],
    [-0.5445989, 1.5081673, 0.0205351],
    [0.0, 0.0, 1.2118128],
];

/// Standard Bradford chromatic-adaptation cone-response matrix.
const BRADFORD: M3 = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

/// CIE D50 reference white (XYZ, Y=1) — the white [`XYZ_TO_PROPHOTO_D50`] maps to RGB [1,1,1].
pub(crate) const WHITE_D50_XYZ: [f64; 3] = [0.96422, 1.0, 0.82521];

/// Linear BT.2020 (D65) → XYZ (D65). Columns are the BT.2020 primaries' XYZ vectors scaled so that
/// RGB=[1,1,1] maps to the D65 white point (derivation asserted in `bt2020_matrix_derivation`).
pub(crate) const BT2020_TO_XYZ_D65: M3 = [
    [0.6369580, 0.1446169, 0.1688810],
    [0.2627002, 0.6779981, 0.0593017],
    [0.0000000, 0.0280727, 1.0609851],
];

pub(crate) fn mat3_mul(a: &M3, b: &M3) -> M3 {
    let mut o = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            o[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    o
}

pub(crate) fn mat3_vec(m: &M3, v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

pub(crate) fn mat3_inv(m: &M3) -> M3 {
    let (a, b, c) = (m[0][0], m[0][1], m[0][2]);
    let (d, e, f) = (m[1][0], m[1][1], m[1][2]);
    let (g, h, i) = (m[2][0], m[2][1], m[2][2]);
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    let inv_det = 1.0 / det;
    [
        [
            (e * i - f * h) * inv_det,
            (c * h - b * i) * inv_det,
            (b * f - c * e) * inv_det,
        ],
        [
            (f * g - d * i) * inv_det,
            (a * i - c * g) * inv_det,
            (c * d - a * f) * inv_det,
        ],
        [
            (d * h - e * g) * inv_det,
            (b * g - a * h) * inv_det,
            (a * e - b * d) * inv_det,
        ],
    ]
}

/// Bradford CAT (XYZ→XYZ) adapting the source white to the destination white.
pub(crate) fn bradford_cat(w_src: [f64; 3], w_dst: [f64; 3]) -> M3 {
    let ls = mat3_vec(&BRADFORD, w_src);
    let ld = mat3_vec(&BRADFORD, w_dst);
    let d: M3 = [
        [ld[0] / ls[0], 0.0, 0.0],
        [0.0, ld[1] / ls[1], 0.0],
        [0.0, 0.0, ld[2] / ls[2]],
    ];
    let b_inv = mat3_inv(&BRADFORD);
    mat3_mul(&mat3_mul(&b_inv, &d), &BRADFORD)
}

/// Linear BT.2020 (D65) → linear ProPhoto (D50), Bradford-adapted. The one matrix the HEIF PQ
/// decode needs: `XYZ→ProPhoto(D50) · CAT(D65→D50) · BT.2020→XYZ(D65)`, packed to f32.
pub(crate) fn bt2020_to_prophoto_d50() -> [[f32; 3]; 3] {
    // Source white = the D65 the BT.2020 matrix itself encodes (its RGB=[1,1,1] image), not a
    // separately-rounded D65 constant — keeps white→white mapping exact through the CAT.
    let w_d65 = mat3_vec(&BT2020_TO_XYZ_D65, [1.0, 1.0, 1.0]);
    let cat = bradford_cat(w_d65, WHITE_D50_XYZ);
    let m = mat3_mul(&mat3_mul(&XYZ_TO_PROPHOTO_D50, &cat), &BT2020_TO_XYZ_D65);
    let mut out = [[0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = m[i][j] as f32;
        }
    }
    out
}

// --- SMPTE ST 2084 (PQ) ---------------------------------------------------------------------------

const PQ_M1: f64 = 2610.0 / 16384.0; // 0.1593017578125
const PQ_M2: f64 = 2523.0 / 4096.0 * 128.0; // 78.84375
const PQ_C1: f64 = 3424.0 / 4096.0; // 0.8359375
const PQ_C2: f64 = 2413.0 / 4096.0 * 32.0; // 18.8515625
const PQ_C3: f64 = 2392.0 / 4096.0 * 32.0; // 18.6875

/// PQ signal 1.0 corresponds to this absolute luminance.
pub(crate) const PQ_MAX_NITS: f64 = 10000.0;

/// The PQ luminance that maps to working-space 1.0. This is the single calibration knob for HEIF
/// brightness: chosen so a CR3+HIF pair of the same capture develops to matching brightness under
/// the shared ACR default tone. Recalibrate via `examples/calibrate_pq.rs` if a same-capture pair
/// shows |ΔEV| > 0.25.
///
/// Calibration record (2026-07-19): started at BT.2408's 203-nit diffuse white; measured against a
/// real EOS R7 pair — `_55A6551.CR3` (the metered frame, 1/80 s f/22 ISO 250) vs `855A6554.HIF`
/// (the camera's HDR-mode composite of the same burst, identical exposure settings) — the HIF
/// developed **+0.572 EV** brighter (mid-tone geomean ratio 1.487), so the anchor was raised to
/// the measured ≈302 → **300**. Caveat: that HIF is an in-camera HDR *composite* (Canon's HDR tone
/// treatment can lift mid-tones), so refine against a plain RAW+HIF simultaneous-recording pair if
/// one becomes available. Public so the fixture tests and `calibrate_pq` derive their expectations
/// from the shipping value instead of a drifting copy.
pub const HDR_DIFFUSE_WHITE_NITS: f64 = 300.0;

/// ST 2084 EOTF: PQ-encoded signal `e ∈ [0,1]` → normalized linear luminance `Y ∈ [0,1]`
/// (multiply by [`PQ_MAX_NITS`] for cd/m²).
pub(crate) fn pq_eotf(e: f64) -> f64 {
    let e = e.clamp(0.0, 1.0);
    let p = e.powf(1.0 / PQ_M2);
    let num = (p - PQ_C1).max(0.0);
    let den = PQ_C2 - PQ_C3 * p;
    (num / den).powf(1.0 / PQ_M1)
}

// --- DNG dual-illuminant camera-matrix selection ---------------------------------------------------

/// Refinement passes for the DNG neutral↔temperature fixed point. The blend is smooth in 1/T and
/// McCamy's approximation is monotone over the calibrated range, so the estimate contracts by ~4×
/// per pass; four passes land within a few Kelvin of the fixed point from the 5000 K seed.
const CCT_ITERATIONS: usize = 4;

/// Correlated colour temperature (K) of a DNG `CalibrationIlluminant` (an EXIF LightSource code).
/// The CIE illuminants use their defined CCT (A 2856, D50 5003, D55 5503, D65 6504, D75 7504,
/// B 4874, C 6774); the descriptive codes use the conventional value profile tooling assumes for
/// them. `Unknown` has no temperature: it cannot take part in the interpolation and is only
/// reachable through [`illuminant_rank`].
fn illuminant_cct(ill: Illuminant) -> Option<u32> {
    Some(match ill {
        Illuminant::A | Illuminant::Tungsten | Illuminant::IsoStudioTungsten => 2856,
        Illuminant::D50 => 5003,
        Illuminant::D55 => 5503,
        Illuminant::D65 => 6504,
        Illuminant::D75 => 7504,
        Illuminant::Daylight | Illuminant::Flash | Illuminant::FineWeather => 5500,
        Illuminant::CloudyWeather => 6500,
        Illuminant::Shade => 7500,
        Illuminant::Fluorescent | Illuminant::CoolWhiteFluorescent => 4230,
        Illuminant::DaylightFluorescent => 6430,
        Illuminant::DaylightWhiteFluorescent => 5000,
        Illuminant::WhiteFluorescent => 3450,
        Illuminant::B => 4874,
        Illuminant::C => 6774,
        Illuminant::Unknown => return None,
    })
}

/// Preference order (lower is better) used when NO entry carries a temperature, and to break a tie
/// between two entries that share one.
///
/// `color_matrix` is a `HashMap`, whose iteration order is unspecified and varies per process — and
/// the preview decode and the export decode are two independent decodes of the same file that must
/// agree on the matrix. Every ordering decision here is therefore keyed on the illuminant, never on
/// map order.
fn illuminant_rank(ill: Illuminant) -> u8 {
    match ill {
        Illuminant::D65 => 0,
        Illuminant::D55 => 1,
        Illuminant::D50 => 2,
        Illuminant::D75 => 3,
        Illuminant::Daylight => 4,
        Illuminant::FineWeather => 5,
        Illuminant::CloudyWeather => 6,
        Illuminant::Shade => 7,
        Illuminant::Fluorescent
        | Illuminant::DaylightFluorescent
        | Illuminant::DaylightWhiteFluorescent
        | Illuminant::CoolWhiteFluorescent
        | Illuminant::WhiteFluorescent => 8,
        Illuminant::A => 9,
        _ => 10,
    }
}

/// One flat `ColorMatrix` entry as a 3×3 f64 (XYZ→camera, row-major), or `None` when it is unusable:
/// fewer than three rows, a length that is not a whole number of rows, or a non-finite coefficient
/// (a NaN there would silently poison every developed pixel). Rows past the third describe a
/// 4-colour sensor and are dropped here — see [`select_cam_matrix`].
fn parse_3x3(flat: &[f32]) -> Option<M3> {
    if flat.len() < 9 || !flat.len().is_multiple_of(3) {
        return None;
    }
    let mut m: M3 = [[0.0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            let c = flat[i * 3 + j];
            if !c.is_finite() {
                return None;
            }
            *v = c as f64;
        }
    }
    Some(m)
}

/// Pad a 3×3 to rawler's 4-row matrix shape. The unused row stays zero, which contributes nothing
/// to `pseudo_inverse`'s normal equations — so a padded 3-colour matrix behaves exactly like a 3×3.
fn pad4(m: &M3) -> [[f32; 3]; 4] {
    let mut out = [[0f32; 3]; 4];
    for (i, row) in m.iter().enumerate() {
        for (j, v) in row.iter().enumerate() {
            out[i][j] = *v as f32;
        }
    }
    out
}

/// The camera-space neutral the as-shot gains describe: white balance multiplies raw channel `c` by
/// `wb[c]` so that a scene white lands equal in all three, hence that white's raw value is
/// `1/wb[c]`. Only its chromaticity matters downstream, so the scale (green-normalized or not) is
/// irrelevant. Falls back to equal-energy when the gains are unusable — `wb_or_neutral` already
/// screens the develop path, but this must be total.
fn camera_neutral(wb: &[f32; 4]) -> [f64; 3] {
    if wb[0..3].iter().all(|c| c.is_finite() && *c > 0.0) {
        [1.0 / wb[0] as f64, 1.0 / wb[1] as f64, 1.0 / wb[2] as f64]
    } else {
        [1.0; 3]
    }
}

/// CCT (K) of the camera neutral `n` seen through `m` (XYZ→camera): invert to XYZ, take the CIE 1931
/// chromaticity, apply McCamy's cubic approximation. `None` for a singular matrix or a degenerate
/// chromaticity (including McCamy's pole at y = 0.1858), where the caller keeps its last estimate.
fn neutral_cct(m: &M3, n: [f64; 3]) -> Option<f64> {
    let xyz = mat3_vec(&mat3_inv(m), n);
    let sum = xyz[0] + xyz[1] + xyz[2];
    if !sum.is_finite() || sum <= 0.0 {
        return None;
    }
    let (x, y) = (xyz[0] / sum, xyz[1] / sum);
    let denom = 0.1858 - y;
    if denom.abs() < 1e-9 {
        return None;
    }
    let t = (x - 0.3320) / denom;
    let cct = 449.0 * t * t * t + 3525.0 * t * t + 6823.3 * t + 5520.33;
    cct.is_finite().then_some(cct)
}

/// DNG ch.6 fixed point: blend `m_lo`/`m_hi` linearly in 1/T at the temperature of the camera
/// neutral `n` — a temperature that is itself read back through the blend, so the two are solved by
/// iteration from a 5000 K seed.
fn interpolate_cam_matrix(m_lo: &M3, t_lo: f64, m_hi: &M3, t_hi: f64, n: [f64; 3]) -> M3 {
    let blend = |t: f64| -> M3 {
        // Linear in reciprocal temperature (mireds), as the spec prescribes; clamped so a neutral
        // outside the calibrated range uses the nearer endpoint instead of extrapolating a matrix
        // nobody measured.
        let a = ((1.0 / t - 1.0 / t_hi) / (1.0 / t_lo - 1.0 / t_hi)).clamp(0.0, 1.0);
        let mut m: M3 = [[0.0; 3]; 3];
        for (i, row) in m.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v = a * m_lo[i][j] + (1.0 - a) * m_hi[i][j];
            }
        }
        m
    };
    let mut t = 5000.0;
    let mut m = blend(t);
    for _ in 0..CCT_ITERATIONS {
        // A degenerate step leaves `m` on the last good temperature rather than poisoning it.
        let Some(next) = neutral_cct(&m, n) else {
            break;
        };
        let next = next.clamp(t_lo, t_hi);
        if !next.is_finite() {
            break;
        }
        t = next;
        m = blend(t);
    }
    log::debug!("camera colour matrix interpolated at {t:.0} K (calibrated {t_lo:.0}-{t_hi:.0} K)");
    m
}

/// Deterministic pick among the entries that share `cct`: best-ranked illuminant, then lowest code.
fn best_matrix_at(entries: &[(u32, Illuminant, M3)], cct: u32) -> Option<M3> {
    entries
        .iter()
        .filter(|(t, _, _)| *t == cct)
        .min_by_key(|(_, ill, _)| (illuminant_rank(*ill), *ill))
        .map(|(_, _, m)| *m)
}

/// Pick — and, for a dual-illuminant camera, INTERPOLATE — the XYZ→camera matrix to develop `wb`
/// with, padded to rawler's `[[f32; 3]; 4]` shape. `None` when the file carries no usable matrix.
///
/// DNG spec ch. 6 ("Mapping Camera Color Space to CIE XYZ") defines a shot's matrix as a blend of
/// the camera's two calibration matrices, weighted linearly in 1/CCT at the temperature of the
/// scene's own neutral. Taking the D65 matrix outright (what this replaced) develops every tungsten
/// frame through a daylight calibration — precisely the error dual-illuminant profiles exist to
/// avoid. Cameras with one matrix (Sony) are unaffected: the single entry is returned unchanged.
pub(crate) fn select_cam_matrix(
    matrices: &HashMap<Illuminant, FlatColorMatrix>,
    wb: &[f32; 4],
) -> Option<[[f32; 3]; 4]> {
    let mut cands: Vec<(Illuminant, usize, M3)> = matrices
        .iter()
        .filter_map(|(ill, flat)| parse_3x3(flat).map(|m| (*ill, flat.len(), m)))
        .collect();
    // A 4-row ColorMatrix belongs to a 4-colour (CYGM/RGBE) sensor, where keeping only the first
    // three rows is an approximation — do that only when the file offers no 3-row entry at all.
    if cands.iter().any(|(_, len, _)| *len == 9) {
        cands.retain(|(_, len, _)| *len == 9);
    }

    let with_cct: Vec<(u32, Illuminant, M3)> = cands
        .iter()
        .filter_map(|(ill, _, m)| illuminant_cct(*ill).map(|t| (t, *ill, *m)))
        .collect();
    let (Some(&lo_k), Some(&hi_k)) = (
        with_cct.iter().map(|(t, _, _)| t).min(),
        with_cct.iter().map(|(t, _, _)| t).max(),
    ) else {
        // Nothing has a temperature (every entry is `Unknown`): fall back to the stable ranking.
        let (_, _, m) = cands
            .iter()
            .min_by_key(|(ill, _, _)| (illuminant_rank(*ill), *ill))?;
        return Some(pad4(m));
    };
    let m_lo = best_matrix_at(&with_cct, lo_k)?;
    if lo_k == hi_k {
        return Some(pad4(&m_lo));
    }
    let m_hi = best_matrix_at(&with_cct, hi_k)?;
    Some(pad4(&interpolate_cam_matrix(
        &m_lo,
        lo_k as f64,
        &m_hi,
        hi_k as f64,
        camera_neutral(wb),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ST 2084 inverse EOTF (encode), test-only.
    fn pq_oetf(y: f64) -> f64 {
        let y = y.clamp(0.0, 1.0);
        let p = y.powf(PQ_M1);
        ((PQ_C1 + PQ_C2 * p) / (1.0 + PQ_C3 * p)).powf(PQ_M2)
    }

    #[test]
    fn pq_endpoints() {
        assert_eq!(pq_eotf(0.0), 0.0);
        assert!((pq_eotf(1.0) - 1.0).abs() < 1e-12);
    }

    /// The canonical spot value: PQ code ≈ 0.5081 encodes 100 cd/m² (SDR peak white).
    #[test]
    fn pq_100_nits_spot_value() {
        let nits = pq_eotf(0.5081) * PQ_MAX_NITS;
        assert!(
            (nits - 100.0).abs() / 100.0 < 0.005,
            "PQ(0.5081) = {nits} nits, expected ≈100"
        );
    }

    #[test]
    fn pq_round_trip() {
        for &y in &[0.0, 1e-6, 1e-4, 0.01, 0.1, 0.0203, 0.5, 0.9, 1.0] {
            let e = pq_oetf(y);
            let back = pq_eotf(e);
            assert!(
                (back - y).abs() < 1e-9,
                "round trip failed at Y={y}: got {back}"
            );
        }
    }

    /// Derive BT.2020→XYZ from the primaries' chromaticities (ITU-R BT.2020-2: R(0.708,0.292),
    /// G(0.170,0.797), B(0.131,0.046), white D65 (0.3127,0.3290)) and assert the hardcoded constant
    /// matches to ≤1e-6 per element.
    #[test]
    fn bt2020_matrix_derivation() {
        let xy = [(0.708, 0.292), (0.170, 0.797), (0.131, 0.046)];
        let white_xy = (0.3127, 0.3290);
        // Columns of P = un-scaled primary XYZ vectors (x/y, 1, (1-x-y)/y).
        let mut p: M3 = [[0.0; 3]; 3];
        for (col, &(x, y)) in xy.iter().enumerate() {
            p[0][col] = x / y;
            p[1][col] = 1.0;
            p[2][col] = (1.0 - x - y) / y;
        }
        let w = [
            white_xy.0 / white_xy.1,
            1.0,
            (1.0 - white_xy.0 - white_xy.1) / white_xy.1,
        ];
        let s = mat3_vec(&mat3_inv(&p), w); // per-column scale so M·[1,1,1]ᵀ = white
        for i in 0..3 {
            for j in 0..3 {
                let derived = p[i][j] * s[j];
                assert!(
                    (derived - BT2020_TO_XYZ_D65[i][j]).abs() <= 1e-6,
                    "element [{i}][{j}]: derived {derived} vs const {}",
                    BT2020_TO_XYZ_D65[i][j]
                );
            }
        }
    }

    /// BT.2020 white [1,1,1] must land on ProPhoto white [1,1,1] (proves the D65→D50 CAT chain).
    #[test]
    fn bt2020_white_maps_to_prophoto_white() {
        let m = bt2020_to_prophoto_d50();
        for row in m {
            let v = row[0] + row[1] + row[2];
            assert!((v - 1.0).abs() < 1e-4, "row sum {v} ≠ 1");
        }
    }

    // --- DNG dual-illuminant camera-matrix selection ---------------------------------------------

    use rawler::imgop::xyz::{XYZ_TO_ADOBERGB_D65, XYZ_TO_SRGB_D65};

    /// A synthetic dual-illuminant camera. The two calibrations are deliberately *different*
    /// well-conditioned matrices (Adobe RGB's and sRGB's XYZ→RGB), because a camera whose two
    /// matrices agree on the neutral has an ambiguous fixed point and would prove nothing.
    fn m_tungsten() -> M3 {
        widen(&XYZ_TO_ADOBERGB_D65)
    }
    fn m_daylight() -> M3 {
        widen(&XYZ_TO_SRGB_D65)
    }

    fn widen(m: &[[f32; 3]; 3]) -> M3 {
        let mut out: M3 = [[0.0; 3]; 3];
        for (i, row) in out.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v = m[i][j] as f64;
            }
        }
        out
    }

    fn flatten(m: &M3) -> FlatColorMatrix {
        m.iter().flatten().map(|v| *v as f32).collect()
    }

    /// XYZ (Y = 1) of a chromaticity.
    fn white_xyz(x: f64, y: f64) -> [f64; 3] {
        [x / y, 1.0, (1.0 - x - y) / y]
    }

    /// As-shot gains a camera with matrix `m` would report under a white of chromaticity `(x, y)`:
    /// the raw neutral is `m · XYZ(white)`, and the gains are its reciprocals.
    fn gains_under(m: &M3, x: f64, y: f64) -> [f32; 4] {
        let n = mat3_vec(m, white_xyz(x, y));
        [
            (1.0 / n[0]) as f32,
            (1.0 / n[1]) as f32,
            (1.0 / n[2]) as f32,
            f32::NAN,
        ]
    }

    /// Recover the interpolation weight `a` (1 = the low-temperature matrix, 0 = the high one) from
    /// a returned matrix, using an element where the two calibrations differ substantially.
    fn recover_a(got: &[[f32; 3]; 4], lo: &M3, hi: &M3) -> f64 {
        let (i, j) = (0, 0);
        assert!(
            (lo[i][j] - hi[i][j]).abs() > 0.5,
            "test matrices must differ at the probed element"
        );
        (got[i][j] as f64 - hi[i][j]) / (lo[i][j] - hi[i][j])
    }

    fn dual_illuminant(lo: Illuminant, hi: Illuminant) -> HashMap<Illuminant, FlatColorMatrix> {
        HashMap::from([(lo, flatten(&m_tungsten())), (hi, flatten(&m_daylight()))])
    }

    /// A tungsten as-shot neutral must pull the blend onto the A calibration (a ≈ 1, T ≈ 2856 K) —
    /// the whole point of dual-illuminant profiles, and exactly what picking D65 outright got wrong.
    #[test]
    fn tungsten_neutral_selects_the_a_matrix() {
        let m = dual_illuminant(Illuminant::A, Illuminant::D65);
        // CIE A: x 0.44757, y 0.40745 (McCamy reads it back as 2857 K).
        let wb = gains_under(&m_tungsten(), 0.44757, 0.40745);
        let got = select_cam_matrix(&m, &wb).expect("a dual-illuminant camera must resolve");
        let a = recover_a(&got, &m_tungsten(), &m_daylight());
        assert!(
            a > 0.95,
            "tungsten neutral interpolated at a = {a} (want ≈1)"
        );
    }

    /// A D65 neutral must land on the daylight calibration untouched (a = 0 after the temperature
    /// clamps at the top of the calibrated range).
    #[test]
    fn daylight_neutral_selects_the_d65_matrix() {
        let m = dual_illuminant(Illuminant::A, Illuminant::D65);
        let wb = gains_under(&m_daylight(), 0.3127, 0.3290);
        let got = select_cam_matrix(&m, &wb).expect("a dual-illuminant camera must resolve");
        let a = recover_a(&got, &m_tungsten(), &m_daylight());
        assert!(
            a.abs() < 1e-3,
            "daylight neutral interpolated at a = {a} (want 0)"
        );
    }

    /// A camera calibrated for A + D50 (no D65 entry) must develop a daylight frame through D50.
    /// The old selection fell back to `min_by_key` over the illuminant CODE, and A (17) sorts below
    /// D50 (23) — so every daylight shot from such a body went through the tungsten matrix.
    #[test]
    fn a_plus_d50_no_longer_picks_tungsten_for_a_daylight_neutral() {
        let m = dual_illuminant(Illuminant::A, Illuminant::D50);
        let wb = gains_under(&m_daylight(), 0.3127, 0.3290);
        let got = select_cam_matrix(&m, &wb).expect("resolve");
        let a = recover_a(&got, &m_tungsten(), &m_daylight());
        assert!(
            a.abs() < 1e-3,
            "daylight neutral resolved at a = {a} — A won again"
        );
    }

    /// A single-matrix body (most Sony) is returned verbatim, whatever the neutral.
    #[test]
    fn single_matrix_is_returned_unchanged() {
        let m = HashMap::from([(Illuminant::D65, flatten(&m_daylight()))]);
        for wb in [[1.0, 1.0, 1.0, f32::NAN], [2.0, 1.0, 1.5, f32::NAN]] {
            let got = select_cam_matrix(&m, &wb).expect("single matrix must resolve");
            for (i, row) in m_daylight().iter().enumerate() {
                for (j, want) in row.iter().enumerate() {
                    assert_eq!(got[i][j], *want as f32, "[{i}][{j}]");
                }
            }
            assert_eq!(got[3], [0.0; 3], "the pad row must stay zero");
        }
    }

    /// Malformed entries (short, empty, NaN-poisoned) are skipped rather than developed with, and a
    /// file whose every entry is malformed reports no matrix at all (→ calibrated fallback).
    #[test]
    fn malformed_matrices_are_skipped() {
        let good = flatten(&m_daylight());
        let mut nan = good.clone();
        nan[4] = f32::NAN;
        let m = HashMap::from([
            (Illuminant::D65, good.clone()),
            (Illuminant::A, vec![1.0, 2.0, 3.0, 4.0, 5.0]), // not a whole number of rows
            (Illuminant::D50, Vec::new()),
            (Illuminant::Tungsten, nan.clone()),
            (Illuminant::D55, vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0]), // only two rows
        ]);
        let got = select_cam_matrix(&m, &[1.0, 1.0, 1.0, f32::NAN]).expect("the D65 entry is fine");
        assert_eq!(got[0][0], m_daylight()[0][0] as f32);

        let all_bad = HashMap::from([
            (Illuminant::D65, Vec::new()),
            (Illuminant::A, nan),
            (Illuminant::D50, vec![1.0, 2.0]),
        ]);
        assert!(select_cam_matrix(&all_bad, &[1.0; 4]).is_none());
    }

    /// A 4-row ColorMatrix (4-colour sensor) is only used when the file offers no 3-row entry.
    #[test]
    fn three_row_matrices_win_over_four_row_ones() {
        let mut four_row = flatten(&m_daylight());
        four_row.extend_from_slice(&[0.5, 0.5, 0.5]);
        let m = HashMap::from([
            (Illuminant::D65, four_row.clone()),
            (Illuminant::A, flatten(&m_tungsten())),
        ]);
        // Only the A entry survives the filter, so there is one temperature and no interpolation.
        let got = select_cam_matrix(&m, &[1.0; 4]).expect("resolve");
        assert_eq!(got[0][0], m_tungsten()[0][0] as f32);

        // With nothing else on offer, the 4-row entry's first three rows are used.
        let only_four = HashMap::from([(Illuminant::D65, four_row)]);
        let got = select_cam_matrix(&only_four, &[1.0; 4]).expect("resolve");
        assert_eq!(got[0][0], m_daylight()[0][0] as f32);
        assert_eq!(got[3], [0.0; 3], "the 4th row is dropped, not carried");
    }

    /// Two entries at the SAME temperature must be separated by [`illuminant_rank`], never by
    /// `HashMap` order: the preview decode and the export decode are independent decodes of one file
    /// and would otherwise be free to disagree on the matrix.
    #[test]
    fn a_tie_on_temperature_is_broken_deterministically() {
        // A, Tungsten and IsoStudioTungsten all mean 2856 K; A outranks the other two.
        let m = HashMap::from([
            (Illuminant::Tungsten, flatten(&m_tungsten())),
            (Illuminant::IsoStudioTungsten, flatten(&m_tungsten())),
            (Illuminant::A, flatten(&m_daylight())),
        ]);
        for _ in 0..8 {
            let got = select_cam_matrix(&m, &[1.0; 4]).expect("resolve");
            assert_eq!(
                got[0][0],
                m_daylight()[0][0] as f32,
                "the A entry must win every time"
            );
        }
    }

    /// Unusable gains must not poison the blend (they fall back to an equal-energy neutral).
    #[test]
    fn garbage_gains_still_resolve_a_finite_matrix() {
        let m = dual_illuminant(Illuminant::A, Illuminant::D65);
        for wb in [
            [0.0, 1.0, 1.0, f32::NAN],
            [f32::NAN, 1.0, 1.0, f32::NAN],
            [-1.0, 1.0, 1.0, f32::NAN],
            [f32::INFINITY, 1.0, 1.0, f32::NAN],
        ] {
            let got = select_cam_matrix(&m, &wb).expect("resolve");
            assert!(
                got.iter().flatten().all(|v| v.is_finite()),
                "{wb:?} produced {got:?}"
            );
        }
    }
}
