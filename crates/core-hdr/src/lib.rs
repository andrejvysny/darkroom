//! core-hdr — exposure-bracket merge math (Merge to HDR, tripod v1).
//!
//! Pure CPU math over [`core_raw::LinearImage`]s: no rawler, no DB, no GPU. The caller (src-tauri
//! `hdr_merge`) decodes frames via `core_raw::develop_linear_wb` (all frames with the REFERENCE
//! frame's white balance), computes per-frame EV from numeric EXIF, and streams frames through a
//! [`MergeAccumulator`]; the result is a scene-linear ProPhoto image whose brightness matches the
//! reference frame, with clipped-in-the-reference highlights reconstructed from the darker frames
//! as >1.0 headroom.
//!
//! The model (Lightroom-style, simplified because raw data is already linear): no radiometric
//! response recovery is needed — scale each frame's radiance by its exposure ratio to the
//! reference (`2^(EV_frame − EV_ref)`; a brighter-exposed frame scales DOWN), then average with
//! per-pixel confidence weights that zero out near-clipped pixels and down-weight noisy shadows.
//!
//! Two merge paths share the same weighted accumulator:
//! - **Tripod** ([`MergeAccumulator::new`] + [`MergeAccumulator::add_frame`]): frames must already be
//!   pixel-aligned and same-sized; every pixel of every frame is trusted.
//! - **Hand-held** ([`MergeAccumulator::with_reference`] + [`MergeAccumulator::add_frame_masked`]):
//!   the caller aligns each frame to the reference (`core_pano::align` → [`warp_into_reference`]),
//!   passes the warp's validity mask (out-of-frame borders are *skipped*, not floored), and the
//!   accumulator down-weights pixels that disagree with the reference radiance (deghosting — moving
//!   subjects, wind, water) so the merge follows the one chosen exposure instead of ghosting.

use core_raw::LinearImage;
use thiserror::Error;

mod warp;
pub use warp::warp_into_reference;

#[derive(Debug, Error)]
pub enum HdrError {
    #[error("invalid exposure data: {0}")]
    InvalidExposure(String),
    #[error("frame size {got_w}x{got_h} differs from first frame {want_w}x{want_h} — merge needs a tripod bracket from one body/lens")]
    SizeMismatch {
        want_w: u32,
        want_h: u32,
        got_w: u32,
        got_h: u32,
    },
    #[error("frame buffer length does not match its dimensions")]
    MalformedFrame,
    #[error("merge needs at least 2 frames")]
    TooFewFrames,
}

/// Numeric exposure settings of one frame (from `core_raw::read_exposure_numeric`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExposureInfo {
    pub exposure_time_s: f64,
    pub f_number: f64,
    pub iso: f64,
}

/// EV at ISO 100: `log2(N²/t · 100/ISO)`. Higher EV₁₀₀ = LESS light captured (darker frame).
pub fn ev100(e: &ExposureInfo) -> Result<f64, HdrError> {
    if e.exposure_time_s <= 0.0 || e.f_number <= 0.0 || e.iso <= 0.0 {
        return Err(HdrError::InvalidExposure(format!(
            "t={} N={} ISO={}",
            e.exposure_time_s, e.f_number, e.iso
        )));
    }
    Ok(((e.f_number * e.f_number / e.exposure_time_s) * (100.0 / e.iso)).log2())
}

/// Radiance scale that normalizes `frame` onto `reference`'s exposure: `2^(EV_frame − EV_ref)`.
/// A frame exposed 1 EV brighter than the reference (lower EV₁₀₀) gets scale 0.5, so the merged
/// output's brightness ≈ the reference frame's.
pub fn relative_scale(frame: &ExposureInfo, reference: &ExposureInfo) -> Result<f64, HdrError> {
    Ok((ev100(frame)? - ev100(reference)?).exp2())
}

