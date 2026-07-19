//! Global bundle adjustment: hand-rolled Levenberg–Marquardt over per-camera rotation (axis-angle)
//! and log-focal, minimizing ray-space reprojection error between matched inliers.
//!
//! Parameterization follows Brown & Lowe: each used camera contributes 3 axis-angle rotation params
//! and 1 log-focal, EXCEPT the reference camera whose rotation is pinned to identity (its 3 rotation
//! params are removed from the state, gauge-fixing the global orientation) while its focal stays free.
//!
//! Residual per correspondence `p_i ↔ q_j`: the unit rays `Rᵢᵀ·Kᵢ⁻¹·[p,1]` and `Rⱼᵀ·Kⱼ⁻¹·[q,1]`
//! should coincide, so the residual is their 3-vector difference, Huber-weighted (δ = 0.01 rad) to
//! resist surviving mismatches. The Jacobian is numeric (forward difference) — exact enough at this
//! scale (≤ ~40 params, a few thousand rows) and far simpler than analytic so(3) derivatives.

use nalgebra::{Cholesky, DMatrix, DVector, Matrix3, Rotation3, Vector3};

use crate::camera::CameraInit;

/// Huber threshold in ray-difference units (radians-ish).
const HUBER_DELTA: f64 = 0.01;
/// Forward-difference step, relative to each parameter magnitude.
const FD_EPS: f64 = 1e-6;
const MAX_LM_ITERS: usize = 50;
/// Relative cost-change convergence tolerance.
const COST_TOL: f64 = 1e-8;

/// One matched inlier, with camera indices *local* to the used-camera arrays.
pub(crate) struct Obs {
    pub cam_i: usize,
    pub cam_j: usize,
    pub p: (f64, f64),
    pub q: (f64, f64),
}

pub(crate) struct BaResult {
    /// Refined rotation per used camera (index-aligned with the `used` slice).
    pub rotations: Vec<Matrix3<f64>>,
    /// Refined focal per used camera.
    pub focals: Vec<f64>,
    pub final_rms: f64,
    /// False if BA diverged (NaN / no valid step); seed poses are returned unchanged in that case.
    pub converged: bool,
}

/// Precomputed per-camera state used inside a residual evaluation.
struct CamState {
    rt: Matrix3<f64>,   // Rᵀ
    kinv: Matrix3<f64>, // K⁻¹
}

fn kinv(focal: f64, ppx: f64, ppy: f64) -> Matrix3<f64> {
    let inv_f = 1.0 / focal;
    Matrix3::new(
        inv_f,
        0.0,
        -ppx * inv_f,
        0.0,
        inv_f,
        -ppy * inv_f,
        0.0,
        0.0,
        1.0,
    )
}

/// Run bundle adjustment. `seed_cams`, the returned rotations/focals, and the local camera indices in
/// `obs` are all aligned with `used` (0-based, in the same order).
pub(crate) fn bundle_adjust(
    used: &[usize],
    reference: usize,
    seed_cams: &[CameraInit],
    obs: &[Obs],
) -> BaResult {
    let n_cam = used.len();
    let pp: Vec<(f64, f64)> = seed_cams.iter().map(|c| (c.ppx, c.ppy)).collect();
    let ref_local = used.iter().position(|&f| f == reference).unwrap_or(0);

    // Build parameter layout and initial state x.
    let mut rot_off: Vec<Option<usize>> = vec![None; n_cam];
    let mut foc_off: Vec<usize> = vec![0; n_cam];
    let mut x: Vec<f64> = Vec::new();
    for (local, cam) in seed_cams.iter().enumerate() {
        if local != ref_local {
            rot_off[local] = Some(x.len());
            let rvec = Rotation3::from_matrix_unchecked(cam.rotation).scaled_axis();
            x.push(rvec.x);
            x.push(rvec.y);
            x.push(rvec.z);
        }
        foc_off[local] = x.len();
        x.push(cam.focal.max(1e-6).ln());
    }

    let eval = |x: &[f64]| -> Vec<f64> {
        // Per-camera Rᵀ and K⁻¹ at these parameters.
        let mut states = Vec::with_capacity(n_cam);
        for local in 0..n_cam {
            let focal = x[foc_off[local]].exp();
            let r = match rot_off[local] {
                Some(o) => {
                    Rotation3::from_scaled_axis(Vector3::new(x[o], x[o + 1], x[o + 2])).into_inner()
                }
                None => Matrix3::identity(),
            };
            states.push(CamState {
                rt: r.transpose(),
                kinv: kinv(focal, pp[local].0, pp[local].1),
            });
        }
        let mut res = Vec::with_capacity(obs.len() * 3);
        for o in obs {
            let si = &states[o.cam_i];
            let sj = &states[o.cam_j];
            let ray_i = ray(&si.rt, &si.kinv, o.p.0, o.p.1);
            let ray_j = ray(&sj.rt, &sj.kinv, o.q.0, o.q.1);
            res.push(ray_i.x - ray_j.x);
            res.push(ray_i.y - ray_j.y);
            res.push(ray_i.z - ray_j.z);
        }
        res
    };

    let seed_raw = eval(&x);
    let seed_rms = rms(&seed_raw);

    // Bail out gracefully if the seed is already unusable (no observations or non-finite geometry).
    if obs.is_empty() || !seed_rms.is_finite() {
        return seed_result(seed_cams, seed_rms, false);
    }

    let n_params = x.len();
    let mut lambda = 1e-3;
    let mut converged_normally = true;

    for _iter in 0..MAX_LM_ITERS {
        let raw = eval(&x);
        let wfac = huber_weights(&raw);
        let f = apply_weights(&raw, &wfac); // weighted residuals at x
        let mut cost: f64 = f.iter().map(|v| v * v).sum();

        // Numeric Jacobian of the weighted residuals (weights fixed at current x — IRLS step).
        let n_rows = f.len();
        let mut jac = DMatrix::<f64>::zeros(n_rows, n_params);
        for p in 0..n_params {
            let step = FD_EPS * (1.0 + x[p].abs());
            let mut xp = x.clone();
            xp[p] += step;
            let raw_p = eval(&xp);
            let fp = apply_weights(&raw_p, &wfac);
            for r in 0..n_rows {
                jac[(r, p)] = (fp[r] - f[r]) / step;
            }
        }

        let fvec = DVector::from_vec(f.clone());
        let jt = jac.transpose();
        let h = &jt * &jac;
        let g = &jt * &fvec; // gradient direction J^T f

        // Inner LM loop: damp until a step decreases the (recomputed) Huber cost.
        let mut stepped = false;
        loop {
            let mut damped = h.clone();
            for d in 0..n_params {
                damped[(d, d)] += lambda;
            }
            let delta = match Cholesky::new(damped) {
                Some(chol) => chol.solve(&(-&g)),
                None => {
                    lambda *= 10.0;
                    if lambda > 1e12 {
                        break;
                    }
                    continue;
                }
            };

            let mut x_new = x.clone();
            for p in 0..n_params {
                x_new[p] += delta[p];
            }
            let raw_new = eval(&x_new);
            let new_cost = weighted_cost(&raw_new);

            if new_cost.is_finite() && new_cost < cost {
                let rel = (cost - new_cost) / cost.max(1e-300);
                x = x_new;
                cost = new_cost;
                lambda = (lambda / 10.0).max(1e-12);
                stepped = true;
                if rel < COST_TOL {
                    // Converged.
                    return finalize(
                        &x, &rot_off, &foc_off, n_cam, seed_cams, &eval, seed_rms, true,
                    );
                }
                break;
            } else {
                lambda *= 10.0;
                if lambda > 1e12 {
                    break;
                }
            }
        }

        if !stepped {
            // Could not improve further; treat as converged (a local minimum) unless cost is bad.
            converged_normally = cost.is_finite();
            break;
        }
    }

    finalize(
        &x,
        &rot_off,
        &foc_off,
        n_cam,
        seed_cams,
        &eval,
        seed_rms,
        converged_normally,
    )
}

