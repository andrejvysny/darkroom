//! Merged-HDR OpenEXR (`.exr`) read/write — the storage format for Merge-to-HDR results.
//!
//! The file IS the develop pipeline's working format: **linear wide-gamut ProPhoto (D50),
//! scene-referred, >1.0 headroom**, stored as fp16 RGB with ZIP compression. Reading it back is a
//! straight channel copy (no color transform), so a merged image plugs into `develop_linear` →
//! GPU `prepare()` exactly like a RAW decode (`display_referred` stays `false`).
//!
//! Self-describing metadata rides inside the file as EXR attributes (no DB migration needed, and a
//! catalog rebuild re-reads it from the file):
//! - standard `chromaticities` = ProPhoto primaries + D50 white (so Nuke/Photoshop interpret the
//!   gamut correctly),
//! - `darkroom:meta` = the reference frame's [`RawMeta`] as JSON (feeds `read_metadata` →
//!   `process_file` unchanged),
//! - `darkroom:sources` = merge parentage ([`HdrSources`]: source image ids/hashes + relative EVs).

use crate::develop::LinearImage;
use crate::error::RawError;
use crate::meta::RawMeta;
use crate::thumb::Thumb;
use exr::prelude::{f16, AttributeValue, Text, Vec2, WritableImage};
use image::DynamicImage;
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::path::Path;

/// One source frame of a merged HDR (stored in the `darkroom:sources` attribute).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdrSourceInfo {
    /// Catalog id of the source image at merge time (informational — ids can be re-assigned by a
    /// catalog rebuild; the content hash is the durable link).
    pub image_id: i64,
    pub content_hash_hex: String,
    /// EV offset relative to the reference frame (frame EV₁₀₀ − reference EV₁₀₀).
    pub relative_ev: f32,
}

/// The `darkroom:sources` attribute payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdrSources {
    pub version: u32,
    /// Index into `sources` of the reference (metered) frame the merge was anchored to.
    pub reference_index: usize,
    pub sources: Vec<HdrSourceInfo>,
}

const META_ATTR: &str = "darkroom:meta";
const SOURCES_ATTR: &str = "darkroom:sources";
/// Largest finite f16 — values are clamped here before storage.
const F16_MAX: f32 = 65504.0;

fn de(e: impl std::fmt::Display) -> RawError {
    RawError::Decode(format!("HDR EXR: {e}"))
}

/// JSON → EXR Text. EXR text is byte-per-char; `serde_json::to_string` with `escape` handles the
/// rare non-ASCII (lens names etc.) by \u-escaping, so this never fails in practice — but fall
/// back to dropping the attribute rather than failing the write.
fn json_text<T: Serialize>(value: &T) -> Option<Text> {
    let json = serde_json::to_string(value).ok()?;
    let ascii: String = json
        .chars()
        .map(|c| if c.is_ascii() { c } else { '?' })
        .collect();
    Text::new_or_none(ascii)
}

/// Write `img` (linear ProPhoto, scene-referred) as an fp16 ZIP-compressed EXR with embedded
/// metadata. Durability mirrors core-import: the bytes go to `<path>.part` first, then an atomic
/// rename — `.part` is never enumerated by the indexer/watcher, so a crash leaves no torn catalog
/// entry.
pub fn write_hdr_exr(
    path: &Path,
    img: &LinearImage,
    meta: &RawMeta,
    sources: &[HdrSourceInfo],
    reference_index: usize,
) -> Result<(), RawError> {
    use exr::image::{Encoding, Image, Layer, SpecificChannels};
    use exr::meta::attribute::Chromaticities;
    use exr::meta::header::LayerAttributes;

    let (w, h) = (img.width as usize, img.height as usize);
    if img.data.len() != w * h * 3 || w == 0 || h == 0 {
        return Err(de("image buffer does not match its dimensions"));
    }

    // Sanitize per sample: NaN→0, clamp to the finite f16 range, floor negatives.
    let sample = |x: usize, y: usize, c: usize| -> f16 {
        let v = img.data[(y * w + x) * 3 + c];
        let v = if v.is_finite() { v } else { 0.0 };
        f16::from_f32(v.clamp(0.0, F16_MAX))
    };
    let channels = SpecificChannels::rgb(|pos: Vec2<usize>| -> (f16, f16, f16) {
        (
            sample(pos.x(), pos.y(), 0),
            sample(pos.x(), pos.y(), 1),
            sample(pos.x(), pos.y(), 2),
        )
    });

    let layer = Layer::new(
        (w, h),
        LayerAttributes::default(),
        Encoding::SMALL_LOSSLESS, // ZIP16 scan lines
        channels,
    );
    let mut image = Image::from_layer(layer);

    // ProPhoto (ROMM) primaries + D50 white — the working space, declared for foreign readers.
    image.attributes.chromaticities = Some(Chromaticities {
        red: Vec2(0.7347, 0.2653),
        green: Vec2(0.1596, 0.8404),
        blue: Vec2(0.0366, 0.0001),
        white: Vec2(0.3457, 0.3585),
    });
    if let (Some(key), Some(text)) = (Text::new_or_none(META_ATTR), json_text(meta)) {
        image
            .attributes
            .other
            .insert(key, AttributeValue::Text(text));
    }
    let sources_payload = HdrSources {
        version: 1,
        reference_index,
        sources: sources.to_vec(),
    };
    if let (Some(key), Some(text)) = (Text::new_or_none(SOURCES_ATTR), json_text(&sources_payload))
    {
        image
            .attributes
            .other
            .insert(key, AttributeValue::Text(text));
    }

    let part = path.with_extension("exr.part");
    {
        let file = std::fs::File::create(&part).map_err(|e| de(e.to_string()))?;
        let writer = std::io::BufWriter::new(file);
        image.write().to_buffered(writer).map_err(de)?;
    }
    std::fs::rename(&part, path).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        de(e.to_string())
    })?;
    Ok(())
}