/// Pick the reference (metered) frame: median EV₁₀₀ (lower-middle for even counts) — the middle
/// exposure of a bracket, matching Lightroom's choice. Returns an index into `exposures`.
pub fn reference_index(exposures: &[ExposureInfo]) -> Result<usize, HdrError> {
    if exposures.len() < 2 {
        return Err(HdrError::TooFewFrames);
    }
    let mut evs: Vec<(usize, f64)> = exposures
        .iter()
        .enumerate()
        .map(|(i, e)| ev100(e).map(|v| (i, v)))
        .collect::<Result<_, _>>()?;
    evs.sort_by(|a, b| a.1.total_cmp(&b.1));
    Ok(evs[(evs.len() - 1) / 2].0)
}

// Weight shape (over each frame's OWN unscaled values, where ≈1.0 is the raw clip point after
// white-level rescale):
/// Full confidence below this; fades to zero at [`W_HIGH_ZERO`] (near-clip pixels excluded — the
/// margin also absorbs WB-gain wobble that can push clipped photosites above/below exactly 1.0).
const W_HIGH_FULL: f32 = 0.75;
const W_HIGH_ZERO: f32 = 0.9;
/// Below this the weight ramps down (shadow noise), floored at [`W_LOW_FLOOR`] so the weighted sum
/// never degenerates for pixels that are dark in every frame.
const W_LOW_KNEE: f32 = 0.10;
const W_LOW_FLOOR: f32 = 0.05;

/// Per-pixel confidence from the frame's own unscaled max-RGB `m`. One weight per pixel (not per
/// channel) — per-channel weights fringe at clip boundaries.
#[inline]
fn hat_weight(m: f32) -> f32 {
    let w_high = ((W_HIGH_ZERO - m) / (W_HIGH_ZERO - W_HIGH_FULL)).clamp(0.0, 1.0);
    let w_low = (m / W_LOW_KNEE).clamp(W_LOW_FLOOR, 1.0);
    w_high * w_low
}

/// Deghosting tuning for the hand-held path. A warped frame's per-pixel weight is scaled by its
/// radiance *consistency* with the reference: `consist = exp(−(d/denom)²)` where `d` is the sum of
/// absolute per-channel differences (moving frame vs reference, reference-exposure scale) and
/// `denom = sigma + k·max(reference)`. Both knobs are in **linear radiance** at the reference
/// exposure — not display units.
#[derive(Debug, Clone, Copy)]
pub struct DeghostParams {
    /// Consistency tolerance at black (absolute radiance noise floor). Larger = more forgiving.
    pub sigma: f32,
    /// Fractional tolerance that grows with brightness, so bright regions get a proportional (not
    /// absolute) allowance before they read as "moved".
    pub k: f32,
}

impl Default for DeghostParams {
    fn default() -> Self {
        DeghostParams {
            sigma: 0.05,
            k: 0.25,
        }
    }
}

/// Streaming exposure-weighted merge accumulator. Holds three flat buffers (7 f32/pixel total —
/// ≈0.9 GB at 33 MP) regardless of frame count — plus one resident copy of the reference (+3 f32/px)
/// on the hand-held path; callers decode → `add_frame`/`add_frame_masked` → drop each frame.
pub struct MergeAccumulator {
    width: u32,
    height: u32,
    /// Σ w·s·x per channel.
    sum: Vec<f32>,
    /// Σ w (single channel).
    wsum: Vec<f32>,
    /// The scaled shortest exposure (highest EV₁₀₀) — sole source for pixels clipped in EVERY
    /// frame, where the weighted sum is empty.
    fallback: Vec<f32>,
    frames: usize,
    /// Reference radiance (reference-exposure scale, 3 f32/px), held on the hand-held path so each
    /// subsequent frame can be deghosted against it. `None` on the tripod path (no deghosting).
    reference: Option<Vec<f32>>,
    /// Deghost tuning; consulted only when `reference` is `Some`.
    deghost: DeghostParams,
}

impl MergeAccumulator {
    /// Tripod accumulator: no reference held, no deghosting. Every pixel of every `add_frame` is
    /// trusted (frames must already be pixel-aligned).
    pub fn new(width: u32, height: u32) -> Self {
        let n = width as usize * height as usize;
        MergeAccumulator {
            width,
            height,
            sum: vec![0.0; n * 3],
            wsum: vec![0.0; n],
            fallback: vec![0.0; n * 3],
            frames: 0,
            reference: None,
            deghost: DeghostParams::default(),
        }
    }

