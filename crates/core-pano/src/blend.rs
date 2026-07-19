//! Streaming multi-band (Laplacian pyramid) blend — Burt & Adelson splining, per Brown & Lowe.
//!
//! Each frame contributes, band by band, its gain-multiplied Laplacian weighted by a Gaussian-blurred
//! copy of its hard seam mask (the band-`k` weight is the mask downsampled through the same Gaussian
//! pyramid as the image, which is exactly the classic feathering-per-band trick). Contributions are
//! summed into canvas-sized accumulator pyramids; `finish` normalizes each band by its accumulated
//! weight, then collapses coarse→fine to the output image.
//!
//! **Streaming:** frames are added one at a time and their pyramids dropped immediately, so peak
//! memory is the canvas accumulators (Σ_bands (3 RGB + 1 weight) floats ≈ 5.3 floats/canvas-pixel) plus
//! a single frame's pyramids.
//!
//! **Sub-rect registration:** a frame's pyramid spans only its own canvas sub-rect, with the origin
//! aligned DOWN to a `2^(bands-1)` grid. Because the down/up-sampling rules are shared and every frame
//! starts from that aligned origin, band-`k` local pixel `(lx,ly)` lands at canvas band pixel
//! `(ox>>k + lx, oy>>k + ly)` — identical lattice for all frames, so the collapsed pyramid has no
//! ghost gridlines. Misaligning the origin here is what causes those artifacts, hence the care.
//!
//! **Pyramid ops:** separable 5-tap binomial `[1 4 6 4 1]` for both downsample (decimate by 2, edges
//! clamped) and the matching Burt expand (zero-insert + same kernel, normalized by the weights that
//! land on real samples, i.e. the ×2 gain). Building the Laplacian and collapsing use the *same*
//! expand, so a single unmasked frame reconstructs to its own image exactly (the pyramid telescopes).

use rayon::prelude::*;

use crate::project::Warped;
use crate::seam::IdMap;

/// Number of pyramid levels (band 0 = full res, coarsest = 1/16).
const BANDS: usize = 5;
/// Weight below which an accumulator pixel is treated as empty.
const EPS: f32 = 1e-6;

/// A single-channel image plane.
struct Plane {
    data: Vec<f32>,
    w: usize,
    h: usize,
}

impl Plane {
    fn zeros(w: usize, h: usize) -> Plane {
        Plane {
            data: vec![0.0; w * h],
            w,
            h,
        }
    }
}

/// Streaming multi-band blender over a fixed canvas.
pub(crate) struct Blender {
    canvas_w: usize,
    canvas_h: usize,
    band_w: [usize; BANDS],
    band_h: [usize; BANDS],
    /// Per band: interleaved-RGB weighted-Laplacian sum (`band_w*band_h*3`).
    acc_rgb: Vec<Vec<f32>>,
    /// Per band: weight sum (`band_w*band_h`).
    acc_w: Vec<Vec<f32>>,
}

impl Blender {
    pub(crate) fn new(canvas_w: usize, canvas_h: usize) -> Blender {
        let mut band_w = [0usize; BANDS];
        let mut band_h = [0usize; BANDS];
        let (mut w, mut h) = (canvas_w.max(1), canvas_h.max(1));
        for k in 0..BANDS {
            band_w[k] = w;
            band_h[k] = h;
            w = w.div_ceil(2);
            h = h.div_ceil(2);
        }
        let acc_rgb = (0..BANDS)
            .map(|k| vec![0.0f32; band_w[k] * band_h[k] * 3])
            .collect();
        let acc_w = (0..BANDS)
            .map(|k| vec![0.0f32; band_w[k] * band_h[k]])
            .collect();
        Blender {
            canvas_w,
            canvas_h,
            band_w,
            band_h,
            acc_rgb,
            acc_w,
        }
    }

