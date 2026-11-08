//! Integration test over a real Canon EOS R7 CR3 from `library/2026`.
//! Skips gracefully if the validation library is not present.

use image::GenericImageView;
use std::path::PathBuf;
use std::sync::Arc;

fn sample_cr3() -> Option<PathBuf> {
    // crates/core-raw/tests -> repo root is ../../..
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../library/2026/2026-06-06")
        .canonicalize()
        .ok()?;
    let mut entry = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("cr3"))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    entry.sort();
    entry.into_iter().next()
}

#[test]
fn metadata_thumbnail_hash_fingerprint() {
    let Some(path) = sample_cr3() else {
        // In CI (DARKROOM_REQUIRE_FIXTURES set) a missing committed fixture is a hard failure, not a
        // silent pass — that's how the committed CR3 disappearing gets caught.
        assert!(
            std::env::var_os("DARKROOM_REQUIRE_FIXTURES").is_none(),
            "CR3 fixture (library/2026) missing but DARKROOM_REQUIRE_FIXTURES is set"
        );
        eprintln!("library/2026 not present — skipping");
        return;
    };

    let bytes = Arc::new(std::fs::read(&path).expect("read file"));
    let digest = core_raw::content_hash(&bytes);
    assert_eq!(digest.len(), 32);
    assert!(!core_raw::hex(&digest).is_empty());

    let src = core_raw::source_from_bytes(bytes.clone(), &path);

    let meta = core_raw::read_metadata(&src).expect("metadata");
    assert!(
        meta.camera_model.as_deref().unwrap_or("").contains("R7"),
        "expected Canon EOS R7, got {:?}",
        meta.camera_model
    );
    assert!(meta.capture_date.is_some(), "capture date should parse");
    assert!(meta.iso.is_some(), "iso should be present");

    let thumb = core_raw::thumbnail_jpeg(&src, 256, 85).expect("thumbnail");
    assert!(thumb.jpeg.len() > 1000, "thumb jpeg too small");
    assert_eq!(&thumb.jpeg[0..2], &[0xFF, 0xD8], "not a JPEG (SOI)");
    assert!(thumb.src_width >= 4000 && thumb.src_height >= 3000);

    let fp = core_raw::capture_fingerprint(&meta, thumb.src_width, thumb.src_height);
    assert!(fp.is_some(), "fingerprint should be high-confidence");
}

#[test]
fn thumbnail_jpegs_one_decode_many_sizes() {
    let Some(path) = sample_cr3() else {
        eprintln!("library/2026 not present — skipping");
        return;
    };
    let bytes = Arc::new(std::fs::read(&path).expect("read file"));
    let src = core_raw::source_from_bytes(bytes, &path);

    let single = core_raw::thumbnail_jpeg(&src, 512, 82).expect("single");
    let many =
        core_raw::thumbnail_jpegs(&src, &[(512, 82), (2560, 85), (100_000, 85)]).expect("multi");
    assert_eq!(many.len(), 3);
    // The grid thumb must not change just because a larger preview rides along.
    assert_eq!(
        many[0].jpeg, single.jpeg,
        "512 entry must be byte-identical"
    );
    assert_eq!(many[0].src_width, single.src_width);
    assert_eq!(many[0].disp_height, single.disp_height);

    let dims = |t: &core_raw::Thumb| image::load_from_memory(&t.jpeg).unwrap().dimensions();
    let (w1, h1) = dims(&many[1]);
    assert_eq!(w1.max(h1), 2560, "embedded preview is larger than 2560");
    // An edge above the embedded preview's own size is never upscaled.
    let (w2, h2) = dims(&many[2]);
    assert!(w2.max(h2) >= 2560 && w2.max(h2) <= 100_000);
    assert!(many[2].jpeg.len() >= many[1].jpeg.len());
}
