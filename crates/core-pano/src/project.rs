//! Projection surface: auto-pick, canvas layout, and per-frame inverse warp (Phase P2).
//!
//! Convention (matches P1 throughout the crate): camera rotations are **world→camera** (`R_i`), so a
//! pixel `p` in frame `i` corresponds to the world ray `r = R_iᵀ·K_i⁻¹·[p,1]`, and the forward map is
//! `x_cam = K_i·R_i·r`, valid when `x_cam.z > 0`. `K_i = [[f,0,ppx],[0,f,ppy],[0,0,1]]`.
//!
//! The pano canvas is a lattice: canvas pixel column `cx` maps to the continuous pano coordinate
//! `u = cx + u_min` (analogously `v = cy + v_min`), where `(u,v)` are already at the final output
//! scale (px-per-radian `s` for spherical/cylindrical, reference-image pixels for perspective). All
//! three projections are expressed by a matching `project_ray` (ray → `(u,v)`) / `unproject`
//! (`(u,v)` → ray) pair so the warp can inverse-map every output pixel.

use nalgebra::{Matrix3, Vector3};
use rayon::prelude::*;

use crate::{Frame, Projection, Registration};

/// Border samples taken per frame edge when measuring the canvas extent / a frame's canvas rect.
const BORDER_SAMPLES_PER_EDGE: usize = 64;
/// Long side (px) targeted by the low-resolution warps used for exposure + seam finding.
pub(crate) const SEAM_LONG_SIDE: f64 = 1400.0;

/// A used camera resolved to the matrices the warp needs.
pub(crate) struct UsedCam {
    pub k: Matrix3<f64>,
    pub k_inv: Matrix3<f64>,
    pub r: Matrix3<f64>,  // world→camera
    pub rt: Matrix3<f64>, // camera→world (Rᵀ)
    pub focal: f64,
    pub w: usize,
    pub h: usize,
}

/// A single frame warped onto the canvas, stored only over its own bounding sub-rect to stream memory.
pub(crate) struct Warped {
    /// `(x0, y0, w, h)` in canvas pixel coordinates.
    pub rect: (usize, usize, usize, usize),
    /// Interleaved RGB over the sub-rect (`w*h*3`); camera-native linear.
    pub rgb: Vec<f32>,
    /// Feather weight over the sub-rect (`w*h`), 0 outside the source frame, ramping 0..1 from edges.
    pub weight: Vec<f32>,
}

impl Warped {
    /// Sample this warp at a canvas pixel; `None` if the pixel is outside the sub-rect.
    #[inline]
    pub(crate) fn sample(&self, cx: usize, cy: usize) -> Option<([f32; 3], f32)> {
        let (x0, y0, w, h) = self.rect;
        if cx < x0 || cy < y0 || cx >= x0 + w || cy >= y0 + h {
            return None;
        }
        let i = (cy - y0) * w + (cx - x0);
        Some((
            [self.rgb[i * 3], self.rgb[i * 3 + 1], self.rgb[i * 3 + 2]],
            self.weight[i],
        ))
    }
}

/// The resolved canvas mapping for one output resolution.
pub(crate) struct PanoMap {
    pub projection: Projection,
    /// Pixels per radian (spherical/cylindrical); unused by perspective, kept for reporting.
    pub s: f64,
    /// Reference optical-axis azimuth, used to recentre θ so the ±π wrap never splits the canvas.
    pub theta_ref: f64,
    pub r_ref: Matrix3<f64>,
    pub k_ref: Matrix3<f64>,
    pub k_ref_inv: Matrix3<f64>,
    pub u_min: f64,
    pub v_min: f64,
    pub width: usize,
    pub height: usize,
}

fn intr(f: f64, ppx: f64, ppy: f64) -> Matrix3<f64> {
    Matrix3::new(f, 0.0, ppx, 0.0, f, ppy, 0.0, 0.0, 1.0)
}

