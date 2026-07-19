//! Feature extraction: registration-scale luma -> FAST corners -> intensity-centroid orientation
//! -> steered BRIEF-256 descriptors.
//!
//! WHY steer BRIEF ourselves: imageproc 0.25's `binary_descriptors::brief` samples a *fixed*
//! test-pair pattern (no per-keypoint rotation), so descriptors are not rotation-invariant. Panorama
//! frames are hand-held and rotate relative to each other, so we roll the standard ORB recipe: take
//! FAST-9 corners, compute each corner's orientation from the intensity centroid, then rotate the one
//! shared test-pair pattern by that angle before sampling (steered BRIEF). imageproc still provides
//! the FAST detector and the box filter; only the steering is ours.
//!
//! Coordinate spaces: detection/description run at *registration scale* (long side ~900 px, because
//! camera-native buffers are large and features are scale-tolerant), but every stored keypoint is
//! mapped back to FULL-RES pixels so the homography/focal math downstream is in the camera's real
//! pixel units. `reg_scale = reg_long / full_long (<= 1)`; `full = reg / reg_scale`.

use image::GrayImage;
use imageproc::corners::corners_fast9;
use imageproc::filter::box_filter;

use crate::rng::SplitMix64;
use crate::Frame;

/// Registration-scale long side. Features are scale-tolerant, and ~900 px keeps FAST + BRIEF fast
/// while retaining enough corner detail for reliable matching on typical 24–40 MP frames.
const REG_LONG_SIDE: u32 = 900;

/// Number of BRIEF test pairs = descriptor length in bits (256 -> `[u64; 4]`).
const DESCRIPTOR_BITS: usize = 256;

/// Keep at most this many strongest corners per frame (brute-force matching is O(n·m) per pair).
const MAX_KEYPOINTS: usize = 1500;

/// Below this many corners the FAST threshold is lowered and detection retried.
const MIN_KEYPOINTS: usize = 300;

/// Adaptive FAST thresholds, tried strongest-first (u8 intensity domain after the percentile stretch).
const FAST_THRESHOLDS: [u8; 4] = [20, 14, 9, 5];

/// Edge margin (registration px) required around every kept corner. A steered test-pair offset
/// reaches at most `13·√2 ≈ 18.4` px from the centre, plus one px for the bilinear neighbour, so 21
/// px guarantees every sample stays in bounds without a per-sample bounds check.
const KEYPOINT_MARGIN: u32 = 21;

/// Intensity-centroid radius for orientation (standard ORB uses the 31 px BRIEF patch, r = 15).
const ORIENTATION_RADIUS: i32 = 15;

/// A 256-bit steered-BRIEF descriptor. Hamming distance is XOR + popcount over four words.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Descriptor {
    words: [u64; 4],
}

impl Descriptor {
    #[inline]
    pub(crate) fn hamming(&self, other: &Descriptor) -> u32 {
        (self.words[0] ^ other.words[0]).count_ones()
            + (self.words[1] ^ other.words[1]).count_ones()
            + (self.words[2] ^ other.words[2]).count_ones()
            + (self.words[3] ^ other.words[3]).count_ones()
    }
}

/// Per-frame features. `points` are FULL-RES pixel coordinates, index-aligned with `descriptors`.
pub(crate) struct FrameFeatures {
    pub points: Vec<(f64, f64)>,
    pub descriptors: Vec<Descriptor>,
    /// `reg_long / full_long` (<= 1). Used to convert the registration-scale RANSAC threshold
    /// (3 px) into full-res pixels (`3 / reg_scale`), since points are stored full-res.
    pub reg_scale: f64,
}

/// The single BRIEF test-pair pattern shared by every frame, as signed offsets `[dx0,dy0,dx1,dy1]`
/// from the patch centre. Generated once from a fixed seed so all descriptors are comparable and the
/// pipeline is reproducible. Offsets follow an isotropic Gaussian (σ ≈ 5) clamped to `[-13, 13]`
/// (the classic Calonder distribution, sized to sit inside the 31 px patch after rotation).
pub(crate) fn fixed_test_pairs() -> Vec<[f32; 4]> {
    const SIGMA: f64 = 5.0;
    const LIMIT: f64 = 13.0;
    let mut rng = SplitMix64::new(0x5EED);
    let mut pairs = Vec::with_capacity(DESCRIPTOR_BITS);
    while pairs.len() < DESCRIPTOR_BITS {
        let sample =
            |rng: &mut SplitMix64| (rng.next_gaussian() * SIGMA).clamp(-LIMIT, LIMIT) as f32;
        pairs.push([
            sample(&mut rng),
            sample(&mut rng),
            sample(&mut rng),
            sample(&mut rng),
        ]);
    }
    pairs
}