    /// Hand-held accumulator seeded with the (already-decoded) `reference` frame, which is both the
    /// deghosting anchor and the first accumulated frame (at scale 1.0 — it *is* the reference
    /// exposure, so `frames == 1` on return). Subsequent frames are added via [`add_frame_masked`]
    /// (aligned + warped) and deghosted against this reference.
    ///
    /// [`add_frame_masked`]: Self::add_frame_masked
    pub fn with_reference(
        reference: &LinearImage,
        ref_is_shortest: bool,
        params: DeghostParams,
    ) -> Result<Self, HdrError> {
        let mut acc = MergeAccumulator::new(reference.width, reference.height);
        acc.check_dims(reference)?;
        acc.reference = Some(reference.data.clone());
        acc.deghost = params;
        // Accumulate the reference itself. With a stored reference the deghost factor is computed,
        // but obs == pred here (d == 0 → consist == 1), so it is an exact no-op for this frame.
        acc.accumulate(&reference.data, 1.0, ref_is_shortest, None);
        acc.frames += 1;
        Ok(acc)
    }

    /// Size + buffer-length guards shared by every accumulate entry point.
    fn check_dims(&self, frame: &LinearImage) -> Result<(), HdrError> {
        if (frame.width, frame.height) != (self.width, self.height) {
            return Err(HdrError::SizeMismatch {
                want_w: self.width,
                want_h: self.height,
                got_w: frame.width,
                got_h: frame.height,
            });
        }
        if frame.data.len() != self.wsum.len() * 3 {
            return Err(HdrError::MalformedFrame);
        }
        Ok(())
    }

    /// Accumulate one frame. `scale` is [`relative_scale`] (frame → reference); `is_shortest`
    /// marks the shortest exposure (highest EV₁₀₀ — the frame with the most surviving highlights),
    /// which doubles as the all-clipped fallback.
    pub fn add_frame(
        &mut self,
        frame: &LinearImage,
        scale: f32,
        is_shortest: bool,
    ) -> Result<(), HdrError> {
        self.check_dims(frame)?;
        self.accumulate(&frame.data, scale, is_shortest, None);
        self.frames += 1;
        Ok(())
    }

    /// Accumulate one **masked** frame (hand-held path): identical to [`add_frame`] but pixels whose
    /// `valid` byte is `0` are skipped entirely — not floored. This is the critical difference from
    /// feeding a warped frame straight through [`add_frame`]: an out-of-frame warp border reads as
    /// `0.0`, and `hat_weight(0.0) == W_LOW_FLOOR (0.05)` would otherwise pull those dark borders
    /// into the weighted sum as visible halos. `valid.len()` must equal the pixel count.
    ///
    /// [`add_frame`]: Self::add_frame
    pub fn add_frame_masked(
        &mut self,
        frame: &LinearImage,
        scale: f32,
        is_shortest: bool,
        valid: &[u8],
    ) -> Result<(), HdrError> {
        self.check_dims(frame)?;
        if valid.len() != self.wsum.len() {
            return Err(HdrError::MalformedFrame);
        }
        self.accumulate(&frame.data, scale, is_shortest, Some(valid));
        self.frames += 1;
        Ok(())
    }

