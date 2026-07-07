//! Raw-domain (pre-demosaic) AI denoise: Bayer packing + overlap-tiling primitives.
//!
//! The model runs on the raw CFA mosaic (Lightroom/DxO parity — noise is still uncorrelated and
//! signal-dependent there). This module holds the **model-independent** numeric plumbing:
//!
//! 1. [`pack`] — split a 2×2 Bayer mosaic into 4 half-resolution planes, permuted to a canonical
//!    `[R, Gr, Gb, B]` channel order (so any Bayer phase — RGGB/BGGR/GRBG/GBRG — feeds the model in
//!    the order it was trained on), black-level-subtracted and normalized by per-position white level.
//! 2. [`tile_and_blend`] — run a fixed-size tile inference over the packed planes with reflect-padded
//!    borders and a linear-feather blend, so a 33 MP sensor goes through the net in bounded, static-
//!    shape patches (a fixed tile size is also what the CoreML EP needs — see `models::build_session`).
//! 3. [`unpack`] — invert the pack (denormalize + de-permute) back into a full-length u16 mosaic,
//!    ready to write into `RawImage` for the unchanged demosaic/color pipeline.
//!
//! The ONNX inference itself (the `MosaicDenoiser` impl over `ort`) layers on top of these once the
//! exported model's exact tensor I/O is pinned; the primitives here are validated by round-trip tests
//! independent of any model.

use std::sync::Mutex;

use ort::session::Session;
use ort::value::Tensor;

use crate::error::AnalyzeError;
use core_raw::MosaicInfo;

/// A packed Bayer image: 4 half-res planes in canonical `[R, Gr, Gb, B]` order, planar (plane-major)
/// `[4][ph][pw]` f32, black-subtracted and normalized by per-position white level (low-clamped at 0,
/// NOT high-clamped — scene highlights above white are preserved for a faithful round-trip).
pub struct Packed {
    pub planes: Vec<f32>,
    pub pw: usize,
    pub ph: usize,
    /// Original mosaic dimensions (packed covers the even `2*pw × 2*ph` region; any odd remainder
    /// row/column is passed through untouched on [`unpack`]).
    pub w: usize,
    pub h: usize,
    /// Scan-position (0..3, row-major in the 2×2 tile) each canonical plane was taken from — the
    /// permutation [`unpack`] inverts.
    plane_src: [usize; 4],
    /// Per-scan-position black/white levels (indexed by scan position, not canonical plane).
    black: [f32; 4],
    white: [f32; 4],
}

/// Fixed tile geometry, in **packed** (half-resolution) pixels. `overlap < tile` required.
pub struct TileCfg {
    pub tile: usize,
    pub overlap: usize,
}

/// Map a 2×2 CFA color pattern (row-major, 0=R 1=G 2=B) to the scan positions of `[R, Gr, Gb, B]`,
/// where Gr is the green sharing R's row and Gb the green sharing B's row. `None` if `pat` is not a
/// standard RGGB-family Bayer tile (exactly one R, one B, two G on different rows).
fn canonical_planes(pat: &[u8]) -> Option<[usize; 4]> {
    if pat.len() != 4 {
        return None;
    }
    let r = (0..4).find(|&p| pat[p] == 0)?;
    let b = (0..4).find(|&p| pat[p] == 2)?;
    let greens: Vec<usize> = (0..4).filter(|&p| pat[p] == 1).collect();
    if greens.len() != 2 {
        return None;
    }
    let (r_row, b_row) = (r / 2, b / 2);
    if r_row == b_row {
        return None;
    }
    let gr = *greens.iter().find(|&&g| g / 2 == r_row)?;
    let gb = *greens.iter().find(|&&g| g / 2 == b_row)?;
    Some([r, gr, gb, b])
}

/// Pack a Bayer mosaic into normalized half-res planes. `None` for non-2×2 / non-RGGB-family CFAs or
/// a degenerate (sub-2px) sensor — the caller then skips denoise.
pub fn pack(info: &MosaicInfo) -> Option<Packed> {
    if info.cfa_width != 2 || info.cfa_height != 2 {
        return None;
    }
    let plane_src = canonical_planes(&info.cfa_pattern)?;
    let (w, h) = (info.width, info.height);
    let (pw, ph) = (w / 2, h / 2);
    if pw == 0 || ph == 0 || info.data.len() < w * h {
        return None;
    }
    let mut planes = vec![0f32; 4 * pw * ph];
    for (c, &p) in plane_src.iter().enumerate() {
        let (py, px) = (p / 2, p % 2);
        let bl = info.black[p];
        let range = (info.white[p] - info.black[p]).max(1e-6);
        let base = c * pw * ph;
        for j in 0..ph {
            let y = j * 2 + py;
            for i in 0..pw {
                let x = i * 2 + px;
                let v = info.data[y * w + x] as f32;
                planes[base + j * pw + i] = ((v - bl) / range).max(0.0);
            }
        }
    }
    Some(Packed {
        planes,
        pw,
        ph,
        w,
        h,
        plane_src,
        black: info.black,
        white: info.white,
    })
}

