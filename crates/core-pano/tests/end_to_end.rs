//! Synthetic end-to-end registration test (Phase P1).
//!
//! Builds ground truth from the shared synthetic-scene machinery (`common`) — a procedurally textured
//! base image viewed by a virtual camera that yaws by a known amount for three overlapping frames —
//! and checks that `register()` recovers the relative yaw angles and the focal. Fully deterministic.

mod common;

use common::{angle_of, three_frames, FV};
use core_pano::{register, Phase};

#[test]
fn three_frame_rotating_camera_registers() {
    let frames = three_frames([1.0, 1.0, 1.0]);

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
