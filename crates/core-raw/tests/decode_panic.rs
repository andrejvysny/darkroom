//! Decoder-panic containment at the seam.
//!
//! `core-raw`'s public entry points must never unwind into their caller: a panic anywhere below
//! them — in rawler, or in an injected `MosaicDenoiser` — has to come back as a typed
//! `RawError::DecoderPanic` so the indexer skips one file instead of aborting the app.

use core_raw::synth::{write_synthetic_bayer_dng, SynthSpec};
use core_raw::{FailureKind, MosaicDenoiser, MosaicInfo, RawError};
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

/// Silence the default hook for a deliberate panic, then restore it.
fn without_panic_output<T>(f: impl FnOnce() -> T) -> T {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let out = f();
    std::panic::set_hook(previous);
    out
}

/// Stands in for a broken denoiser (e.g. an ONNX session that trips an assert mid-inference).
struct Panicky;
impl MosaicDenoiser for Panicky {
    fn denoise(&self, _info: &MosaicInfo) -> Vec<u16> {
        panic!("denoiser exploded");
    }
}

#[test]
fn panicking_denoiser_is_contained() {
    let Some(path) = sample_cr3() else {
        assert!(
            std::env::var_os("DARKROOM_REQUIRE_FIXTURES").is_none(),
            "CR3 fixture (library/2026) missing but DARKROOM_REQUIRE_FIXTURES is set"
        );
        eprintln!("library/2026 not present — skipping");
        return;
    };
    let src = core_raw::source_from_path(&path).expect("open source");

    let err = without_panic_output(|| {
        core_raw::develop_linear_denoised(&src, &Panicky)
            .err()
            .expect("a panicking denoiser must not unwind into the caller")
    });
    assert!(
        matches!(err, RawError::DecoderPanic(_)),
        "expected a contained decoder panic, got {err}"
    );
    assert_eq!(err.kind(), FailureKind::Panic);
    assert!(err.to_string().contains("denoiser exploded"), "{err}");

    // The process is still healthy: the very next decode works.
    let clean = core_raw::develop_linear(&src).expect("develop after a contained panic");
    assert!(clean.width > 1000 && clean.height > 1000);
}

#[test]
fn truncated_dng_errors_without_crashing() {
    let path = std::env::temp_dir().join(format!(
        "darkroom-truncated-{}-{}.dng",
        std::process::id(),
        "60pct"
    ));
    let _ = std::fs::remove_file(&path);
    write_synthetic_bayer_dng(&path, &SynthSpec::default()).expect("write synthetic DNG");

    let whole = std::fs::read(&path).expect("read back");
    let cut = whole.len() * 60 / 100;
    std::fs::write(&path, &whole[..cut]).expect("truncate");

    let src = core_raw::source_from_path(&path).expect("open truncated DNG");
    let err = without_panic_output(|| {
        core_raw::develop_linear(&src).err().expect(
            "a DNG cut in half must fail, not return pixels — and must not take the process down",
        )
    });
    // Any typed kind is acceptable here; what matters is that it is an `Err` and not an abort.
    assert_ne!(err.kind(), FailureKind::Io, "the file itself reads: {err}");
    let _ = std::fs::remove_file(&path);
}
