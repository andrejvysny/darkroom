//! Panorama support: camera-native linear decode + LinearRaw DNG authoring.
//!
//! The stitcher (crate `core-pano`) works on **camera-native** linear RGB — demosaiced and
//! black/white-normalized but with NEITHER white balance NOR any color matrix applied. The merged
//! composite is written back as a 16-bit `PhotometricInterpretation=LinearRaw` DNG carrying the
//! reference frame's `ColorMatrix` + `AsShotNeutral`, so on re-decode it flows through the exact
//! same wb+matrix math (`develop_linear_from`'s headroom-preserving path) as a native CFA raw —
//! full white-balance latitude survives the merge, and no tone curve is ever baked.
//!
//! Like everything else in this crate, all rawler calls stay HERE; the public surface exposes no
//! rawler types beyond the crate-wide [`RawSource`](rawler::rawsource::RawSource) convention.
//! [`PanoColorMeta`] is intentionally opaque: callers thread it from
//! [`develop_camera_native`] into [`write_pano_dng`] untouched.

use std::collections::HashMap;
use std::io::BufWriter;
use std::path::Path;

use rawler::decoders::{Camera, RawDecodeParams, RawMetadata};
use rawler::dng::writer::DngWriter;
use rawler::dng::{CropMode, DngCompression, DngPhotometricConversion, DNG_VERSION_V1_4};
use rawler::imgop::matrix::{multiply, normalize, pseudo_inverse};
use rawler::imgop::xyz::{FlatColorMatrix, Illuminant, SRGB_TO_XYZ_D65};
use rawler::pixarray::PixU16;
use rawler::rawimage::{BlackLevel, RawPhotometricInterpretation, WhiteLevel};
use rawler::rawsource::RawSource;
use rawler::tags::ExifTag;
use rawler::{RawImage, CFA};

use crate::develop::{cam_xyz2cam, demosaic_camera_native, wb_or_neutral, LinearImage};
use crate::error::RawError;

fn de(e: impl std::fmt::Display) -> RawError {
    RawError::Decode(e.to_string())
}

/// Color + provenance metadata captured at decode time and required to author the merged DNG.
/// Opaque by design: the rawler-typed internals never cross a crate boundary.
#[derive(Clone)]
pub struct PanoColorMeta {
    color_matrix: HashMap<Illuminant, FlatColorMatrix>,
    wb_coeffs: [f32; 4],
    metadata: RawMetadata,
    make: String,
    model: String,
    clean_make: String,
    clean_model: String,
    /// Capture focal length in millimetres, when the file reported one.
    pub focal_length_mm: Option<f64>,
    /// EXIF orientation of the SOURCE file (1–8). The decoded pixels are already uprighted.
    pub orientation: Option<u16>,
}

impl PanoColorMeta {
    /// `(make, model)` as encoded in the file — used to refuse mixed-camera merges.
    pub fn camera_id(&self) -> (&str, &str) {
        (&self.make, &self.model)
    }
}

/// One source frame decoded to camera-native linear RGB (interleaved, cpp=3, ~0..1 with >1.0
/// specular headroom possible), EXIF-uprighted, plus the metadata needed to author the output DNG.
pub struct CameraNativeImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f32>,
    pub meta: PanoColorMeta,
}

