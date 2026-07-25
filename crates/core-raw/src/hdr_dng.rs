//! Export a merged-HDR (linear ProPhoto, fp16-in-`.exr`) image as an uncompressed FLOAT LinearRaw
//! DNG — Lightroom/ACR interop. Unlike [`crate::pano::write_pano_dng`] (16-bit integer, so
//! DARKROOM's own reimport reproduces the exact camera wb+matrix math), this format exists purely
//! for EXPORT: the pixel data is IEEE-754 float, so the full >1.0 scene-referred headroom
//! Merge-to-HDR produced survives untouched — no bit-depth quantization, no clipping. Darkroom
//! itself never re-reads this file (it always re-decodes the source `.exr`), so there is no
//! round-trip requirement beyond "a DNG reader decodes it correctly."
//!
//! Like the rest of this crate, all rawler calls stay HERE.

use std::collections::HashMap;
use std::io::BufWriter;
use std::path::Path;

use rawler::decoders::{Camera, RawMetadata};
use rawler::dng::writer::DngWriter;
use rawler::dng::{CropMode, DngCompression, DngPhotometricConversion, DNG_VERSION_V1_4};
use rawler::exif::Exif;
use rawler::formats::tiff::Rational;
use rawler::imgop::xyz::Illuminant;
use rawler::rawimage::{BlackLevel, RawImageData, RawPhotometricInterpretation, WhiteLevel};
use rawler::tags::ExifTag;
use rawler::{RawImage, CFA};

use crate::color::XYZ_TO_PROPHOTO_D50;
use crate::develop::LinearImage;
use crate::display::linear_to_srgb_rgb8;
use crate::error::RawError;
use crate::meta::RawMeta;

fn de(e: impl std::fmt::Display) -> RawError {
    RawError::Decode(e.to_string())
}

