//! Embedded-preview → thumbnail JPEG extraction.
//!
//! For Canon CR3 the embedded preview is full-resolution (e.g. 6960×4640), so we downscale it
//! to a grid-friendly edge and re-encode as JPEG. This is the demosaic-free Tier-0/1 path.

use crate::error::RawError;
use image::codecs::jpeg::JpegEncoder;
use image::metadata::Orientation;
use image::{DynamicImage, ExtendedColorType, GenericImageView};
use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;

/// A generated thumbnail plus the source (full-image) dimensions.
pub struct Thumb {
    pub jpeg: Vec<u8>,
    /// NATIVE (pre-orientation, sensor-native) full-image dims. Kept stable for the capture
    /// fingerprint, which must not shift when orientation handling changes.
    pub src_width: u32,
    pub src_height: u32,
    /// ORIENTED (display) full-image dims — width/height after applying EXIF orientation, so a
    /// portrait shot reads as portrait. This is what the catalog stores for aspect/UI logic.
    pub disp_width: u32,
    pub disp_height: u32,
}

fn de(e: impl std::fmt::Display) -> RawError {
    RawError::Decode(e.to_string())
}

/// The embedded preview of a RAW, or `None` when the file has none we can read.
///
/// Canon's **HDR-PQ CR3** (`CompressorVersion` `CanonCR3_003`, written whenever the body is in
/// HDR PQ mode) stores its preview as HEVC rather than JPEG, and rawler returns a hard error for
/// it ("Unable to extract preview image from CR3 HDR-PQ file", dnglab#7) instead of an empty
/// preview. That error is not a corrupt file — the mosaic beside it decodes perfectly — so it must
/// not sink the whole image. Errors are folded into `None` here and callers fall back to
/// developing the RAW themselves ([`developed_preview`]); a file that is genuinely undecodable
/// fails later, on the mosaic, with a truthful error.
fn embedded_preview(
    decoder: &dyn rawler::decoders::Decoder,
    src: &RawSource,
    params: &RawDecodeParams,
) -> Option<DynamicImage> {
    decoder
        .preview_image(src, params)
        .ok()
        .flatten()
        .or_else(|| decoder.full_image(src, params).ok().flatten())
}

/// Demosaic the RAW ourselves and render it to display sRGB — the fallback for files whose
/// embedded preview is unreadable (see [`embedded_preview`]). Half-res via
/// [`crate::develop::develop_linear_preview`] (~0.2 s on a 32 MP HDR-PQ CR3 vs ~1 s full), already
/// EXIF-uprighted by the develop path, then the same scene-linear → sRGB conversion the HEIF/EXR
/// thumbnails use, so a fallback thumbnail matches the rest of the grid.
fn developed_preview(src: &RawSource, max_edge: u32) -> Result<DynamicImage, RawError> {
    let lin = crate::develop::develop_linear_preview(src)?;
    let small = lin.downscale_into(max_edge.max(1));
    Ok(DynamicImage::ImageRgb8(
        crate::display::linear_to_srgb_rgb8(&small),
    ))
}

/// Decode the largest embedded preview to pixels (preview → full-image fallback chain).
pub fn preview_image(src: &RawSource) -> Result<DynamicImage, RawError> {
    use crate::display::ImageKind;
    match crate::display::classify(src.path()) {
        ImageKind::Jpeg | ImageKind::Png => {
            return crate::display::decode_display_preview(&src.as_vec()?)
        }
        ImageKind::Heif => return crate::heif::decode_heif_preview(&src.as_vec()?),
        ImageKind::Hdr => return crate::hdr_file::decode_hdr_preview(&src.as_vec()?),
        ImageKind::Raw => {}
    }
    let decoder = rawler::get_decoder(src).map_err(de)?;
    let params = RawDecodeParams::default();
    if let Some(img) = embedded_preview(decoder.as_ref(), src, &params) {
        return Ok(img);
    }
    // No readable embedded preview (HDR-PQ CR3): develop the mosaic instead. u32::MAX keeps the
    // native preview resolution — callers of this fn size it themselves.
    developed_preview(src, u32::MAX)
}

/// Decode the embedded preview **once** and return the sensor-native pixels plus the EXIF
/// orientation (if any). A unified scan derives the native view (object detectors, which are
/// calibrated on sensor-native pixels) directly and the display view (faces) by applying the
/// orientation — so the JPEG is decoded a single time instead of twice. Mirrors [`preview_image`]'s
/// preview→full fallback chain so the native pixels are byte-identical to it.
pub fn preview_with_orientation(
    src: &RawSource,
) -> Result<(DynamicImage, Option<Orientation>), RawError> {
    use crate::display::ImageKind;
    match crate::display::classify(src.path()) {
        ImageKind::Jpeg | ImageKind::Png => {
            return crate::display::decode_display_preview_native(&src.as_vec()?)
        }
        // HEIF/HDR previews are decoded already-upright (libheif applies container transforms;
        // EXR has no orientation concept), so the native view IS the display view.
        ImageKind::Heif => return Ok((crate::heif::decode_heif_preview(&src.as_vec()?)?, None)),
        ImageKind::Hdr => return Ok((crate::hdr_file::decode_hdr_preview(&src.as_vec()?)?, None)),
        ImageKind::Raw => {}
    }
    let decoder = rawler::get_decoder(src).map_err(de)?;
    let params = RawDecodeParams::default();
    let orientation = decoder
        .raw_metadata(src, &params)
        .ok()
        .and_then(|md| md.exif.orientation)
        .and_then(|v| Orientation::from_exif(v as u8));
    match embedded_preview(decoder.as_ref(), src, &params) {
        Some(img) => Ok((img, orientation)),
        // Developed fallback (HDR-PQ CR3) comes back already uprighted, so report no further
        // rotation — otherwise the caller would apply the EXIF tag a second time.
        None => Ok((developed_preview(src, u32::MAX)?, None)),
    }
}