    /// Per-pixel accumulation shared by every entry point. `valid` (when `Some`) skips masked-out
    /// pixels; a stored `reference` (hand-held path) additionally deghosts each pixel against the
    /// reference radiance. Callers own the `frames` counter and the dimension guards.
    fn accumulate(&mut self, data: &[f32], scale: f32, is_shortest: bool, valid: Option<&[u8]>) {
        // Move the reference buffer out so we can read it while mutating sum/wsum/fallback (a split
        // borrow the checker won't grant through `self`); restore it before returning.
        let reference = self.reference.take();
        let deghost = self.deghost;

        for (i, px) in data.chunks_exact(3).enumerate() {
            if let Some(v) = valid {
                if v[i] == 0 {
                    continue; // out-of-frame warp border — contributes nothing this frame
                }
            }
            let m = px[0].max(px[1]).max(px[2]);
            let mut w = hat_weight(m);

            if let Some(ref_data) = &reference {
                // Deghost: down-weight this frame where its radiance disagrees with the reference.
                let pred = [ref_data[i * 3], ref_data[i * 3 + 1], ref_data[i * 3 + 2]];
                let pred_max = pred[0].max(pred[1]).max(pred[2]).max(0.0);
                let obs = [scale * px[0], scale * px[1], scale * px[2]];
                let ref_conf = hat_weight(pred_max);
                let d =
                    (obs[0] - pred[0]).abs() + (obs[1] - pred[1]).abs() + (obs[2] - pred[2]).abs();
                let denom = deghost.sigma + deghost.k * pred_max;
                let ratio = d / denom;
                let consist = (-(ratio * ratio)).exp();
                // Where the reference clips, ref_conf → 0, factor → 1 (deghost off) so darker frames
                // can still fill blown highlights — the classic recover-highlights-lose-deghost trade.
                w *= 1.0 - ref_conf * (1.0 - consist);
            }

            let ws = w * scale;
            self.sum[i * 3] += ws * px[0];
            self.sum[i * 3 + 1] += ws * px[1];
            self.sum[i * 3 + 2] += ws * px[2];
            self.wsum[i] += w;
            if is_shortest {
                self.fallback[i * 3] = scale * px[0];
                self.fallback[i * 3 + 1] = scale * px[1];
                self.fallback[i * 3 + 2] = scale * px[2];
            }
        }

        self.reference = reference;
    }

