//! Canon **HDR-PQ CR3** (`CompressorVersion` `CanonCR3_003`, written whenever the body shoots in
//! HDR PQ mode) has no JPEG preview to extract — rawler returns a hard error for it (dnglab#7).
//! The mosaic beside it decodes perfectly, so `thumb.rs` falls back to developing the RAW rather
//! than failing the file; before that fallback existed, every such file failed to index (557 of
//! 1911 CR3s in the author's real R7 corpus).
//!
//! Fixture-gated, like the other real-file tests: point `$DARKROOM_CR3_FIXTURES` (or
//! `$DARKROOM_HDR_FIXTURES`) at a folder of R7 files. Skips when none is present, so CI without
//! fixtures stays green — the parser-level guards live in `core-raw::heif`'s unit tests.

#![cfg(not(windows))]

use std::path::PathBuf;

fn fixture_dirs() -> Vec<PathBuf> {
    ["DARKROOM_CR3_FIXTURES", "DARKROOM_HDR_FIXTURES"]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .collect()
}

/// The first CR3 in `dir` whose compressor-version tag marks it HDR PQ. The tag sits in the
/// container's early metadata boxes, so only a header slice is read — this runs against corpora of
/// thousands of 24 MB files, and reading them whole cost minutes.
fn find_hdr_pq_cr3(dir: &PathBuf) -> Option<PathBuf> {
    use std::io::Read;
    const TAG: &[u8] = b"CanonCR3_003";
    const HEAD: usize = 64 * 1024;

    let rd = std::fs::read_dir(dir).ok()?;
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("cr3"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    files.into_iter().find(|p| {
        let Ok(mut f) = std::fs::File::open(p) else {
            return false;
        };
        let mut head = vec![0u8; HEAD];
        let Ok(n) = f.read(&mut head) else {
            return false;
        };
        head[..n].windows(TAG.len()).any(|w| w == TAG)
    })
}

#[test]
fn hdr_pq_cr3_thumbnails_via_developed_fallback() {
    let Some(path) = fixture_dirs().iter().find_map(find_hdr_pq_cr3) else {
        eprintln!("skipping: no HDR-PQ CR3 fixture (set DARKROOM_CR3_FIXTURES)");
        return;
    };
    let src = core_raw::source_from_path(&path).expect("open source");

    // The whole point: an unreadable embedded preview must not fail the file.
    let thumb = core_raw::thumbnail_jpeg(&src, 512, 90)
        .unwrap_or_else(|e| panic!("{}: thumbnail failed: {e}", path.display()));

    assert!(!thumb.jpeg.is_empty(), "empty thumbnail JPEG");
    // Dimensions must describe the FULL image, never the scaled thumbnail — `src_*` feeds the
    // capture fingerprint and `disp_*` drives catalog aspect logic.
    assert!(
        thumb.src_width > 512 && thumb.src_height > 512,
        "src dims look like thumbnail dims, not sensor dims: {}x{}",
        thumb.src_width,
        thumb.src_height
    );
    // Native vs display differ only by a quarter turn.
    let native = (thumb.src_width, thumb.src_height);
    let disp = (thumb.disp_width, thumb.disp_height);
    assert!(
        disp == native || disp == (native.1, native.0),
        "display dims {disp:?} are neither native {native:?} nor its transpose"
    );

    // And the mosaic itself was always fine — that's why the fallback is safe.
    let lin = core_raw::develop_linear(&src).expect("develop HDR-PQ CR3");
    assert_eq!(
        (lin.width.max(lin.height), lin.width.min(lin.height)),
        (native.0.max(native.1), native.0.min(native.1)),
        "developed dims disagree with the reported sensor dims"
    );
}