/// Extract keypoints + steered descriptors for one frame using the shared test-pair `pattern`.
pub(crate) fn extract(frame: &Frame, pattern: &[[f32; 4]]) -> FrameFeatures {
    let full_long = frame.width.max(frame.height) as u32;
    let reg_scale = if full_long <= REG_LONG_SIDE {
        1.0
    } else {
        REG_LONG_SIDE as f64 / full_long as f64
    };
    let dw = ((frame.width as f64 * reg_scale).round() as u32).max(1);
    let dh = ((frame.height as f64 * reg_scale).round() as u32).max(1);

    let luma_full = luma_from_rgb(frame);
    let luma_reg = downscale(
        &luma_full,
        frame.width,
        frame.height,
        dw as usize,
        dh as usize,
    );
    let gray = stretch_to_u8(&luma_reg, dw, dh);
    // BRIEF samples the box-smoothed image (Calonder/Rublee): a 3×3 box filter suppresses pixel
    // noise so the intensity comparisons are stable. Orientation is computed on the un-smoothed
    // gray, matching the ORB centroid.
    let smoothed = box_filter(&gray, 1, 1);

    let corners = detect_corners(&gray, dw, dh);

    let inv_scale = 1.0 / reg_scale;
    let mut points = Vec::with_capacity(corners.len());
    let mut descriptors = Vec::with_capacity(corners.len());
    for (cx, cy, _score) in corners {
        let angle = intensity_centroid(&gray, cx, cy);
        let desc = steered_descriptor(&smoothed, cx as f32, cy as f32, angle, pattern);
        points.push((cx as f64 * inv_scale, cy as f64 * inv_scale));
        descriptors.push(desc);
    }

    FrameFeatures {
        points,
        descriptors,
        reg_scale,
    }
}

/// Rec.601 luma on the linear camera-native RGB. WHY percentile-stretch afterwards: camera-native
/// values are dim and keep >1.0 headroom, so a naive `*255` clips almost everything to black; FAST
/// needs contrast in the u8 domain.
fn luma_from_rgb(frame: &Frame) -> Vec<f32> {
    let n = frame.width * frame.height;
    let mut out = vec![0.0f32; n];
    for (i, o) in out.iter_mut().enumerate() {
        let r = frame.rgb[i * 3];
        let g = frame.rgb[i * 3 + 1];
        let b = frame.rgb[i * 3 + 2];
        *o = 0.299 * r + 0.587 * g + 0.114 * b;
    }
    out
}

/// Area-average downscale of an f32 plane. Deterministic and dependency-free (avoids relying on
/// image-crate resize trait bounds over f32), and area sampling anti-aliases better than point
/// sampling for feature detection.
fn downscale(src: &[f32], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<f32> {
    if dw == sw && dh == sh {
        return src.to_vec();
    }
    let mut out = vec![0.0f32; dw * dh];
    let sx = sw as f64 / dw as f64;
    let sy = sh as f64 / dh as f64;
    for dy in 0..dh {
        let y0 = (dy as f64 * sy).floor() as usize;
        let y1 = (((dy + 1) as f64 * sy).ceil() as usize).min(sh).max(y0 + 1);
        for dx in 0..dw {
            let x0 = (dx as f64 * sx).floor() as usize;
            let x1 = (((dx + 1) as f64 * sx).ceil() as usize).min(sw).max(x0 + 1);
            let mut acc = 0.0f64;
            let mut count = 0.0f64;
            for y in y0..y1 {
                let row = y * sw;
                for x in x0..x1 {
                    acc += src[row + x] as f64;
                    count += 1.0;
                }
            }
            out[dy * dw + dx] = (acc / count) as f32;
        }
    }
    out
}

/// Map an f32 plane to u8 by stretching the 2nd..98th percentile to 0..255 (robust to a few hot
/// pixels / specular highlights).
fn stretch_to_u8(luma: &[f32], w: u32, h: u32) -> GrayImage {
    let mut sorted: Vec<f32> = luma.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    let lo = sorted[(n as f64 * 0.02) as usize];
    let hi = sorted[((n as f64 * 0.98) as usize).min(n - 1)];
    let (lo, hi) = if (hi - lo) > 1e-9 {
        (lo, hi)
    } else {
        // Flat plane: fall back to full min/max, then to a unit range so the map is finite.
        let mn = sorted[0];
        let mx = sorted[n - 1];
        if (mx - mn) > 1e-9 {
            (mn, mx)
        } else {
            (mn, mn + 1.0)
        }
    };
    let inv = 255.0 / (hi - lo);
    let mut img = GrayImage::new(w, h);
    for (px, &v) in img.as_mut().iter_mut().zip(luma.iter()) {
        *px = (((v - lo) * inv).clamp(0.0, 255.0)) as u8;
    }
    img
}

/// FAST-9 detection with an adaptive threshold and edge-margin filtering, returning the strongest
/// `MAX_KEYPOINTS` corners as `(x, y, score)` in registration pixels.
fn detect_corners(gray: &GrayImage, w: u32, h: u32) -> Vec<(u32, u32, f32)> {
    if w <= 2 * KEYPOINT_MARGIN || h <= 2 * KEYPOINT_MARGIN {
        return Vec::new();
    }
    let mut kept: Vec<(u32, u32, f32)> = Vec::new();
    for &thresh in &FAST_THRESHOLDS {
        kept = corners_fast9(gray, thresh)
            .into_iter()
            .filter(|c| {
                c.x >= KEYPOINT_MARGIN
                    && c.y >= KEYPOINT_MARGIN
                    && c.x < w - KEYPOINT_MARGIN
                    && c.y < h - KEYPOINT_MARGIN
            })
            .map(|c| (c.x, c.y, c.score))
            .collect();
        if kept.len() >= MIN_KEYPOINTS {
            break;
        }
    }
    // Keep the strongest corners. Descending by FAST score; ties broken by position for determinism.
    kept.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
            .then(a.1.cmp(&b.1))
    });
    kept.truncate(MAX_KEYPOINTS);
    kept
}