/// Scale a pinhole intrinsic uniformly (focal + principal point by `ratio`, bottom-right kept 1), so
/// the projected pixel coordinates scale by exactly `ratio`.
fn scale_k(k: &Matrix3<f64>, ratio: f64) -> Matrix3<f64> {
    intr(k[(0, 0)] * ratio, k[(0, 2)] * ratio, k[(1, 2)] * ratio)
}

/// Wrap an angle to `(-π, π]`.
#[inline]
fn wrap_pi(a: f64) -> f64 {
    use std::f64::consts::{PI, TAU};
    (a + PI).rem_euclid(TAU) - PI
}

impl PanoMap {
    /// Ray → `(u, v)` at the final output scale, before the canvas offset. `None` if the ray does not
    /// project (perspective points at/behind the reference plane; cylindrical looking straight up).
    fn project_ray(&self, r: &Vector3<f64>) -> Option<(f64, f64)> {
        match self.projection {
            Projection::Spherical => {
                let theta = wrap_pi(r.x.atan2(r.z) - self.theta_ref);
                let n = r.norm();
                if n < 1e-12 {
                    return None;
                }
                let phi = (r.y / n).clamp(-1.0, 1.0).asin();
                Some((theta * self.s, phi * self.s))
            }
            Projection::Cylindrical => {
                let theta = wrap_pi(r.x.atan2(r.z) - self.theta_ref);
                let hyp = (r.x * r.x + r.z * r.z).sqrt();
                if hyp < 1e-12 {
                    return None;
                }
                Some((theta * self.s, (r.y / hyp) * self.s))
            }
            Projection::Perspective => {
                let v = self.k_ref * (self.r_ref * r);
                if v.z <= 1e-9 {
                    return None;
                }
                Some((v.x / v.z, v.y / v.z))
            }
            Projection::Auto => None, // never stored resolved-as-Auto
        }
    }

    /// `(u, v)` (final scale, pre-offset) → world ray (not necessarily normalized).
    fn unproject(&self, u: f64, v: f64) -> Vector3<f64> {
        match self.projection {
            Projection::Spherical => {
                let theta = u / self.s + self.theta_ref;
                let phi = v / self.s;
                let (st, ct) = theta.sin_cos();
                let (sp, cp) = phi.sin_cos();
                Vector3::new(st * cp, sp, ct * cp)
            }
            Projection::Cylindrical => {
                let theta = u / self.s + self.theta_ref;
                let (st, ct) = theta.sin_cos();
                Vector3::new(st, v / self.s, ct)
            }
            Projection::Perspective | Projection::Auto => {
                self.r_ref.transpose() * (self.k_ref_inv * Vector3::new(u, v, 1.0))
            }
        }
    }

    /// Derive a lower-resolution map by scaling everything by `ratio` (≤ 1). Because every mapping is
    /// linear in the scale, this is exact — the lowres canvas registers pixel-for-pixel with the full
    /// one up to rounding.
    pub(crate) fn scaled(&self, ratio: f64) -> PanoMap {
        let k_ref = scale_k(&self.k_ref, ratio);
        PanoMap {
            projection: self.projection,
            s: self.s * ratio,
            theta_ref: self.theta_ref,
            r_ref: self.r_ref,
            k_ref,
            k_ref_inv: k_ref.try_inverse().unwrap_or_else(Matrix3::identity),
            u_min: self.u_min * ratio,
            v_min: self.v_min * ratio,
            width: (((self.width as f64) * ratio).round() as usize).max(1),
            height: (((self.height as f64) * ratio).round() as usize).max(1),
        }
    }
}

/// Resolve the used cameras (index-aligned with `reg.used_indices`).
pub(crate) fn build_used_cams(frames: &[Frame], reg: &Registration) -> Vec<UsedCam> {
    reg.used_indices
        .iter()
        .map(|&idx| {
            let cam = reg.cameras[idx]
                .as_ref()
                .expect("used frame must have a pose");
            let k = intr(cam.focal_px, cam.ppx, cam.ppy);
            UsedCam {
                k,
                k_inv: k.try_inverse().unwrap_or_else(Matrix3::identity),
                r: cam.rotation,
                rt: cam.rotation.transpose(),
                focal: cam.focal_px,
                w: frames[idx].width,
                h: frames[idx].height,
            }
        })
        .collect()
}

