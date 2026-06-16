//! Validates the fast half-resolution superpixel preview decode against the full-quality path.
//! Skips gracefully if the validation library is not present.

use std::path::PathBuf;

fn sample_cr3() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../library/2026/2026-06-06")
        .canonicalize()
        .ok()?;
    let mut entries = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("cr3"))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries.into_iter().next()
}

fn channel_means(img: &core_raw::LinearImage) -> [f64; 3] {
    let mut sum = [0f64; 3];
    for px in img.data.chunks_exact(3) {
        sum[0] += px[0] as f64;
        sum[1] += px[1] as f64;
        sum[2] += px[2] as f64;
    }
    let n = (img.data.len() / 3).max(1) as f64;
    [sum[0] / n, sum[1] / n, sum[2] / n]
}

#[test]
fn preview_matches_full_within_tolerance() {
    let Some(path) = sample_cr3() else {
        eprintln!("library/2026 not present — skipping");
        return;
    };
    let src = core_raw::source_from_path(&path).expect("source");
    let full = core_raw::develop_linear(&src).expect("full decode");
    let src2 = core_raw::source_from_path(&path).expect("source");
    let preview = core_raw::develop_linear_preview(&src2).expect("preview decode");

    // Superpixel output is ~half each dimension (within rounding / crop differences).
    let wr = preview.width as f64 / full.width as f64;
    let hr = preview.height as f64 / full.height as f64;
    assert!(
        (0.45..0.56).contains(&wr) && (0.45..0.56).contains(&hr),
        "preview should be ~half-res: full {}x{}, preview {}x{}",
        full.width,
        full.height,
        preview.width,
        preview.height
    );

    // Aspect ratio preserved.
    let ar_full = full.width as f64 / full.height as f64;
    let ar_prev = preview.width as f64 / preview.height as f64;
    assert!(
        (ar_full - ar_prev).abs() < 0.02,
        "aspect ratio mismatch: {ar_full} vs {ar_prev}"
    );

    // Same color math → per-channel global means should be very close (detail differs, mean doesn't).
    let mf = channel_means(&full);
    let mp = channel_means(&preview);
    for c in 0..3 {
        assert!(
            (mf[c] - mp[c]).abs() < 0.02,
            "channel {c} mean diverged: full {:.4} vs preview {:.4}",
            mf[c],
            mp[c]
        );
    }
}