/// Author an uncompressed FLOAT LinearRaw DNG from a merged-HDR linear-ProPhoto image.
///
/// `img`/`meta` are exactly [`crate::hdr_file::read_hdr_linear`] / [`crate::hdr_file::read_hdr_meta`]'s
/// output — the caller re-decodes the source `.exr`; this function never touches the catalog (it is
/// export-only, not an import path). Durability mirrors `hdr_file::write_hdr_exr`: the bytes go to
/// `<dest>.part` first, then an atomic rename.
pub fn write_hdr_dng(dest: &Path, img: &LinearImage, meta: &RawMeta) -> Result<(), RawError> {
    let (w, h) = (img.width as usize, img.height as usize);
    if img.data.len() != w * h * 3 || w == 0 || h == 0 {
        return Err(de(format!(
            "HDR DNG buffer is {} samples but {w}x{h}x3 = {} expected",
            img.data.len(),
            w * h * 3
        )));
    }

    let mut cam = Camera::new();
    cam.make = meta
        .camera_make
        .clone()
        .unwrap_or_else(|| "Darkroom".into());
    cam.model = meta
        .camera_model
        .clone()
        .unwrap_or_else(|| "Merged HDR".into());
    cam.clean_make = cam.make.clone();
    cam.clean_model = cam.model.clone();
    // The merged buffer IS ProPhoto already (white balance + color matrix are baked in by the
    // merge/decode) — unlike `write_pano_dng`'s sensor-native buffer, "camera native" here already
    // equals ProPhoto. So ColorMatrix1 is fixed at XYZ→ProPhoto(D50) — the same working-space
    // constant `color.rs` uses for the HEIF PQ decode (identical to rawler's own
    // `imgop::xyz::XYZ_TO_PROFOTORGB_D50`, per that module's doc comment) — and AsShotNeutral is
    // identity: there is no white balance left to apply on decode.
    let xyz_to_prophoto: Vec<f32> = XYZ_TO_PROPHOTO_D50
        .iter()
        .flatten()
        .map(|&v| v as f32)
        .collect();
    cam.color_matrix = HashMap::from([(Illuminant::D50, xyz_to_prophoto)]);
    // Never consulted for cpp=3 LinearRaw with explicit black/white levels below — mirrors
    // `write_pano_dng`'s defensive default (rawler's `calc_black_levels` asserts on an empty CFA).
    cam.cfa = CFA::new("RGGB");

    let rawimage = RawImage::new_with_data(
        cam,
        RawImageData::Float(img.data.clone()),
        w * 3,
        h,
        3,
        [1.0, 1.0, 1.0, f32::NAN], // AsShotNeutral = [1,1,1] — see ColorMatrix1 note above.
        RawPhotometricInterpretation::LinearRaw,
        Some(BlackLevel::new(&[0_u32, 0, 0], 1, 1, 3)),
        Some(WhiteLevel::new(vec![1_u32, 1, 1])),
        false,
    );

    let part = dest.with_extension("dng.part");
    {
        let file = std::fs::File::create(&part).map_err(RawError::Io)?;
        let mut dng = DngWriter::new(BufWriter::new(file), DNG_VERSION_V1_4).map_err(de)?;

        let mut raw_frame = dng.subframe(0);
        raw_frame
            .raw_image(
                &rawimage,
                CropMode::None,
                // MUST be Uncompressed: rawler's `write_rawimage` silently force-converts Float data
                // to u16 before Lossless (LJPEG92) compression —
                // `if compression == DngCompression::Lossless && matches!(rawimage.data,
                // RawImageData::Float(_)) { rawimage.to_mut().data.force_integer(); ... }`
                // (rawler 0.7.2 `src/dng/writer.rs::write_rawimage`, ~line 146-153) — which would
                // silently discard the entire point of a float DNG (headroom beyond 1.0, no 16-bit
                // quantization). Uncompressed strips are the only rawler path that keeps float data
                // float (`dng_put_raw_uncompressed` writes SampleFormat=3/IEEE-float, BitsPerSample=32).
                DngCompression::Uncompressed,
                DngPhotometricConversion::Original,
                1,
            )
            .map_err(de)?;
        raw_frame.finalize().map_err(de)?;

        // Embedded sRGB preview + thumbnail — this is what the library thumbnail path (and a DNG
        // viewer without linear-float support) picks up. Downscale in LINEAR light first (cheap;
        // mirrors `hdr_file::decode_hdr_thumb`), then the display transition; `dng.preview()` /
        // `dng.thumbnail()` each further downscale internally, so this only needs to be "small
        // enough", not exact.
        let preview_lin = img.downscaled(1024);
        let preview = image::DynamicImage::ImageRgb8(linear_to_srgb_rgb8(&preview_lin));
        let mut preview_frame = dng.subframe(1);
        preview_frame.preview(&preview, 0.85).map_err(de)?;
        preview_frame.finalize().map_err(de)?;
        dng.thumbnail(&preview).map_err(de)?;

        dng.load_base_tags(&rawimage).map_err(de)?;
        dng.load_metadata(&metadata_from_raw_meta(meta))
            .map_err(de)?;
        // The merged buffer is already upright (same convention as `write_pano_dng`) — any source
        // orientation EXIF pass-through would be stale, so pin it to 1.
        dng.root_ifd_mut().add_tag(ExifTag::Orientation, 1_u16);
        dng.root_ifd_mut()
            .add_tag(rawler::tags::TiffCommonTag::Software, "Darkroom");

        dng.close().map_err(de)?;
    }
    std::fs::rename(&part, dest).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        de(e.to_string())
    })?;
    Ok(())
}

/// Best-effort [`RawMetadata`] (rawler's own EXIF container) built from Darkroom's thinner
/// [`RawMeta`], so `dng.load_metadata()` writes the EXIF tags through the exact same machinery
/// `write_pano_dng` uses — just from a smaller source (the merged EXR stores `RawMeta` JSON, not a
/// full rawler `RawMetadata`). Every `Exif` field is `Option`, so whatever `RawMeta` doesn't carry
/// (GPS, lens serial, …) is simply omitted from the DNG, not faked.
fn metadata_from_raw_meta(meta: &RawMeta) -> RawMetadata {
    let exif = Exif {
        date_time_original: meta.date_time_original.clone(),
        fnumber: meta.aperture.map(|a| Rational::new_f64(a, 100)),
        focal_length: meta.focal_length.map(|f| Rational::new_f64(f, 100)),
        iso_speed_ratings: meta.iso.and_then(|v| u16::try_from(v).ok()),
        lens_model: meta.lens.clone(),
        serial_number: meta.body_serial.clone(),
        exposure_time: meta.shutter.as_deref().and_then(parse_shutter),
        ..Default::default()
    };
    RawMetadata {
        exif,
        model: meta.camera_model.clone().unwrap_or_default(),
        make: meta.camera_make.clone().unwrap_or_default(),
        ..Default::default()
    }
}

