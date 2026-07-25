//! HDR PQ HEIF (`.hif`) decode — Canon's 10-bit BT.2020/ST-2084 stills (EOS R7 et al.).
//!
//! All `libheif` calls are isolated in this module (mirroring the crate's rawler rule). The decode
//! chain: HEVC 10-bit 4:2:2 → libheif chroma-upsample + YCbCr→RGB (per the file's nclx matrix,
//! output still PQ-encoded) → PQ EOTF (ST 2084) → linear BT.2020 → Bradford-adapted matrix →
//! **linear ProPhoto (D50)** [`LinearImage`], the develop pipeline's working format.
//!
//! Luminance anchor: PQ is absolute; we normalize so [`crate::color::HDR_DIFFUSE_WHITE_NITS`]
//! (300 cd/m², calibrated against a real R7 CR3+HIF pair — see `color.rs`) lands at working-space
//! 1.0 — SDR content sits in [0,1] and speculars extend to ≈33× as scene-referred headroom,
//! consumed by the same ACR base tone operator as RAW (`display_referred` stays `false`).
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

    /// ISO/IEC 14496-12 codes for the profile this module implements.
    const NCLX_PRIMARIES_BT2020: u16 = 9;
    const NCLX_TRANSFER_PQ: u16 = 16;

    /// Every `nclx` colour description in the container, as raw `(primaries, transfer)` code points.
    ///
    /// Needed because libheif only reports an nclx profile that hangs off the *item* it was asked
    /// about: a real Canon `.HIF` primary item is a **4×5 grid** (a derived item), and
    /// `heif_image_handle_get_nclx_color_profile` on that grid handle answers `Unspecified/
    /// Unspecified` even though `heif-info` lists an nclx and every tile carries BT.2020 PQ. The
    /// synthetic fixtures are single-item, so they never exercised this. Rather than trust the
    /// bindings' view, read the `colr` boxes out of the container ourselves.
    ///
    /// Boxes are walked structurally (size/type headers), descending only into the containers that
    /// can hold `colr` — `meta` and `moov` are FullBoxes, so their 4 version/flags bytes are skipped.
    fn container_nclx(bytes: &[u8]) -> Vec<(u16, u16)> {
        fn walk(b: &[u8], out: &mut Vec<(u16, u16)>, depth: u32) {
            // ipco nests a few levels below meta; the cap just stops a malformed file recursing.
            if depth > 8 {
                return;
            }
            let mut pos = 0usize;
            while pos + 8 <= b.len() {
                let size32 = u32::from_be_bytes([b[pos], b[pos + 1], b[pos + 2], b[pos + 3]]);
                let typ = &b[pos + 4..pos + 8];
                let (header, size) = match size32 {
                    // size 1 → 64-bit largesize follows the type.
                    1 => {
                        if pos + 16 > b.len() {
                            return;
                        }
                        let large =
                            u64::from_be_bytes(b[pos + 8..pos + 16].try_into().expect("8 bytes"));
                        (16usize, large as usize)
                    }
                    // size 0 → the box runs to the end of the enclosing box.
                    0 => (8usize, b.len() - pos),
                    n => (8usize, n as usize),
                };
                if size < header || pos + size > b.len() {
                    return;
                }
                let body = &b[pos + header..pos + size];
                match typ {
                    b"colr" => {
                        // colour_type (4 bytes) then, for 'nclx', three u16 code points.
                        if body.len() >= 10 && &body[0..4] == b"nclx" {
                            let primaries = u16::from_be_bytes([body[4], body[5]]);
                            let transfer = u16::from_be_bytes([body[6], body[7]]);
                            out.push((primaries, transfer));
                        }
                    }
                    b"meta" | b"moov" => {
                        // FullBox: skip version+flags before the children.
                        if body.len() > 4 {
                            walk(&body[4..], out, depth + 1);
                        }
                    }
                    b"iprp" | b"ipco" | b"trak" | b"mdia" | b"minf" | b"stbl" => {
                        walk(body, out, depth + 1)
                    }
                    _ => {}
                }
                pos += size;
            }
        }
        let mut out = Vec::new();
        walk(bytes, &mut out, 0);
        out
    }

    /// Accept only the profile this module implements: BT.2020 primaries + ST-2084 (PQ) transfer.
    /// Anything else (iPhone HEIC, SDR HEIF, HLG) gets a clean error instead of wrong colors.
    ///
    /// The container's own `colr` boxes are authoritative (see [`container_nclx`]); the handle's
    /// nclx is only consulted when the container carries none. **Every** nclx found must agree on
    /// BT.2020 PQ — if a file mixed profiles across its items we could not say which one describes
    /// the buffer we actually decoded, so refusing is the honest answer.
    fn verify_pq_bt2020(bytes: &[u8], handle: &ImageHandle) -> Result<(), RawError> {
        let found = container_nclx(bytes);
        if !found.is_empty() {
            if let Some(&(primaries, transfer)) = found
                .iter()
                .find(|&&(p, t)| p != NCLX_PRIMARIES_BT2020 || t != NCLX_TRANSFER_PQ)
            {
                return Err(de(format!(
                    "unsupported color profile (nclx primaries {primaries}, transfer {transfer}) — \
                     only BT.2020 PQ (Canon HDR .HIF) is supported"
                )));
            }
            return Ok(());
        }

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

    /// Decode ONE image handle (primary or embedded thumbnail) → scene-linear ProPhoto. The PQ
    /// signal chain runs per-row in parallel (rayon): at 32.5 MP the serial conversion added ~6 s
    /// on top of the ~5 s HEVC decode.
    fn decode_handle_linear(lib: &LibHeif, handle: &ImageHandle) -> Result<LinearImage, RawError> {
        use rayon::prelude::*;

        let img = lib
            .decode(handle, ColorSpace::Rgb(RgbChroma::HdrRgbLe), None)
            .map_err(de)?;
        let planes = img.planes();
        let plane = planes
            .interleaved
            .ok_or_else(|| de("decoded image has no interleaved RGB plane"))?;
        let (w, h) = (plane.width as usize, plane.height as usize);
        let bits = plane.bits_per_pixel; // per channel (10 for Canon HIF)
        if !(9..=16).contains(&bits) {
            return Err(de(format!("unexpected bit depth {bits} (expected 10–16)")));
        }
        let n_codes = 1usize << bits;
        let row_len = w * 6; // 3 channels × u16-LE
        if h > 0 && plane.data.len() < (h - 1) * plane.stride + row_len {
            return Err(de("decoded plane shorter than stride × height"));
        }

        // PQ EOTF per code value via LUT (≤65k entries) — pq_eotf is pow-heavy per call.
        // Signal → linear BT.2020, scaled so diffuse white = 1.0.
        let scale = PQ_MAX_NITS / HDR_DIFFUSE_WHITE_NITS;
        let lut: Vec<f32> = (0..n_codes)
            .map(|c| (pq_eotf(c as f64 / (n_codes - 1) as f64) * scale) as f32)
            .collect();
        let m = bt2020_to_prophoto_d50();

        let mut data = vec![0f32; w * h * 3];
        data.par_chunks_exact_mut(w * 3)
            .enumerate()
            .for_each(|(row, out)| {
                let start = row * plane.stride;
                let row_bytes = &plane.data[start..start + row_len];
                for (o, px) in out.chunks_exact_mut(3).zip(row_bytes.chunks_exact(6)) {
                    let code = |i: usize| u16::from_le_bytes([px[i], px[i + 1]]) as usize;
                    let r = lut[code(0).min(n_codes - 1)];
                    let g = lut[code(2).min(n_codes - 1)];
                    let b = lut[code(4).min(n_codes - 1)];
                    // linear BT.2020 → linear ProPhoto; floor negatives only (keep >1.0 headroom),
                    // mirroring rawler's `clip_negative` on the RAW path.
                    o[0] = (m[0][0] * r + m[0][1] * g + m[0][2] * b).max(0.0);
                    o[1] = (m[1][0] * r + m[1][1] * g + m[1][2] * b).max(0.0);
                    o[2] = (m[2][0] * r + m[2][1] * g + m[2][2] * b).max(0.0);
                }
            });
        Ok(LinearImage {
            width: w as u32,
            height: h as u32,
            data,
        })
    }

    /// The largest embedded thumbnail handle, if any. Canon .HIF files carry 10-bit PQ thumbnails
    /// (R7: 320×214 and 1620×1080) — decoding one is ~20× faster than the 32.5 MP primary.
    fn largest_thumb_handle(handle: &ImageHandle) -> Option<ImageHandle> {
        let mut ids = vec![0; handle.number_of_thumbnails()];
        handle.thumbnail_ids(&mut ids);
        ids.iter()
            .filter_map(|id| handle.thumbnail(*id).ok())
            .max_by_key(|t| t.width())
    }

    /// Decode the primary image → scene-linear ProPhoto [`LinearImage`] (headroom >1.0 preserved).
    pub fn decode_heif_linear(bytes: &[u8]) -> Result<LinearImage, RawError> {
        let ctx = HeifContext::read_from_bytes(bytes).map_err(de)?;
        let handle = ctx.primary_image_handle().map_err(de)?;
        verify_pq_bt2020(bytes, &handle)?;
        let lib = LibHeif::new();
        decode_handle_linear(&lib, &handle)
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
    /// Decode for preview/thumbnail duty at ≥ `min_edge` px: the largest embedded thumbnail when
    /// it is big enough (Canon .HIF carries a 10-bit PQ 1620×1080 — ~20× faster than the 32.5 MP
    /// primary), else the primary. Returns the linear image plus the PRIMARY's dims (the catalog /
    /// capture-fingerprint identity, regardless of which handle supplied the pixels).
    fn decode_preview_linear(
        bytes: &[u8],
        min_edge: u32,
    ) -> Result<(LinearImage, u32, u32), RawError> {
        let ctx = HeifContext::read_from_bytes(bytes).map_err(de)?;
        let handle = ctx.primary_image_handle().map_err(de)?;
        verify_pq_bt2020(bytes, &handle)?;
        let lib = LibHeif::new();
        let (pw, ph) = (handle.width(), handle.height());

        if let Some(th) = largest_thumb_handle(&handle) {
            // Use the thumb only when it can serve the requested edge (no upscaling) and matches
            // the primary's aspect (a letterboxed/cropped thumb would shift derived boxes).
            let big_enough = th.width().max(th.height()) >= min_edge.min(pw.max(ph));
            let aspect_ok = (th.width() as f64 * ph as f64 - th.height() as f64 * pw as f64).abs()
                / (pw.max(ph) as f64)
                < 4.0;
            if big_enough && aspect_ok {
                if let Ok(lin) = decode_handle_linear(&lib, &th) {
                    return Ok((lin, pw, ph));
                }
            }
        }
        Ok((decode_handle_linear(&lib, &handle)?, pw, ph))
    }

    /// Clamped-SDR preview (serves AI scan / dedup / instant develop paint). Uses the embedded
    /// thumbnail when it can serve ~1620 px, else the full primary. PQ headroom is hard-clipped;
    /// the GPU canonical render supersedes this wherever tone quality matters.
    pub fn decode_heif_preview(bytes: &[u8]) -> Result<DynamicImage, RawError> {
        let (lin, _, _) = decode_preview_linear(bytes, 1080)?;
        Ok(DynamicImage::ImageRgb8(
            crate::display::linear_to_srgb_rgb8(&lin),
        ))
    }

    /// Thumbnail via PQ decode → linear downscale → clamped SDR JPEG, preferring the embedded
    /// thumbnail when it covers `max_edge`. Colorimetrically identical to the primary path (same
    /// PQ→ProPhoto chain); the ThumbQueue's canonical GPU render replaces it asynchronously.
    pub fn decode_heif_thumb(bytes: &[u8], max_edge: u32, quality: u8) -> Result<Thumb, RawError> {
        let (lin, pw, ph) = decode_preview_linear(bytes, max_edge)?;
        let small = lin.downscale_into(max_edge);
        let jpeg = crate::display::linear_to_srgb_jpeg(&small, quality)?;
        // Already upright: native == display dims — always the PRIMARY's (fingerprint stability).
        Ok(Thumb {
            jpeg,
            src_width: pw,
            src_height: ph,
            disp_width: pw,
            disp_height: ph,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// `size||type||payload`, the ISOBMFF box header this parser walks.
        fn boxed(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
            let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
            v.extend_from_slice(typ);
            v.extend_from_slice(payload);
            v
        }

        fn colr_nclx(primaries: u16, transfer: u16, matrix: u16) -> Vec<u8> {
            let mut p = b"nclx".to_vec();
            p.extend_from_slice(&primaries.to_be_bytes());
            p.extend_from_slice(&transfer.to_be_bytes());
            p.extend_from_slice(&matrix.to_be_bytes());
            p.push(0x80); // full_range_flag
            boxed(b"colr", &p)
        }

        /// The shape a real Canon `.HIF` has: the colour description lives in `meta/iprp/ipco`, not
        /// on the (grid) item handle libheif answers for. Nesting + the `meta` FullBox skip are the
        /// two things that must hold for the grid case to be read at all.
        fn canon_like(colr: &[u8]) -> Vec<u8> {
            let ipco = boxed(b"ipco", colr);
            let iprp = boxed(b"iprp", &ipco);
            let mut meta_payload = vec![0, 0, 0, 0]; // FullBox version+flags
            meta_payload.extend_from_slice(&iprp);
            boxed(b"meta", &meta_payload)
        }

        #[test]
        fn finds_nested_nclx_like_a_canon_grid_hif() {
            let f = canon_like(&colr_nclx(
                NCLX_PRIMARIES_BT2020,
                NCLX_TRANSFER_PQ,
                9, // BT.2020 non-constant luminance
            ));
            assert_eq!(
                container_nclx(&f),
                vec![(NCLX_PRIMARIES_BT2020, NCLX_TRANSFER_PQ)]
            );
        }

        #[test]
        fn ignores_icc_colr_so_a_gain_map_heic_still_falls_through() {
            // Apple's HEIC carries `colr` of type `prof` (ICC) — no nclx codes to trust.
            let f = canon_like(&boxed(b"colr", b"prof\x00\x01\x02\x03"));
            assert!(container_nclx(&f).is_empty());
        }

        #[test]
        fn non_pq_transfer_is_reported_not_silently_accepted() {
            // sRGB primaries/transfer (1, 13) — the shape an SDR HEIF would have.
            let f = canon_like(&colr_nclx(1, 13, 1));
            assert_eq!(container_nclx(&f), vec![(1, 13)]);
        }

        /// A truncated/garbage box must terminate the walk rather than loop or panic.
        #[test]
        fn malformed_boxes_terminate_the_walk() {
            assert!(container_nclx(&[0, 0, 0, 0]).is_empty());
            assert!(container_nclx(&[0xff, 0xff, 0xff, 0xff, b'c', b'o', b'l', b'r']).is_empty());
            let mut truncated = canon_like(&colr_nclx(9, 16, 9));
            truncated.truncate(truncated.len() - 4);
            let _ = container_nclx(&truncated); // must not panic
        }
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