/// Invert [`pack`]: denormalize `denoised_planes` (canonical order, same layout as [`Packed::planes`])
/// back into a full-length u16 mosaic. Starts from `orig` so any odd remainder row/column and every
/// non-packed sample pass through unchanged; only the even packed region is overwritten.
pub fn unpack(packed: &Packed, denoised_planes: &[f32], orig: &[u16]) -> Vec<u16> {
    let mut out = orig.to_vec();
    let (pw, ph, w) = (packed.pw, packed.ph, packed.w);
    for (c, &p) in packed.plane_src.iter().enumerate() {
        let (py, px) = (p / 2, p % 2);
        let bl = packed.black[p];
        let range = (packed.white[p] - packed.black[p]).max(1e-6);
        let base = c * pw * ph;
        for j in 0..ph {
            let y = j * 2 + py;
            for i in 0..pw {
                let x = i * 2 + px;
                let val = denoised_planes[base + j * pw + i] * range + bl;
                out[y * w + x] = val.round().clamp(0.0, u16::MAX as f32) as u16;
            }
        }
    }
    out
}

/// Reflect an index into `[0, n)` (reflect-101: mirrors without repeating the edge sample).
fn reflect(mut i: isize, n: usize) -> usize {
    let n = n as isize;
    if n == 1 {
        return 0;
    }
    loop {
        if i < 0 {
            i = -i;
        } else if i >= n {
            i = 2 * n - 2 - i;
        } else {
            return i as usize;
        }
    }
}

/// Tile origins covering `n` with the given `tile`/`overlap`; the final origin is clamped to `n-tile`
/// so the last tile stays in-bounds (dedup'd if it coincides with the previous stride).
fn tile_origins(n: usize, tile: usize, overlap: usize) -> Vec<usize> {
    if n <= tile {
        return vec![0];
    }
    let stride = tile - overlap;
    let mut origins = Vec::new();
    let mut o = 0usize;
    loop {
        if o + tile >= n {
            let last = n - tile;
            if origins.last() != Some(&last) {
                origins.push(last);
            }
            break;
        }
        origins.push(o);
        o += stride;
    }
    origins
}

/// Run `infer` over fixed `tile`×`tile`×4 patches of the packed planes (reflect-padded at borders)
/// and linear-feather-blend the results back into a full `4 × ph × pw` planar buffer. `infer` maps a
/// `4*tile*tile` planar patch to the denoised patch of the same shape (the ONNX call, in practice).
///
/// The feather weight `min(u+1, tile-u) * min(v+1, tile-v)` is ≥1 everywhere and peaks tile-center, so
/// overlap seams cross-fade smoothly and every output pixel has non-zero coverage (no divide-by-zero).
pub fn tile_and_blend<F>(
    planes: &[f32],
    pw: usize,
    ph: usize,
    cfg: &TileCfg,
    mut infer: F,
) -> Vec<f32>
where
    F: FnMut(&[f32]) -> Vec<f32>,
{
    let tile = cfg.tile.min(pw.max(ph)).max(1);
    let overlap = cfg.overlap.min(tile.saturating_sub(1));
    let xs = tile_origins(pw, tile, overlap);
    let ys = tile_origins(ph, tile, overlap);
    let plane = pw * ph;
    let tplane = tile * tile;
    let mut acc = vec![0f32; 4 * plane];
    let mut wsum = vec![0f32; plane];
    let mut patch = vec![0f32; 4 * tplane];

    for &oy in &ys {
        for &ox in &xs {
            // Gather the reflect-padded patch.
            for c in 0..4 {
                let (src, dst) = (c * plane, c * tplane);
                for v in 0..tile {
                    let sy = reflect(oy as isize + v as isize, ph);
                    for u in 0..tile {
                        let sx = reflect(ox as isize + u as isize, pw);
                        patch[dst + v * tile + u] = planes[src + sy * pw + sx];
                    }
                }
            }
            let out = infer(&patch);
            debug_assert_eq!(out.len(), 4 * tplane);
            // Blend the in-bounds region.
            for v in 0..tile {
                let gy = oy + v;
                if gy >= ph {
                    continue;
                }
                let wy = (v + 1).min(tile - v);
                for u in 0..tile {
                    let gx = ox + u;
                    if gx >= pw {
                        continue;
                    }
                    let wgt = (wy * (u + 1).min(tile - u)) as f32;
                    let idx = gy * pw + gx;
                    wsum[idx] += wgt;
                    for c in 0..4 {
                        acc[c * plane + idx] += out[c * tplane + v * tile + u] * wgt;
                    }
                }
            }
        }
    }
    for idx in 0..plane {
        let s = wsum[idx].max(1e-6);
        for c in 0..4 {
            acc[c * plane + idx] /= s;
        }
    }
    acc
}