/// Decode + demosaic to **camera-native** linear RGB: black/white rescale, demosaic, active +
/// recommended crop, EXIF upright — and nothing else (no white balance, no color matrix, no gamma).
///
/// Errors for sources a panorama can't be authored from: display images (JPEG/PNG have no raw
/// color pipeline to preserve), sensors without a usable color matrix (the merged DNG could not
/// carry one, and the read-back would fall to the calibrated path), and non-3-channel sensors.
pub fn develop_camera_native(src: &RawSource) -> Result<CameraNativeImage, RawError> {
    if crate::display::is_display(src.path()) {
        return Err(RawError::Decode(
            "panorama merge requires RAW sources (JPEG/PNG have no raw color pipeline)".into(),
        ));
    }
    let decoder = rawler::get_decoder(src).map_err(de)?;
    let params = RawDecodeParams::default();
    let metadata = decoder.raw_metadata(src, &params).map_err(de)?;
    // `RawImage.orientation` is hardcoded Normal in rawler 0.7.2 — take it from EXIF (as elsewhere).
    let orientation = metadata.exif.orientation;
    let raw = decoder.raw_image(src, &params, false).map_err(de)?;

    if cam_xyz2cam(&raw).is_none() {
        return Err(RawError::Decode(
            "panorama merge requires a camera color matrix (none found in this file)".into(),
        ));
    }
    let Some(px) = demosaic_camera_native(&raw)? else {
        return Err(RawError::Decode(
            "panorama merge requires a 3-channel (RGB) sensor".into(),
        ));
    };

    let focal_length_mm = metadata
        .exif
        .focal_length
        .as_ref()
        .map(|r| r.n as f64 / r.d.max(1) as f64);

    // Upright in camera-native space (pure pixel shuffle — reuses the LinearImage helper, which is
    // colorspace-agnostic). The DNG is written with Orientation=1, so this never double-applies.
    let native = LinearImage {
        width: px.width as u32,
        height: px.height as u32,
        data: px.flatten(),
    }
    .oriented(orientation);

    Ok(CameraNativeImage {
        width: native.width,
        height: native.height,
        data: native.data,
        meta: PanoColorMeta {
            color_matrix: raw.color_matrix.clone(),
            wb_coeffs: wb_or_neutral(&raw),
            metadata,
            make: raw.make.clone(),
            model: raw.model.clone(),
            clean_make: raw.clean_make.clone(),
            clean_model: raw.clean_model.clone(),
            focal_length_mm,
            orientation,
        },
    })
}

/// Author a 16-bit LinearRaw DNG from a camera-native linear composite.
///
/// `rgb_native` is interleaved RGB, `width*height*3` long, nominally 0..1 (1.0 = sensor white;
/// values outside are clamped — the merge normalizes before calling). The DNG carries the
/// reference frame's ColorMatrix + AsShotNeutral (so absolute WB latitude survives), an embedded
/// sRGB preview + thumbnail (fast library thumbnails without a full develop), the source's EXIF
/// via rawler's metadata pass-through, and `Orientation=1` (pixels are already upright).
pub fn write_pano_dng(
    dest: &Path,
    width: u32,
    height: u32,
    rgb_native: &[f32],
    meta: &PanoColorMeta,
) -> Result<(), RawError> {
    let (w, h) = (width as usize, height as usize);
    if rgb_native.len() != w * h * 3 {
        return Err(RawError::Decode(format!(
            "panorama buffer is {} samples but {}x{}x3 = {} expected",
            rgb_native.len(),
            width,
            height,
            w * h * 3
        )));
    }
    // A DNG without a color matrix would silently re-decode through the calibrated fallback,
    // losing the raw-latitude guarantee this whole path exists for. Refuse loudly instead.
    if meta.color_matrix.is_empty() || meta.color_matrix.values().any(|m| m.is_empty()) {
        return Err(RawError::Decode(
            "refusing to write a panorama DNG without a camera color matrix".into(),
        ));
    }

    // Quantize to the full 16-bit range (WhiteLevel below matches).
    let data_u16: Vec<u16> = rgb_native
        .iter()
        .map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16)
        .collect();

    let mut cam = Camera::new();
    cam.make = meta.make.clone();
    cam.model = meta.model.clone();
    cam.clean_make = meta.clean_make.clone();
    cam.clean_model = meta.clean_model.clone();
    cam.color_matrix = meta.color_matrix.clone();
    // Mirrors rawler's own `rgb_image_u16` defensive default; never consulted for LinearRaw data
    // with explicit black/white levels, but `calc_black_levels` asserts on an empty CFA.
    cam.cfa = CFA::new("RGGB");

    let pix = PixU16::new_with(data_u16, w * 3, h);
    let rawimage = RawImage::new(
        cam,
        pix,
        3,
        meta.wb_coeffs,
        RawPhotometricInterpretation::LinearRaw,
        Some(BlackLevel::new(&[0_u32, 0, 0], 1, 1, 3)),
        Some(WhiteLevel::new_bits(16, 3)),
        false,
    );

    let file = std::fs::File::create(dest).map_err(RawError::Io)?;
    let mut dng = DngWriter::new(BufWriter::new(file), DNG_VERSION_V1_4).map_err(de)?;

    // Raw subframe (type 0); thumbnail goes to the root IFD below, mirroring rawler's converter.
    let mut raw_frame = dng.subframe(0);
    raw_frame
        .raw_image(
            &rawimage,
            CropMode::None,
            DngCompression::Lossless,
            DngPhotometricConversion::Original,
            1,
        )
        .map_err(de)?;
    raw_frame.finalize().map_err(de)?;

    // Embedded sRGB preview (subframe 1) + root thumbnail — this is what the library thumbnail
    // path picks up, so a merged pano gets a grid thumb without a full raw develop.
    let preview = preview_srgb(width, height, rgb_native, meta);
    let mut preview_frame = dng.subframe(1);
    preview_frame.preview(&preview, 0.85).map_err(de)?;
    preview_frame.finalize().map_err(de)?;
    dng.thumbnail(&preview).map_err(de)?;

    dng.load_base_tags(&rawimage).map_err(de)?;
    dng.load_metadata(&meta.metadata).map_err(de)?;
    // The EXIF pass-through may re-emit the SOURCE orientation; our pixels are already upright.
    // `add_tag` replaces, so this always wins.
    dng.root_ifd_mut().add_tag(ExifTag::Orientation, 1_u16);
    dng.root_ifd_mut()
        .add_tag(rawler::tags::TiffCommonTag::Software, "Darkroom");

    dng.close().map_err(de)?;
    Ok(())
}

