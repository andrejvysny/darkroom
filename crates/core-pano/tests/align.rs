//! Public two-image alignment (`estimate_alignment_rgb`) over the shared synthetic-scene machinery.
//!
//! We build a richly-textured reference view, warp a copy by a KNOWN transform, and check that the
//! recovered transform reproduces that ground truth to sub-pixel accuracy — plus the degenerate
//! near-blank case (must return `None` rather than a garbage fit).

mod common;

use common::{make_base, render_view};
use core_pano::{estimate_alignment_rgb, AlignModel};

/// Bilinear sample of an interleaved-RGB buffer; black outside the pixel-centre grid.
fn sample(rgb: &[f32], w: usize, h: usize, x: f64, y: f64) -> [f32; 3] {
    if x < 0.0 || y < 0.0 || x > (w - 1) as f64 || y > (h - 1) as f64 {
        return [0.0; 3];
    }
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = (x - x0 as f64) as f32;
    let fy = (y - y0 as f64) as f32;
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

/// Apply a row-major 3×3 to `(x, y, 1)` with a homogeneous divide.
fn apply(m: &[[f64; 3]; 3], x: f64, y: f64) -> (f64, f64) {
    let w = m[2][0] * x + m[2][1] * y + m[2][2];
    (
        (m[0][0] * x + m[0][1] * y + m[0][2]) / w,
        (m[1][0] * x + m[1][1] * y + m[1][2]) / w,
    )
}

/// Build a "moving" buffer from `ref_rgb` such that the transform recovered by
/// `estimate_alignment_rgb(ref, mov)` (mov → ref) equals `m_true`: each mov pixel `p` shows the
/// reference content at `m_true·p`.
fn warp_buffer(ref_rgb: &[f32], w: usize, h: usize, m_true: &[[f64; 3]; 3]) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = apply(m_true, x as f64, y as f64);
            let c = sample(ref_rgb, w, h, sx, sy);
            let i = (y * w + x) * 3;
            out[i] = c[0];
            out[i + 1] = c[1];
            out[i + 2] = c[2];
        }
    }
    out
}

/// Max Euclidean disagreement (px) between two transforms over a handful of interior test points.
fn max_transform_error(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3], pts: &[(f64, f64)]) -> f64 {
    pts.iter().fold(0.0, |acc, &(px, py)| {
        let (ax, ay) = apply(a, px, py);
        let (bx, by) = apply(b, px, py);
        acc.max(((ax - bx).powi(2) + (ay - by).powi(2)).sqrt())
    })
}

#[test]
fn affine_warp_recovered_within_one_pixel() {
    let base = make_base();
    let refv = render_view(&base, 0.0, 1.0);
    let (w, h) = (refv.width, refv.height);

    // Mild affine: 2.5° rotation about the image centre + a small translation + 1.5% scale — the
    // kind of delta hand-held brackets exhibit.
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let ang = 2.5f64.to_radians();
    let s = 1.015;
    let (c, sn) = (ang.cos() * s, ang.sin() * s);
    // Rotate about the centre (T(c)·R·T(-c)), then nudge by (+5, −4).
    let tx = cx - (c * cx - sn * cy) + 5.0;
    let ty = cy - (sn * cx + c * cy) - 4.0;
    let m_true = [[c, -sn, tx], [sn, c, ty], [0.0, 0.0, 1.0]];

    let mov = warp_buffer(&refv.rgb, w, h, &m_true);
    let m = estimate_alignment_rgb(&refv.rgb, w, h, &mov, w, h, AlignModel::Affine)
        .expect("affine alignment should succeed on a textured scene");

    let err = max_transform_error(
        &m,
        &m_true,
        &[
            (150.0, 120.0),
            (500.0, 300.0),
            (350.0, 250.0),
            (600.0, 400.0),
        ],
    );
    assert!(
        err < 1.0,
        "recovered affine off by {err:.3} px at full scale"
    );
}

#[test]
fn near_blank_scene_returns_none() {
    // A flat plane has no corners → no matches → no confident fit.
    let (w, h) = (400usize, 300usize);
    let flat = vec![0.1f32; w * h * 3];
    assert!(estimate_alignment_rgb(&flat, w, h, &flat, w, h, AlignModel::Affine).is_none());
}

#[test]
fn homography_recovers_projective_warp() {
    let base = make_base();
    let refv = render_view(&base, 0.0, 1.0);
    let (w, h) = (refv.width, refv.height);

    // Mild projective: near-identity rotation/translation + small perspective terms.
    let m_true = [
        [0.998, -0.02, 8.0],
        [0.02, 0.998, -5.0],
        [2.0e-5, -1.0e-5, 1.0],
    ];
    let mov = warp_buffer(&refv.rgb, w, h, &m_true);
    let m = estimate_alignment_rgb(&refv.rgb, w, h, &mov, w, h, AlignModel::Homography)
        .expect("homography alignment should succeed on a textured scene");

    let err = max_transform_error(
        &m,
        &m_true,
        &[(200.0, 150.0), (450.0, 300.0), (350.0, 250.0)],
    );
    assert!(err < 2.0, "recovered homography off by {err:.3} px");
}
