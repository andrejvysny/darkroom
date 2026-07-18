//! Spike-S1 HEIF gate: validate that libheif (+libde265) decodes a real Canon R7 HDR PQ `.HIF`
//! (10-bit HEVC 4:2:2) and that its metadata/color signaling matches what `core-raw::heif` assumes.
//! Run on BOTH Linux and macOS before building anything on top:
//!   cargo run -p core-raw --example heif_gate [DIR_OR_FILE]
//!
//! Per file it prints: container/handle dims + bit depth, nclx codes (expect primaries=BT.2020,
//! transfer=PQ), decode timing, code-value percentiles (sanity: 10-bit range used), embedded
//! thumbnail items, Exif parse (model/ISO/shutter/orientation), a whole-path `develop_linear`
//! check, and the PORTRAIT check: decoded dims with vs without container transformations — if a
//! portrait file shows identical dims both ways but carries EXIF orientation 6/8, Canon relies on
//! the EXIF tag and `heif.rs` must apply `.oriented()` (risk R4 in the plan).

#[cfg(windows)]
fn main() {
    eprintln!("heif_gate is not supported on Windows (no system libheif)");
}

#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    use libheif_rs::{ColorSpace, DecodingOptions, HeifContext, LibHeif, RgbChroma};
    use std::path::PathBuf;
    use std::time::Instant;

    let arg = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "library/fixtures-hdr".to_string());
    let path = PathBuf::from(&arg);
    let mut files: Vec<PathBuf> = if path.is_dir() {
        std::fs::read_dir(&path)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("hif") || s.eq_ignore_ascii_case("heic"))
                    .unwrap_or(false)
            })
            .collect()
    } else {
        vec![path]
    };
    files.sort();
    if files.is_empty() {
        eprintln!("no .hif files under {arg} — copy R7 fixtures there and re-run");
        return Ok(());
    }

    let lib = LibHeif::new();
    println!(
        "libheif {:?}; HEVC decoders: {:?}\n",
        lib.version(),
        lib.decoder_descriptors(8, Some(libheif_rs::CompressionFormat::Hevc))
            .iter()
            .map(|d| d.id().to_string())
            .collect::<Vec<_>>()
    );

    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy();
        println!("=== {name}");
        let bytes = std::fs::read(f)?;

        let ctx = HeifContext::read_from_bytes(&bytes)?;
        let handle = ctx.primary_image_handle()?;
        println!(
            "  handle: {}x{} luma_bits={} chroma_bits={} ispe={}x{}",
            handle.width(),
            handle.height(),
            handle.luma_bits_per_pixel(),
            handle.chroma_bits_per_pixel(),
            handle.ispe_width(),
            handle.ispe_height()
        );
        match handle.color_profile_nclx() {
            Some(nclx) => println!(
                "  nclx: primaries={:?} transfer={:?} matrix={:?} full_range={}",
                nclx.color_primaries(),
                nclx.transfer_characteristics(),
                nclx.matrix_coefficients(),
                nclx.full_range_flag()
            ),
            None => println!("  nclx: ABSENT (heif.rs would reject this file)"),
        }

        // Embedded thumbnails (a fast-path candidate for decode_heif_thumb).
        let mut thumb_ids = vec![0; handle.number_of_thumbnails()];
        handle.thumbnail_ids(&mut thumb_ids);
        for id in &thumb_ids {
            if let Ok(th) = handle.thumbnail(*id) {
                println!(
                    "  embedded thumb: {}x{} bits={}",
                    th.width(),
                    th.height(),
                    th.luma_bits_per_pixel()
                );
            }
        }

        // Decode with transformations (the shipping path).
        let t = Instant::now();
        let img = lib.decode(&handle, ColorSpace::Rgb(RgbChroma::HdrRgbLe), None)?;
        let dt = t.elapsed();
        let planes = img.planes();
        let plane = planes.interleaved.expect("interleaved RGB plane");
        println!(
            "  decoded {}x{} in {:.2?}; plane bits={} storage_bits={} stride={}",
            plane.width,
            plane.height,
            dt,
            plane.bits_per_pixel,
            plane.storage_bits_per_pixel,
            plane.stride
        );

        // Code-value percentiles over a subsample (sanity: real 10-bit PQ content).
        let n_codes = 1u32 << plane.bits_per_pixel;
        let mut samples: Vec<u16> = Vec::new();
        for row in (0..plane.height as usize).step_by(37) {
            let start = row * plane.stride;
            let row_bytes = &plane.data[start..start + plane.width as usize * 6];
            for px in row_bytes.chunks_exact(6).step_by(31) {
                samples.push(u16::from_le_bytes([px[0], px[1]]));
                samples.push(u16::from_le_bytes([px[2], px[3]]));
                samples.push(u16::from_le_bytes([px[4], px[5]]));
            }
        }
        samples.sort_unstable();
        let pct = |p: f64| samples[((samples.len() - 1) as f64 * p) as usize];
        println!(
            "  code values (max {}): min={} p1={} p50={} p99={} max={}",
            n_codes - 1,
            samples.first().unwrap(),
            pct(0.01),
            pct(0.50),
            pct(0.99),
            samples.last().unwrap()
        );

        // PORTRAIT check: decode ignoring container transforms; differing dims → irot present.
        let mut opts = DecodingOptions::new().expect("decoding options");
        opts.set_ignore_transformations(true);
        let raw_img = lib.decode(&handle, ColorSpace::Rgb(RgbChroma::HdrRgbLe), Some(opts))?;
        let rp = raw_img.planes();
        let rplane = rp.interleaved.expect("interleaved RGB plane");
        println!(
            "  transforms: with={}x{} without={}x{} (differ → container irot handles rotation)",
            plane.width, plane.height, rplane.width, rplane.height
        );

        // Exif block.
        match core_raw::heif::heif_exif_bytes(&bytes) {
            Some(tiff) => {
                println!("  exif blob: {} bytes", tiff.len());
                let meta = core_raw::read_metadata(&core_raw::source_from_bytes(
                    std::sync::Arc::new(bytes.clone()),
                    f,
                ))?;
                println!(
                    "  meta: model={:?} iso={:?} shutter={:?} aperture={:?} exif_orientation(raw tag)={:?}",
                    meta.camera_model, meta.iso, meta.shutter, meta.aperture, meta.orientation
                );
            }
            None => println!("  exif blob: ABSENT"),
        }

        // Whole shipping path: develop_linear → linear ProPhoto stats.
        let t = Instant::now();
        let src = core_raw::source_from_path(f)?;
        match core_raw::develop_linear(&src) {
            Ok(lin) => {
                let mut mx = 0f32;
                let mut sum = 0f64;
                for v in &lin.data {
                    mx = mx.max(*v);
                    sum += *v as f64;
                }
                println!(
                    "  develop_linear: {}x{} in {:.2?}; mean={:.4} max={:.2} (>1.0 = headroom)",
                    lin.width,
                    lin.height,
                    t.elapsed(),
                    sum / lin.data.len() as f64,
                    mx
                );
            }
            Err(e) => println!("  develop_linear FAILED: {e}"),
        }
        println!();
    }
    Ok(())
}