/// Edge-preserving bilateral filter over one reflect-padded `tile`×`tile`×4 planar patch (the tile
/// side is derived from the patch length). Radius `r`, spatial σ `sigma_s`, range σ `sigma_r` (in the
/// normalized plane domain). Border taps are clamped inside the patch.
fn bilateral_tile(patch: &[f32], r: usize, sigma_s: f32, sigma_r: f32) -> Vec<f32> {
    let tile = ((patch.len() / 4) as f64).sqrt() as usize;
    let tplane = tile * tile;
    let inv_s = 1.0 / (2.0 * sigma_s * sigma_s);
    let inv_r = 1.0 / (2.0 * sigma_r * sigma_r);
    let mut out = vec![0f32; patch.len()];
    for c in 0..4 {
        let base = c * tplane;
        for y in 0..tile {
            for x in 0..tile {
                let center = patch[base + y * tile + x];
                let mut acc = 0f32;
                let mut wsum = 0f32;
                let y0 = y.saturating_sub(r);
                let y1 = (y + r).min(tile - 1);
                let x0 = x.saturating_sub(r);
                let x1 = (x + r).min(tile - 1);
                for ny in y0..=y1 {
                    for nx in x0..=x1 {
                        let s = patch[base + ny * tile + nx];
                        let ds = ((ny as f32 - y as f32).powi(2) + (nx as f32 - x as f32).powi(2))
                            * inv_s;
                        let dr = (s - center).powi(2) * inv_r;
                        let w = (-(ds + dr)).exp();
                        acc += s * w;
                        wsum += w;
                    }
                }
                out[base + y * tile + x] = if wsum > 0.0 { acc / wsum } else { center };
            }
        }
    }
    out
}

/// Classical CPU raw-domain denoiser used to validate the full integration rail (and as a no-GPU/no-
/// model fallback) BEHIND the same [`core_raw::MosaicDenoiser`] seam the neural ONNX model will use.
/// `strength` 0..1 scales the range σ (more smoothing). Non-Bayer input passes through unchanged.
pub struct CpuDenoiser {
    pub strength: f32,
}

impl core_raw::MosaicDenoiser for CpuDenoiser {
    fn denoise(&self, info: &MosaicInfo) -> Vec<u16> {
        let Some(packed) = pack(info) else {
            return info.data.to_vec();
        };
        let cfg = TileCfg {
            tile: 256,
            overlap: 32,
        };
        let sigma_r = (0.015 + 0.06 * self.strength.clamp(0.0, 1.0)).max(1e-3);
        let out = tile_and_blend(&packed.planes, packed.pw, packed.ph, &cfg, |patch| {
            bilateral_tile(patch, 2, 1.5, sigma_r)
        });
        unpack(&packed, &out, info.data)
    }
}

// ── PMRID neural denoiser (pretrained, Apache-2.0) ────────────────────────────────────────────────

/// PMRID's k-Sigma variance-stabilizing transform (ported verbatim from `run_benchmark.py`). Maps a
/// [0,1]-normalized mosaic at capture `iso` into the anchor-ISO domain the net was trained on, and
/// back. `K` is linear in ISO, `Sigma` quadratic (numpy `poly1d`, highest-degree coefficient first).
#[derive(Clone, Copy)]
pub struct KSigma {
    pub k_coeff: [f64; 2],
    pub b_coeff: [f64; 3],
    pub anchor: f64,
    pub v: f64,
}

impl KSigma {
    fn k(&self, iso: f64) -> f64 {
        self.k_coeff[0] * iso + self.k_coeff[1]
    }
    fn sigma(&self, iso: f64) -> f64 {
        self.b_coeff[0] * iso * iso + self.b_coeff[1] * iso + self.b_coeff[2]
    }
    /// Precompute the `(cvt_k, cvt_b)` conversion for a given ISO (constant across the frame).
    fn convert(&self, iso: f64) -> (f64, f64) {
        let (k, sigma) = (self.k(iso), self.sigma(iso));
        let (ka, sa) = (self.k(self.anchor), self.sigma(self.anchor));
        let cvt_k = ka / k;
        let cvt_b = (sigma / (k * k) - sa / (ka * ka)) * ka;
        (cvt_k, cvt_b)
    }
}

