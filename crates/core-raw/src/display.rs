//! Display-referred image support (JPEG / PNG) — the non-RAW decode path.
//!
//! Unlike RAW (scene-linear, demosaiced + color-matrixed here), JPEG/PNG arrive **already
//! developed**: 8-bit, sRGB-encoded, with the camera tone curve baked in. We decode them into the
//! same [`LinearImage`] contract the GPU pipeline expects — **linear wide-gamut ProPhoto** — by
//! applying the sRGB EOTF then mapping linear sRGB → linear ProPhoto with the exact inverse of the
//! shader's `pp_to_srgb` matrix. Because the develop shader converts ProPhoto → sRGB at the display
//! transition (and the base tone operator is bypassed for these images, see `display_referred` in
//! core-pipeline), an unedited JPEG round-trips to itself.
//!
//! All `rawler` calls stay out of this module; the RAW public fns in `develop`/`thumb`/`meta`
//! dispatch here when the source path is a display format.
//!
//! Known limitations (MVP): assumes sRGB primaries (embedded ICC profiles — Display-P3, Adobe RGB —
//! are not yet honored); PNG alpha is dropped (treated opaque); 16-bit PNG is down-converted to 8-bit.

use crate::develop::LinearImage;
use crate::error::RawError;
use crate::meta::RawMeta;
use image::metadata::Orientation;
use image::{DynamicImage, GenericImageView};
use rawler::imgop::matrix::pseudo_inverse;
use std::path::Path;

/// Source image family, decided by file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Raw,
    Jpeg,
    Png,
}

impl ImageKind {
    /// Catalog/UI string (`"raw" | "jpeg" | "png"`).
    pub fn as_str(self) -> &'static str {
        match self {
            ImageKind::Raw => "raw",
            ImageKind::Jpeg => "jpeg",
            ImageKind::Png => "png",
        }
    }
}

/// Classify a path by extension (case-insensitive). Unknown extensions are treated as `Raw` so the
/// existing rawler path handles them (and reports its own decode error), preserving prior behavior.
pub fn classify(path: &Path) -> ImageKind {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => ImageKind::Jpeg,
        Some("png") => ImageKind::Png,
        _ => ImageKind::Raw,
    }
}

/// `true` when the path is an already-developed display image (JPEG/PNG) that must skip the rawler
/// decode path.
pub fn is_display(path: &Path) -> bool {
    !matches!(classify(path), ImageKind::Raw)
}

/// The shader's linear-ProPhoto → linear-sRGB matrix (`develop.wgsl:pp_to_srgb`), row-major. We
/// invert it so decode is the exact inverse of the display transition → unedited round-trip.
const PP_TO_SRGB: [[f32; 3]; 3] = [
    [1.8215216, -0.5579748, -0.2635469],
    [-0.2385862, 1.2344216, 0.0041646],
    [-0.0199185, -0.1907297, 1.2106482],
];

/// linear sRGB → linear ProPhoto (the inverse of [`PP_TO_SRGB`]).
fn srgb_to_prophoto() -> [[f32; 3]; 3] {
    pseudo_inverse(PP_TO_SRGB)
}

