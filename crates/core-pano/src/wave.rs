//! Wave correction — a port of OpenCV `detail::waveCorrect` (WAVE_CORRECT_HORIZ).
//!
//! WHY: bundle adjustment fixes relative orientation but leaves the panorama free to "wave" (tilt/roll
//! drift) about the horizon, because that global degree of freedom is unconstrained. waveCorrect finds
//! the global up vector as the least-dominant direction of the cameras' X axes and rotates every pose
//! so that direction becomes vertical, flattening the horizon. Applied after BA.

use nalgebra::{Matrix3, Vector3};

/// Apply horizontal wave correction in place to a set of camera rotations.
pub(crate) fn wave_correct_horiz(rmats: &mut [Matrix3<f64>]) {
    if rmats.len() <= 1 {
        return;
    }

    // moment = Σ x_axis · x_axisᵀ, where x_axis = R.col(0).
    let mut moment = Matrix3::<f64>::zeros();
    for r in rmats.iter() {
        let col0 = r.column(0).into_owned();
        moment += col0 * col0.transpose();
    }

    // Global up = eigenvector of the least eigenvalue (the direction the X axes vary along least).
    let eig = moment.symmetric_eigen();
    let mut min_idx = 0;
    for i in 1..3 {
        if eig.eigenvalues[i] < eig.eigenvalues[min_idx] {
            min_idx = i;
        }
    }
    let mut rg1 = eig.eigenvectors.column(min_idx).into_owned();

    // img_k = Σ optical axes (R.col(2)); used to make rg0 point "right".
    let mut img_k = Vector3::<f64>::zeros();
    for r in rmats.iter() {
        img_k += r.column(2).into_owned();
    }

    let mut rg0 = rg1.cross(&img_k);
    let rg0_norm = rg0.norm();
    if rg0_norm <= f64::MIN_POSITIVE {
        return; // degenerate (X axes span the up direction) — leave poses unchanged
    }
    rg0 /= rg0_norm;

    // rg2 is computed before the sign disambiguation; flipping rg0 and rg1 together leaves rg2 valid
    // since (−rg0)×(−rg1) = rg0×rg1 (matches OpenCV, which does not recompute rg2 after the flip).
    let rg2 = rg0.cross(&rg1);

    let mut conf = 0.0;
    for r in rmats.iter() {
        conf += rg0.dot(&r.column(0));
    }
    if conf < 0.0 {
        rg0 = -rg0;
        rg1 = -rg1;
    }

    // Global correction: rows are the new basis; premultiply every pose.
    let r_global = Matrix3::new(
        rg0.x, rg0.y, rg0.z, rg1.x, rg1.y, rg1.z, rg2.x, rg2.y, rg2.z,
    );
    for r in rmats.iter_mut() {
        *r = r_global * *r;
    }
}