/// ORB intensity-centroid orientation over a `(2r+1)²` patch: `θ = atan2(m01, m10)` with
/// `m10 = Σ dx·I`, `m01 = Σ dy·I` (dy positive downward). The convention only needs to be consistent
/// across frames, which it is.
fn intensity_centroid(gray: &GrayImage, cx: u32, cy: u32) -> f32 {
    let (w, h) = (gray.width() as i32, gray.height() as i32);
    let mut m10 = 0.0f64;
    let mut m01 = 0.0f64;
    for dy in -ORIENTATION_RADIUS..=ORIENTATION_RADIUS {
        let y = cy as i32 + dy;
        if y < 0 || y >= h {
            continue;
        }
        let row = (y as u32) * gray.width();
        let buf = gray.as_raw();
        for dx in -ORIENTATION_RADIUS..=ORIENTATION_RADIUS {
            let x = cx as i32 + dx;
            if x < 0 || x >= w {
                continue;
            }
            let i = buf[(row + x as u32) as usize] as f64;
            m10 += dx as f64 * i;
            m01 += dy as f64 * i;
        }
    }
    (m01.atan2(m10)) as f32
}

/// Compute a steered BRIEF-256 descriptor at `(x, y)` by rotating each test-pair offset by `angle`
/// before bilinear-sampling the smoothed image.
fn steered_descriptor(
    smoothed: &GrayImage,
    x: f32,
    y: f32,
    angle: f32,
    pattern: &[[f32; 4]],
) -> Descriptor {
    let (s, c) = angle.sin_cos();
    let mut words = [0u64; 4];
    for (k, p) in pattern.iter().enumerate() {
        let ax = x + c * p[0] - s * p[1];
        let ay = y + s * p[0] + c * p[1];
        let bx = x + c * p[2] - s * p[3];
        let by = y + s * p[2] + c * p[3];
        if bilinear(smoothed, ax, ay) < bilinear(smoothed, bx, by) {
            words[k >> 6] |= 1u64 << (k & 63);
        }
    }
    Descriptor { words }
}

/// Bilinear sample of a u8 gray image. Callers guarantee the point is inside the `KEYPOINT_MARGIN`,
/// so indices are clamped only defensively.
#[inline]
fn bilinear(img: &GrayImage, x: f32, y: f32) -> f32 {
    let w = img.width();
    let h = img.height();
    let x0 = (x.floor() as i64).clamp(0, w as i64 - 2) as u32;
    let y0 = (y.floor() as i64).clamp(0, h as i64 - 2) as u32;
    let fx = (x - x0 as f32).clamp(0.0, 1.0);
    let fy = (y - y0 as f32).clamp(0.0, 1.0);
    let buf = img.as_raw();
    let row0 = (y0 * w) as usize;
    let row1 = row0 + w as usize;
    let p00 = buf[row0 + x0 as usize] as f32;
    let p10 = buf[row0 + x0 as usize + 1] as f32;
    let p01 = buf[row1 + x0 as usize] as f32;
    let p11 = buf[row1 + x0 as usize + 1] as f32;
    let top = p00 + (p10 - p00) * fx;
    let bot = p01 + (p11 - p01) * fx;
    top + (bot - top) * fy
}
