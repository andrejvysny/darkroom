//! HDR PQ HEIF (`.hif`) decode — Canon's 10-bit BT.2020/ST-2084 stills (EOS R7 et al.).
//!
//! All `libheif` calls are isolated in this module (mirroring the crate's rawler rule). The decode
//! chain: HEVC 10-bit 4:2:2 → libheif chroma-upsample + YCbCr→RGB (per the file's nclx matrix,
//! output still PQ-encoded) → PQ EOTF (ST 2084) → linear BT.2020 → Bradford-adapted matrix →
//! **linear ProPhoto (D50)** [`LinearImage`], the develop pipeline's working format.
//!
//! Luminance anchor: PQ is absolute; we normalize so BT.2408 diffuse white
//! ([`crate::color::HDR_DIFFUSE_WHITE_NITS`] = 203 cd/m²) lands at working-space 1.0 — SDR content
//! sits in [0,1] and speculars extend to ≈49× as scene-referred headroom, consumed by the same ACR
//! base tone operator as RAW (`display_referred` stays `false`).
//!
//! Orientation: libheif's decode applies the container's geometric transforms (irot/imir/clap)
//! itself, so decoded pixels arrive upright — the EXIF orientation tag must NOT be applied again
//! (verified with the `heif_gate` example; see `ignore_transformations` there).
//!
//! Not built on Windows (no system libheif): a sibling stub returns a clean decode error.

#[cfg(not(windows))]
mod imp {
    use crate::color::{bt2020_to_prophoto_d50, pq_eotf, HDR_DIFFUSE_WHITE_NITS, PQ_MAX_NITS};
    use crate::develop::LinearImage;
    use crate::error::RawError;
    use crate::meta::RawMeta;
    use crate::thumb::Thumb;
    use image::DynamicImage;
    use libheif_rs::{
        ColorPrimaries, ColorSpace, HeifContext, ImageHandle, LibHeif, RgbChroma,
        TransferCharacteristics,
    };

    fn de(e: impl std::fmt::Display) -> RawError {
        RawError::Decode(format!("HEIF: {e}"))
    }

    /// Accept only the profile this module implements: BT.2020 primaries + ST-2084 (PQ) transfer.
    /// Anything else (iPhone HEIC, SDR HEIF, HLG) gets a clean error instead of wrong colors.
    fn verify_pq_bt2020(handle: &ImageHandle) -> Result<(), RawError> {
        let nclx = handle.color_profile_nclx().ok_or_else(|| {
            de("no nclx color profile (expected BT.2020 PQ, e.g. Canon HDR .HIF)")
        })?;
        let primaries = nclx.color_primaries();
        let transfer = nclx.transfer_characteristics();
        if primaries != ColorPrimaries::ITU_R_BT_2020_2_and_2100_0
            || transfer != TransferCharacteristics::ITU_R_BT_2100_0_PQ
        {
            return Err(de(format!(
                "unsupported color profile (primaries {primaries:?}, transfer {transfer:?}) — \
                 only BT.2020 PQ (Canon HDR .HIF) is supported"
            )));
        }
        Ok(())
    }

    /// Decode the primary image → scene-linear ProPhoto [`LinearImage`] (headroom >1.0 preserved).
    pub fn decode_heif_linear(bytes: &[u8]) -> Result<LinearImage, RawError> {
        let ctx = HeifContext::read_from_bytes(bytes).map_err(de)?;
        let handle = ctx.primary_image_handle().map_err(de)?;
        verify_pq_bt2020(&handle)?;

        let lib = LibHeif::new();
        let img = lib
            .decode(&handle, ColorSpace::Rgb(RgbChroma::HdrRgbLe), None)
            .map_err(de)?;
        let planes = img.planes();
        let plane = planes
            .interleaved
            .ok_or_else(|| de("decoded image has no interleaved RGB plane"))?;
        let (w, h) = (plane.width, plane.height);
        let bits = plane.bits_per_pixel; // per channel (10 for Canon HIF)
        if !(9..=16).contains(&bits) {
            return Err(de(format!("unexpected bit depth {bits} (expected 10–16)")));
        }
        let n_codes = 1usize << bits;

        // PQ EOTF per code value via LUT (≤65k entries) — pq_eotf is pow-heavy per call.
        // Signal → linear BT.2020, scaled so diffuse white (203 nits) = 1.0.
        let scale = PQ_MAX_NITS / HDR_DIFFUSE_WHITE_NITS;
        let lut: Vec<f32> = (0..n_codes)
            .map(|c| (pq_eotf(c as f64 / (n_codes - 1) as f64) * scale) as f32)
            .collect();
        let m = bt2020_to_prophoto_d50();

        let row_len = w as usize * 6; // 3 channels × u16-LE
        let mut data = Vec::with_capacity(w as usize * h as usize * 3);
        for row in 0..h as usize {
            let start = row * plane.stride;
            let row_bytes = plane
                .data
                .get(start..start + row_len)
                .ok_or_else(|| de("decoded plane shorter than stride × height"))?;
            for px in row_bytes.chunks_exact(6) {
                let code = |i: usize| u16::from_le_bytes([px[i], px[i + 1]]) as usize;
                let r = lut[code(0).min(n_codes - 1)];
                let g = lut[code(2).min(n_codes - 1)];
                let b = lut[code(4).min(n_codes - 1)];
                // linear BT.2020 → linear ProPhoto; floor negatives only (keep >1.0 headroom),
                // mirroring rawler's `clip_negative` on the RAW path.
                data.push((m[0][0] * r + m[0][1] * g + m[0][2] * b).max(0.0));
                data.push((m[1][0] * r + m[1][1] * g + m[1][2] * b).max(0.0));
                data.push((m[2][0] * r + m[2][1] * g + m[2][2] * b).max(0.0));
            }
        }
        Ok(LinearImage {
            width: w,
            height: h,
            data,
        })
    }

