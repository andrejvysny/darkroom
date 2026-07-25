//! Merge a real exposure bracket end-to-end (the no-GUI validation harness for Merge-to-HDR). Run
//! with a directory of 2–9 bracketed CR3s of a static scene:
//!   cargo run -p core-hdr --example merge_one [BRACKET_DIR] [--no-align]
//!
//! By default each non-reference frame is *aligned* to the reference (hand-held path:
//! `core_pano::align` + `warp_into_reference` + masked/deghosting accumulate); `--no-align` uses the
//! legacy tripod accumulator (`new` + `add_frame`). A true tripod fixture should align to an
//! identity-like transform, so both paths land in the same place there.
//!
//! Prints per-frame EV/scale/timings, writes `/tmp/darkroom-hdr.exr` (the shipping fp16
//! linear-ProPhoto format) and `/tmp/darkroom-hdr-preview.jpg` (clamped-SDR eyeball). For a real
//! tone-mapped look, render the EXR through the GPU pipeline:
//!   cargo run -p core-pipeline --example render_one /tmp/darkroom-hdr.exr

use std::path::PathBuf;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let no_align = args.iter().any(|a| a == "--no-align");
    let dir = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| "library/fixtures-hdr".to_string());
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("cr3"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    if files.len() < 2 {
        eprintln!(
            "need ≥2 CR3s in {dir} (a tripod AEB bracket) — found {}",
            files.len()
        );
        return Ok(());
    }
    if files.len() > 9 {
        files.truncate(9);
    }

    // Numeric EXIF → EV₁₀₀ per frame; reference = median EV.
    let mut exposures = Vec::new();
    for f in &files {
        let src = core_raw::source_from_path(f)?;
        let Some((t, n, iso)) = core_raw::read_exposure_numeric(&src)? else {
            anyhow::bail!("{}: missing exposure EXIF", f.display());
        };
        exposures.push(core_hdr::ExposureInfo {
            exposure_time_s: t,
            f_number: n,
            iso,
        });
    }
    let ref_idx = core_hdr::reference_index(&exposures)?;
    let ref_exp = exposures[ref_idx];
    let shortest_idx = exposures
        .iter()
        .enumerate()
        .max_by(|a, b| {
            core_hdr::ev100(a.1)
                .unwrap()
                .total_cmp(&core_hdr::ev100(b.1).unwrap())
        })
        .map(|(i, _)| i)
        .unwrap();
    println!(
        "{} frames; reference = [{}] {:?} (EV100 {:.2}); shortest = [{}]",
        files.len(),
        ref_idx,
        files[ref_idx].file_name().unwrap(),
        core_hdr::ev100(&ref_exp)?,
        shortest_idx,
    );

    // Decode the reference first (capturing its as-shot WB), then stream every frame through the
    // accumulator with the reference WB + relative scale.
    let t0 = Instant::now();
    let src = core_raw::source_from_path(&files[ref_idx])?;
    let (ref_img, ref_wb) = core_raw::develop_linear_wb(&src, None)?;
    println!(
        "decoded reference {}x{} in {:.2?} (as-shot WB {:?})",
        ref_img.width,
        ref_img.height,
        t0.elapsed(),
        ref_wb
    );
    let (ref_w, ref_h) = (ref_img.width, ref_img.height);
    let mut acc = if no_align {
        let mut a = core_hdr::MergeAccumulator::new(ref_w, ref_h);
        a.add_frame(&ref_img, 1.0, ref_idx == shortest_idx)?;
        a
    } else {
        core_hdr::MergeAccumulator::with_reference(
            &ref_img,
            ref_idx == shortest_idx,
            core_hdr::DeghostParams::default(),
        )?
    };

    for (i, f) in files.iter().enumerate() {
        if i == ref_idx {
            continue;
        }
        let t = Instant::now();
        let src = core_raw::source_from_path(f)?;
        let (img, _) = core_raw::develop_linear_wb(&src, Some(ref_wb))?;
        let scale = core_hdr::relative_scale(&exposures[i], &ref_exp)? as f32;
        let ev = core_hdr::ev100(&exposures[i])?;
        let name = f.file_name().unwrap().to_owned();

        if no_align {
            acc.add_frame(&img, scale, i == shortest_idx)?;
            println!(
                "  [{i}] {name:?}: EV100 {ev:.2}, scale ×{scale:.3}, decoded+merged in {:.2?}",
                t.elapsed()
            );
        } else {
            match core_pano::align::estimate_alignment_rgb(
                &ref_img.data,
                ref_w as usize,
                ref_h as usize,
                &img.data,
                img.width as usize,
                img.height as usize,
                core_pano::align::AlignModel::Affine,
            ) {
                Some(m) => {
                    let (warped, mask) = core_hdr::warp_into_reference(&img, ref_w, ref_h, &m);
                    let valid = mask.iter().filter(|&&v| v != 0).count();
                    let pct = 100.0 * valid as f64 / mask.len() as f64;
                    let warped_img = core_raw::LinearImage {
                        width: ref_w,
                        height: ref_h,
                        data: warped,
                    };
                    acc.add_frame_masked(&warped_img, scale, i == shortest_idx, &mask)?;
                    println!(
                        "  [{i}] {name:?}: EV100 {ev:.2}, scale ×{scale:.3}, aligned ({pct:.1}% valid), merged in {:.2?}",
                        t.elapsed()
                    );
                }
                None => {
                    acc.add_frame(&img, scale, i == shortest_idx)?;
                    println!(
                        "  [{i}] {name:?}: EV100 {ev:.2}, scale ×{scale:.3}, UNALIGNED (fell back), merged in {:.2?}",
                        t.elapsed()
                    );
                }
            }
        }
    }
    drop(ref_img);

    let t = Instant::now();
    let merged = acc.finish()?;
    let mx = merged.data.iter().cloned().fold(0f32, f32::max);
    println!(
        "merged {}x{} in {:.2?}; max value {:.2} (>1.0 = recovered headroom)",
        merged.width,
        merged.height,
        t.elapsed(),
        mx
    );

    // Write the shipping EXR with the reference frame's metadata + parentage.
    let meta = core_raw::read_metadata(&core_raw::source_from_path(&files[ref_idx])?)?;
    let sources: Vec<core_raw::HdrSourceInfo> = files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let (hash, _) = core_raw::hash_file(f).expect("hash");
            core_raw::HdrSourceInfo {
                image_id: -1,
                content_hash_hex: core_raw::hex(&hash),
                relative_ev: (core_hdr::ev100(&exposures[i]).unwrap()
                    - core_hdr::ev100(&ref_exp).unwrap()) as f32,
            }
        })
        .collect();
    let out = std::env::temp_dir().join("darkroom-hdr.exr");
    core_raw::write_hdr_exr(&out, &merged, &meta, &sources, ref_idx)?;
    println!("wrote {}", out.display());

    // Clamped-SDR eyeball via the shipping thumbnail path over the EXR we just wrote.
    let src = core_raw::source_from_path(&out)?;
    let thumb = core_raw::thumbnail_jpeg(&src, 2560, 90)?;
    let jpg = std::env::temp_dir().join("darkroom-hdr-preview.jpg");
    std::fs::write(&jpg, &thumb.jpeg)?;
    println!(
        "wrote {} (clamped SDR; use core-pipeline render_one for the tone-mapped look)",
        jpg.display()
    );
    Ok(())
}
