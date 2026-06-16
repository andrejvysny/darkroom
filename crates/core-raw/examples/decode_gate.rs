//! Phase-0 decode gate: validate that rawler can decode Canon EOS R7 CR3 sensor data
//! AND extract embedded thumbnail/preview JPEGs. Run:
//!   cargo run -p core-raw --example decode_gate [DIR]

use image::GenericImageView;
use rawler::analyze::{extract_preview_pixels, extract_thumbnail_pixels};
use rawler::decoders::RawDecodeParams;
use rawler::{decode_file, RawImageData};
use std::path::PathBuf;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| {
        "/Users/andrejvysny/workspace/darkroom/library/2026/2026-06-06".to_string()
    });

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
    files.truncate(8);
    println!("Probing {} CR3 files from {dir}\n", files.len());

    let params = RawDecodeParams::default();
    let (mut dec_ok, mut thumb_ok, mut prev_ok) = (0usize, 0usize, 0usize);

    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy();
        println!("{name}");

        let t = Instant::now();
        match decode_file(f) {
            Ok(raw) => {
                dec_ok += 1;
                let (dlen, dkind) = match &raw.data {
                    RawImageData::Integer(v) => (v.len(), "u16"),
                    RawImageData::Float(v) => (v.len(), "f32"),
                };
                let expect = raw.width * raw.height * raw.cpp;
                println!(
                    "  decode  OK  {:>7.1?}  {} {}  {}x{} cpp={} bps={}",
                    t.elapsed(),
                    raw.clean_make,
                    raw.clean_model,
                    raw.width,
                    raw.height,
                    raw.cpp,
                    raw.bps
                );
                println!(
                    "          wb={:?}  illuminants={}  orientation={:?}",
                    raw.wb_coeffs,
                    raw.color_matrix.len(),
                    raw.orientation
                );
                println!(
                    "          black={:?} white={:?}",
                    raw.blacklevel, raw.whitelevel
                );
                println!(
                    "          data={dlen} {dkind} (expect {expect}) match={}",
                    dlen == expect
                );
            }
            Err(e) => println!("  decode  FAIL  {e}"),
        }

        let t = Instant::now();
        match extract_thumbnail_pixels(f, &params) {
            Ok(img) => {
                thumb_ok += 1;
                let (w, h) = img.dimensions();
                println!("  thumb   OK  {:>7.1?}  {w}x{h}", t.elapsed());
            }
            Err(e) => println!("  thumb   FAIL  {e}"),
        }

        let t = Instant::now();
        match extract_preview_pixels(f, &params) {
            Ok(img) => {
                prev_ok += 1;
                let (w, h) = img.dimensions();
                println!("  preview OK  {:>7.1?}  {w}x{h}", t.elapsed());
            }
            Err(e) => println!("  preview FAIL  {e}"),
        }
        println!();
    }

    let n = files.len();
    println!("=== SUMMARY: decode {dec_ok}/{n}  thumb {thumb_ok}/{n}  preview {prev_ok}/{n} ===");
    Ok(())
}
