//! HEIF (.hif) decode integration tests.
//!
//! Two tiers:
//! 1. `synthetic_pq_heif_round_trips` — decodes the COMMITTED fixture
//!    `tests/fixtures/synthetic_pq.hif` (96×64, 10-bit HEVC lossless, BT.2020 PQ nclx, three
//!    neutral bands at exact PQ codes; regenerate with `examples/gen_pq_fixture.rs` + `heif-enc`)
//!    through the SHIPPING path (`source_from_path` → `develop_linear`) and asserts the
//!    linear-ProPhoto values. Runs anywhere libheif+libde265 are present (a build requirement on
//!    non-Windows).
//! 2. `real_r7_hif_fixture` — exercises a real Canon EOS R7 file when present (skip-if-missing,
//!    same pattern as the CR3-gated tests). Fixture dir: `$DARKROOM_HDR_FIXTURES` or
//!    `../../library/fixtures-hdr`.
#![cfg(not(windows))]

use std::path::PathBuf;

/// ST 2084 EOTF, mirror of the shipping constant chain in `core-raw::color`.
fn pq_eotf(e: f64) -> f64 {
    const M1: f64 = 2610.0 / 16384.0;
    const M2: f64 = 2523.0 / 4096.0 * 128.0;
    const C1: f64 = 3424.0 / 4096.0;
    const C2: f64 = 2413.0 / 4096.0 * 32.0;
    const C3: f64 = 2392.0 / 4096.0 * 32.0;
    let p = e.clamp(0.0, 1.0).powf(1.0 / M2);
    (((p - C1).max(0.0)) / (C2 - C3 * p)).powf(1.0 / M1)
}

/// Shared assertions for the committed synthetic PQ fixtures (both chroma variants carry the same
/// three neutral bands at exact 10-bit PQ codes — see `gen_pq_fixture.rs`).
fn assert_pq_fixture(path: &str) {
    const BAND_CODES: [u16; 3] = [153, 594, 1023]; // ≈1 nit, 203 nits (diffuse white), 10 000 nits
    const W: u32 = 96;
    const H: u32 = 64;

    let path = PathBuf::from(path);
    assert!(
        path.exists(),
        "committed fixture missing: {}",
        path.display()
    );

    assert_eq!(core_raw::classify(&path), core_raw::ImageKind::Heif);
    assert!(!core_raw::is_display(&path));

    let src = core_raw::source_from_path(&path).unwrap();
    let lin = core_raw::develop_linear(&src).unwrap();
    assert_eq!((lin.width, lin.height), (W, H));

    // Expected values derive from the SHIPPING anchor, so a recalibration of
    // HDR_DIFFUSE_WHITE_NITS never breaks this test — the fixture's bands are defined in absolute
    // nits (≈1 / 203 / 10 000), independent of where the anchor puts working-space 1.0.
    let anchor = core_raw::HDR_DIFFUSE_WHITE_NITS;
    let expected: Vec<f32> = BAND_CODES
        .iter()
        .map(|&c| (pq_eotf(c as f64 / 1023.0) * 10000.0 / anchor) as f32)
        .collect();
    // Sanity on the derivation itself: band 1 encodes 203 nits, band 2 the 10 000-nit PQ peak.
    assert!(
        (expected[1] - (203.0 / anchor) as f32).abs() < 0.01,
        "band-code table drifted"
    );
    assert!((expected[2] - (10000.0 / anchor) as f32).abs() / expected[2] < 0.01);

    for (band, &want) in expected.iter().enumerate() {
        let col = (W as usize / 3) * band + W as usize / 6;
        let idx = ((H as usize / 2) * W as usize + col) * 3;
        let px = [lin.data[idx], lin.data[idx + 1], lin.data[idx + 2]];
        let got = (px[0] + px[1] + px[2]) / 3.0;
        let rel = (got - want).abs() / want.max(1e-3);
        assert!(
            rel < 0.03,
            "band {band}: expected ≈{want}, got {got} (px {px:?})"
        );
        // Neutral input must stay neutral through BT.2020→ProPhoto (white maps to white).
        let spread = px.iter().cloned().fold(f32::MIN, f32::max)
            - px.iter().cloned().fold(f32::MAX, f32::min);
        assert!(
            spread <= 0.03 * got.max(0.02),
            "band {band}: neutral drifted {px:?}"
        );
    }

    // Thumbnail + preview + metadata over the same dispatch must not error (no Exif → empty meta).
    let thumb = core_raw::thumbnail_jpeg(&src, 48, 82).unwrap();
    assert!(!thumb.jpeg.is_empty());
    assert_eq!((thumb.src_width, thumb.src_height), (W, H));
    assert_eq!((thumb.disp_width, thumb.disp_height), (W, H));
    let prev = core_raw::preview_image(&src).unwrap();
    assert_eq!((prev.width(), prev.height()), (W, H));
    let meta = core_raw::read_metadata(&src).unwrap();
    assert!(meta.camera_model.is_none());
}

