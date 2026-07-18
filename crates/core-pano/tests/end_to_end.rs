//! Synthetic end-to-end registration test.
//!
//! WHY synthetic (not the CR3 fixture): core-pano must not link core-raw, even as a dev-dependency,
//! to stay a pure f32-buffer consumer. So we build ground truth ourselves — a procedurally textured
//! base image viewed by a virtual camera that yaws by a known amount for three overlapping frames —
//! and check that `register()` recovers the relative yaw angles and the focal. This runs everywhere,
//! with no fixture, and is fully deterministic (seeded texture).

use nalgebra::{Matrix3, Unit, Vector3};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use core_pano::{register, Frame, Phase};

const BASE_W: usize = 1200;
const BASE_H: usize = 800;
const FB: f64 = 800.0; // base (scene) focal
const VIEW_W: usize = 700;
const VIEW_H: usize = 500;
const FV: f64 = 900.0; // per-view focal — this is what register() must recover

/// Camera rotation for a given "pan" angle, about an axis tilted slightly off vertical.
///
/// WHY a tilted axis (not pure `rot_y`): a rotation about an image-aligned axis makes the pairwise
/// homography degenerate for `focalsFromHomography` (0/0 — see the unit test in `camera.rs`). Tilting
/// the pan axis ~14° off Y keeps the *relative* angles exactly the pan differences (12°, 24°, since
/// all frames rotate about the same axis) while making the homography well conditioned — mirroring a
/// real hand-held pano that is never perfectly level.
fn view_rotation(pan_deg: f64) -> Matrix3<f64> {
    let axis = Unit::new_normalize(Vector3::new(0.2, 1.0, 0.15));
    nalgebra::Rotation3::from_axis_angle(&axis, pan_deg.to_radians()).into_inner()
}

/// Procedural base texture: dim background sprinkled with random rectangles for dense corner content.
fn make_base() -> Vec<f32> {
    let mut rng = StdRng::seed_from_u64(7);
    let mut base = vec![0.05f32; BASE_W * BASE_H * 3];
    for _ in 0..900 {
        let x0 = rng.gen_range(0..BASE_W);
        let y0 = rng.gen_range(0..BASE_H);
        let w = rng.gen_range(6..60);
        let h = rng.gen_range(6..60);
        let col = [
            rng.gen_range(0.02..0.18f32),
            rng.gen_range(0.02..0.18f32),
            rng.gen_range(0.02..0.18f32),
        ];
        for y in y0..(y0 + h).min(BASE_H) {
            for x in x0..(x0 + w).min(BASE_W) {
                let idx = (y * BASE_W + x) * 3;
                base[idx] = col[0];
                base[idx + 1] = col[1];
                base[idx + 2] = col[2];
            }
        }
    }
    base
}

fn sample_base(base: &[f32], u: f64, v: f64) -> [f32; 3] {
    if u < 0.0 || v < 0.0 || u >= (BASE_W - 1) as f64 || v >= (BASE_H - 1) as f64 {
        return [0.0, 0.0, 0.0];
    }
    let x0 = u.floor() as usize;
    let y0 = v.floor() as usize;
    let fx = (u - x0 as f64) as f32;
    let fy = (v - y0 as f64) as f32;
    let mut out = [0.0f32; 3];
    for (ch, o) in out.iter_mut().enumerate() {
        let p00 = base[(y0 * BASE_W + x0) * 3 + ch];
        let p10 = base[(y0 * BASE_W + x0 + 1) * 3 + ch];
        let p01 = base[((y0 + 1) * BASE_W + x0) * 3 + ch];
        let p11 = base[((y0 + 1) * BASE_W + x0 + 1) * 3 + ch];
        let top = p00 + (p10 - p00) * fx;
        let bot = p01 + (p11 - p01) * fx;
        *o = top + (bot - top) * fy;
    }
    out
}