/// Embedded preview **uprighted to its EXIF orientation** — i.e. display space, matching what
/// [`thumbnail_jpeg`] serves (unlike [`preview_image`], which is sensor-native). Use this when boxes
/// derived from the pixels must line up with the displayed thumbnail (face detection / overlays).
pub fn oriented_preview(src: &RawSource) -> Result<DynamicImage, RawError> {
    let (mut img, orientation) = preview_with_orientation(src)?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(img)
}

/// Extract the embedded preview, apply EXIF orientation, downscale so the longest edge ≤ `max_edge`,
/// encode JPEG at `quality`.
///
/// One decoder handles both the preview decode and the orientation read (the embedded preview is
/// sensor-native, so portraits arrive sideways until we upright them from the EXIF tag). The
/// returned `src_*` dims are the *native* preview dimensions (pre-orientation) so the capture
/// fingerprint stays stable across this change.
pub fn thumbnail_jpeg(src: &RawSource, max_edge: u32, quality: u8) -> Result<Thumb, RawError> {
    use crate::display::ImageKind;
    match crate::display::classify(src.path()) {
        ImageKind::Jpeg | ImageKind::Png => {
            let bytes = src.as_vec()?;
            let orientation = crate::display::exif_orientation(&bytes);
            return crate::display::decode_display_thumb(&bytes, orientation, max_edge, quality);
        }
        ImageKind::Heif => {
            return crate::heif::decode_heif_thumb(&src.as_vec()?, max_edge, quality)
        }
        ImageKind::Hdr => {
            return crate::hdr_file::decode_hdr_thumb(&src.as_vec()?, max_edge, quality)
        }
        ImageKind::Raw => {}
    }
    let decoder = rawler::get_decoder(src).map_err(de)?;
    let params = RawDecodeParams::default();
    let exif_orientation = decoder
        .raw_metadata(src, &params)
        .ok()
        .and_then(|md| md.exif.orientation);

    // `(w, h)` = NATIVE (pre-orientation) dims, which feed the capture fingerprint; `(ow, oh)` =
    // ORIENTED (display) dims for the catalog. Both describe the FULL image, never the thumbnail.
    let (img, w, h, ow, oh) = match embedded_preview(decoder.as_ref(), src, &params) {
        Some(img) => {
            let (w, h) = img.dimensions();
            // Upright the preview from its EXIF orientation (1–8). Absent/unknown → already upright.
            let mut img = img;
            if let Some(o) = exif_orientation.and_then(|v| Orientation::from_exif(v as u8)) {
                img.apply_orientation(o);
            }
            let (ow, oh) = img.dimensions();
            (img, w, h, ow, oh)
        }
        None => {
            // HDR-PQ CR3: no readable embedded preview, so develop the mosaic. `develop_linear`
            // (not the half-res preview path) because the dims reported here must be the TRUE
            // sensor ones. Its output is already uprighted, so those dims ARE the display dims and
            // the native pair is recovered by undoing a quarter-turn orientation.
            let lin = crate::develop::develop_linear(src)?;
            let (ow, oh) = (lin.width, lin.height);
            let (w, h) = if matches!(exif_orientation, Some(5..=8)) {
                (oh, ow)
            } else {
                (ow, oh)
            };
            // Downscale in linear light before the sRGB encode (the shared tail below then has
            // nothing left to do), so a 32 MP mosaic never materializes as a full-size RGB8 buffer.
            let small = lin.downscale_into(max_edge.max(1));
            let img = DynamicImage::ImageRgb8(crate::display::linear_to_srgb_rgb8(&small));
            (img, w, h, ow, oh)
        }
    };

    let (iw, ih) = img.dimensions();
    let scaled = if iw.max(ih) > max_edge {
        // `thumbnail` preserves aspect ratio, fitting within the box; fast triangle filter.
        img.thumbnail(max_edge, max_edge)
    } else {
        img
    };
    let rgb = scaled.to_rgb8();
    let mut buf = Vec::new();
    let mut enc = JpegEncoder::new_with_quality(&mut buf, quality);
    enc.encode(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        ExtendedColorType::Rgb8,
    )?;
    Ok(Thumb {
        jpeg: buf,
        src_width: w,
        src_height: h,
        disp_width: ow,
        disp_height: oh,
    })
}