/// Unit ray in world space for pixel `(x, y)`: `normalize(Rᵀ · K⁻¹ · [x, y, 1])`.
fn ray(rt: &Matrix3<f64>, kinv: &Matrix3<f64>, x: f64, y: f64) -> Vector3<f64> {
    let d = rt * (kinv * Vector3::new(x, y, 1.0));
    let n = d.norm();
    if n > 1e-12 {
        d / n
    } else {
        d
    }
}

/// Per-correspondence Huber IRLS weight factor (applied to each of the 3 residual components).
fn huber_weights(raw: &[f64]) -> Vec<f64> {
    let mut w = Vec::with_capacity(raw.len() / 3);
    for chunk in raw.chunks_exact(3) {
        let e = (chunk[0] * chunk[0] + chunk[1] * chunk[1] + chunk[2] * chunk[2]).sqrt();
        w.push(if e <= HUBER_DELTA {
            1.0
        } else {
            (HUBER_DELTA / e).sqrt()
        });
    }
    w
}

fn apply_weights(raw: &[f64], wfac: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(raw.len());
    for (c, chunk) in raw.chunks_exact(3).enumerate() {
        let w = wfac[c];
        out.push(chunk[0] * w);
        out.push(chunk[1] * w);
        out.push(chunk[2] * w);
    }
    out
}

fn weighted_cost(raw: &[f64]) -> f64 {
    let wfac = huber_weights(raw);
    apply_weights(raw, &wfac).iter().map(|v| v * v).sum()
}

/// RMS of the *unweighted* per-correspondence ray difference (for reporting / comparison).
fn rms(raw: &[f64]) -> f64 {
    if raw.is_empty() {
        return 0.0;
    }
    let n = (raw.len() / 3) as f64;
    let sum: f64 = raw
        .chunks_exact(3)
        .map(|c| c[0] * c[0] + c[1] * c[1] + c[2] * c[2])
        .sum();
    (sum / n).sqrt()
}

#[allow(clippy::too_many_arguments)]
fn finalize<F: Fn(&[f64]) -> Vec<f64>>(
    x: &[f64],
    rot_off: &[Option<usize>],
    foc_off: &[usize],
    n_cam: usize,
    seed_cams: &[CameraInit],
    eval: &F,
    seed_rms: f64,
    converged: bool,
) -> BaResult {
    let final_raw = eval(x);
    let final_rms = rms(&final_raw);
    if !final_rms.is_finite() {
        return seed_result(seed_cams, seed_rms, false);
    }
    let mut rotations = Vec::with_capacity(n_cam);
    let mut focals = Vec::with_capacity(n_cam);
    for local in 0..n_cam {
        let focal = x[foc_off[local]].exp();
        let r = match rot_off[local] {
            Some(o) => {
                Rotation3::from_scaled_axis(Vector3::new(x[o], x[o + 1], x[o + 2])).into_inner()
            }
            None => Matrix3::identity(),
        };
        rotations.push(r);
        focals.push(focal);
    }
    BaResult {
        rotations,
        focals,
        final_rms,
        converged,
    }
}

fn seed_result(seed_cams: &[CameraInit], seed_rms: f64, converged: bool) -> BaResult {
    BaResult {
        rotations: seed_cams.iter().map(|c| c.rotation).collect(),
        focals: seed_cams.iter().map(|c| c.focal).collect(),
        final_rms: seed_rms,
        converged,
    }
}
