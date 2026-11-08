#[cfg(test)]
use super::*;
/// Map a -100..100 white-balance temp/tint pair to per-channel linear gains.
/// Free function so the per-mask WB deltas share the exact same math. (The GLOBAL WB now uses the
/// chromatic-adaptation matrix below; this gain remains the per-mask local-WB delta.)
pub(crate) fn wb_gain_from(temp: f32, tint: f32) -> [f32; 3] {
    let t = (temp / 100.0).clamp(-1.0, 1.0);
    let g = (tint / 100.0).clamp(-1.0, 1.0);
    // Warm: boost R, cut B. Tint+ (magenta): boost R&B, cut G slightly.
    [
        1.0 + 0.45 * t + 0.10 * g,
        1.0 - 0.10 * g,
        1.0 - 0.45 * t + 0.10 * g,
    ]
}

// --- Global white balance: Planckian-locus target white + Bradford CAT (Phase 1c) -----------------
// All matrices are row-major `[[f64;3];3]`; computed in f64 and packed to f32 for the GPU. Kept
// self-contained (no rawler dep in core-pipeline); the ProPhoto constant mirrors rawler's
// `XYZ_TO_PROFOTORGB_D50` so this composes exactly with core-raw's ProPhoto working buffer.

pub(super) type M3 = [[f64; 3]; 3];

