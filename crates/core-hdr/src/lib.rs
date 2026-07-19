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
//! v1 is tripod-only: no alignment, no deghosting — frames must be pixel-aligned and same-sized.

use core_raw::LinearImage;
use thiserror::Error;

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

/// Streaming exposure-weighted merge accumulator. Holds three flat buffers (7 f32/pixel total —
/// ≈0.9 GB at 33 MP) regardless of frame count; callers decode → `add_frame` → drop each frame.
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
}

impl MergeAccumulator {
    pub fn new(width: u32, height: u32) -> Self {
        let n = width as usize * height as usize;
        MergeAccumulator {
            width,
            height,
            sum: vec![0.0; n * 3],
            wsum: vec![0.0; n],
            fallback: vec![0.0; n * 3],
            frames: 0,
        }
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

        for (i, px) in frame.data.chunks_exact(3).enumerate() {
            let m = px[0].max(px[1]).max(px[2]);
            let w = hat_weight(m);
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
        self.frames += 1;
        Ok(())
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
}
