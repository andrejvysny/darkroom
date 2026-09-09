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
use rawler::RawImage;

/// Sensor-native full-image dims: the recommended (else active) crop rawler resolves from the
/// geometry tags, falling back to the whole mosaic. This is what a develop of the file produces, and
/// therefore what the catalog must record — the embedded preview is only *sometimes* the same size
/// (Canon CR3 ships a full-res one; a Sony ARW's is 1616×1080 next to a 6000×4000 mosaic).
fn sensor_dims(raw: &RawImage) -> (u32, u32) {
    // Each rect is emptiness-tested on its own: a degenerate `crop_area` must fall through to the
    // active area, not swallow it.
    raw.crop_area
        .filter(|r| !r.is_empty())
        .or(raw.active_area.filter(|r| !r.is_empty()))
        .map(|r| (r.d.w as u32, r.d.h as u32))
        .unwrap_or((raw.width as u32, raw.height as u32))
}

/// Apply an EXIF orientation to a dimension pair: 5-8 are the quarter-turns, which swap the axes.
fn oriented_dims((w, h): (u32, u32), orientation: Option<u16>) -> (u32, u32) {
    if matches!(orientation, Some(5..=8)) {
        (h, w)
    } else {
        (w, h)
    }
}

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

/// The embedded preview of a RAW, or `None` when the file has none we can read.
///
/// Canon's **HDR-PQ CR3** (`CompressorVersion` `CanonCR3_003`, written whenever the body is in
/// HDR PQ mode) stores its preview as HEVC rather than JPEG, and rawler returns a hard error for
/// it ("Unable to extract preview image from CR3 HDR-PQ file", dnglab#7) instead of an empty
/// preview. That error is not a corrupt file — the mosaic beside it decodes perfectly — so it must
/// not sink the whole image. Errors are folded into `None` here and callers fall back to
/// developing the RAW themselves ([`developed_preview`]); a file that is genuinely undecodable
/// fails later, on the mosaic, with a truthful error.
///
/// Only `preview_image` is consulted. rawler 0.8 swapped the two names: the real per-format
/// extractors (CR3, CR2, NEF, ARW, DNG, RAF, PEF, RW2) now live on `preview_image`, and
/// `full_image` is the unimplemented trait default that logs "Decoder has no full image support"
/// for every file — calling it was pure noise.
fn embedded_preview(
    decoder: &dyn rawler::decoders::Decoder,
    src: &RawSource,
    params: &RawDecodeParams,
) -> Option<DynamicImage> {
    decoder.preview_image(src, params).ok().flatten()
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

/// Decode the largest embedded preview to pixels, developing the mosaic when there is none.
pub fn preview_image(src: &RawSource) -> Result<DynamicImage, RawError> {
    crate::panic::catch_decode_panic("preview_image", || preview_image_inner(src))
}

fn preview_image_inner(src: &RawSource) -> Result<DynamicImage, RawError> {
    use crate::display::ImageKind;
    match crate::display::classify(src.path()) {
        ImageKind::Jpeg | ImageKind::Png => {
            return crate::display::decode_display_preview(&src.as_vec()?)
        }
        ImageKind::Heif => return crate::heif::decode_heif_preview(&src.as_vec()?),
        ImageKind::Hdr => return crate::hdr_file::decode_hdr_preview(&src.as_vec()?),
        ImageKind::Raw => {}
    }
    let decoder = rawler::get_decoder(src)?;
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
/// extraction + developed-fallback chain so the native pixels are byte-identical to it.
pub fn preview_with_orientation(
    src: &RawSource,
) -> Result<(DynamicImage, Option<Orientation>), RawError> {
    crate::panic::catch_decode_panic("preview_with_orientation", || {
        preview_with_orientation_inner(src)
    })
}

fn preview_with_orientation_inner(
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
    let decoder = rawler::get_decoder(src)?;
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
/// One decoder handles the preview decode, the orientation read and the geometry read. The returned
/// `src_*` dims are the *sensor-native* (pre-orientation) full-image dims — see [`sensor_dims`] —
/// which is the pair the capture fingerprint is built from.
pub fn thumbnail_jpeg(src: &RawSource, max_edge: u32, quality: u8) -> Result<Thumb, RawError> {
    crate::panic::catch_decode_panic("thumbnail_jpeg", || {
        thumbnail_jpeg_inner(src, max_edge, quality)
    })
}

fn thumbnail_jpeg_inner(src: &RawSource, max_edge: u32, quality: u8) -> Result<Thumb, RawError> {
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
    let decoder = rawler::get_decoder(src)?;
    let params = RawDecodeParams::default();
    let exif_orientation = decoder
        .raw_metadata(src, &params)
        .ok()
        .and_then(|md| md.exif.orientation);

    // Sensor geometry WITHOUT decoding a pixel: `dummy = true` makes rawler parse the tags and
    // allocate an uninitialized mosaic, so this costs a tag walk rather than a demosaic. Its own
    // panic guard because a file can have a perfectly readable embedded preview and a mosaic whose
    // geometry trips one of rawler's asserts — losing the exact dims must not lose the thumbnail.
    let native_dims = crate::panic::catch_decode_panic("thumbnail_dims", || {
        Ok(sensor_dims(&decoder.raw_image(src, &params, true)?))
    })
    .or_else(|_| {
        // Canon sRAW/mRAW (cpp=3): rawler's dummy path trips a `pixarray` `initialized` assert in
        // the YUV→RGB unpack, so pay for one real decode of this rare legacy format rather than
        // catalog the embedded preview's (larger, different) dimensions.
        crate::panic::catch_decode_panic("thumbnail_dims_full", || {
            Ok(sensor_dims(&decoder.raw_image(src, &params, false)?))
        })
    })
    .ok();

    // `(w, h)` = NATIVE (pre-orientation) dims, which feed the capture fingerprint; `(ow, oh)` =
    // ORIENTED (display) dims for the catalog. Both describe the FULL image, never the thumbnail.
    let (img, w, h, ow, oh) = match embedded_preview(decoder.as_ref(), src, &params) {
        Some(img) => {
            // Only if the geometry read failed do the PREVIEW's own dims stand in for the sensor's.
            let (w, h) = native_dims.unwrap_or_else(|| img.dimensions());
            // Upright the preview from its EXIF orientation (1–8). Absent/unknown → already upright.
            let mut img = img;
            if let Some(o) = exif_orientation.and_then(|v| Orientation::from_exif(v as u8)) {
                img.apply_orientation(o);
            }
            let (ow, oh) = oriented_dims((w, h), exif_orientation);
            (img, w, h, ow, oh)
        }
        None => {
            // HDR-PQ CR3: no readable embedded preview, so develop the mosaic. `develop_linear`
            // (not the half-res preview path) because the dims reported here must be the TRUE
            // sensor ones. Its output is already uprighted, so those dims ARE the display dims and
            // the native pair is recovered by undoing a quarter-turn orientation.
            let lin = crate::develop::develop_linear(src)?;
            let (w, h) = native_dims
                .unwrap_or_else(|| oriented_dims((lin.width, lin.height), exif_orientation));
            let (ow, oh) = oriented_dims((w, h), exif_orientation);
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