/// Read EXR bytes → [`LinearImage`]. No color transform — the file is stored in the working format
/// (linear ProPhoto D50). Negatives are floored (foreign EXRs may contain them; ours never do).
pub fn read_hdr_linear(bytes: &[u8]) -> Result<LinearImage, RawError> {
    use exr::prelude::{ReadChannels, ReadLayers};

    struct Px {
        width: usize,
        data: Vec<f32>,
    }
    let image = exr::prelude::read()
        .no_deep_data()
        .largest_resolution_level()
        .rgb_channels(
            |size: Vec2<usize>, _ch: &_| Px {
                width: size.x(),
                data: vec![0f32; size.x() * size.y() * 3],
            },
            |px: &mut Px, pos: Vec2<usize>, (r, g, b): (f32, f32, f32)| {
                let i = (pos.y() * px.width + pos.x()) * 3;
                px.data[i] = r.max(0.0);
                px.data[i + 1] = g.max(0.0);
                px.data[i + 2] = b.max(0.0);
            },
        )
        .first_valid_layer()
        .all_attributes()
        .from_buffered(Cursor::new(bytes))
        .map_err(de)?;

    let size = image.layer_data.size;
    Ok(LinearImage {
        width: size.x() as u32,
        height: size.y() as u32,
        data: image.layer_data.channel_data.pixels.data,
    })
}

/// Find a text attribute by name across headers. exr files place image-level `other` attributes in
/// the layer's own attributes on write (observed with exr 1.74 single-layer files), so check both
/// the shared (image) and own (layer) maps.
fn find_text_attr(bytes: &[u8], name: &str) -> Option<String> {
    let meta_data = exr::meta::MetaData::read_from_buffered(Cursor::new(bytes), false).ok()?;
    let key = Text::new_or_none(name)?;
    for header in &meta_data.headers {
        for attrs in [
            &header.shared_attributes.other,
            &header.own_attributes.other,
        ] {
            if let Some(AttributeValue::Text(t)) = attrs.get(&key) {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// Catalog metadata from the `darkroom:meta` attribute — header-only read, no pixel decode (the
/// fast indexing path). Foreign/attribute-less EXRs yield an empty meta (the indexer's file-mtime
/// fallback covers capture date).
pub fn read_hdr_meta(bytes: &[u8]) -> RawMeta {
    find_text_attr(bytes, META_ATTR)
        .and_then(|json| serde_json::from_str::<RawMeta>(&json).ok())
        .unwrap_or_default()
}

/// Merge parentage from the `darkroom:sources` attribute (header-only read). `None` for foreign
/// EXRs or files predating the attribute.
pub fn read_hdr_sources(bytes: &[u8]) -> Option<HdrSources> {
    let json = find_text_attr(bytes, SOURCES_ATTR)?;
    serde_json::from_str::<HdrSources>(&json).ok()
}

/// Full-res clamped-SDR preview (AI scan / dedup / instant develop paint). Headroom hard-clips;
/// the GPU canonical render supersedes this wherever tone quality matters.
pub fn decode_hdr_preview(bytes: &[u8]) -> Result<DynamicImage, RawError> {
    let lin = read_hdr_linear(bytes)?;
    Ok(DynamicImage::ImageRgb8(
        crate::display::linear_to_srgb_rgb8(&lin),
    ))
}

/// Thumbnail via EXR decode → linear downscale → clamped SDR JPEG (EXR has no embedded preview).
pub fn decode_hdr_thumb(bytes: &[u8], max_edge: u32, quality: u8) -> Result<Thumb, RawError> {
    let lin = read_hdr_linear(bytes)?;
    let (sw, sh) = (lin.width, lin.height);
    let small = lin.downscale_into(max_edge);
    let jpeg = crate::display::linear_to_srgb_jpeg(&small, quality)?;
    Ok(Thumb {
        jpeg,
        src_width: sw,
        src_height: sh,
        disp_width: sw,
        disp_height: sh,
    })
}