/// World optical axis (unit) of a camera: `Rᵀ·[0,0,1]`.
fn optical_axis(cam: &UsedCam) -> Vector3<f64> {
    let a = cam.rt * Vector3::new(0.0, 0.0, 1.0);
    let n = a.norm();
    if n > 1e-12 {
        a / n
    } else {
        Vector3::new(0.0, 0.0, 1.0)
    }
}

/// Approximate horizontal and vertical angular spans (degrees) covered by the used cameras.
///
/// Horizontal span = max pairwise optical-axis angle + widest per-frame horizontal FOV; vertical span
/// = pitch range of the axes + widest per-frame vertical FOV. This is a coarse heuristic (the pairwise
/// term folds in some vertical spread) but it is monotone in coverage, which is all the projection
/// auto-pick needs.
pub(crate) fn angular_spans(cams: &[UsedCam]) -> (f64, f64) {
    let axes: Vec<Vector3<f64>> = cams.iter().map(optical_axis).collect();

    let mut max_pair = 0.0f64;
    for i in 0..axes.len() {
        for j in (i + 1)..axes.len() {
            let d = axes[i].dot(&axes[j]).clamp(-1.0, 1.0);
            max_pair = max_pair.max(d.acos());
        }
    }

    let mut min_pitch = f64::INFINITY;
    let mut max_pitch = f64::NEG_INFINITY;
    for a in &axes {
        let pitch = a.y.clamp(-1.0, 1.0).asin();
        min_pitch = min_pitch.min(pitch);
        max_pitch = max_pitch.max(pitch);
    }
    let pitch_range = (max_pitch - min_pitch).max(0.0);

    let mut max_hfov = 0.0f64;
    let mut max_vfov = 0.0f64;
    for c in cams {
        max_hfov = max_hfov.max(2.0 * (c.w as f64 / (2.0 * c.focal)).atan());
        max_vfov = max_vfov.max(2.0 * (c.h as f64 / (2.0 * c.focal)).atan());
    }

    (
        (max_pair + max_hfov).to_degrees(),
        (pitch_range + max_vfov).to_degrees(),
    )
}

/// Pick the projection surface from the requested option and the measured spans.
///
/// Auto: Perspective if the horizontal span < 100°, else Cylindrical if the vertical span < 60°, else
/// Spherical. Explicit choices are honored, EXCEPT Perspective past 150° of horizontal span, where a
/// rectilinear projection blows up toward the frame edges (points approach the plane at infinity), so
/// we fall back to Cylindrical and report the substitution via `projection_used`.
pub(crate) fn resolve_projection(
    requested: Projection,
    h_span_deg: f64,
    v_span_deg: f64,
) -> Projection {
    match requested {
        Projection::Auto => {
            if h_span_deg < 100.0 {
                Projection::Perspective
            } else if v_span_deg < 60.0 {
                Projection::Cylindrical
            } else {
                Projection::Spherical
            }
        }
        Projection::Perspective if h_span_deg > 150.0 => Projection::Cylindrical,
        other => other,
    }
}

/// Median focal (px) across the used cameras — the px-per-radian scale, so output resolution ≈ source.
fn median_focal(cams: &[UsedCam]) -> f64 {
    let mut f: Vec<f64> = cams.iter().map(|c| c.focal).collect();
    f.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = f.len();
    if n == 0 {
        1.0
    } else if n % 2 == 1 {
        f[n / 2]
    } else {
        0.5 * (f[n / 2 - 1] + f[n / 2])
    }
}