    /// Finish the merge: weighted average where any frame contributed, the scaled shortest
    /// exposure where every frame clipped. Negatives floored (mirrors `clip_negative`).
    pub fn finish(self) -> Result<LinearImage, HdrError> {
        if self.frames < 2 {
            return Err(HdrError::TooFewFrames);
        }
        let mut data = self.sum;
        for (i, &w) in self.wsum.iter().enumerate() {
            if w > 1e-4 {
                let inv = 1.0 / w;
                for c in 0..3 {
                    data[i * 3 + c] = (data[i * 3 + c] * inv).max(0.0);
                }
            } else {
                for c in 0..3 {
                    data[i * 3 + c] = self.fallback[i * 3 + c].max(0.0);
                }
            }
        }
        Ok(LinearImage {
            width: self.width,
            height: self.height,
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32, f: impl Fn(usize) -> [f32; 3]) -> LinearImage {
        let n = (w * h) as usize;
        let mut data = Vec::with_capacity(n * 3);
        for i in 0..n {
            data.extend_from_slice(&f(i));
        }
        LinearImage {
            width: w,
            height: h,
            data,
        }
    }

    fn exposure(t: f64, n: f64, iso: f64) -> ExposureInfo {
        ExposureInfo {
            exposure_time_s: t,
            f_number: n,
            iso,
        }
    }

    #[test]
    fn ev100_known_values() {
        // f/8, 1/125 s, ISO 100 → log2(64·125) ≈ 12.97.
        let ev = ev100(&exposure(1.0 / 125.0, 8.0, 100.0)).unwrap();
        assert!((ev - 12.966).abs() < 0.01, "got {ev}");
        // Doubling ISO = 1 EV more light = EV₁₀₀ drops by 1.
        let ev200 = ev100(&exposure(1.0 / 125.0, 8.0, 200.0)).unwrap();
        assert!((ev - ev200 - 1.0).abs() < 1e-9);
        // 4× the shutter time = 2 EV brighter.
        let ev_long = ev100(&exposure(4.0 / 125.0, 8.0, 100.0)).unwrap();
        assert!((ev - ev_long - 2.0).abs() < 1e-9);
    }

    #[test]
    fn relative_scale_direction() {
        let base = exposure(1.0 / 125.0, 8.0, 100.0);
        let brighter = exposure(4.0 / 125.0, 8.0, 100.0); // +2 EV of light
        let s = relative_scale(&brighter, &base).unwrap();
        assert!(
            (s - 0.25).abs() < 1e-9,
            "brighter frame must scale DOWN: {s}"
        );
        let s_inv = relative_scale(&base, &brighter).unwrap();
        assert!((s_inv - 4.0).abs() < 1e-9);
    }

    #[test]
    fn reference_is_median_ev() {
        let under = exposure(1.0 / 500.0, 8.0, 100.0);
        let metered = exposure(1.0 / 125.0, 8.0, 100.0);
        let over = exposure(1.0 / 30.0, 8.0, 100.0);
        // Any input order → the metered (middle) frame.
        assert_eq!(reference_index(&[under, metered, over]).unwrap(), 1);
        assert_eq!(reference_index(&[over, under, metered]).unwrap(), 2);
        // Even count → lower-middle of the ascending-EV order (the brighter of the two middles).
        let idx =
            reference_index(&[under, metered, over, exposure(1.0 / 2000.0, 8.0, 100.0)]).unwrap();
        assert_eq!(idx, 1);
    }

    #[test]
    fn identity_merge_of_equal_frames() {
        let src = img(8, 4, |i| [0.01 + i as f32 * 0.02, 0.3, 0.6]);
        let mut acc = MergeAccumulator::new(8, 4);
        for k in 0..3 {
            acc.add_frame(&src, 1.0, k == 0).unwrap();
        }
        let out = acc.finish().unwrap();
        for (a, b) in src.data.iter().zip(out.data.iter()) {
            assert!((a - b).abs() < 1e-5, "{a} vs {b}");
        }
    }

    /// Synthetic bracket of a radiance ramp shot at ×¼ / ×1 / ×4 exposure with clipping at 1.0:
    /// the merge must reconstruct the true radiance, including values far above 1.0.
    #[test]
    fn synthetic_bracket_recovers_radiance() {
        const W: u32 = 64;
        // True scene radiance in reference scale: 0.001 → ~3.2 (well past reference clip).
        let radiance = |i: usize| 0.001 * 1.14f32.powi(i as i32);
        let shoot = |gain: f32| {
            img(W, 1, |i| {
                let v = (radiance(i) * gain).min(1.0); // sensor clips at 1.0
                [v, v * 0.8, v * 0.5].map(|c| c.min(1.0))
            })
        };

        let mut acc = MergeAccumulator::new(W, 1);
        acc.add_frame(&shoot(0.25), 4.0, true).unwrap(); // −2 EV frame, scaled ×4
        acc.add_frame(&shoot(1.0), 1.0, false).unwrap(); // reference
        acc.add_frame(&shoot(4.0), 0.25, false).unwrap(); // +2 EV frame, scaled ×¼
        let out = acc.finish().unwrap();

        for i in 0..W as usize {
            let want = radiance(i);
            // Ground truth is recoverable wherever the darkest frame hasn't clipped R.
            if want * 0.25 < 0.9 {
                let got = out.data[i * 3];
                let rel = (got - want).abs() / want;
                assert!(rel < 0.02, "px {i}: want {want}, got {got}");
            }
        }
        // Headroom actually present in the output.
        assert!(out.data.iter().any(|&v| v > 2.0));
    }

    /// Where the reference clips but a darker frame doesn't, the darker frame's scaled values must
    /// win (highlight recovery), and where ALL frames clip, the scaled shortest exposure is used.
    #[test]
    fn clipped_highlights_fall_back_to_shortest() {
        const TRUE_RADIANCE: f32 = 8.0; // way past clip in every frame
        let frame_clipped = img(4, 1, |_| [1.0, 1.0, 1.0]);
        let mut acc = MergeAccumulator::new(4, 1);
        acc.add_frame(&frame_clipped, 4.0, true).unwrap(); // −2 EV, still clipped
        acc.add_frame(&frame_clipped, 1.0, false).unwrap();
        let out = acc.finish().unwrap();
        // All clipped → shortest×scale = 4.0 (best available estimate, not TRUE_RADIANCE).
        for px in out.data.chunks_exact(3) {
            assert!((px[0] - 4.0).abs() < 1e-5, "got {px:?}");
        }
        let _ = TRUE_RADIANCE;
    }

    #[test]
    fn size_mismatch_is_an_error() {
        let mut acc = MergeAccumulator::new(8, 8);
        let wrong = img(4, 4, |_| [0.1, 0.1, 0.1]);
        assert!(matches!(
            acc.add_frame(&wrong, 1.0, false),
            Err(HdrError::SizeMismatch { .. })
        ));
    }

    #[test]
    fn too_few_frames_is_an_error() {
        let mut acc = MergeAccumulator::new(2, 2);
        acc.add_frame(&img(2, 2, |_| [0.5; 3]), 1.0, true).unwrap();
        assert!(matches!(acc.finish(), Err(HdrError::TooFewFrames)));
        assert!(matches!(
            reference_index(&[exposure(0.01, 8.0, 100.0)]),
            Err(HdrError::TooFewFrames)
        ));
    }

    #[test]
    fn invalid_exposure_is_an_error() {
        assert!(ev100(&exposure(0.0, 8.0, 100.0)).is_err());
        assert!(ev100(&exposure(0.01, -1.0, 100.0)).is_err());
        assert!(ev100(&exposure(0.01, 8.0, 0.0)).is_err());
    }

    // ---------- Hand-held path: masked accumulate + deghosting ----------

    /// An all-valid masked accumulate must be bit-identical to `add_frame` (the mask machinery adds
    /// nothing when every pixel is valid and no reference is held).
    #[test]
    fn masked_all_valid_matches_add_frame() {
        let a = img(6, 5, |i| [0.02 + i as f32 * 0.01, 0.3, 0.55]);
        let b = img(6, 5, |i| [0.05 + i as f32 * 0.013, 0.28, 0.6]);
        let valid = vec![1u8; 6 * 5];

        let mut plain = MergeAccumulator::new(6, 5);
        plain.add_frame(&a, 1.0, true).unwrap();
        plain.add_frame(&b, 0.5, false).unwrap();
        let out_plain = plain.finish().unwrap();

        let mut masked = MergeAccumulator::new(6, 5);
        masked.add_frame_masked(&a, 1.0, true, &valid).unwrap();
        masked.add_frame_masked(&b, 0.5, false, &valid).unwrap();
        let out_masked = masked.finish().unwrap();

        assert_eq!(out_plain.data, out_masked.data);
    }

    /// A pixel masked out in one frame must contribute nothing there — the merged value equals the
    /// merge of only the remaining frames, with no dark pull from the masked frame's (0.0) border.
    #[test]
    fn masked_border_contributes_nothing() {
        let a = img(4, 4, |_| [0.30, 0.30, 0.30]);
        let b = img(4, 4, |_| [0.32, 0.32, 0.32]);
        // c is bright everywhere, but pixel 0 is a dark out-of-frame border we mask off.
        let c = img(4, 4, |i| if i == 0 { [0.0; 3] } else { [0.85; 3] });
        let mut valid = vec![1u8; 16];
        valid[0] = 0;

        let mut with_c = MergeAccumulator::new(4, 4);
        with_c.add_frame(&a, 1.0, true).unwrap();
        with_c.add_frame(&b, 1.0, false).unwrap();
        with_c.add_frame_masked(&c, 1.0, false, &valid).unwrap();
        let out = with_c.finish().unwrap();

        // Reference: the same merge WITHOUT c at all.
        let mut without_c = MergeAccumulator::new(4, 4);
        without_c.add_frame(&a, 1.0, true).unwrap();
        without_c.add_frame(&b, 1.0, false).unwrap();
        let out_ref = without_c.finish().unwrap();

        // Masked pixel 0 == the a+b-only merge (no contribution, no dark pull).
        assert!(
            (out.data[0] - out_ref.data[0]).abs() < 1e-6,
            "{}",
            out.data[0]
        );
        assert!(
            out.data[0] > 0.25,
            "masked border darkened pixel: {}",
            out.data[0]
        );
        // A fully-valid pixel (1) DID take c into account (sanity that the mask is per-pixel).
        assert!(out.data[3] > out_ref.data[3]);
    }

    /// Three frames where one has a moved bright object at a pixel: deghosting must down-weight it so
    /// the merge follows the reference radiance there, not the naive average.
    #[test]
    fn deghost_downweights_moved_pixel() {
        let reference = img(2, 2, |_| [0.30, 0.30, 0.30]);
        let consistent = img(2, 2, |_| [0.30, 0.30, 0.30]); // agrees with the reference
        let moved = img(2, 2, |_| [0.90, 0.90, 0.90]); // a bright object that moved into frame
        let valid = vec![1u8; 4];

        let mut acc =
            MergeAccumulator::with_reference(&reference, false, DeghostParams::default()).unwrap();
        acc.add_frame_masked(&consistent, 1.0, false, &valid)
            .unwrap();
        acc.add_frame_masked(&moved, 1.0, true, &valid).unwrap();
        let out = acc.finish().unwrap();

        // Naive average would be (0.3+0.3+0.9)/3 = 0.5; deghosting drops the moved frame → ≈ 0.30.
        for px in out.data.chunks_exact(3) {
            assert!(
                (px[0] - 0.30).abs() < 0.03,
                "expected ≈0.30 (deghosted), got {}",
                px[0]
            );
        }
    }

    /// Where the reference clips, deghosting must switch OFF (ref_conf → 0) so a darker frame's
    /// (differing, higher) radiance recovers the blown highlight instead of being rejected as a ghost.
    #[test]
    fn deghost_off_where_reference_clipped() {
        let reference = img(2, 2, |_| [1.0, 1.0, 1.0]); // clipped everywhere
                                                        // Darker (shorter) frame: unclipped 0.75, exposed 2 stops down → scale ×4 → true radiance 3.0.
        let darker = img(2, 2, |_| [0.75, 0.75, 0.75]);
        let valid = vec![1u8; 4];

        let mut acc =
            MergeAccumulator::with_reference(&reference, false, DeghostParams::default()).unwrap();
        acc.add_frame_masked(&darker, 4.0, true, &valid).unwrap();
        let out = acc.finish().unwrap();

        // The clipped reference contributes zero weight (hat_weight(1.0)=0), deghost is OFF, so the
        // darker frame alone sets the value: 4.0 × 0.75 = 3.0.
        for px in out.data.chunks_exact(3) {
            assert!(
                (px[0] - 3.0).abs() < 0.02,
                "highlight not recovered: {}",
                px[0]
            );
        }
    }

    // ---------- Hand-held path: warp ----------

    fn warp_scene(w: u32, h: u32) -> LinearImage {
        // Distinct per-pixel values so a shift is detectable.
        img(w, h, |i| {
            [
                0.10 + i as f32 * 0.03,
                0.20 + i as f32 * 0.017,
                0.05 + i as f32 * 0.021,
            ]
        })
    }

    #[test]
    fn warp_identity_round_trips() {
        let mov = warp_scene(5, 4);
        let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let (rgb, mask) = warp_into_reference(&mov, 5, 4, &identity);
        // Sampling at integer coordinates is exact → the buffer round-trips byte-for-byte.
        assert_eq!(rgb, mov.data);
        assert!(mask.iter().all(|&m| m == 1));
    }

    #[test]
    fn warp_translation_shifts_and_masks_border() {
        let mov = warp_scene(4, 2);
        // q = p + (1, 0): moving-frame content lands one pixel to the right on the reference grid.
        let m = [[1.0, 0.0, 1.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let (rgb, mask) = warp_into_reference(&mov, 4, 2, &m);

        for y in 0..2usize {
            // Column 0 was vacated (back-projects to x = -1, outside mov) → masked off.
            assert_eq!(mask[y * 4], 0, "vacated border not masked at row {y}");
            for x in 1..4usize {
                let out_i = y * 4 + x;
                let src_i = y * 4 + (x - 1);
                assert_eq!(mask[out_i], 1);
                for c in 0..3 {
                    assert!(
                        (rgb[out_i * 3 + c] - mov.data[src_i * 3 + c]).abs() < 1e-5,
                        "content not shifted at ({x},{y}) ch{c}"
                    );
                }
            }
        }
    }
}