/// Best-effort inverse of `meta.rs`'s shutter formatting (`"1/80"` | `"2.0s"`) back into an EXIF
/// ExposureTime rational. Lossy — the display string already lost precision — which is fine for a
/// passthrough EXIF hint; nothing in Darkroom itself depends on this round-tripping.
fn parse_shutter(s: &str) -> Option<Rational> {
    if let Some(denom) = s.strip_prefix("1/") {
        denom.parse::<u32>().ok().map(|d| Rational::new(1, d))
    } else if let Some(secs) = s.strip_suffix('s') {
        secs.parse::<f32>().ok().map(|v| Rational::new_f32(v, 1000))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawler::decoders::RawDecodeParams;
    use rawler::rawsource::RawSource;

    /// Deterministic gradient with values both inside [0,1] and above 1.0 (specular headroom) —
    /// exactly the kind of data Merge-to-HDR produces and a 16-bit path would clip/quantize.
    fn test_image(w: u32, h: u32) -> LinearImage {
        let mut data = Vec::with_capacity(w as usize * h as usize * 3);
        for y in 0..h {
            for x in 0..w {
                data.push(x as f32 / (w - 1).max(1) as f32 * 2.5); // > 1.0 headroom
                data.push(y as f32 / (h - 1).max(1) as f32);
                data.push(0.001_234); // a value 16-bit quantization would visibly round
            }
        }
        LinearImage {
            width: w,
            height: h,
            data,
        }
    }

    fn temp_dng(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "darkroom-hdr-dng-{}-{}.dng",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// THE round-trip guarantee: a written HDR DNG re-decodes as cpp=3 LinearRaw with the exact
    /// dimensions, and the raw sample data — read back with no color/WB processing — matches the
    /// input to float precision (no 16-bit quantization, headroom above 1.0 intact).
    #[test]
    fn float_dng_round_trips_without_quantization() {
        let (w, h) = (16u32, 12u32);
        let img = test_image(w, h);
        let meta = RawMeta {
            camera_make: Some("Darkroom".into()),
            camera_model: Some("Merge Test".into()),
            iso: Some(400),
            aperture: Some(8.0),
            shutter: Some("1/80".into()),
            focal_length: Some(24.0),
            ..Default::default()
        };
        let path = temp_dng("roundtrip");
        write_hdr_dng(&path, &img, &meta).expect("write HDR DNG");

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
        assert_eq!((raw.width, raw.height), (w as usize, h as usize));

        let decoded = raw.data.as_f32();
        assert_eq!(decoded.len(), img.data.len());
        // Probe a spread of pixels (corners + middle), including a >1.0 headroom sample and the
        // fixed low-value channel a 16-bit path would round.
        let probes = [
            0usize,
            (w as usize - 1) * 3,
            decoded.len() - 3,
            decoded.len() / 2,
        ];
        for i in probes {
            for c in 0..3 {
                let (a, b) = (decoded[i + c], img.data[i + c]);
                assert!(
                    (a - b).abs() < 1e-6,
                    "sample [{i}+{c}] = {a} (want {b}) — float data did not survive uncompressed"
                );
            }
        }
        let max_headroom = decoded.iter().cloned().fold(0f32, f32::max);
        assert!(
            max_headroom > 1.0,
            "specular headroom above 1.0 must survive the float DNG (got max {max_headroom})"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_hdr_dng_refuses_wrong_buffer_length() {
        let meta = RawMeta::default();
        let path = temp_dng("badlen");
        let img = LinearImage {
            width: 8,
            height: 8,
            data: vec![0.0; 10],
        };
        assert!(write_hdr_dng(&path, &img, &meta).is_err());
        assert!(!path.exists() || std::fs::remove_file(&path).is_ok());
    }

    /// `parse_shutter` round-trips `meta.rs`'s own formatting for both branches it produces.
    #[test]
    fn parse_shutter_matches_meta_rs_formatting() {
        let r = parse_shutter("1/80").expect("fast shutter");
        assert_eq!((r.n, r.d), (1, 80));
        let r = parse_shutter("2.5s").expect("slow shutter");
        assert!((r.as_f32() - 2.5).abs() < 1e-3);
        assert!(parse_shutter("bogus").is_none());
    }
}
