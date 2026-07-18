//! Self-contained color/transfer math for the HDR decode paths (HEIF PQ → linear ProPhoto).
//!
//! Duplicates a handful of tiny f64 mat3 helpers from `core-pipeline/src/params.rs` — they cannot
//! be shared: core-pipeline depends on core-raw, so importing them here would create a dependency
//! cycle. Both copies mirror rawler's `XYZ_TO_PROFOTORGB_D50`, so the chain composes exactly with
//! the linear-ProPhoto working buffer produced by `develop`/`display`.

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

/// The PQ luminance that maps to working-space 1.0 — BT.2408's diffuse/graphics white. This is the
/// single calibration knob for HEIF brightness: chosen so a CR3+HIF pair of the same capture
/// develops to matching brightness under the shared ACR default tone. Recalibrate via
/// `examples/calibrate_pq.rs` if a same-capture pair shows |ΔEV| > 0.25.
pub(crate) const HDR_DIFFUSE_WHITE_NITS: f64 = 203.0;

/// ST 2084 EOTF: PQ-encoded signal `e ∈ [0,1]` → normalized linear luminance `Y ∈ [0,1]`
/// (multiply by [`PQ_MAX_NITS`] for cd/m²).
pub(crate) fn pq_eotf(e: f64) -> f64 {
    let e = e.clamp(0.0, 1.0);
    let p = e.powf(1.0 / PQ_M2);
    let num = (p - PQ_C1).max(0.0);
    let den = PQ_C2 - PQ_C3 * p;
    (num / den).powf(1.0 / PQ_M1)
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
}
