//! Data-driven RAW-decode regression over the multi-maker corpus (`tests/corpus/`).
//!
//! The committed fixture is one Canon R7 CR3 — enough to prove the pipeline runs, useless for
//! proving it runs *the same way* on a Nikon HE NEF, a Canon sRAW, a Leica monochrome DNG or a
//! phone's ProRAW. This test walks `manifest.toml`, develops every sample the fetch script put on
//! disk, and compares ~12 machine-recorded statistics per file against `expected.toml`.
//!
//! Nothing here is fixed by hand. `expected.toml` is written by
//! `cargo run --release -p core-raw --example corpus_probe -- --record`, and that example is also
//! the implementation this test runs (`#[path]`-included below), so recorder and checker cannot
//! drift. A colour-pipeline change is *supposed* to move these numbers: re-record, and the diff
//! shows exactly which makers moved and by how much.
//!
//! Gating (the corpus is ~680 MB and is never committed):
//! - no corpus directory → skip, unless `DARKROOM_REQUIRE_CORPUS=1` (CI) makes it a hard failure
//! - `DARKROOM_CORPUS_TIER` (default 1) — 2 adds the extended set
//! - `DARKROOM_CORPUS_QUICK=1` — skip the expensive Tier-B checks
//! - `DARKROOM_RAW_CORPUS` — corpus location (default `target/raw-corpus`)
//!
//! Every divergence is accumulated and reported at the end, so one run tells you everything that
//! moved rather than the first thing that moved.

// The example carries its own inner `#![allow(dead_code)]` — each build uses a subset of it.
#[path = "../examples/corpus_probe.rs"]
mod probe;

use core_raw::{develop_linear, develop_linear_wb, source_from_path};
use probe::{
    center_patch_mean, chroma, corpus_dir, load_expected, load_manifest, measure, panics_outcome,
    unsupported_outcome, FileEntry, Golden, Measured, SaturateAll, PATCH_FULL, PATCH_PREVIEW,
};
use std::path::Path;

/// Total download budget for the whole corpus. The manifest is the CI cache key and the fetch is
/// on the critical path of every corpus run, so growth has to be a deliberate edit.
const BYTE_BUDGET: u64 = 800 * 1_000_000;

/// Keys every `[[file]]` block must carry, exactly once, each on its own line.
const FILE_KEYS: &[&str] = &[
    "id", "path", "sha256", "bytes", "make", "model", "tier", "tags", "support", "deep", "xfail",
];

// ---------------------------------------------------------------------------------------------
// Failure collection + XFAIL bookkeeping
// ---------------------------------------------------------------------------------------------

/// Routes one named assertion through its file's `xfail` list.
///
/// The inversion is the point: an assertion listed as known-red that starts PASSING is itself a
/// failure ("remove from xfail"). Otherwise a fix quietly leaves a stale exemption behind and the
/// next regression slips through under it.
struct Checks<'a> {
    id: &'a str,
    xfail: &'a [String],
    ran: Vec<&'static str>,
    failures: &'a mut Vec<String>,
}

impl<'a> Checks<'a> {
    fn new(entry: &'a FileEntry, failures: &'a mut Vec<String>) -> Self {
        Checks {
            id: &entry.id,
            xfail: &entry.xfail,
            ran: Vec::new(),
            failures,
        }
    }

    fn run(&mut self, name: &'static str, result: Result<(), String>) {
        self.ran.push(name);
        let expected_red = self.xfail.iter().any(|x| x == name);
        match (result, expected_red) {
            (Ok(()), false) | (Err(_), true) => {}
            (Ok(()), true) => self.failures.push(format!(
                "{}/{name}: listed in xfail but PASSED — remove it from the manifest's xfail",
                self.id
            )),
            (Err(e), false) => self.failures.push(format!("{}/{name}: {e}", self.id)),
        }
    }

    /// An `xfail` naming an assertion that never ran is a typo, not an exemption.
    fn finish(self) {
        for x in self.xfail {
            if !self.ran.iter().any(|n| n == x) {
                self.failures.push(format!(
                    "{}: xfail lists {x:?}, which is not an assertion that ran ({:?})",
                    self.id, self.ran
                ));
            }
        }
    }
}

fn ok_if(cond: bool, msg: impl FnOnce() -> String) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(msg())
    }
}

/// Absolute 2e-3 OR relative 1 % — decode is deterministic, but the corpus is recorded on one
/// machine and checked on another, and f32 demosaic reductions do not reassociate identically.
fn close3(a: [f64; 3], b: [f64; 3]) -> bool {
    a.iter().zip(b.iter()).all(|(x, y)| {
        let d = (x - y).abs();
        d <= 2e-3 || d <= 0.01 * y.abs().max(x.abs())
    })
}