/// Iterate the border pixels (all four edges) of a `w×h` frame rect.
fn border_pixels(w: usize, h: usize) -> impl Iterator<Item = (f64, f64)> {
    let (wf, hf) = ((w.max(1) - 1) as f64, (h.max(1) - 1) as f64);
    let n = BORDER_SAMPLES_PER_EDGE;
    (0..=n).flat_map(move |t| {
        let f = t as f64 / n as f64;
        [(f * wf, 0.0), (f * wf, hf), (0.0, f * hf), (wf, f * hf)].into_iter()
    })
}

/// Union of every used frame's projected border, in pre-offset `(u,v)` coordinates of `map`.
fn extent_of(cams: &[UsedCam], map: &PanoMap) -> Option<(f64, f64, f64, f64)> {
    let (mut umin, mut umax) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut vmin, mut vmax) = (f64::INFINITY, f64::NEG_INFINITY);
    for cam in cams {
        for (px, py) in border_pixels(cam.w, cam.h) {
            let ray = cam.rt * (cam.k_inv * Vector3::new(px, py, 1.0));
            if let Some((u, v)) = map.project_ray(&ray) {
                umin = umin.min(u);
                umax = umax.max(u);
                vmin = vmin.min(v);
                vmax = vmax.max(v);
            }
        }
    }
    if umin.is_finite() && umax > umin && vmax > vmin {
        Some((umin, umax, vmin, vmax))
    } else {
        None
    }
}

/// Build the full-resolution canvas map for `projection`, capping the long side to `max_long_side`.
/// Returns the map and whether the cap forced a downscale.
pub(crate) fn build_map(
    cams: &[UsedCam],
    ref_slot: usize,
    projection: Projection,
    max_long_side: u32,
) -> (PanoMap, bool) {
    let s_base = median_focal(cams);
    let ref_cam = &cams[ref_slot];
    let axis = optical_axis(ref_cam);
    let theta_ref = axis.x.atan2(axis.z);

    // Provisional map (no cap, no offset yet) just to measure the natural extent.
    let mut map = PanoMap {
        projection,
        s: s_base,
        theta_ref,
        r_ref: ref_cam.r,
        k_ref: ref_cam.k,
        k_ref_inv: ref_cam.k_inv,
        u_min: 0.0,
        v_min: 0.0,
        width: 0,
        height: 0,
    };

    let (umin0, umax0, vmin0, vmax0) = extent_of(cams, &map).unwrap_or((-1.0, 1.0, -1.0, 1.0));
    let long0 = (umax0 - umin0).max(vmax0 - vmin0);

    let capped = long0 > max_long_side as f64;
    let cap = if capped {
        max_long_side as f64 / long0
    } else {
        1.0
    };

    // Apply the cap uniformly (linear in scale ⇒ the extent just scales by `cap`).
    map.s = s_base * cap;
    map.k_ref = scale_k(&ref_cam.k, cap);
    map.k_ref_inv = map.k_ref.try_inverse().unwrap_or_else(Matrix3::identity);
    map.u_min = umin0 * cap;
    map.v_min = vmin0 * cap;
    map.width = (((umax0 - umin0) * cap).ceil() as usize).max(1);
    map.height = (((vmax0 - vmin0) * cap).ceil() as usize).max(1);

    (map, capped)
}

/// A used frame's bounding sub-rect on the canvas of `map` (via border sampling), clamped in-bounds.
fn frame_canvas_rect(cam: &UsedCam, map: &PanoMap) -> Option<(usize, usize, usize, usize)> {
    let (mut xmin, mut xmax) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut ymin, mut ymax) = (f64::INFINITY, f64::NEG_INFINITY);
    for (px, py) in border_pixels(cam.w, cam.h) {
        let ray = cam.rt * (cam.k_inv * Vector3::new(px, py, 1.0));
        if let Some((u, v)) = map.project_ray(&ray) {
            xmin = xmin.min(u - map.u_min);
            xmax = xmax.max(u - map.u_min);
            ymin = ymin.min(v - map.v_min);
            ymax = ymax.max(v - map.v_min);
        }
    }
    if !xmin.is_finite() {
        return None;
    }
    let x0 = (xmin.floor() as isize).clamp(0, map.width as isize - 1) as usize;
    let y0 = (ymin.floor() as isize).clamp(0, map.height as isize - 1) as usize;
    let x1 = (xmax.ceil() as isize).clamp(0, map.width as isize - 1) as usize;
    let y1 = (ymax.ceil() as isize).clamp(0, map.height as isize - 1) as usize;
    if x1 < x0 || y1 < y0 {
        return None;
    }
    Some((x0, y0, x1 - x0 + 1, y1 - y0 + 1))
}

