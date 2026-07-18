//! Merged-HDR EXR round-trip: write a synthetic scene-referred gradient with full metadata, read
//! it back through the SHIPPING dispatch (`source_from_path` → `develop_linear` /
//! `read_metadata` / `thumbnail_jpeg`), and assert values + metadata survive fp16 storage.
//! Runs everywhere (no fixtures, no GPU).

use core_raw::{HdrSourceInfo, LinearImage, RawMeta};

fn test_meta() -> RawMeta {
    RawMeta {
        camera_make: Some("Canon".into()),
        camera_model: Some("Canon EOS R7".into()),
        body_serial: Some("012345".into()),
        lens: Some("RF-S18-150mm F3.5-6.3 IS STM".into()),
        capture_date: Some(1_780_000_000),
        date_time_original: Some("2026:06:06 12:00:00".into()),
        sub_sec: Some("42".into()),
        iso: Some(100),
        shutter: Some("1/125".into()),
        aperture: Some(8.0),
        focal_length: Some(50.0),
        orientation: None,
    }
}

#[test]
fn exr_round_trips_values_and_metadata() {
    const W: u32 = 64;
    const H: u32 = 40;
    // Scene-referred test values spanning deep shadow → far above diffuse white (merge headroom).
    let levels: [f32; 4] = [0.0, 0.18, 1.0, 8.0];
    let mut data = Vec::with_capacity((W * H * 3) as usize);
    for y in 0..H {
        for x in 0..W {
            let v = levels[(x as usize * levels.len() / W as usize).min(levels.len() - 1)];
            // Slight per-channel offsets so channel order mistakes can't cancel out.
            data.extend_from_slice(&[v, v * 0.5, v * 0.25]);
            let _ = y;
        }
    }
    let img = LinearImage {
        width: W,
        height: H,
        data,
    };

    let sources = vec![
        HdrSourceInfo {
            image_id: 11,
            content_hash_hex: "aa".repeat(32),
            relative_ev: -2.0,
        },
        HdrSourceInfo {
            image_id: 12,
            content_hash_hex: "bb".repeat(32),
            relative_ev: 0.0,
        },
        HdrSourceInfo {
            image_id: 13,
            content_hash_hex: "cc".repeat(32),
            relative_ev: 2.0,
        },
    ];

    let path = std::env::temp_dir().join(format!("darkroom_hdr_{}.exr", std::process::id()));
    core_raw::write_hdr_exr(&path, &img, &test_meta(), &sources, 1).unwrap();
    assert!(
        !path.with_extension("exr.part").exists(),
        ".part temp must be renamed away"
    );

    // Kind + dispatch.
    assert_eq!(core_raw::classify(&path), core_raw::ImageKind::Hdr);
    assert!(!core_raw::is_display(&path));

    // Pixel round-trip through the shipping path (fp16 storage: rel error ≤ 1e-3).
    let src = core_raw::source_from_path(&path).unwrap();
    let back = core_raw::develop_linear(&src).unwrap();
    assert_eq!((back.width, back.height), (W, H));
    for (i, (&a, &b)) in img.data.iter().zip(back.data.iter()).enumerate() {
        let rel = (a - b).abs() / a.abs().max(1e-3);
        assert!(rel <= 1e-3, "sample {i}: wrote {a}, read {b}");
    }

    // Metadata round-trip (drives process_file → catalog columns with zero override plumbing).
    let meta = core_raw::read_metadata(&src).unwrap();
    assert_eq!(meta.camera_model.as_deref(), Some("Canon EOS R7"));
    assert_eq!(meta.iso, Some(100));
    assert_eq!(meta.shutter.as_deref(), Some("1/125"));
    assert_eq!(meta.aperture, Some(8.0));
    assert_eq!(meta.capture_date, Some(1_780_000_000));

    // Parentage attribute.
    let bytes = std::fs::read(&path).unwrap();
    let parsed = core_raw::read_hdr_sources(&bytes).expect("sources attribute present");
    assert_eq!(parsed.version, 1);
    assert_eq!(parsed.reference_index, 1);
    assert_eq!(parsed.sources.len(), 3);
    assert_eq!(parsed.sources[0].relative_ev, -2.0);
    assert_eq!(parsed.sources[2].image_id, 13);

    // Thumbnail + preview must work (no embedded preview in EXR — decoded + downscaled).
    let thumb = core_raw::thumbnail_jpeg(&src, 32, 82).unwrap();
    assert!(!thumb.jpeg.is_empty());
    assert_eq!((thumb.src_width, thumb.src_height), (W, H));
    let prev = core_raw::preview_image(&src).unwrap();
    assert_eq!((prev.width(), prev.height()), (W, H));

    let _ = std::fs::remove_file(&path);
}

/// A foreign EXR (no darkroom attributes) must still decode with empty metadata, never error.
#[test]
fn foreign_exr_reads_with_empty_meta() {
    let path = std::env::temp_dir().join(format!("darkroom_foreign_{}.exr", std::process::id()));
    // Write via the plain exr helper — no chromaticities, no darkroom attributes.
    exr::prelude::write_rgb_file(&path, 8, 8, |_x, _y| (0.5f32, 0.25f32, 0.125f32)).unwrap();

    let src = core_raw::source_from_path(&path).unwrap();
    let lin = core_raw::develop_linear(&src).unwrap();
    assert_eq!((lin.width, lin.height), (8, 8));
    assert!((lin.data[0] - 0.5).abs() < 1e-3);

    let meta = core_raw::read_metadata(&src).unwrap();
    assert!(meta.camera_model.is_none() && meta.capture_date.is_none());
    assert!(core_raw::read_hdr_sources(&std::fs::read(&path).unwrap()).is_none());

    let _ = std::fs::remove_file(&path);
}