    /// Add one warped frame owned (per the seam id map) by `slot`, multiplying its RGB by `gain`.
    pub(crate) fn add(&mut self, warped: &Warped, slot: usize, seams: &IdMap, gain: [f32; 3]) {
        let (rx0, ry0, rw, rh) = warped.rect;
        // Align the sub-rect origin down to the coarsest band's grid so all bands register.
        let align = 1usize << (BANDS - 1);
        let ox = (rx0 / align) * align;
        let oy = (ry0 / align) * align;
        // Local plane spans from the aligned origin, padded up to the alignment grid.
        let lw = ((rx0 + rw - ox).div_ceil(align)) * align;
        let lh = ((ry0 + rh - oy).div_ceil(align)) * align;

        // Band 0: gain-multiplied RGB planes + hard seam mask (0/1), over the local sub-rect.
        let mut r = Plane::zeros(lw, lh);
        let mut g = Plane::zeros(lw, lh);
        let mut b = Plane::zeros(lw, lh);
        let mut mask = Plane::zeros(lw, lh);
        for fy in 0..rh {
            for fx in 0..rw {
                let (cx, cy) = (rx0 + fx, ry0 + fy);
                let i = fy * rw + fx;
                if warped.weight[i] <= 0.0 {
                    continue;
                }
                let (lx, ly) = (cx - ox, cy - oy);
                let li = ly * lw + lx;
                r.data[li] = warped.rgb[i * 3] * gain[0];
                g.data[li] = warped.rgb[i * 3 + 1] * gain[1];
                b.data[li] = warped.rgb[i * 3 + 2] * gain[2];
                if seams.owner_at(cx, cy, self.canvas_w, self.canvas_h) == slot as u8 {
                    mask.data[li] = 1.0;
                }
            }
        }

        // Gaussian pyramids of the three colour planes and the mask.
        let gp_r = gaussian_pyramid(r);
        let gp_g = gaussian_pyramid(g);
        let gp_b = gaussian_pyramid(b);
        let gp_m = gaussian_pyramid(mask);
        // Laplacian of each colour channel (top band stays Gaussian).
        let lp_r = laplacian_from(&gp_r);
        let lp_g = laplacian_from(&gp_g);
        let lp_b = laplacian_from(&gp_b);

        // Accumulate mask-weighted Laplacian into the canvas band accumulators at the aligned offset.
        for k in 0..BANDS {
            let cbw = self.band_w[k];
            let cbh = self.band_h[k];
            let bw = lp_r[k].w;
            let bh = lp_r[k].h;
            let offx = ox >> k;
            let offy = oy >> k;
            let acc_rgb = &mut self.acc_rgb[k];
            let acc_w = &mut self.acc_w[k];
            let (lr, lg, lb, wm) = (&lp_r[k], &lp_g[k], &lp_b[k], &gp_m[k]);
            for ly in 0..bh {
                let cy = offy + ly;
                if cy >= cbh {
                    break;
                }
                for lx in 0..bw {
                    let cx = offx + lx;
                    if cx >= cbw {
                        break;
                    }
                    let m = wm.data[ly * bw + lx];
                    if m <= 0.0 {
                        continue;
                    }
                    let li = ly * bw + lx;
                    let ci = cy * cbw + cx;
                    acc_rgb[ci * 3] += m * lr.data[li];
                    acc_rgb[ci * 3 + 1] += m * lg.data[li];
                    acc_rgb[ci * 3 + 2] += m * lb.data[li];
                    acc_w[ci] += m;
                }
            }
        }
    }

    /// Normalize each band by its weight, collapse coarse→fine, and return `(rgb, valid)`.
    pub(crate) fn finish(self) -> (Vec<f32>, Vec<u8>) {
        // valid = band-0 coverage union (pre-normalization weight sum > EPS).
        let valid: Vec<u8> = self.acc_w[0]
            .iter()
            .map(|&w| if w > EPS { 1 } else { 0 })
            .collect();

        // Normalize the coarsest band into the working result planes.
        let top = BANDS - 1;
        let (mut rr, mut rg, mut rb) = normalize_band(
            &self.acc_rgb[top],
            &self.acc_w[top],
            self.band_w[top],
            self.band_h[top],
        );

        for k in (0..top).rev() {
            let (tw, th) = (self.band_w[k], self.band_h[k]);
            rr = add_planes(
                &expand(&rr, tw, th),
                &normalize_channel(&self.acc_rgb[k], &self.acc_w[k], tw, th, 0),
            );
            rg = add_planes(
                &expand(&rg, tw, th),
                &normalize_channel(&self.acc_rgb[k], &self.acc_w[k], tw, th, 1),
            );
            rb = add_planes(
                &expand(&rb, tw, th),
                &normalize_channel(&self.acc_rgb[k], &self.acc_w[k], tw, th, 2),
            );
        }

        let n = self.canvas_w * self.canvas_h;
        let mut rgb = vec![0.0f32; n * 3];
        for i in 0..n {
            rgb[i * 3] = rr.data[i].max(0.0);
            rgb[i * 3 + 1] = rg.data[i].max(0.0);
            rgb[i * 3 + 2] = rb.data[i].max(0.0);
        }
        (rgb, valid)
    }
}

/// Normalize a whole band's interleaved-RGB accumulator by its weight into three planes.
fn normalize_band(acc_rgb: &[f32], acc_w: &[f32], w: usize, h: usize) -> (Plane, Plane, Plane) {
    (
        normalize_channel(acc_rgb, acc_w, w, h, 0),
        normalize_channel(acc_rgb, acc_w, w, h, 1),
        normalize_channel(acc_rgb, acc_w, w, h, 2),
    )
}

/// Normalize one channel of an interleaved-RGB accumulator by the shared weight.
fn normalize_channel(acc_rgb: &[f32], acc_w: &[f32], w: usize, h: usize, ch: usize) -> Plane {
    let mut p = Plane::zeros(w, h);
    for i in 0..w * h {
        let wt = acc_w[i];
        p.data[i] = if wt > EPS {
            acc_rgb[i * 3 + ch] / wt
        } else {
            0.0
        };
    }
    p
}