/// sRGB EOTF (gamma decode): display-encoded [0,1] → linear [0,1].
fn srgb_eotf(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Decode JPEG/PNG bytes → linear-ProPhoto [`LinearImage`], uprighted to `orientation`.
pub fn decode_display_linear(
    bytes: &[u8],
    orientation: Option<u16>,
) -> Result<LinearImage, RawError> {
    let img = image::load_from_memory(bytes)?;
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    let m = srgb_to_prophoto();

    // Precompute the 8-bit sRGB → linear-sRGB LUT once (256 entries) — far cheaper than powf/px.
    let mut eotf = [0f32; 256];
    for (i, e) in eotf.iter_mut().enumerate() {
        *e = srgb_eotf(i as f32 / 255.0);
    }

    let mut data = Vec::with_capacity(w as usize * h as usize * 3);
    for px in rgb.pixels() {
        let r = eotf[px[0] as usize];
        let g = eotf[px[1] as usize];
        let b = eotf[px[2] as usize];
        // linear sRGB → linear ProPhoto (sRGB gamut ⊂ ProPhoto, so components stay in [0,1]).
        data.push(m[0][0] * r + m[0][1] * g + m[0][2] * b);
        data.push(m[1][0] * r + m[1][1] * g + m[1][2] * b);
        data.push(m[2][0] * r + m[2][1] * g + m[2][2] * b);
    }

    Ok(LinearImage {
        width: w,
        height: h,
        data,
    }
    .oriented(orientation))
}

/// Generate a thumbnail JPEG from JPEG/PNG bytes (decode → orient → downscale → encode). Mirrors the
/// [`crate::thumb::Thumb`] shape produced by the RAW path. The source IS the preview — no embedded
/// extraction.
pub fn decode_display_thumb(
    bytes: &[u8],
    orientation: Option<u16>,
    max_edge: u32,
    quality: u8,
) -> Result<crate::thumb::Thumb, RawError> {
    use image::codecs::jpeg::JpegEncoder;
    use image::ExtendedColorType;

    let mut img = image::load_from_memory(bytes)?;
    let (w, h) = img.dimensions();
    if let Some(o) = orientation.and_then(|v| Orientation::from_exif(v as u8)) {
        img.apply_orientation(o);
    }
    let (ow, oh) = img.dimensions();
    let scaled = if ow.max(oh) > max_edge {
        img.thumbnail(max_edge, max_edge)
    } else {
        img
    };
    let rgb = scaled.to_rgb8();
    let mut buf = Vec::new();
    JpegEncoder::new_with_quality(&mut buf, quality).encode(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        ExtendedColorType::Rgb8,
    )?;
    Ok(crate::thumb::Thumb {
        jpeg: buf,
        src_width: w,
        src_height: h,
        disp_width: ow,
        disp_height: oh,
    })
}

/// Decode JPEG/PNG bytes to oriented pixels (display space) — the analogue of the RAW embedded
/// preview, for thumbnail-free callers (AI scan, import preview). Applies EXIF orientation.
pub fn decode_display_preview(bytes: &[u8]) -> Result<DynamicImage, RawError> {
    let mut img = image::load_from_memory(bytes)?;
    if let Some(o) = exif_orientation(bytes).and_then(|v| Orientation::from_exif(v as u8)) {
        img.apply_orientation(o);
    }
    Ok(img)
}

/// Decode JPEG/PNG bytes to native (un-oriented) pixels plus the EXIF orientation — mirrors
/// [`crate::thumb::preview_with_orientation`] so a unified scan can derive both views from one decode.
pub fn decode_display_preview_native(
    bytes: &[u8],
) -> Result<(DynamicImage, Option<Orientation>), RawError> {
    let img = image::load_from_memory(bytes)?;
    let o = exif_orientation(bytes).and_then(|v| Orientation::from_exif(v as u8));
    Ok((img, o))
}

/// Read the EXIF orientation tag (1–8) from JPEG/PNG bytes, if present.
pub fn exif_orientation(bytes: &[u8]) -> Option<u16> {
    let exif = read_exif(bytes)?;
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .map(|v| v as u16)
}

fn read_exif(bytes: &[u8]) -> Option<exif::Exif> {
    let mut cursor = std::io::Cursor::new(bytes);
    exif::Reader::new().read_from_container(&mut cursor).ok()
}

/// Catalog metadata for a JPEG/PNG via its EXIF segment (best-effort — many fields absent for
/// screenshots / web images / PNG). Orientation always reflects the tag; capture-date falls back to
/// file mtime at the indexer (`core-library`).
pub fn read_display_meta(bytes: &[u8]) -> RawMeta {
    let Some(exif) = read_exif(bytes) else {
        return RawMeta::default();
    };

    let ascii = |tag: exif::Tag| -> Option<String> {
        exif.get_field(tag, exif::In::PRIMARY).and_then(|f| {
            if let exif::Value::Ascii(ref v) = f.value {
                v.first()
                    .map(|b| String::from_utf8_lossy(b).trim().to_string())
                    .filter(|s| !s.is_empty())
            } else {
                None
            }
        })
    };
    let rational = |tag: exif::Tag| -> Option<f64> {
        exif.get_field(tag, exif::In::PRIMARY).and_then(|f| {
            if let exif::Value::Rational(ref v) = f.value {
                v.first().map(|r| r.to_f64())
            } else {
                None
            }
        })
    };
    let uint = |tag: exif::Tag| -> Option<u32> {
        exif.get_field(tag, exif::In::PRIMARY)
            .and_then(|f| f.value.get_uint(0))
    };

    let dto = ascii(exif::Tag::DateTimeOriginal).or_else(|| ascii(exif::Tag::DateTime));
    let shutter = exif
        .get_field(exif::Tag::ExposureTime, exif::In::PRIMARY)
        .and_then(|f| {
            if let exif::Value::Rational(ref v) = f.value {
                v.first().copied()
            } else {
                None
            }
        })
        .map(|r| {
            let val = r.to_f64();
            if val >= 1.0 {
                format!("{}s", (val * 10.0).round() / 10.0)
            } else if r.num != 0 {
                format!("1/{}", (r.denom as f64 / r.num as f64).round() as u32)
            } else {
                "0".to_string()
            }
        });

    RawMeta {
        camera_make: ascii(exif::Tag::Make),
        camera_model: ascii(exif::Tag::Model),
        body_serial: ascii(exif::Tag::BodySerialNumber),
        lens: ascii(exif::Tag::LensModel),
        capture_date: dto.as_deref().and_then(crate::meta::parse_exif_dt),
        date_time_original: dto,
        sub_sec: ascii(exif::Tag::SubSecTimeOriginal),
        iso: uint(exif::Tag::PhotographicSensitivity).map(|v| v as i64),
        shutter,
        aperture: rational(exif::Tag::FNumber),
        focal_length: rational(exif::Tag::FocalLength),
        orientation: uint(exif::Tag::Orientation).map(|v| v as i64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn classify_by_extension() {
        assert_eq!(classify(Path::new("a.JPG")), ImageKind::Jpeg);
        assert_eq!(classify(Path::new("a.jpeg")), ImageKind::Jpeg);
        assert_eq!(classify(Path::new("a.PNG")), ImageKind::Png);
        assert_eq!(classify(Path::new("a.cr3")), ImageKind::Raw);
        assert_eq!(classify(Path::new("a.nef")), ImageKind::Raw);
        assert!(is_display(Path::new("a.png")) && !is_display(Path::new("a.dng")));
    }

    /// Apply the shader's display transition (linear ProPhoto → linear sRGB → sRGB OETF → 8-bit),
    /// mirroring `pp_to_srgb` + `srgb_encode` in `develop.wgsl` with the base tone operator bypassed.
    fn prophoto_to_srgb8(pp: [f32; 3]) -> [u8; 3] {
        let lin = [
            PP_TO_SRGB[0][0] * pp[0] + PP_TO_SRGB[0][1] * pp[1] + PP_TO_SRGB[0][2] * pp[2],
            PP_TO_SRGB[1][0] * pp[0] + PP_TO_SRGB[1][1] * pp[1] + PP_TO_SRGB[1][2] * pp[2],
            PP_TO_SRGB[2][0] * pp[0] + PP_TO_SRGB[2][1] * pp[1] + PP_TO_SRGB[2][2] * pp[2],
        ];
        let oetf = |c: f32| -> f32 {
            let c = c.clamp(0.0, 1.0);
            if c <= 0.0031308 {
                c * 12.92
            } else {
                1.055 * c.powf(1.0 / 2.4) - 0.055
            }
        };
        [
            (oetf(lin[0]) * 255.0).round() as u8,
            (oetf(lin[1]) * 255.0).round() as u8,
            (oetf(lin[2]) * 255.0).round() as u8,
        ]
    }

    /// A lossless PNG decoded to linear-ProPhoto and pushed back through the display transition (base
    /// tone bypassed, as for a display-referred image) must reproduce the original pixels (±1/255).
    #[test]
    fn srgb_png_round_trips_through_display_transition() {
        let colors: [[u8; 3]; 6] = [
            [0, 0, 0],
            [255, 255, 255],
            [128, 64, 32],
            [200, 10, 90],
            [12, 200, 240],
            [127, 127, 127],
        ];
        let mut img = image::RgbImage::new(colors.len() as u32, 1);
        for (x, c) in colors.iter().enumerate() {
            img.put_pixel(x as u32, 0, image::Rgb(*c));
        }
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();

        let lin = decode_display_linear(&png, None).unwrap();
        assert_eq!((lin.width, lin.height), (colors.len() as u32, 1));
        for (i, expect) in colors.iter().enumerate() {
            let pp = [lin.data[i * 3], lin.data[i * 3 + 1], lin.data[i * 3 + 2]];
            let got = prophoto_to_srgb8(pp);
            for ch in 0..3 {
                assert!(
                    (got[ch] as i32 - expect[ch] as i32).abs() <= 1,
                    "channel {ch} of color {i}: got {got:?} expected {expect:?}"
                );
            }
        }
    }

    /// The real ingest path: a `.png` on disk → `source_from_path` → `develop_linear` dispatches to
    /// the display decoder (via `RawSource::path()`/`buf()`), never the rawler path. Also exercises
    /// `read_metadata` + `thumbnail_jpeg` on the same source.
    #[test]
    fn on_disk_png_dispatches_to_display_path() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("darkroom_disp_{}.png", std::process::id()));
        let mut img = image::RgbImage::new(4, 3);
        img.put_pixel(0, 0, image::Rgb([200, 100, 50]));
        image::DynamicImage::ImageRgb8(img).save(&path).unwrap();

        let src = crate::source_from_path(&path).unwrap();
        let lin = crate::develop_linear(&src).unwrap();
        assert_eq!((lin.width, lin.height), (4, 3));
        // Metadata + thumbnail must succeed too (no rawler decode error).
        let _meta = crate::read_metadata(&src).unwrap();
        let thumb = crate::thumbnail_jpeg(&src, 512, 82).unwrap();
        assert!(!thumb.jpeg.is_empty());

        let _ = std::fs::remove_file(&path);
    }

    /// A PNG with no EXIF segment yields an all-`None` metadata (mtime fallback happens at the
    /// indexer), never an error.
    #[test]
    fn png_without_exif_has_empty_meta() {
        let img = image::RgbImage::new(2, 2);
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let meta = read_display_meta(&png);
        assert!(meta.camera_model.is_none() && meta.capture_date.is_none());
        assert!(exif_orientation(&png).is_none());
    }
}