// ---------------------------------------------------------------------------------------------
// Per-file checks
// ---------------------------------------------------------------------------------------------

/// Tier A: ONE develop (plus a mosaic-only decode for the white balance), metadata and thumbnail.
fn tier_a(c: &mut Checks, entry: &FileEntry, g: &Golden, m: &Measured) {
    let a = &m.golden;
    let dims = (a.develop_w, a.develop_h);

    c.run(
        "meta_identity",
        ok_if(
            m.make
                .as_deref()
                .unwrap_or_default()
                .to_lowercase()
                .contains(&entry.make.to_lowercase())
                && m.model
                    .as_deref()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&entry.model.to_lowercase()),
            || {
                format!(
                    "EXIF reports make={:?} model={:?}; manifest expects them to contain {:?} / {:?}",
                    m.make, m.model, entry.make, entry.model
                )
            },
        ),
    );

    c.run("wb_finite", {
        let wb = a.wb;
        let sane = wb
            .iter()
            .all(|v| v.is_finite() && *v > 0.0 && (0.05..=20.0).contains(v));
        // A mosaic sensor's coefficients are always reported relative to green; a green gain that
        // is not 1 means someone normalised (or failed to normalise) somewhere new.
        let green_normalised = a.cfa.is_empty() || (wb[1] - 1.0).abs() < 1e-3;
        ok_if(sane && green_normalised, || {
            format!("as-shot white balance {wb:?} (cfa {:?})", a.cfa)
        })
    });

    c.run(
        "develop_dims",
        ok_if(dims == (g.develop_w, g.develop_h), || {
            format!(
                "developed {}x{}, golden says {}x{}",
                a.develop_w, a.develop_h, g.develop_w, g.develop_h
            )
        }),
    );

    c.run(
        "catalog_dims",
        ok_if(m.thumb_disp == dims, || {
            format!(
                "thumbnail reports display dims {:?} but the develop is {dims:?}",
                m.thumb_disp
            )
        }),
    );

    let transposed = (dims.1, dims.0);
    c.run(
        "native_dims",
        ok_if(m.thumb_src == dims || m.thumb_src == transposed, || {
            format!(
                "thumbnail reports native dims {:?}, neither {dims:?} nor {transposed:?}",
                m.thumb_src
            )
        }),
    );

    c.run("orientation", {
        if a.orientation != g.orientation {
            Err(format!(
                "EXIF orientation {}, golden says {}",
                a.orientation, g.orientation
            ))
        } else if (5..=8).contains(&g.orientation) {
            // A quarter-turn orientation must have transposed the develop relative to the sensor.
            ok_if(m.thumb_src == transposed, || {
                format!(
                    "orientation {} is a quarter turn, so the develop {dims:?} should be the \
                     transpose of the native {:?}",
                    g.orientation, m.thumb_src
                )
            })
        } else {
            ok_if(m.thumb_src == dims, || {
                format!(
                    "orientation {} is not a quarter turn, so the develop {dims:?} should match \
                     the native {:?}",
                    g.orientation, m.thumb_src
                )
            })
        }
    });

    c.run(
        "cfa",
        ok_if(a.cfa == g.cfa, || {
            format!("CFA {:?}, golden says {:?}", a.cfa, g.cfa)
        }),
    );

    c.run(
        "levels",
        ok_if(a.black == g.black && a.white == g.white, || {
            format!(
                "levels black={:?} white={:?}, golden says black={:?} white={:?}",
                a.black, a.white, g.black, g.white
            )
        }),
    );

    c.run(
        "patch_mean",
        ok_if(close3(a.patch_mean_full, g.patch_mean_full), || {
            format!(
                "centre {PATCH_FULL}x{PATCH_FULL} mean {:?}, golden says {:?}",
                a.patch_mean_full, g.patch_mean_full
            )
        }),
    );

    c.run(
        "highlight_chroma",
        ok_if(
            (a.highlight_chroma - g.highlight_chroma).abs() <= 0.02,
            || {
                format!(
                    "highlight chroma {:.4}, golden says {:.4}",
                    a.highlight_chroma, g.highlight_chroma
                )
            },
        ),
    );

    c.run(
        "thumbnail_sane",
        ok_if(m.thumb_is_jpeg && m.thumb_len > 1000, || {
            format!(
                "thumbnail is {} bytes, JPEG SOI = {}",
                m.thumb_len, m.thumb_is_jpeg
            )
        }),
    );

    c.run(
        "fingerprint_some",
        ok_if(m.fingerprint_some, || {
            "capture fingerprint is None — the catalog cannot group this capture".to_string()
        }),
    );
}

