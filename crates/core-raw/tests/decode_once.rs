//! Phase-0 guarantee for the unified AI pipeline: decoding the embedded preview ONCE via
//! `preview_with_orientation` and deriving the two views (sensor-native for object detectors,
//! EXIF-oriented for faces) is pixel-identical to the two legacy decoders (`preview_image` /
//! `oriented_preview`). This is what lets the merged pass stop decoding each RAW twice WITHOUT
//! re-validating any model (each keeps its exact input recipe). Skips if the fixture is absent.

use std::path::PathBuf;

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
fn one_decode_derives_both_views_identically() {
    let Some(path) = sample_cr3() else {
        assert!(
            std::env::var_os("DARKROOM_REQUIRE_FIXTURES").is_none(),
            "CR3 fixture (library/2026) missing but DARKROOM_REQUIRE_FIXTURES is set"
        );
        eprintln!("library/2026 not present — skipping");
        return;
    };
    let src = core_raw::source_from_path(&path).expect("open source");

    let (native, orientation) = core_raw::preview_with_orientation(&src).expect("combined decode");

    // Native view must equal the legacy sensor-native decoder byte-for-byte (object detectors are
    // calibrated on exactly these pixels).
    let legacy_native = core_raw::preview_image(&src).expect("preview_image");
    assert_eq!(
        native.to_rgb8().into_raw(),
        legacy_native.to_rgb8().into_raw(),
        "native view diverged from preview_image"
    );

    // Applying the returned orientation must reproduce the legacy oriented decoder byte-for-byte
    // (faces are detected/aligned on exactly these pixels).
    let mut derived_oriented = native;
    if let Some(o) = orientation {
        derived_oriented.apply_orientation(o);
    }
    let legacy_oriented = core_raw::oriented_preview(&src).expect("oriented_preview");
    assert_eq!(
        derived_oriented.to_rgb8().into_raw(),
        legacy_oriented.to_rgb8().into_raw(),
        "oriented view diverged from oriented_preview"
    );
}