/// PMRID's shipped k-Sigma coefficients (anchor ISO 1600). Calibrated for the PMRID authors' sensor —
/// approximate on a Canon, but the transform still normalizes toward the trained domain. A per-camera
/// table (e.g. ELD's Canon calibration) can override this later (plan phase-1 note).
pub const PMRID_KSIGMA: KSigma = KSigma {
    k_coeff: [0.0005995267, 0.00868861],
    b_coeff: [7.11772e-7, 6.514934e-4, 0.11492713],
    anchor: 1600.0,
    v: 959.0,
};

const PMRID_TILE: usize = 256;
const PMRID_INP_SCALE: f32 = 256.0;

/// Pretrained PMRID neural raw-Bayer denoiser (Apache-2.0), behind the same [`core_raw::MosaicDenoiser`]
/// seam as [`CpuDenoiser`]. Fixed-256 tiled ONNX with k-Sigma ISO conditioning applied OUTSIDE the graph
/// (the graph stays a pure conv net). Non-Bayer input passes through unchanged.
pub struct PmridDenoiser {
    session: Mutex<Session>,
    ksigma: KSigma,
}

impl PmridDenoiser {
    /// Run one tile: [0,1] plane → forward k-Sigma × inp_scale → ONNX → ÷ inp_scale → inverse k-Sigma.
    /// `cvt` is the frame's precomputed `(cvt_k, cvt_b)`.
    fn run_tile(&self, tile: &[f32], cvt: (f64, f64)) -> Result<Vec<f32>, AnalyzeError> {
        let (cvt_k, cvt_b) = cvt;
        let v = self.ksigma.v;
        let inp: Vec<f32> = tile
            .iter()
            .map(|&x| ((((x as f64 * v) * cvt_k + cvt_b) / v) as f32) * PMRID_INP_SCALE)
            .collect();
        let input = Tensor::from_array(([1usize, 4, PMRID_TILE, PMRID_TILE], inp))
            .map_err(AnalyzeError::inference)?;
        let out_vec: Vec<f32> = {
            let mut session = self.session.lock().expect("pmrid mutex poisoned");
            let outputs = session
                .run(ort::inputs![input])
                .map_err(AnalyzeError::inference)?;
            outputs[0]
                .try_extract_array::<f32>()
                .map_err(AnalyzeError::inference)?
                .iter()
                .copied()
                .collect()
        };
        Ok(out_vec
            .into_iter()
            .map(|o| {
                let pred = (o / PMRID_INP_SCALE) as f64;
                ((((pred * v) - cvt_b) / cvt_k) / v) as f32
            })
            .collect())
    }
}

impl core_raw::MosaicDenoiser for PmridDenoiser {
    fn denoise(&self, info: &MosaicInfo) -> Vec<u16> {
        let Some(mut packed) = pack(info) else {
            return info.data.to_vec();
        };
        // PMRID expects the mosaic clipped to [0,1] (our `pack` only low-clamps at 0).
        for p in packed.planes.iter_mut() {
            *p = p.min(1.0);
        }
        let iso = info.iso.map(|i| i as f64).unwrap_or(self.ksigma.anchor);
        let cvt = self.ksigma.convert(iso);
        let cfg = TileCfg {
            tile: PMRID_TILE,
            overlap: 16,
        };
        // On any tile inference error, fall back to the un-denoised mosaic (never crash the render).
        let mut failed = false;
        let out = tile_and_blend(
            &packed.planes,
            packed.pw,
            packed.ph,
            &cfg,
            |tile| match self.run_tile(tile, cvt) {
                Ok(v) if !failed => v,
                Ok(_) => tile.to_vec(),
                Err(_) => {
                    failed = true;
                    tile.to_vec()
                }
            },
        );
        if failed {
            return info.data.to_vec();
        }
        unpack(&packed, &out, info.data)
    }
}

/// The embedded PMRID ONNX (present only when `scripts/export_pmrid.py` has produced the asset — see
/// `build.rs`). ~4 MB; embeds only on the non-Intel build (this crate is cfg-gated off on Intel macOS).
#[cfg(have_pmrid)]
static PMRID_ONNX: &[u8] = include_bytes!("../assets/pmrid_4x256x256.onnx");