/// Tier B: the expensive invariants — three more develops per file, so only for `deep` entries.
fn tier_b(c: &mut Checks, path: &Path) {
    let src = match source_from_path(path) {
        Ok(s) => s,
        Err(e) => {
            c.run("wb_identity", Err(format!("open: {e}")));
            return;
        }
    };

    c.run("wb_identity", {
        // Documented on `develop_linear_wb`: passing no override must be the byte-for-byte same
        // develop. Compared as bit patterns so a stray NaN cannot compare equal to itself.
        (|| -> Result<(), String> {
            let overridden = develop_linear_wb(&src, None).map_err(|e| e.to_string())?.0;
            let plain = develop_linear(&src).map_err(|e| e.to_string())?;
            if (overridden.width, overridden.height) != (plain.width, plain.height) {
                return Err("develop_linear_wb(None) changed the dimensions".to_string());
            }
            let differing = overridden
                .data
                .iter()
                .zip(plain.data.iter())
                .filter(|(a, b)| a.to_bits() != b.to_bits())
                .count();
            ok_if(differing == 0, || {
                format!("{differing} of {} samples differ", plain.data.len())
            })
        })()
    });

    c.run("preview_agrees", {
        (|| -> Result<(), String> {
            let full = develop_linear(&src).map_err(|e| e.to_string())?;
            let want = center_patch_mean(&full, PATCH_FULL);
            drop(full);
            let preview = core_raw::develop_linear_preview(&src).map_err(|e| e.to_string())?;
            let got = center_patch_mean(&preview, PATCH_PREVIEW);
            ok_if(
                want.iter()
                    .zip(got.iter())
                    .all(|(a, b)| (a - b).abs() <= 0.02),
                || format!("preview centre {got:?} vs full centre {want:?}"),
            )
        })()
    });

    c.run("clipped_neutral", {
        // Every mosaic sample forced to its own saturation level: after the black/white rescale
        // every channel is 1.0, so white balance and the colour matrix must leave it neutral.
        // A per-channel gain that survives the clip is exactly how magenta highlights happen.
        (|| -> Result<(), String> {
            let out =
                core_raw::develop_linear_denoised(&src, &SaturateAll).map_err(|e| e.to_string())?;
            let Some(d) = out.denoised else {
                return Err(
                    "no denoised output — `deep` should only be set on RGB Bayer sensors".into(),
                );
            };
            let mut worst = 0f64;
            let mut at = 0usize;
            for (i, px) in d.data.chunks_exact(3).enumerate() {
                let ch = chroma(px);
                if ch > worst {
                    worst = ch;
                    at = i;
                }
            }
            ok_if(worst < 0.02, || {
                let px = &d.data[at * 3..at * 3 + 3];
                format!("worst chroma {worst:.4} at pixel {at} ({px:?})")
            })
        })()
    });
}