    /// Raw TIFF/Exif payload from the container's `Exif` metadata block (the stored block starts
    /// with a 4-byte big-endian offset to the TIFF header, per the HEIF spec).
    pub fn heif_exif_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
        let ctx = HeifContext::read_from_bytes(bytes).ok()?;
        let handle = ctx.primary_image_handle().ok()?;
        for md in handle.all_metadata() {
            if md.item_type.0 != *b"Exif" {
                continue;
            }
            let raw = &md.raw_data;
            if raw.len() < 4 {
                continue;
            }
            let off = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
            let start = 4usize.saturating_add(off);
            if start < raw.len() {
                return Some(raw[start..].to_vec());
            }
        }
        None
    }

    /// Catalog metadata from the embedded Exif block (best-effort; empty meta when absent — the
    /// indexer's file-mtime fallback covers capture date).
    pub fn read_heif_meta(bytes: &[u8]) -> RawMeta {
        let Some(tiff) = heif_exif_bytes(bytes) else {
            return RawMeta::default();
        };
        match exif::Reader::new().read_raw(tiff) {
            Ok(exif) => {
                let mut meta = crate::display::meta_from_exif(&exif);
                // Decoded pixels are already upright (libheif applies irot/imir); the EXIF tag
                // must not be re-applied downstream.
                meta.orientation = None;
                meta
            }
            Err(_) => RawMeta::default(),
        }
    }

    /// Full-res clamped-SDR preview (serves AI scan / dedup / instant develop paint). PQ headroom
    /// is hard-clipped; the GPU canonical render supersedes this wherever tone quality matters.
    pub fn decode_heif_preview(bytes: &[u8]) -> Result<DynamicImage, RawError> {
        let lin = decode_heif_linear(bytes)?;
        Ok(DynamicImage::ImageRgb8(
            crate::display::linear_to_srgb_rgb8(&lin),
        ))
    }

    /// Thumbnail via full PQ decode → linear downscale → clamped SDR JPEG. Slower than an embedded
    /// preview (there is no full-size SDR preview in a .HIF) but colorimetrically right; the
    /// ThumbQueue's canonical GPU render replaces it asynchronously.
    pub fn decode_heif_thumb(bytes: &[u8], max_edge: u32, quality: u8) -> Result<Thumb, RawError> {
        let lin = decode_heif_linear(bytes)?;
        // Already upright: native == display dims (the capture fingerprint uses src_*).
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
}

#[cfg(windows)]
mod imp {
    use crate::develop::LinearImage;
    use crate::error::RawError;
    use crate::meta::RawMeta;
    use crate::thumb::Thumb;
    use image::DynamicImage;

    fn unsupported() -> RawError {
        RawError::Decode("HEIF (.HIF) is not supported in Windows builds yet".into())
    }

    pub fn decode_heif_linear(_bytes: &[u8]) -> Result<LinearImage, RawError> {
        Err(unsupported())
    }

    pub fn decode_heif_preview(_bytes: &[u8]) -> Result<DynamicImage, RawError> {
        Err(unsupported())
    }

    pub fn decode_heif_thumb(
        _bytes: &[u8],
        _max_edge: u32,
        _quality: u8,
    ) -> Result<Thumb, RawError> {
        Err(unsupported())
    }

    pub fn read_heif_meta(_bytes: &[u8]) -> RawMeta {
        RawMeta::default()
    }
}

pub use imp::*;