/// Whether this build embedded the PMRID model (compile-time — no session build). Lets the UI report
/// the neural accelerator without constructing an ort Session.
pub fn pmrid_available() -> bool {
    cfg!(have_pmrid)
}

/// Build the PMRID neural denoiser from the embedded ONNX, if this build included the model asset.
/// Returns `None` when the model isn't present (→ caller falls back to [`CpuDenoiser`]) or the ort
/// Session fails to build.
pub fn try_pmrid() -> Option<PmridDenoiser> {
    #[cfg(have_pmrid)]
    {
        // level1=false (a plain conv graph tolerates full optimization); mlprogram=true for static
        // CoreML shapes (our tiles are a fixed 256×256).
        crate::models::build_session_from_memory(PMRID_ONNX, false, true)
            .ok()
            .map(|session| PmridDenoiser {
                session: Mutex::new(session),
                ksigma: PMRID_KSIGMA,
            })
    }
    #[cfg(not(have_pmrid))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic RGGB `MosaicInfo` with a deterministic gradient in [black, white*1.2].
    fn synth(w: usize, h: usize) -> (Vec<u16>, [f32; 4], [f32; 4], Vec<u8>) {
        let black = [512.0; 4];
        let white = [16000.0; 4];
        let mut data = vec![0u16; w * h];
        for y in 0..h {
            for x in 0..w {
                // Span from black up to above white (>white is preserved; below-black would clamp).
                let v = 512 + ((x * 37 + y * 101) % 17000) as u16;
                data[y * w + x] = v;
            }
        }
        (data, black, white, vec![0, 1, 1, 2]) // RGGB
    }

    #[test]
    fn pack_unpack_roundtrip_rggb() {
        let (data, black, white, pat) = synth(64, 48);
        let info = MosaicInfo {
            data: &data,
            width: 64,
            height: 48,
            black,
            white,
            cfa_width: 2,
            cfa_height: 2,
            cfa_pattern: pat,
            iso: Some(3200),
        };
        let packed = pack(&info).expect("RGGB packs");
        assert_eq!((packed.pw, packed.ph), (32, 24));
        let back = unpack(&packed, &packed.planes, &data);
        // Round-trip is exact within ±1 LSB (normalize/denormalize rounding) for all samples ≥ black.
        for (a, b) in data.iter().zip(back.iter()) {
            assert!(
                (*a as i32 - *b as i32).abs() <= 1,
                "pack/unpack drift: {a} vs {b}"
            );
        }
    }

    #[test]
    fn packing_permutes_bggr_to_canonical() {
        // BGGR: plane order must map R,Gr,Gb,B to scan positions [3,2,1,0].
        assert_eq!(canonical_planes(&[2, 1, 1, 0]), Some([3, 2, 1, 0]));
        // GRBG: R@1, B@2 → [1,0,3,2].
        assert_eq!(canonical_planes(&[1, 0, 2, 1]), Some([1, 0, 3, 2]));
        // Non-Bayer (X-Trans-ish tile) rejected.
        assert_eq!(canonical_planes(&[0, 1, 2, 1, 1, 0]), None);
    }

    #[test]
    fn tiling_identity_reconstructs_input() {
        // A non-multiple-of-tile size forces partial tiles + reflect padding + the clamped last tile.
        let (pw, ph) = (37usize, 29usize);
        let plane = pw * ph;
        let mut planes = vec![0f32; 4 * plane];
        for (i, p) in planes.iter_mut().enumerate() {
            *p = (i % 251) as f32 / 251.0;
        }
        let cfg = TileCfg {
            tile: 16,
            overlap: 4,
        };
        // Identity inference: the feather blend must reconstruct the input exactly (weights sum out).
        let out = tile_and_blend(&planes, pw, ph, &cfg, |patch| patch.to_vec());
        for (a, b) in planes.iter().zip(out.iter()) {
            assert!((a - b).abs() < 1e-4, "tile blend drift: {a} vs {b}");
        }
    }

    #[test]
    fn tiling_covers_when_tile_exceeds_image() {
        let (pw, ph) = (10usize, 6usize);
        let plane = pw * ph;
        let planes: Vec<f32> = (0..4 * plane).map(|i| (i % 7) as f32).collect();
        let cfg = TileCfg {
            tile: 32,
            overlap: 8,
        };
        let out = tile_and_blend(&planes, pw, ph, &cfg, |patch| patch.to_vec());
        for (a, b) in planes.iter().zip(out.iter()) {
            assert!((a - b).abs() < 1e-4);
        }
    }
}
