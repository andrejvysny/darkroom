//! Seam test for the raw-domain denoise integration point.
//!
//! `develop_linear_denoised` decodes once, hands the Bayer mosaic to a `MosaicDenoiser`, writes the
//! result back into the sensor buffer, and re-develops through the SAME color pipeline as
//! `develop_linear`. This test drives that path with an **identity** denoiser (returns the mosaic
//! unchanged): the denoised output must then be byte-for-byte identical to a plain `develop_linear`.
//! That proves the write-back + re-develop mechanism is lossless — the only thing a real denoiser
//! changes is the mosaic values, nothing in demosaic/WB/color. Skips if the fixture is absent.

use core_raw::{DenoiseOutput, MosaicDenoiser, MosaicInfo};
use std::path::PathBuf;

fn sample_cr3() -> Option<PathBuf> {
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

/// Returns the mosaic unchanged — the denoise no-op.
struct Identity;
impl MosaicDenoiser for Identity {
    fn denoise(&self, info: &MosaicInfo) -> Vec<u16> {
        info.data.to_vec()
    }
}

/// Bit-exact view of an f32 buffer (so a stray NaN compares by its stable bit pattern, not `!=`).
fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}

#[test]
fn identity_denoise_matches_clean_develop() {
    let Some(path) = sample_cr3() else {
        assert!(
            std::env::var_os("DARKROOM_REQUIRE_FIXTURES").is_none(),
            "CR3 fixture (library/2026) missing but DARKROOM_REQUIRE_FIXTURES is set"
        );
        eprintln!("library/2026 not present — skipping");
        return;
    };
    let src = core_raw::source_from_path(&path).expect("open source");

    let reference = core_raw::develop_linear(&src).expect("develop_linear");
    let DenoiseOutput { clean, denoised } =
        core_raw::develop_linear_denoised(&src, &Identity).expect("develop_linear_denoised");

    // R7 is a standard RGB Bayer sensor → the denoised path must be taken.
    let denoised = denoised.expect("R7 CR3 must produce a denoised (RGB Bayer) output");

    // The clean develop from the shared decode must equal the standalone `develop_linear`.
    assert_eq!(
        (clean.width, clean.height),
        (reference.width, reference.height)
    );
    assert_eq!(
        bits(&clean.data),
        bits(&reference.data),
        "clean output diverged from develop_linear"
    );

    // The seam property: identity denoise (mosaic unchanged) re-develops byte-for-byte identically.
    assert_eq!(
        (denoised.width, denoised.height),
        (clean.width, clean.height)
    );
    assert_eq!(
        bits(&denoised.data),
        bits(&clean.data),
        "identity-denoised output diverged from clean — write-back/re-develop is not lossless"
    );
}
