//! Develop-path behaviour proved on **synthetic** DNGs (see `core_raw::synth`).
//!
//! Everything here runs with no fixture: each test authors a few-kilobyte DNG carrying exactly the
//! sensor property under test — a pre-divided neutral patch, a blown highlight, a portrait
//! orientation tag, an X-Trans CFA, a monochrome sensor — and then drives the real public decode
//! entry points over it. Assertions stay on `core-raw`'s public API so no rawler type leaks out of
//! the crate (the rawler-level round-trip assertions live in `synth.rs`'s own unit tests).

use core_raw::synth::{write_synthetic_bayer_dng, write_synthetic_mono_dng, SynthSpec};
use core_raw::{
    as_shot_wb, develop_linear, develop_linear_preview, develop_linear_wb, source_from_path,
    FailureKind, LinearImage,
};
use std::path::PathBuf;

/// A unique scratch path per test (tests share a process, so the name must not collide).
fn temp_dng(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "darkroom-synth-{}-{}.dng",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

/// Per-pixel chroma `(max - min) / max`; 0 for a perfectly neutral pixel.
fn chroma(px: &[f32]) -> f32 {
    let mx = px[0].max(px[1]).max(px[2]);
    let mn = px[0].min(px[1]).min(px[2]);
    if mx <= 1e-6 {
        0.0
    } else {
        (mx - mn) / mx
    }
}

/// Worst chroma over a rectangle of the developed image.
fn max_chroma_in(img: &LinearImage, x0: u32, y0: u32, x1: u32, y1: u32) -> f32 {
    let mut worst = 0f32;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * img.width + x) * 3) as usize;
            worst = worst.max(chroma(&img.data[i..i + 3]));
        }
    }
    worst
}

fn mean_of(img: &LinearImage, x0: u32, y0: u32, x1: u32, y1: u32) -> [f64; 3] {
    let mut acc = [0f64; 3];
    let mut n = 0f64;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * img.width + x) * 3) as usize;
            for (c, a) in acc.iter_mut().enumerate() {
                *a += img.data[i + c] as f64;
            }
            n += 1.0;
        }
    }
    [acc[0] / n, acc[1] / n, acc[2] / n]
}

/// Bit-exact view of an f32 buffer (a stray NaN compares by its bit pattern, not `!=`).
fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}