/// Display-referred JPEG of a camera-native buffer (as-shot WB + camera→sRGB + OETF), long side
/// ≤ `max_edge`. Serves the merge dialog's live preview — same math as the embedded DNG preview,
/// so what the dialog shows is what the thumbnail will look like.
pub fn native_to_srgb_jpeg(
    width: u32,
    height: u32,
    rgb_native: &[f32],
    meta: &PanoColorMeta,
    max_edge: u32,
    quality: u8,
) -> Result<Vec<u8>, RawError> {
    if rgb_native.len() != width as usize * height as usize * 3 {
        return Err(RawError::Decode("preview buffer length mismatch".into()));
    }
    let img = preview_srgb_edge(width, height, rgb_native, meta, max_edge);
    let mut out = std::io::Cursor::new(Vec::new());
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    img.write_with_encoder(enc).map_err(de)?;
    Ok(out.into_inner())
}

/// Small display-referred preview of a camera-native buffer: box-downsample to ≤1024 px, then
/// as-shot WB + camera→sRGB(D65) matrix + sRGB OETF. Preview-only math (the raw subframe is the
/// source of truth), so the D50/D65 nuance vs the develop path's ProPhoto pipeline is irrelevant.
fn preview_srgb(
    width: u32,
    height: u32,
    rgb_native: &[f32],
    meta: &PanoColorMeta,
) -> image::DynamicImage {
    preview_srgb_edge(width, height, rgb_native, meta, 1024)
}

fn preview_srgb_edge(
    width: u32,
    height: u32,
    rgb_native: &[f32],
    meta: &PanoColorMeta,
    max_edge: u32,
) -> image::DynamicImage {
    let (w, h) = (width as usize, height as usize);
    let long = w.max(h);
    let step = long.div_ceil(max_edge as usize).max(1);
    let (pw, ph) = (w.div_ceil(step), h.div_ceil(step));

    // cam→sRGB, row-normalized so camera neutral maps to sRGB neutral (same recipe as
    // `map_3ch_to_rgb`, targeting sRGB primaries instead of ProPhoto).
    let xyz2cam = padded_xyz2cam(&meta.color_matrix);
    let rgb2cam = normalize(multiply(&xyz2cam, &SRGB_TO_XYZ_D65));
    let cam2rgb = pseudo_inverse(rgb2cam);
    let wb = meta.wb_coeffs;

    let mut out = image::RgbImage::new(pw as u32, ph as u32);
    for (py, row) in out.rows_mut().enumerate() {
        let sy = (py * step).min(h - 1);
        for (px_i, px) in row.enumerate() {
            let sx = (px_i * step).min(w - 1);
            let i = (sy * w + sx) * 3;
            let r = rgb_native[i] * wb[0];
            let g = rgb_native[i + 1] * wb[1];
            let b = rgb_native[i + 2] * wb[2];
            let srgb = [
                cam2rgb[0][0] * r + cam2rgb[0][1] * g + cam2rgb[0][2] * b,
                cam2rgb[1][0] * r + cam2rgb[1][1] * g + cam2rgb[1][2] * b,
                cam2rgb[2][0] * r + cam2rgb[2][1] * g + cam2rgb[2][2] * b,
            ];
            *px = image::Rgb([
                (srgb_oetf(srgb[0]) * 255.0).round() as u8,
                (srgb_oetf(srgb[1]) * 255.0).round() as u8,
                (srgb_oetf(srgb[2]) * 255.0).round() as u8,
            ]);
        }
    }
    image::DynamicImage::ImageRgb8(out)
}