#[test]
fn synthetic_pq_heif_round_trips() {
    assert_pq_fixture("tests/fixtures/synthetic_pq.hif");
}

/// Same content encoded as **10-bit YCbCr 4:2:2** (verified with `heif-info`) — the exact codec
/// profile Canon HDR PQ `.HIF` files use (HEVC Main 4:2:2 10 intra). Guards libde265's
/// range-extensions decode path in CI (risk R1 of the HDR plan).
#[test]
fn synthetic_pq422_heif_round_trips() {
    assert_pq_fixture("tests/fixtures/synthetic_pq422.hif");
}

/// A non-PQ HEIF (real iPhone 13 Pro gain-map HDR HEIC, MIT-licensed — see APPLE_HDR_LICENSE)
/// must be rejected with the clean profile error, never decoded to wrong colors — while its Exif
/// metadata still reads fine.
#[test]
fn non_pq_heif_rejected_cleanly() {
    let path = PathBuf::from("tests/fixtures/apple_hdr_gainmap.hif");
    assert!(path.exists(), "committed fixture missing");

    let src = core_raw::source_from_path(&path).unwrap();
    let msg = match core_raw::develop_linear(&src) {
        Ok(_) => panic!("non-PQ profile must be rejected"),
        Err(e) => e.to_string(),
    };
    assert!(
        msg.contains("expected BT.2020 PQ"),
        "unexpected error text: {msg}"
    );
    // The thumbnail path goes through the same decode and must fail the same clean way.
    assert!(core_raw::thumbnail_jpeg(&src, 128, 82).is_err());

    // Metadata is independent of pixel decode and still works.
    let meta = core_raw::read_metadata(&src).unwrap();
    assert_eq!(meta.camera_model.as_deref(), Some("iPhone 13 Pro"));
    assert_eq!(
        meta.orientation, None,
        "HEIF meta must not re-expose EXIF orientation"
    );
}

fn fixture_dir() -> PathBuf {
    std::env::var_os("DARKROOM_HDR_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("../../library/fixtures-hdr"))
}

#[test]
fn real_r7_hif_fixture() {
    let dir = fixture_dir();
    let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("hif"))
                    .unwrap_or(false)
            })
            .collect(),
        Err(_) => {
            eprintln!("skipping: no HDR fixture dir at {}", dir.display());
            return;
        }
    };
    files.sort();
    let Some(f) = files.first() else {
        eprintln!("skipping: no .hif fixtures in {}", dir.display());
        return;
    };

    let src = core_raw::source_from_path(f).unwrap();
    let meta = core_raw::read_metadata(&src).unwrap();
    assert!(
        meta.camera_model.as_deref().unwrap_or("").contains("Canon"),
        "expected a Canon body, got {:?}",
        meta.camera_model
    );
    assert!(meta.iso.is_some() && meta.shutter.is_some());

    let lin = core_raw::develop_linear(&src).unwrap();
    assert!(lin.width > 1000 && lin.height > 1000);
    // Sanity on the PQ normalization: the brightest pixel of a real shot should decode to a
    // plausible absolute luminance — above 100 nits (virtually any real scene) and at most the
    // 10 000-nit PQ peak. (A fixed ">1.0 headroom" check would wrongly fail dark scenes whose
    // brightest content sits below the anchor.)
    let max_v = lin.data.iter().cloned().fold(0f32, f32::max) as f64;
    let max_nits = max_v * core_raw::HDR_DIFFUSE_WHITE_NITS;
    assert!(
        (100.0..=10000.0).contains(&max_nits),
        "brightest pixel ≈{max_nits:.0} nits — PQ normalization suspect"
    );

    let thumb = core_raw::thumbnail_jpeg(&src, 512, 82).unwrap();
    assert!(!thumb.jpeg.is_empty());
}