#[test]
fn synthetic_bayer_dng_round_trips() {
    let spec = SynthSpec::default();
    let path = temp_dng("roundtrip");
    write_synthetic_bayer_dng(&path, &spec).expect("write synthetic DNG");
    let src = source_from_path(&path).expect("open synthetic DNG");

    let img = develop_linear(&src).expect("develop synthetic DNG");
    assert_eq!((img.width, img.height), (spec.width, spec.height));
    assert_eq!(
        img.data.len(),
        (spec.width * spec.height * 3) as usize,
        "buffer length must match the reported dims"
    );

    let wb = as_shot_wb(&src).expect("as-shot WB");
    for (i, (got, want)) in wb.iter().zip(spec.wb.iter()).take(3).enumerate() {
        assert!(
            (got - want).abs() < 1e-3,
            "wb[{i}] = {got} (want {want}) — AsShotNeutral did not round-trip"
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn neutral_patch_stays_neutral() {
    let spec = SynthSpec {
        neutral_block: Some((16, 12, 32, 24)),
        ..Default::default()
    };
    let path = temp_dng("neutral");
    write_synthetic_bayer_dng(&path, &spec).expect("write synthetic DNG");
    let img = develop_linear(&source_from_path(&path).expect("open")).expect("develop");

    // Inset by the demosaic's reach: PPG interpolates across the patch edge, so only the interior
    // carries the pre-divided values unmixed. 6 px is well beyond PPG's neighbourhood.
    let worst = max_chroma_in(&img, 16 + 6, 12 + 6, 16 + 32 - 6, 12 + 24 - 6);
    assert!(
        worst < 0.005,
        "WB-pre-divided patch developed with chroma {worst} — white balance and the colour matrix \
         are no longer wired together correctly"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn wb_override_none_is_byte_identical() {
    let path = temp_dng("wbnone");
    write_synthetic_bayer_dng(&path, &SynthSpec::default()).expect("write");
    let src = source_from_path(&path).expect("open");

    let plain = develop_linear(&src).expect("develop_linear");
    let (via_wb, wb) = develop_linear_wb(&src, None).expect("develop_linear_wb(None)");
    assert_eq!((plain.width, plain.height), (via_wb.width, via_wb.height));
    assert_eq!(
        bits(&plain.data),
        bits(&via_wb.data),
        "develop_linear(src) must equal develop_linear_wb(src, None).0 byte-for-byte"
    );
    assert!(
        (wb[0] - 2.0).abs() < 1e-3 && (wb[2] - 1.5).abs() < 1e-3,
        "{wb:?}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn wb_override_changes_color_predictably() {
    let path = temp_dng("wboverride");
    write_synthetic_bayer_dng(&path, &SynthSpec::default()).expect("write");
    let src = source_from_path(&path).expect("open");

    // The frame is a numerically FLAT mosaic, so it is neutral only when the gains are neutral.
    let (as_shot, _) = develop_linear_wb(&src, None).expect("as-shot");
    let (overridden, wb) =
        develop_linear_wb(&src, Some([1.0, 1.0, 1.0, f32::NAN])).expect("override");

    assert_eq!(
        wb[0..3],
        [1.0, 1.0, 1.0],
        "the override must be reported back"
    );
    assert_ne!(
        bits(&as_shot.data),
        bits(&overridden.data),
        "a WB override must change the pixels"
    );
    assert!(
        max_chroma_in(&overridden, 8, 8, 56, 40) < 0.005,
        "a flat mosaic with neutral gains must develop neutral"
    );
    assert!(
        max_chroma_in(&as_shot, 8, 8, 56, 40) > 0.05,
        "the same flat mosaic with [2, 1, 1.5] gains must NOT be neutral"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn as_shot_wb_is_finite_and_positive() {
    let path = temp_dng("aswb");
    write_synthetic_bayer_dng(&path, &SynthSpec::default()).expect("write");
    let wb = as_shot_wb(&source_from_path(&path).expect("open")).expect("as-shot WB");
    for (i, c) in wb[0..3].iter().enumerate() {
        assert!(c.is_finite() && *c > 0.0, "wb[{i}] = {c}");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn portrait_orientation_transposes_dims() {
    let spec = SynthSpec {
        orientation: 6, // rotate 90° CW on display
        ..Default::default()
    };
    let path = temp_dng("portrait");
    write_synthetic_bayer_dng(&path, &spec).expect("write");
    let img = develop_linear(&source_from_path(&path).expect("open")).expect("develop");
    assert_eq!(
        (img.width, img.height),
        (spec.height, spec.width),
        "EXIF orientation 6 must upright the buffer, swapping width/height"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn preview_and_full_agree_on_center_patch() {
    let spec = SynthSpec {
        width: 256,
        height: 192,
        // Covers the sampled centre in both resolutions, so the patch is genuinely coloured data
        // rather than the frame's flat fill.
        neutral_block: Some((64, 48, 128, 96)),
        ..Default::default()
    };
    let path = temp_dng("preview");
    write_synthetic_bayer_dng(&path, &spec).expect("write");
    let src = source_from_path(&path).expect("open");

    let full = develop_linear(&src).expect("full develop");
    let preview = develop_linear_preview(&src).expect("preview develop");
    assert_eq!((full.width, full.height), (256, 192));
    assert_eq!(
        (preview.width, preview.height),
        (128, 96),
        "the superpixel path must halve the resolution"
    );

    let a = mean_of(&full, 112, 80, 144, 112);
    let b = mean_of(&preview, 56, 40, 72, 56);
    for c in 0..3 {
        assert!(
            (a[c] - b[c]).abs() < 0.02,
            "channel {c}: full {:?} vs preview {:?} — the two paths' colour math has diverged",
            a,
            b
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn garbage_file_errors_not_panics() {
    // Deterministic pseudo-random bytes with a plausible RAW extension: the decoder is selected by
    // extension, so this drives a real decoder over nonsense.
    let mut bytes = Vec::with_capacity(64 * 1024);
    let mut state = 0x1234_5678u32;
    for _ in 0..64 * 1024 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        bytes.push((state >> 24) as u8);
    }
    let path = temp_dng("garbage").with_extension("nef");
    std::fs::write(&path, &bytes).expect("write garbage");

    let src = source_from_path(&path).expect("open garbage");
    // `.err().expect(..)` rather than `expect_err`: `LinearImage` is deliberately not `Debug`
    // (a 400 MB buffer must never be formatted into a panic message).
    let err = develop_linear(&src).err().expect("garbage must not decode");
    assert!(
        !matches!(err.kind(), FailureKind::Io),
        "the file reads fine; the failure must be about its content, got {err}"
    );
    // The metadata and thumbnail entry points must survive it too.
    assert!(core_raw::read_metadata(&src).is_err());
    assert!(core_raw::thumbnail_jpeg(&src, 256, 85).is_err());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn monochrome_dng_develops_gray_without_panic() {
    let spec = SynthSpec::default();
    let path = temp_dng("mono");
    write_synthetic_mono_dng(&path, &spec).expect("write mono DNG");
    let img = develop_linear(&source_from_path(&path).expect("open")).expect(
        "a monochrome sensor must develop — rawler's own `apply_scaling` is `todo!()` for it",
    );

    assert_eq!((img.width, img.height), (spec.width, spec.height));
    let expect = (spec.base_level as f32 - spec.black as f32) / (spec.white - spec.black) as f32;
    for (i, px) in img.data.chunks_exact(3).enumerate() {
        assert!(
            px[0] == px[1] && px[1] == px[2],
            "pixel {i} is not grey: {px:?}"
        );
        assert!(
            (px[0] - expect).abs() < 1e-4,
            "pixel {i} = {} (want {expect}) — black/white normalization is wrong",
            px[0]
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn non_bayer_cfa_is_typed_unsupported() {
    let spec = SynthSpec {
        // A real Fujifilm X-Trans 6×6 tile. rawler 0.8 ships a bilinear X-Trans demosaic, but its
        // colour has never been validated here, so `guard_developable` must refuse it — loudly and
        // typed, never by panicking inside `Superpixel3Channel` or a `todo!()`.
        cfa: "GGRGGBGGBGGRBRGRBGGGBGGRGGRGGBRBGBRG",
        width: 60,
        height: 48,
        ..Default::default()
    };
    let path = temp_dng("xtrans");
    write_synthetic_bayer_dng(&path, &spec).expect("write X-Trans DNG");
    let src = source_from_path(&path).expect("open");

    let err = develop_linear(&src)
        .err()
        .expect("X-Trans must be refused, not developed");
    assert_eq!(err.kind(), FailureKind::Unsupported, "got {err}");
    assert!(
        err.user_detail().contains("X-Trans"),
        "the message should name the reason, got {:?}",
        err.user_detail()
    );
    // The half-res path must refuse it too — `Superpixel3Channel` panics on a 6×6 tile.
    assert_eq!(
        develop_linear_preview(&src)
            .err()
            .expect("preview must refuse X-Trans")
            .kind(),
        FailureKind::Unsupported
    );
    let _ = std::fs::remove_file(&path);
}

/// H1: a frame clipped to the sensor's white level is NUMERICALLY neutral, so it must develop
/// neutral. Applying the as-shot gains alone does not — it tints every blown highlight by the
/// balance (magenta, for typical daylight gains) — which is what the develop path's
/// clipped-highlight reconstruction exists to undo.
#[test]
fn clipped_sensor_white_develops_neutral() {
    let spec = SynthSpec {
        saturated_block: Some((0, 0, 64, 48)),
        wb: [2.0, 1.0, 1.5, f32::NAN],
        ..Default::default()
    };
    let path = temp_dng("clipped");
    write_synthetic_bayer_dng(&path, &spec).expect("write");
    let img = develop_linear(&source_from_path(&path).expect("open")).expect("develop");

    let worst = max_chroma_in(&img, 4, 4, img.width - 4, img.height - 4);
    assert!(
        worst < 0.02,
        "a fully clipped frame developed with chroma {worst} — clipped highlights are tinted by \
         the as-shot white balance"
    );
    let _ = std::fs::remove_file(&path);
}