/// Bilinear sample of an interleaved RGB frame at `(sx, sy)` (caller guarantees it is in-bounds).
#[inline]
fn bilinear_rgb(frame: &Frame, sx: f64, sy: f64) -> [f32; 3] {
    let x0 = sx.floor() as usize;
    let y0 = sy.floor() as usize;
    let x1 = (x0 + 1).min(frame.width - 1);
    let y1 = (y0 + 1).min(frame.height - 1);
    let fx = (sx - x0 as f64) as f32;
    let fy = (sy - y0 as f64) as f32;
    let mut out = [0.0f32; 3];
    let w = frame.width;
    for (ch, o) in out.iter_mut().enumerate() {
        let p00 = frame.rgb[(y0 * w + x0) * 3 + ch];
        let p10 = frame.rgb[(y0 * w + x1) * 3 + ch];
        let p01 = frame.rgb[(y1 * w + x0) * 3 + ch];
        let p11 = frame.rgb[(y1 * w + x1) * 3 + ch];
        let top = p00 + (p10 - p00) * fx;
        let bot = p01 + (p11 - p01) * fx;
        *o = top + (bot - top) * fy;
    }
    out
}

/// Inverse-warp one frame onto the canvas of `map`, returning `None` if it projects to nothing.
///
/// Each output pixel in the frame's sub-rect is mapped back through the pano surface to a source
/// pixel; in-bounds hits are bilinear-sampled and given a distance-to-edge feather weight (in source
/// pixels), normalized against `0.1·min(w,h)` and clamped to `0..1`.
pub(crate) fn warp(cam: &UsedCam, frame: &Frame, map: &PanoMap) -> Option<Warped> {
    let (x0, y0, rw, rh) = frame_canvas_rect(cam, map)?;
    let feather_denom = 0.1 * (cam.w.min(cam.h) as f64);
    let feather_denom = if feather_denom > 1e-6 {
        feather_denom
    } else {
        1.0
    };
    let (wf, hf) = ((cam.w - 1) as f64, (cam.h - 1) as f64);

    let mut rgb = vec![0.0f32; rw * rh * 3];
    let mut weight = vec![0.0f32; rw * rh];

    rgb.par_chunks_mut(rw * 3)
        .zip(weight.par_chunks_mut(rw))
        .enumerate()
        .for_each(|(ly, (row_rgb, row_w))| {
            let v = (y0 + ly) as f64 + map.v_min;
            for lx in 0..rw {
                let u = (x0 + lx) as f64 + map.u_min;
                let ray = map.unproject(u, v);
                let xc = cam.k * (cam.r * ray);
                if xc.z <= 1e-9 {
                    continue;
                }
                let sx = xc.x / xc.z;
                let sy = xc.y / xc.z;
                if sx < 0.0 || sy < 0.0 || sx > wf || sy > hf {
                    continue;
                }
                let c = bilinear_rgb(frame, sx, sy);
                row_rgb[lx * 3] = c[0];
                row_rgb[lx * 3 + 1] = c[1];
                row_rgb[lx * 3 + 2] = c[2];
                let dist = sx.min(sy).min(wf - sx).min(hf - sy);
                row_w[lx] = (dist / feather_denom).clamp(0.0, 1.0) as f32;
            }
        });

    Some(Warped {
        rect: (x0, y0, rw, rh),
        rgb,
        weight,
    })
}