/// Render one yawed view by inverse-warping the base through the view's camera model.
fn render_view(base: &[f32], angle_deg: f64) -> Frame {
    let k_view = Matrix3::new(
        FV,
        0.0,
        (VIEW_W as f64) * 0.5,
        0.0,
        FV,
        (VIEW_H as f64) * 0.5,
        0.0,
        0.0,
        1.0,
    );
    let k_view_inv = k_view.try_inverse().unwrap();
    let k_base = Matrix3::new(
        FB,
        0.0,
        (BASE_W as f64) * 0.5,
        0.0,
        FB,
        (BASE_H as f64) * 0.5,
        0.0,
        0.0,
        1.0,
    );
    let r = view_rotation(angle_deg); // world -> camera
    let rt = r.transpose();

    let mut rgb = vec![0.0f32; VIEW_W * VIEW_H * 3];
    for y in 0..VIEW_H {
        for x in 0..VIEW_W {
            let d = rt * (k_view_inv * Vector3::new(x as f64, y as f64, 1.0));
            let p = k_base * d;
            if p.z.abs() < 1e-9 {
                continue;
            }
            let c = sample_base(base, p.x / p.z, p.y / p.z);
            let idx = (y * VIEW_W + x) * 3;
            rgb[idx] = c[0];
            rgb[idx + 1] = c[1];
            rgb[idx + 2] = c[2];
        }
    }
    Frame {
        width: VIEW_W,
        height: VIEW_H,
        rgb,
        focal_seed_px: None,
    }
}

/// Rotation angle (radians) of a rotation matrix.
fn angle_of(m: &Matrix3<f64>) -> f64 {
    (((m.trace() - 1.0) * 0.5).clamp(-1.0, 1.0)).acos()
}

#[test]
fn three_frame_rotating_camera_registers() {
    let base = make_base();
    let frames = vec![
        render_view(&base, -12.0),
        render_view(&base, 0.0),
        render_view(&base, 12.0),
    ];

    let reg = register(&frames, &|_p: Phase| {}).expect("registration should succeed");

    // All three frames must be in the panorama.
    let mut used = reg.used_indices.clone();
    used.sort_unstable();
    assert_eq!(used, vec![0, 1, 2], "all three frames should register");

    let cam = |i: usize| reg.cameras[i].as_ref().expect("frame should have a pose");
    let (r0, r1, r2) = (cam(0).rotation, cam(1).rotation, cam(2).rotation);

    // Relative rotation angles are gauge-invariant, so compare directly to ground truth.
    let a01 = angle_of(&(r1 * r0.transpose())).to_degrees();
    let a12 = angle_of(&(r2 * r1.transpose())).to_degrees();
    let a02 = angle_of(&(r2 * r0.transpose())).to_degrees();
    assert!((a01 - 12.0).abs() < 1.5, "angle 0->1 = {a01}, want ~12");
    assert!((a12 - 12.0).abs() < 1.5, "angle 1->2 = {a12}, want ~12");
    assert!((a02 - 24.0).abs() < 1.5, "angle 0->2 = {a02}, want ~24");

    // Focal within 5% of the true per-view focal.
    for i in 0..3 {
        let f = cam(i).focal_px;
        assert!(
            (f - FV).abs() < 0.05 * FV,
            "focal[{i}] = {f}, want within 5% of {FV}"
        );
    }

    assert!(reg.ba_converged, "bundle adjustment should converge");
    assert!(
        reg.ba_final_rms.is_finite() && reg.ba_final_rms < 0.02,
        "BA RMS should be small, got {}",
        reg.ba_final_rms
    );

    println!(
        "recovered: angles 0->1={a01:.3}° 1->2={a12:.3}° 0->2={a02:.3}°; \
         focals=[{:.1}, {:.1}, {:.1}] (true {FV}); ba_rms={:.2e}, converged={}",
        cam(0).focal_px,
        cam(1).focal_px,
        cam(2).focal_px,
        reg.ba_final_rms,
        reg.ba_converged
    );
}