fn check_file(
    entry: &FileEntry,
    path: &Path,
    golden: Option<&Golden>,
    quick: bool,
    out: &mut Vec<String>,
) {
    let mut c = Checks::new(entry, out);

    match entry.support.as_str() {
        "unsupported" => {
            c.run("unsupported_typed", unsupported_outcome(path));
            c.finish();
            return;
        }
        "panics" => {
            c.run("panics_contained", panics_outcome(path));
            c.finish();
            return;
        }
        "ok" => {}
        other => {
            c.failures
                .push(format!("{}: unknown support value {other:?}", entry.id));
            c.finish();
            return;
        }
    }

    let Some(g) = golden else {
        c.failures.push(format!(
            "{}: no golden in expected.toml — run the corpus_probe recorder",
            entry.id
        ));
        c.finish();
        return;
    };

    match measure(path, false) {
        Ok(m) => tier_a(&mut c, entry, g, &m),
        Err(e) => c
            .failures
            .push(format!("{}: develop failed: {e}", entry.id)),
    }

    if entry.deep && !quick {
        tier_b(&mut c, path);
    }
    c.finish();
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn corpus_matches_goldens() {
    let manifest = load_manifest().expect("tests/corpus/manifest.toml");
    let dir = corpus_dir();
    let require = std::env::var("DARKROOM_REQUIRE_CORPUS").as_deref() == Ok("1");

    if !dir.is_dir() {
        assert!(
            !require,
            "DARKROOM_REQUIRE_CORPUS=1 but no corpus at {} — run scripts/fetch_raw_corpus.sh",
            dir.display()
        );
        eprintln!(
            "no RAW corpus at {} — skipping (scripts/fetch_raw_corpus.sh --tier 1)",
            dir.display()
        );
        return;
    }

    let tier: u8 = std::env::var("DARKROOM_CORPUS_TIER")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let quick = std::env::var("DARKROOM_CORPUS_QUICK").as_deref() == Ok("1");
    let expected = load_expected().expect("tests/corpus/expected.toml");

    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;
    for entry in manifest.files.iter().filter(|f| f.tier <= tier) {
        let path = dir.join(&entry.path);
        if !path.exists() {
            if require {
                failures.push(format!(
                    "{}: tier {} file missing from {} — the fetch is incomplete",
                    entry.id,
                    entry.tier,
                    dir.display()
                ));
            } else {
                eprintln!("skip {} (not fetched)", entry.id);
            }
            continue;
        }
        eprintln!("check {} ({})", entry.id, entry.path);
        check_file(entry, &path, expected.get(&entry.id), quick, &mut failures);
        checked += 1;
    }

    eprintln!(
        "corpus: checked {checked} file(s) at tier {tier}{}",
        if quick { " (quick)" } else { "" }
    );
    assert!(
        failures.is_empty(),
        "{} corpus divergence(s):\n  {}\n\nRe-record with:\n  \
         cargo run --release -p core-raw --example corpus_probe -- --record > tests/corpus/expected.toml",
        failures.len(),
        failures.join("\n  ")
    );
}

/// Corpus-free: the manifest is machine-parsed by `scripts/fetch_raw_corpus.sh` (awk) *and* by the
/// test above (serde), so its shape has to stay in the intersection of both — one `key = value` per
/// line — and the whole set has to stay inside the download budget.
#[test]
fn manifest_shape_is_parsable() {
    let manifest = load_manifest().expect("tests/corpus/manifest.toml");
    assert!(
        !manifest.files.is_empty(),
        "manifest lists no [[file]] entries"
    );

    let total: u64 = manifest.files.iter().map(|f| f.bytes).sum();
    assert!(
        total <= BYTE_BUDGET,
        "corpus is {total} bytes, over the {BYTE_BUDGET} budget — drop a sample before adding one"
    );

    for f in &manifest.files {
        assert!(
            f.sha256.len() == 64 && f.sha256.chars().all(|c| c.is_ascii_hexdigit()),
            "{}: sha256 {:?} is not 64 hex digits",
            f.id,
            f.sha256
        );
        assert!(f.bytes > 0, "{}: bytes must be non-zero", f.id);
        assert!(
            f.tier == 1 || f.tier == 2,
            "{}: tier must be 1 or 2, got {}",
            f.id,
            f.tier
        );
        assert!(
            matches!(f.support.as_str(), "ok" | "unsupported" | "panics"),
            "{}: support must be ok|unsupported|panics, got {:?}",
            f.id,
            f.support
        );
        assert!(
            manifest.files.iter().filter(|o| o.id == f.id).count() == 1,
            "{}: duplicate id",
            f.id
        );
        assert!(
            manifest.files.iter().filter(|o| o.path == f.path).count() == 1,
            "{}: duplicate path {:?}",
            f.id,
            f.path
        );
    }

    // The awk parser in the fetch script reads `key = value` line by line: a value continued onto
    // a second line (a multi-line array, a multi-line string) would silently truncate. Checked as
    // plain string work rather than with a regex crate — this is the only pattern that matters.
    let text = std::fs::read_to_string(probe::manifest_path()).expect("read manifest");
    let mut in_file = false;
    let mut seen: Vec<&str> = Vec::new();
    let mut blocks = 0usize;
    let close_block = |seen: &mut Vec<&str>, blocks: &mut usize| {
        if !seen.is_empty() {
            *blocks += 1;
            for key in FILE_KEYS {
                assert_eq!(
                    seen.iter().filter(|k| *k == key).count(),
                    1,
                    "every [[file]] block needs exactly one `{key} = ...` line (block {}: {seen:?})",
                    *blocks
                );
            }
            seen.clear();
        }
    };
    for (n, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if trimmed == "[[file]]" {
            close_block(&mut seen, &mut blocks);
            in_file = true;
            continue;
        }
        if !in_file {
            continue;
        }
        let key = trimmed.split('=').next().map(str::trim).unwrap_or_default();
        assert!(
            FILE_KEYS.contains(&key),
            "line {}: unexpected key {key:?} inside a [[file]] block",
            n + 1
        );
        assert!(
            trimmed.matches('"').count() % 2 == 0
                && trimmed.matches('[').count() == trimmed.matches(']').count(),
            "line {}: `{key}` value is not closed on its own line — the awk parser in \
             scripts/fetch_raw_corpus.sh reads one key per line",
            n + 1
        );
        seen.push(key);
    }
    close_block(&mut seen, &mut blocks);
    assert_eq!(
        blocks,
        manifest.files.len(),
        "text scan found {blocks} [[file]] blocks, serde parsed {}",
        manifest.files.len()
    );
}