/// Warp every used frame at a fixed low resolution (long side ≈ [`SEAM_LONG_SIDE`]) for exposure and
/// seam finding; the map itself is [`PanoMap::scaled`] from the full map.
pub(crate) fn warp_lowres(
    cams: &[UsedCam],
    frames: &[usize],
    all_frames: &[Frame],
    full_map: &PanoMap,
) -> (PanoMap, Vec<Option<Warped>>) {
    let long = full_map.width.max(full_map.height) as f64;
    let ratio = if long > SEAM_LONG_SIDE {
        SEAM_LONG_SIDE / long
    } else {
        1.0
    };
    let low_map = full_map.scaled(ratio);
    let warps = cams
        .iter()
        .zip(frames.iter())
        .map(|(cam, &fidx)| warp(cam, &all_frames[fidx], &low_map))
        .collect();
    (low_map, warps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nalgebra::Rotation3;

    /// A synthetic used camera looking at (`yaw`, `pitch`) degrees. `R` (world→camera) is built so the
    /// world optical axis `Rᵀ·e3` sits at that yaw/pitch.
    fn cam(yaw_deg: f64, pitch_deg: f64, focal: f64, w: usize, h: usize) -> UsedCam {
        let ry = Rotation3::from_axis_angle(&Vector3::y_axis(), yaw_deg.to_radians()).into_inner();
        let rx =
            Rotation3::from_axis_angle(&Vector3::x_axis(), pitch_deg.to_radians()).into_inner();
        let r = ry * rx;
        let k = intr(focal, w as f64 * 0.5, h as f64 * 0.5);
        UsedCam {
            k,
            k_inv: k.try_inverse().unwrap(),
            r,
            rt: r.transpose(),
            focal,
            w,
            h,
        }
    }

    #[test]
    fn auto_pick_small_span_is_perspective() {
        // Two cameras 8° apart, ~42° HFOV -> horizontal span < 100° -> Perspective.
        let cams = vec![
            cam(0.0, 0.0, 900.0, 700, 500),
            cam(8.0, 0.0, 900.0, 700, 500),
        ];
        let (h, v) = angular_spans(&cams);
        assert!(h < 100.0, "h_span = {h}");
        assert_eq!(
            resolve_projection(Projection::Auto, h, v),
            Projection::Perspective
        );
    }

    #[test]
    fn auto_pick_wide_single_row_is_cylindrical() {
        // Five cameras spanning 120° horizontally, all level -> wide but shallow -> Cylindrical.
        let cams: Vec<UsedCam> = [-60.0, -30.0, 0.0, 30.0, 60.0]
            .into_iter()
            .map(|y| cam(y, 0.0, 900.0, 700, 500))
            .collect();
        let (h, v) = angular_spans(&cams);
        assert!(h >= 100.0 && v < 60.0, "h_span = {h}, v_span = {v}");
        assert_eq!(
            resolve_projection(Projection::Auto, h, v),
            Projection::Cylindrical
        );
    }

    #[test]
    fn auto_pick_wide_two_row_is_spherical() {
        // Two rows (±20° pitch) across 120° of yaw -> deep + wide -> Spherical.
        let mut cams = Vec::new();
        for yaw in [-60.0, 0.0, 60.0] {
            for pitch in [20.0, -20.0] {
                cams.push(cam(yaw, pitch, 900.0, 700, 500));
            }
        }
        let (h, v) = angular_spans(&cams);
        assert!(h >= 100.0 && v >= 60.0, "h_span = {h}, v_span = {v}");
        assert_eq!(
            resolve_projection(Projection::Auto, h, v),
            Projection::Spherical
        );
    }

    #[test]
    fn explicit_wide_perspective_falls_back_to_cylindrical() {
        // Perspective requested past 150° of span -> rectilinear blows up -> Cylindrical.
        assert_eq!(
            resolve_projection(Projection::Perspective, 160.0, 40.0),
            Projection::Cylindrical
        );
        // ...but a moderate-span explicit Perspective is honored as-is.
        assert_eq!(
            resolve_projection(Projection::Perspective, 120.0, 40.0),
            Projection::Perspective
        );
    }
}