fn add_planes(a: &Plane, b: &Plane) -> Plane {
    let mut out = Plane::zeros(a.w, a.h);
    for i in 0..a.data.len() {
        out.data[i] = a.data[i] + b.data[i];
    }
    out
}

/// Build a `BANDS`-level Gaussian pyramid (level 0 = input, consumed).
fn gaussian_pyramid(base: Plane) -> Vec<Plane> {
    let mut levels = Vec::with_capacity(BANDS);
    levels.push(base);
    for k in 1..BANDS {
        levels.push(downsample(&levels[k - 1]));
    }
    levels
}

/// Laplacian pyramid from a Gaussian pyramid: `L_k = G_k − expand(G_{k+1})`, top band = `G_top`.
fn laplacian_from(gp: &[Plane]) -> Vec<Plane> {
    let mut out = Vec::with_capacity(gp.len());
    for k in 0..gp.len() - 1 {
        let up = expand(&gp[k + 1], gp[k].w, gp[k].h);
        let mut l = Plane::zeros(gp[k].w, gp[k].h);
        for i in 0..l.data.len() {
            l.data[i] = gp[k].data[i] - up.data[i];
        }
        out.push(l);
    }
    // Coarsest band kept as-is.
    let top = &gp[gp.len() - 1];
    out.push(Plane {
        data: top.data.clone(),
        w: top.w,
        h: top.h,
    });
    out
}

const KERNEL: [f32; 5] = [1.0, 4.0, 6.0, 4.0, 1.0];

/// Separable binomial downsample by 2 (edges clamped). Output size = `ceil(n/2)` per axis.
fn downsample(src: &Plane) -> Plane {
    let ow = src.w.div_ceil(2);
    let oh = src.h.div_ceil(2);
    // Horizontal pass: (src.w -> ow), height unchanged.
    let mut tmp = vec![0.0f32; ow * src.h];
    tmp.par_chunks_mut(ow).enumerate().for_each(|(y, row)| {
        for (ox, o) in row.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for t in 0..5 {
                let sx = (2 * ox as isize + t - 2).clamp(0, src.w as isize - 1) as usize;
                acc += KERNEL[t as usize] * src.data[y * src.w + sx];
            }
            *o = acc / 16.0;
        }
    });
    // Vertical pass: (src.h -> oh).
    let mut out = Plane::zeros(ow, oh);
    out.data
        .par_chunks_mut(ow)
        .enumerate()
        .for_each(|(oy, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                let mut acc = 0.0f32;
                for t in 0..5 {
                    let sy = (2 * oy as isize + t - 2).clamp(0, src.h as isize - 1) as usize;
                    acc += KERNEL[t as usize] * tmp[sy * ow + x];
                }
                *o = acc / 16.0;
            }
        });
    out
}

/// Burt expand to an explicit target size `(tw, th)` — the matching inverse of [`downsample`]
/// (zero-insert + the same kernel, normalized by the weights that land on real samples).
fn expand(src: &Plane, tw: usize, th: usize) -> Plane {
    // Horizontal pass: (src.w -> tw), height unchanged.
    let mut tmp = vec![0.0f32; tw * src.h];
    tmp.par_chunks_mut(tw).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            *o = expand_tap(&src.data[y * src.w..y * src.w + src.w], x);
        }
    });
    // Vertical pass: (src.h -> th).
    let mut out = Plane::zeros(tw, th);
    // Transpose-free vertical expand: gather along columns.
    out.data
        .par_chunks_mut(tw)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                *o = expand_col_tap(&tmp, tw, src.h, x, y);
            }
        });
    out
}

/// One expand tap over a 1-D row `src` at destination index `x`.
#[inline]
fn expand_tap(src: &[f32], x: usize) -> f32 {
    let mut acc = 0.0f32;
    let mut wsum = 0.0f32;
    for t in 0..5i64 {
        let idx = x as i64 - (t - 2); // index into the zero-inserted signal
        if idx >= 0 && idx % 2 == 0 {
            let si = (idx / 2) as usize;
            if si < src.len() {
                acc += KERNEL[t as usize] * src[si];
                wsum += KERNEL[t as usize];
            }
        }
    }
    if wsum > 0.0 {
        acc / wsum
    } else {
        0.0
    }
}

/// One expand tap along column `x` of the horizontally-expanded buffer `tmp` (width `tw`, height `sh`)
/// at destination row `y`.
#[inline]
fn expand_col_tap(tmp: &[f32], tw: usize, sh: usize, x: usize, y: usize) -> f32 {
    let mut acc = 0.0f32;
    let mut wsum = 0.0f32;
    for t in 0..5i64 {
        let idx = y as i64 - (t - 2);
        if idx >= 0 && idx % 2 == 0 {
            let si = (idx / 2) as usize;
            if si < sh {
                acc += KERNEL[t as usize] * tmp[si * tw + x];
                wsum += KERNEL[t as usize];
            }
        }
    }
    if wsum > 0.0 {
        acc / wsum
    } else {
        0.0
    }
}
