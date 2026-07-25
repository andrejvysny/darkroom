//! Export a merged-HDR `.exr` to a float DNG (Lightroom interop) without the GUI.
//!
//! Usage: `cargo run --release -p core-raw --example export_hdr_dng -- <in.exr> [out.dng]`
//!
//! Verify the result with exiftool — the raw SubIFD (not IFD0, which is the 8-bit preview) must
//! read `Float; Float; Float` / `32 32 32` / `Linear Raw`:
//! `exiftool -a -G1 -SampleFormat -BitsPerSample -PhotometricInterpretation <out.dng>`

use std::path::{Path, PathBuf};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(exr) = args.next() else {
        eprintln!("usage: export_hdr_dng <in.exr> [out.dng]");
        std::process::exit(2);
    };
    let dest = PathBuf::from(
        args.next()
            .unwrap_or_else(|| "/tmp/darkroom-hdr-export.dng".to_string()),
    );

    let bytes = std::fs::read(&exr).expect("read exr");
    let img = core_raw::hdr_file::read_hdr_linear(&bytes).expect("decode exr");
    let meta = core_raw::hdr_file::read_hdr_meta(&bytes);
    let max = img.data.iter().copied().fold(0.0f32, f32::max);
    println!(
        "read {}x{} linear ProPhoto (max {max:.3}; >1.0 = recovered headroom)",
        img.width, img.height
    );

    core_raw::write_hdr_dng(&dest, &img, &meta).expect("write dng");
    let size_mb = std::fs::metadata(&dest).expect("stat dng").len() as f64 / 1e6;
    println!(
        "wrote {} ({size_mb:.1} MB, uncompressed float)",
        dest.display()
    );

    // Re-open through the normal decode path: proves the DNG is well-formed enough for rawler.
    let src = core_raw::source_from_path(Path::new(&dest)).expect("open dng");
    let back = core_raw::read_metadata(&src).expect("read dng metadata");
    println!(
        "re-read metadata: {} {}",
        back.camera_make.as_deref().unwrap_or("?"),
        back.camera_model.as_deref().unwrap_or("?")
    );
}