/// Deterministic matrix pick + padding, mirroring `develop::cam_xyz2cam` but over a bare map.
fn padded_xyz2cam(matrices: &HashMap<Illuminant, FlatColorMatrix>) -> [[f32; 3]; 4] {
    let flat = matrices
        .get(&Illuminant::D65)
        .or_else(|| matrices.iter().min_by_key(|(ill, _)| **ill).map(|(_, m)| m))
        .cloned()
        .unwrap_or_default();
    let mut xyz2cam = [[0f32; 3]; 4];
    let comps = (flat.len() / 3).min(4);
    for i in 0..comps {
        for j in 0..3 {
            xyz2cam[i][j] = flat[i * 3 + j];
        }
    }
    xyz2cam
}

/// sRGB OETF (gamma encode): linear [0,1] → display-encoded [0,1]. Inverse of `display::srgb_eotf`.
fn srgb_oetf(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::develop::map_3ch_to_rgb;
    use rawler::pixarray::Color2D;

    /// A plausible synthetic camera: sRGB-primaries sensor (xyz→cam = XYZ→sRGB), D65 slot only.
    fn test_meta() -> PanoColorMeta {
        let mut color_matrix = HashMap::new();
        let flat: FlatColorMatrix = rawler::imgop::xyz::XYZ_TO_SRGB_D65
            .iter()
            .flatten()
            .copied()
            .collect();
        color_matrix.insert(Illuminant::D65, flat);
        PanoColorMeta {
            color_matrix,
            wb_coeffs: [2.0, 1.0, 1.5, f32::NAN],
            metadata: RawMetadata::default(),
            make: "Darkroom".into(),
            model: "PanoTest".into(),
            clean_make: "Darkroom".into(),
            clean_model: "PanoTest".into(),
            focal_length_mm: Some(24.0),
            orientation: None,
        }
    }

    /// Deterministic 0..1 gradient with all three channels varying independently.
    fn test_native(w: usize, h: usize) -> Vec<f32> {
        let mut data = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                data.push(x as f32 / (w - 1) as f32);
                data.push(y as f32 / (h - 1) as f32);
                data.push(((x + y) as f32 / (w + h - 2) as f32) * 0.5);
            }
        }
        data
    }

    fn temp_dng(name: &str) -> std::path::PathBuf {
        let p =
            std::env::temp_dir().join(format!("darkroom-pano-{}-{}.dng", name, std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// THE P0 guarantee: a written pano DNG re-decodes as cpp=3 LinearRaw with the matrix and
    /// as-shot WB intact, and `develop_linear` on it reproduces the exact wb+matrix mapping of the
    /// original camera-native buffer — i.e. the headroom-preserving ProPhoto branch is taken, NOT
    /// the calibrated fallback. This is what makes a merged pano edit like a native raw.
    #[test]
    fn linear_dng_round_trip_preserves_color_pipeline() {
        let (w, h) = (32usize, 24usize);
        let native = test_native(w, h);
        let meta = test_meta();
        let path = temp_dng("roundtrip");
        write_pano_dng(&path, w as u32, h as u32, &native, &meta).expect("write DNG");

        // Raw-level assertions: photometric, cpp, wb, matrix, whitelevel.
        let src = RawSource::new(&path).expect("open DNG");
        let decoder = rawler::get_decoder(&src).expect("decoder for DNG");
        let raw = decoder
            .raw_image(&src, &RawDecodeParams::default(), false)
            .expect("decode DNG");
        assert!(
            matches!(raw.photometric, RawPhotometricInterpretation::LinearRaw),
            "photometric must survive as LinearRaw, got {:?}",
            raw.photometric
        );
        assert_eq!(raw.cpp, 3, "cpp must survive as 3");
        assert_eq!((raw.width, raw.height), (w, h));
        for (i, expect) in [2.0f32, 1.0, 1.5].iter().enumerate() {
            assert!(
                (raw.wb_coeffs[i] - expect).abs() < 1e-3,
                "wb[{i}] = {} (want {expect}) — AsShotNeutral did not round-trip",
                raw.wb_coeffs[i]
            );
        }
        assert!(
            cam_xyz2cam(&raw).is_some(),
            "color matrix missing after round-trip — decode would fall to the calibrated path"
        );
        assert_eq!(raw.whitelevel.0[0], 65535, "whitelevel must be full 16-bit");

        // Pipeline-level assertion: develop_linear(DNG) == map_3ch_to_rgb(native) within
        // quantization tolerance (u16 samples + 1e-4 SRational matrix coefficients).
        let developed = crate::develop_linear(&src).expect("develop pano DNG");
        assert_eq!((developed.width, developed.height), (w as u32, h as u32));
        let px = Color2D::new_with(
            native.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
            w,
            h,
        );
        let expected = map_3ch_to_rgb(&px, &meta.wb_coeffs, padded_xyz2cam(&meta.color_matrix));
        let expected = expected.flatten();
        let mut max_err = 0f32;
        for (a, b) in developed.data.iter().zip(expected.iter()) {
            max_err = max_err.max((a - b).abs());
        }
        assert!(
            max_err < 3e-3,
            "developed pano diverges from the native wb+matrix mapping (max err {max_err}) — \
             the calibrated fallback likely ran"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_pano_dng_refuses_missing_matrix() {
        let (w, h) = (4usize, 4usize);
        let native = test_native(w, h);
        let mut meta = test_meta();
        meta.color_matrix.clear();
        let path = temp_dng("nomatrix");
        let err = write_pano_dng(&path, w as u32, h as u32, &native, &meta);
        assert!(err.is_err(), "an empty color matrix must be refused");
        assert!(!path.exists() || std::fs::remove_file(&path).is_ok());
    }

    #[test]
    fn write_pano_dng_refuses_wrong_buffer_length() {
        let meta = test_meta();
        let path = temp_dng("badlen");
        assert!(write_pano_dng(&path, 8, 8, &[0.0; 10], &meta).is_err());
    }

    /// Fixture-gated: camera-native decode of the committed R7 CR3 (skips without library/2026).
    #[test]
    fn develop_camera_native_decodes_committed_cr3() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../library/2026/2026-06-06");
        let Ok(dir) = dir.canonicalize() else {
            assert!(
                std::env::var_os("DARKROOM_REQUIRE_FIXTURES").is_none(),
                "CR3 fixture (library/2026) missing but DARKROOM_REQUIRE_FIXTURES is set"
            );
            eprintln!("library/2026 not present — skipping");
            return;
        };
        let Some(path) = std::fs::read_dir(&dir).ok().and_then(|rd| {
            let mut v: Vec<_> = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.extension()
                        .and_then(|s| s.to_str())
                        .map(|s| s.eq_ignore_ascii_case("cr3"))
                        .unwrap_or(false)
                })
                .collect();
            v.sort();
            v.into_iter().next()
        }) else {
            eprintln!("no CR3 in fixture dir — skipping");
            return;
        };

        let src = crate::source_from_path(&path).expect("open CR3");
        let native = develop_camera_native(&src).expect("camera-native decode");
        assert!(native.width > 1000 && native.height > 1000);
        assert_eq!(
            native.data.len(),
            native.width as usize * native.height as usize * 3
        );
        let (make, model) = native.meta.camera_id();
        assert!(!make.is_empty() && !model.is_empty());
        assert!(!native.meta.color_matrix.is_empty());
        // Rescaled data is nominally 0..1 (small excursions allowed for specular/negative noise).
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for v in native.data.iter().step_by(1001) {
            lo = lo.min(*v);
            hi = hi.max(*v);
        }
        assert!(lo > -0.5 && hi < 4.0, "implausible native range {lo}..{hi}");

        // And the full circle: write a downscaled crop back out as a DNG and re-develop it.
        let small = LinearImage {
            width: native.width,
            height: native.height,
            data: native.data.clone(),
        }
        .downscale_into(512);
        let path = temp_dng("cr3-e2e");
        write_pano_dng(&path, small.width, small.height, &small.data, &native.meta)
            .expect("write CR3-derived DNG");
        let dng_src = crate::source_from_path(&path).expect("open written DNG");
        let developed = crate::develop_linear(&dng_src).expect("develop written DNG");
        assert_eq!(
            (developed.width, developed.height),
            (small.width, small.height)
        );
        let _ = std::fs::remove_file(&path);
    }
}