/// XYZ → linear-ProPhoto (D50), identical to rawler's `XYZ_TO_PROFOTORGB_D50`.
const XYZ_TO_PROPHOTO_D50: M3 = [
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

fn mat3_mul(a: &M3, b: &M3) -> M3 {
    let mut o = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            o[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    o
}

fn mat3_vec(m: &M3, v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

fn mat3_inv(m: &M3) -> M3 {
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

/// CCT (Kelvin) → CIE 1931 xy on the Planckian locus (Kim et al. 2002 cubic approximation).
fn kim_xy(t: f64) -> (f64, f64) {
    let (t2, t3) = (t * t, t * t * t);
    let x = if t >= 4000.0 {
        -3.0258469e9 / t3 + 2.1070379e6 / t2 + 0.2226347e3 / t + 0.240390
    } else {
        -0.2661239e9 / t3 - 0.2343589e6 / t2 + 0.8776956e3 / t + 0.179910
    };
    let (x2, x3) = (x * x, x * x * x);
    let y = if t >= 4000.0 {
        3.0817580 * x3 - 5.8733867 * x2 + 3.75112997 * x - 0.37001483
    } else if t >= 2222.0 {
        -0.9549476 * x3 - 1.37418593 * x2 + 2.09137015 * x - 0.16748867
    } else {
        -1.1063814 * x3 - 1.3481102 * x2 + 2.18555832 * x - 0.20219683
    };
    (x, y)
}

/// temp/tint (-100..100) → target white xy. temp+ = warmer (lower CCT, along the Planckian locus);
/// tint+ = magenta (lower y). Mired (reciprocal-temp) is ~perceptually uniform; the piecewise map
/// spans the full [83.33, 333.33] mired range symmetrically (no dead zone). Reference = the same
/// function at (0,0), so `wb_matrix(0,0)` is exactly identity.
const WB_BASE_MIRED: f64 = 153.85; // ~6500 K
fn white_xy(temp: f64, tint: f64) -> (f64, f64) {
    let t = temp / 100.0;
    let mired = if t >= 0.0 {
        WB_BASE_MIRED + t * (333.33 - WB_BASE_MIRED)
    } else {
        WB_BASE_MIRED + t * (WB_BASE_MIRED - 83.33)
    };
    let cct = (1.0e6 / mired).clamp(1667.0, 25000.0);
    let (x, mut y) = kim_xy(cct);
    y -= (tint / 100.0) * 0.04; // green↔magenta offset
    (x, y.max(1.0e-4))
}

fn xyy_to_xyz(x: f64, y: f64) -> [f64; 3] {
    [x / y, 1.0, (1.0 - x - y) / y]
}

/// Bradford CAT (XYZ→XYZ) adapting the source white to the destination white.
fn bradford_cat(w_src: [f64; 3], w_dst: [f64; 3]) -> M3 {
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

/// Global white-balance matrix for the linear-ProPhoto working space (row-major). At temp=tint=0 the
/// target white equals the reference white, so this is the exact identity (neutral untouched).
pub(crate) fn wb_matrix(temp: f32, tint: f32) -> [[f32; 3]; 3] {
    let (bx, by) = white_xy(0.0, 0.0);
    let w_ref = xyy_to_xyz(bx, by);
    let (tx, ty) = white_xy(temp as f64, tint as f64);
    let w_t = xyy_to_xyz(tx, ty);
    let cat = bradford_cat(w_ref, w_t);
    // M_pp = XYZ_TO_PP · CAT · PP_TO_XYZ (D50 throughout, matching the buffer + display matrices).
    let pp_to_xyz = mat3_inv(&XYZ_TO_PROPHOTO_D50);
    let m = mat3_mul(&mat3_mul(&XYZ_TO_PROPHOTO_D50, &cat), &pp_to_xyz);
    let mut out = [[0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = m[i][j] as f32;
        }
    }
    out
}

// --- Color-balance-RGB grading color space (Filmlight/Kirk grading RGB, D65) ----------------------
// Darktable's `colorbalancergb` runs the 4-way + chroma in a D65 grading RGB built on the CIE 2006
// LMS. We collapse the round trip ProPhoto(D50) ⇄ grading RGB to two premultiplied mat3, reusing the
// existing toolkit (`XYZ_TO_PROPHOTO_D50`, `mat3_inv/mul`, `bradford_cat`). Matrices verbatim from
// darktable `colorspaces_inline_conversions.h`. The chain + numbers are GPT-5.5-verified (round-trip
// identity to 1e-9; cond≈3.9); see `workspace/logs/codex-curve-review.out`.

/// CIE D50 / D65 reference whites (XYZ, Y=1).
const WHITE_D50_XYZ: [f64; 3] = [0.96422, 1.0, 0.82521];
const WHITE_D65_XYZ: [f64; 3] = [0.95047, 1.0, 1.08883];
/// XYZ(D65) → CIE 2006 LMS (darktable `XYZ_D65_to_LMS_2006_D65`).
const XYZ_D65_TO_LMS: M3 = [
    [0.257085, 0.859943, -0.031061],
    [-0.394427, 1.175800, 0.106423],
    [0.064856, -0.076250, 0.559067],
];
/// CIE 2006 LMS → grading RGB (darktable `gradingRGB_to_LMS` inverse).
const LMS_TO_GRADING: M3 = [
    [1.0877193, -0.0877193, 0.0],
    [-0.66666667, 1.66666667, 0.0],
    [0.02061856, -0.05154639, 1.03092784],
];

/// Premultiplied (ProPhoto-D50 → grading-RGB-D65, grading-RGB-D65 → ProPhoto-D50) mat3 pair, row-major.
/// Forward = LMS→grading · XYZ_D65→LMS · CAT(D50→D65) · ProPhoto→XYZ_D50.
pub(super) fn grading_matrices() -> (M3, M3) {
    let pp_to_xyz = mat3_inv(&XYZ_TO_PROPHOTO_D50);
    let cat_d50_d65 = bradford_cat(WHITE_D50_XYZ, WHITE_D65_XYZ);
    let fwd = mat3_mul(
        &mat3_mul(&LMS_TO_GRADING, &XYZ_D65_TO_LMS),
        &mat3_mul(&cat_d50_d65, &pp_to_xyz),
    );
    (fwd, mat3_inv(&fwd))
}

#[cfg(test)]
mod grading_space_tests {
    use super::*;

    fn mul(a: &M3, b: &M3) -> M3 {
        mat3_mul(a, b)
    }
    fn mv(m: &M3, v: [f64; 3]) -> [f64; 3] {
        mat3_vec(m, v)
    }

    #[test]
    fn prophoto_grading_round_trip_is_identity() {
        let (fwd, inv) = grading_matrices();
        let id = mul(&inv, &fwd);
        let mut max_err = 0.0f64;
        for (i, row) in id.iter().enumerate() {
            for (j, value) in row.iter().enumerate() {
                let expect = if i == j { 1.0 } else { 0.0 };
                max_err = max_err.max((value - expect).abs());
            }
        }
        assert!(max_err < 1e-4, "ProPhoto⇄grading round trip err {max_err}");
    }

    #[test]
    fn matches_codex_verified_numbers() {
        // GPT-5.5-verified forward matrix (M) — locks the chain order + CAT direction.
        let (fwd, _) = grading_matrices();
        let expect: M3 = [
            [0.4605891, 0.6311525, -0.0077856],
            [-0.2534295, 0.8953998, 0.1723560],
            [0.0395190, -0.0838124, 0.6316055],
        ];
        for i in 0..3 {
            for j in 0..3 {
                assert!(
                    (fwd[i][j] - expect[i][j]).abs() < 1e-5,
                    "fwd[{i}][{j}] = {} (want {})",
                    fwd[i][j],
                    expect[i][j]
                );
            }
        }
        // Neutral ProPhoto grey is NOT neutral in grading RGB (expected): [.195,.147,.106].
        let g = mv(&fwd, [0.18, 0.18, 0.18]);
        assert!(
            (g[0] - 0.1951).abs() < 1e-3
                && (g[1] - 0.1466).abs() < 1e-3
                && (g[2] - 0.1057).abs() < 1e-3
        );
    }

    #[test]
    fn cbrgb_active_flag_and_identity() {
        // Default is a no-op ⇒ inactive (shader skips the round trip → byte-identical render).
        let d = DevelopParams::default();
        assert!(d.cb_rgb.is_identity());
        assert_eq!(d.to_cbrgb().params[2], 0.0, "default cb must be inactive");
        // Any nonzero control flips the active flag, and the verified matrices ship in the uniform.
        let mut p = DevelopParams::default();
        p.cb_rgb.shadows = [0.1, 0.0, 0.0];
        assert!(!p.cb_rgb.is_identity());
        let u = p.to_cbrgb();
        assert_eq!(u.params[2], 1.0, "nonzero cb must be active");
        // std140 column packing: u.fwd[col][row]; fwd[0][0] = M[0][0] (GPT-5.5-verified).
        assert!((u.fwd[0][0] - 0.4605891).abs() < 1e-5);
    }

    #[test]
    fn channel_mix_active_flag_and_identity() {
        // Default is identity ⇒ inactive (shader passes color through → byte-identical render).
        let d = DevelopParams::default();
        assert!(d.channel_mix.is_identity());
        assert_eq!(
            d.to_chanmix().params[0],
            0.0,
            "default mixer must be inactive"
        );
        // Swap green↔blue: rows are permuted, active flag flips, packed rows match.
        let mut p = DevelopParams::default();
        p.channel_mix.green = [0.0, 0.0, 1.0];
        p.channel_mix.blue = [0.0, 1.0, 0.0];
        assert!(!p.channel_mix.is_identity());
        let u = p.to_chanmix();
        assert_eq!(u.params[0], 1.0, "swapped mixer must be active");
        assert_eq!(u.g, [0.0, 0.0, 1.0, 0.0]);
        assert_eq!(u.b, [0.0, 1.0, 0.0, 0.0]);
    }
}
#[cfg(test)]
mod wb_tests {
    use super::*;

    // Apply the (row-major) global WB matrix to a neutral [1,1,1] → per-channel response.
    fn neutral(temp: f32, tint: f32) -> [f32; 3] {
        let m = wb_matrix(temp, tint);
        [
            m[0][0] + m[0][1] + m[0][2],
            m[1][0] + m[1][1] + m[1][2],
            m[2][0] + m[2][1] + m[2][2],
        ]
    }

    #[test]
    fn kim_6500_is_near_d65() {
        let (x, y) = kim_xy(6500.0);
        // Kim's locus at 6500 K is close to but NOT exactly D65 — the WB reference uses this same
        // value so the slider zero is still exact identity (see wb_matrix_zero_is_identity).
        assert!((x - 0.31349).abs() < 1e-3, "x={x}");
        assert!((y - 0.32366).abs() < 1e-3, "y={y}");
    }

    #[test]
    fn wb_matrix_zero_is_identity() {
        let m = wb_matrix(0.0, 0.0);
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let mut max = 0.0f32;
        for i in 0..3 {
            for j in 0..3 {
                max = max.max((m[i][j] - id[i][j]).abs());
            }
        }
        assert!(max < 1e-6, "wb_matrix(0,0) must be identity, max dev {max}");
    }

    #[test]
    fn temp_warms_tint_greens() {
        let warm = neutral(80.0, 0.0);
        assert!(warm[0] - warm[2] > 0.05, "temp+ must warm (R>B): {warm:?}");
        let cool = neutral(-80.0, 0.0);
        assert!(cool[0] - cool[2] < -0.05, "temp- must cool (R<B): {cool:?}");
        let magenta = neutral(0.0, 20.0);
        assert!(
            magenta[1] < 1.0,
            "tint+ must reduce G (magenta): {magenta:?}"
        );
        // Every response must be finite.
        for v in warm.iter().chain(cool.iter()).chain(magenta.iter()) {
            assert!(v.is_finite(), "WB response must be finite");
        }
    }
}
