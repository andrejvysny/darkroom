//! Decode probe **and golden recorder** for the multi-maker RAW corpus (`tests/corpus/`).
//!
//! ```text
//! cargo run --release -p core-raw --example corpus_probe                          # stats for the committed fixture
//! cargo run --release -p core-raw --example corpus_probe -- FILE...               # stats for arbitrary files
//! cargo run --release -p core-raw --example corpus_probe -- --record > tests/corpus/expected.toml
//! cargo run --release -p core-raw --example corpus_probe -- --diff                # golden vs actual table
//! ```
//!
//! This file is also the **shared implementation** behind `tests/corpus.rs`, which pulls it in with
//! `#[path = "../examples/corpus_probe.rs"] mod probe;`. Everything the test needs — the manifest
//! and golden types, the stat helpers, the `MosaicDenoiser` probes and the one-develop
//! [`measure`] — lives here so the recorder and the checker can never drift apart. Tests must
//! never write into the repository, which is exactly why `--record` is an example and not a test.
#![allow(dead_code)] // the example build and the test build each use a subset of this file

use core_raw::{
    as_shot_wb, capture_fingerprint, develop_linear, develop_linear_denoised,
    develop_linear_preview, read_metadata, source_from_path, LinearImage, MosaicDenoiser,
    MosaicInfo, RawError,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

/// Centre patch edge (px) measured on the full-resolution develop.
pub const PATCH_FULL: u32 = 64;
/// Centre patch edge (px) measured on the half-resolution preview develop.
pub const PATCH_PREVIEW: u32 = 32;
/// Grid thumbnail edge — mirrors `core_library::THUMB_SIZE` (core-raw cannot depend on core-library).
pub const THUMB_EDGE: u32 = 512;
/// Fraction of the brightest pixels [`highlight_chroma`] averages over.
pub const HIGHLIGHT_FRACTION: f64 = 0.0005;

// ---------------------------------------------------------------------------------------------
// Manifest / goldens
// ---------------------------------------------------------------------------------------------

/// `tests/corpus/manifest.toml` — identity and acquisition only (it is the CI cache key).
#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub source_base: String,
    pub filelist: String,
    pub upstream_timestamp: i64,
    #[serde(default, rename = "file")]
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Deserialize)]
pub struct FileEntry {
    pub id: String,
    /// Exact upstream path (`<Make>/<Model>/<File>`); upstream casing is inconsistent, so this is
    /// never normalised.
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    /// Substring the decoded EXIF make must contain (case-insensitive).
    pub make: String,
    /// Substring the decoded EXIF model must contain (case-insensitive).
    pub model: String,
    pub tier: u8,
    #[serde(default)]
    pub tags: Vec<String>,
    /// `"ok"` | `"unsupported"` | `"panics"` — what this build is expected to do with the file.
    pub support: String,
    /// Run the expensive Tier-B checks over this file.
    #[serde(default)]
    pub deep: bool,
    /// Assertion names that are known-red today. An xfail that PASSES is itself a failure.
    #[serde(default)]
    pub xfail: Vec<String>,
}

impl FileEntry {
    pub fn is_ok(&self) -> bool {
        self.support == "ok"
    }
}

/// One recorded golden. Every field is machine-measured — nothing here is hand-written.
#[derive(Debug, Clone, Deserialize)]
pub struct Golden {
    pub develop_w: u32,
    pub develop_h: u32,
    /// EXIF orientation (1–8), or 0 when the file carries none.
    pub orientation: i64,
    /// CFA pattern as rawler names it (`"RGGB"`, …), or empty for sensors with no 2×2 mosaic
    /// (monochrome, sRAW, linear DNG) — those never reach the `MosaicDenoiser` seam.
    pub cfa: String,
    pub black: [f64; 4],
    pub white: [f64; 4],
    pub wb: [f64; 3],
    pub patch_mean_full: [f64; 3],
    pub patch_mean_preview: [f64; 3],
    pub highlight_chroma: f64,
}

pub type Goldens = BTreeMap<String, Golden>;

/// Repository root (this crate is `crates/core-raw`).
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Where `scripts/fetch_raw_corpus.sh` puts the corpus.
pub fn corpus_dir() -> PathBuf {
    match std::env::var_os("DARKROOM_RAW_CORPUS") {
        Some(v) => PathBuf::from(v),
        None => repo_root().join("target/raw-corpus"),
    }
}

pub fn manifest_path() -> PathBuf {
    repo_root().join("tests/corpus/manifest.toml")
}

pub fn expected_path() -> PathBuf {
    repo_root().join("tests/corpus/expected.toml")
}

pub fn load_manifest() -> Result<Manifest, String> {
    let p = manifest_path();
    let text = std::fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
    toml::from_str(&text).map_err(|e| format!("parse {}: {e}", p.display()))
}

/// Goldens keyed by manifest id. A missing file parses as "no goldens recorded yet".
pub fn load_expected() -> Result<Goldens, String> {
    let p = expected_path();
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Goldens::new()),
        Err(e) => return Err(format!("read {}: {e}", p.display())),
    };
    toml::from_str(&text).map_err(|e| format!("parse {}: {e}", p.display()))
}

// ---------------------------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------------------------

/// Per-pixel chroma `(max - min) / max`; 0 for a perfectly neutral pixel.
pub fn chroma(px: &[f32]) -> f64 {
    let mx = px[0].max(px[1]).max(px[2]);
    let mn = px[0].min(px[1]).min(px[2]);
    if mx <= 1e-6 {
        0.0
    } else {
        ((mx - mn) / mx) as f64
    }
}

/// Mean linear-ProPhoto RGB over the centre `n`×`n` patch — the cheapest stat that moves whenever
/// demosaic, white balance or the camera→ProPhoto matrix moves.
pub fn center_patch_mean(img: &LinearImage, n: u32) -> [f64; 3] {
    let n = n.min(img.width).min(img.height).max(1);
    let x0 = (img.width - n) / 2;
    let y0 = (img.height - n) / 2;
    let mut acc = [0f64; 3];
    for y in y0..y0 + n {
        for x in x0..x0 + n {
            let i = ((y * img.width + x) * 3) as usize;
            for (c, a) in acc.iter_mut().enumerate() {
                *a += img.data[i + c] as f64;
            }
        }
    }
    let cnt = f64::from(n * n);
    [acc[0] / cnt, acc[1] / cnt, acc[2] / cnt]
}

/// Mean chroma over the brightest [`HIGHLIGHT_FRACTION`] of pixels (by max channel). A clipped
/// highlight that develops with a colour cast — the classic sign of broken highlight handling or a
/// wrongly applied white balance — shows up here and nowhere else.
pub fn highlight_chroma(img: &LinearImage) -> f64 {
    let px: Vec<f32> = img
        .data
        .chunks_exact(3)
        .map(|p| p[0].max(p[1]).max(p[2]))
        .collect();
    if px.is_empty() {
        return 0.0;
    }
    let take = ((px.len() as f64 * HIGHLIGHT_FRACTION) as usize).max(1);
    let mut idx: Vec<u32> = (0..px.len() as u32).collect();
    // `select_nth` + a sort of the tail: the full sort of a 60 MP index vector costs seconds.
    let nth = px.len() - take;
    idx.select_nth_unstable_by(nth, |a, b| {
        px[*a as usize]
            .partial_cmp(&px[*b as usize])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut acc = 0f64;
    for &i in &idx[nth..] {
        let p = &img.data[i as usize * 3..i as usize * 3 + 3];
        acc += chroma(p);
    }
    acc / take as f64
}

// ---------------------------------------------------------------------------------------------
// Mosaic probes
// ---------------------------------------------------------------------------------------------

/// The CFA facts a [`MosaicInfo`] carries, copied out of the borrow.
#[derive(Debug, Clone, Default)]
pub struct Mosaic {
    pub cfa: String,
    pub black: [f64; 4],
    pub white: [f64; 4],
    pub width: usize,
    pub height: usize,
}

/// Records the mosaic description and then returns a DELIBERATELY short buffer.
///
/// `develop_linear_denoised` treats a wrong-length result as "denoise unavailable" and returns the
/// clean develop only — so this reads out the CFA pattern and the black/white levels for the price
/// of ONE develop, instead of the two a real (identity) denoiser would cost. The clean image it
/// returns is bit-identical to `develop_linear`.
#[derive(Default)]
pub struct MosaicProbe {
    seen: Mutex<Option<Mosaic>>,
}

impl MosaicProbe {
    pub fn taken(&self) -> Option<Mosaic> {
        self.seen.lock().expect("probe mutex").clone()
    }
}

/// rawler's colour indices are `0=R, 1=G, 2=B`; the pattern string is the readable form the
/// goldens store (and the only place a CFA phase flip would be visible).
fn cfa_string(info: &MosaicInfo) -> String {
    info.cfa_pattern
        .iter()
        .map(|c| match c {
            0 => 'R',
            1 => 'G',
            2 => 'B',
            _ => '?',
        })
        .collect()
}

impl MosaicDenoiser for MosaicProbe {
    fn denoise(&self, info: &MosaicInfo) -> Vec<u16> {
        *self.seen.lock().expect("probe mutex") = Some(Mosaic {
            cfa: cfa_string(info),
            black: info.black.map(f64::from),
            white: info.white.map(f64::from),
            width: info.width,
            height: info.height,
        });
        Vec::new()
    }
}

/// Replaces every mosaic sample with its per-CFA-position white level — a fully blown frame.
/// Whatever the sensor, the develop of a uniformly clipped mosaic must be neutral: black/white
/// rescale takes every channel to 1.0, and white balance plus the colour matrix have to preserve
/// that. Any per-channel gain applied after the clip shows up as chroma.
pub struct SaturateAll;

impl MosaicDenoiser for SaturateAll {
    fn denoise(&self, info: &MosaicInfo) -> Vec<u16> {
        let mut out = vec![0u16; info.width * info.height];
        for y in 0..info.height {
            for x in 0..info.width {
                let pos = (y % info.cfa_height) * info.cfa_width + (x % info.cfa_width);
                let white = info.white.get(pos.min(3)).copied().unwrap_or(0.0);
                out[y * info.width + x] = white.clamp(0.0, f32::from(u16::MAX)) as u16;
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------------------------

/// Everything one Tier-A pass observes about a file.
pub struct Measured {
    pub golden: Golden,
    pub make: Option<String>,
    pub model: Option<String>,
    /// NATIVE (pre-orientation) full-image dims reported by the thumbnailer.
    pub thumb_src: (u32, u32),
    /// ORIENTED (display) full-image dims — what the catalog stores.
    pub thumb_disp: (u32, u32),
    pub thumb_len: usize,
    pub thumb_is_jpeg: bool,
    pub fingerprint_some: bool,
    pub develop_secs: f64,
}

/// One develop + one mosaic-only decode + metadata + thumbnail.
///
/// `with_preview` adds the half-resolution develop that fills `patch_mean_preview`; the checker
/// leaves it off in Tier A (the preview has its own Tier-B assertion) and the recorder turns it on.
pub fn measure(path: &Path, with_preview: bool) -> Result<Measured, RawError> {
    let src = source_from_path(path)?;
    let meta = read_metadata(&src)?;
    let wb = as_shot_wb(&src)?;

    let probe = MosaicProbe::default();
    let t = Instant::now();
    let out = develop_linear_denoised(&src, &probe)?;
    let develop_secs = t.elapsed().as_secs_f64();
    let img = out.clean;
    let mosaic = probe.taken().unwrap_or_default();

    let patch_mean_preview = if with_preview {
        center_patch_mean(&develop_linear_preview(&src)?, PATCH_PREVIEW)
    } else {
        [0.0; 3]
    };

    let thumb = core_raw::thumbnail_jpeg(&src, THUMB_EDGE, 82)?;
    let fingerprint_some = capture_fingerprint(&meta, thumb.src_width, thumb.src_height).is_some();

    Ok(Measured {
        golden: Golden {
            develop_w: img.width,
            develop_h: img.height,
            orientation: meta.orientation.unwrap_or(0),
            cfa: mosaic.cfa,
            black: mosaic.black,
            white: mosaic.white,
            wb: [f64::from(wb[0]), f64::from(wb[1]), f64::from(wb[2])],
            patch_mean_full: center_patch_mean(&img, PATCH_FULL),
            patch_mean_preview,
            highlight_chroma: highlight_chroma(&img),
        },
        make: meta.camera_make,
        model: meta.camera_model,
        thumb_src: (thumb.src_width, thumb.src_height),
        thumb_disp: (thumb.disp_width, thumb.disp_height),
        thumb_len: thumb.jpeg.len(),
        thumb_is_jpeg: thumb.jpeg.starts_with(&[0xFF, 0xD8]),
        fingerprint_some,
        develop_secs,
    })
}

// ---------------------------------------------------------------------------------------------
// TOML rendering (record mode)
// ---------------------------------------------------------------------------------------------

/// TOML float literal. `{:?}` on f64 always round-trips and always keeps a decimal point (so a
/// whole number does not deserialize as an integer); the non-finite cases need TOML's own spelling.
fn toml_f64(v: f64) -> String {
    if v.is_nan() {
        "nan".to_string()
    } else if v.is_infinite() {
        if v > 0.0 { "inf" } else { "-inf" }.to_string()
    } else {
        format!("{v:?}")
    }
}

fn toml_f64_list(v: &[f64]) -> String {
    let items: Vec<String> = v.iter().copied().map(toml_f64).collect();
    format!("[{}]", items.join(", "))
}

pub fn render_golden(id: &str, g: &Golden) -> String {
    format!(
        "[{id}]\n\
         develop_w = {w}\n\
         develop_h = {h}\n\
         orientation = {o}\n\
         cfa = \"{cfa}\"\n\
         black = {black}\n\
         white = {white}\n\
         wb = {wb}\n\
         patch_mean_full = {pf}\n\
         patch_mean_preview = {pp}\n\
         highlight_chroma = {hc}\n",
        id = id,
        w = g.develop_w,
        h = g.develop_h,
        o = g.orientation,
        cfa = g.cfa,
        black = toml_f64_list(&g.black),
        white = toml_f64_list(&g.white),
        wb = toml_f64_list(&g.wb),
        pf = toml_f64_list(&g.patch_mean_full),
        pp = toml_f64_list(&g.patch_mean_preview),
        hc = toml_f64(g.highlight_chroma),
    )
}

const RECORD_HEADER: &str = "\
# MACHINE-RECORDED goldens for the RAW corpus — do not hand-edit.
#
#   scripts/fetch_raw_corpus.sh --tier 2
#   cargo run --release -p core-raw --example corpus_probe -- --record > tests/corpus/expected.toml
#
# One table per `tests/corpus/manifest.toml` id, for every entry with `support = \"ok\"` that is
# present in the corpus directory. FETCH BOTH TIERS BEFORE RE-RECORDING: `> expected.toml` truncates
# this file first, so an entry that is not on disk simply loses its golden (the recorder says so on
# stderr).
#
# `orientation = 0` means the file carries no EXIF orientation tag. An empty `cfa` (with all-zero
# levels) means the file never reaches the mosaic seam — monochrome, sRAW, or linear DNG.
# See README.md for the tolerances each field is compared with.
";

// ---------------------------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------------------------

/// `--tier N` restricts what is walked; without it, every tier is (the recorder then measures
/// whatever is actually on disk).
fn selected(manifest: &Manifest, tier: Option<u8>) -> Vec<&FileEntry> {
    let limit = tier.unwrap_or(u8::MAX);
    manifest.files.iter().filter(|f| f.tier <= limit).collect()
}

fn record(tier: Option<u8>) -> i32 {
    let manifest = match load_manifest() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let previous = load_expected().unwrap_or_default();
    let dir = corpus_dir();
    let mut out = String::from(RECORD_HEADER);
    let mut recorded = 0usize;
    let mut kept = 0usize;

    // ALL tiers by default: the recorder measures whatever is on disk. `--record` is normally used
    // as `--record > tests/corpus/expected.toml`, and that redirection truncates the file before
    // this process starts — so a golden that is not re-measured here is simply gone. Anything
    // missing is therefore reported loudly at the end rather than quietly dropped. (The
    // previous-golden fallback below only helps when the output goes somewhere else.)
    let mut missing: Vec<&str> = Vec::new();
    for entry in selected(&manifest, tier) {
        if !entry.is_ok() {
            continue;
        }
        let path = dir.join(&entry.path);
        if !path.exists() {
            if let Some(old) = previous.get(&entry.id) {
                out.push('\n');
                out.push_str(&render_golden(&entry.id, old));
                kept += 1;
                eprintln!("keep    {} (not fetched)", entry.id);
            } else {
                missing.push(&entry.id);
                eprintln!("skip    {} (not fetched, no previous golden)", entry.id);
            }
            continue;
        }
        match measure(&path, true) {
            Ok(m) => {
                out.push('\n');
                out.push_str(&render_golden(&entry.id, &m.golden));
                recorded += 1;
                eprintln!(
                    "record  {} ({}x{} in {:.2}s)",
                    entry.id, m.golden.develop_w, m.golden.develop_h, m.develop_secs
                );
            }
            Err(e) => eprintln!("FAILED  {}: {e}", entry.id),
        }
    }
    print!("{out}");
    eprintln!("recorded {recorded}, kept {kept}");
    if !missing.is_empty() {
        eprintln!(
            "\nWARNING: {} entr{} no golden in this recording because {} not fetched:\n  {}\n\
             Run `scripts/fetch_raw_corpus.sh --tier 2` and re-record, or those goldens are lost.",
            missing.len(),
            if missing.len() == 1 {
                "y has"
            } else {
                "ies have"
            },
            if missing.len() == 1 {
                "it was"
            } else {
                "they were"
            },
            missing.join("\n  ")
        );
    }
    0
}

fn diff(tier: Option<u8>) -> i32 {
    let manifest = match load_manifest() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let expected = load_expected().unwrap_or_default();
    let dir = corpus_dir();
    println!("{:<26} {:>12} {:>12}  field", "id", "golden", "actual");
    for entry in selected(&manifest, tier) {
        let path = dir.join(&entry.path);
        if !entry.is_ok() || !path.exists() {
            continue;
        }
        let Some(g) = expected.get(&entry.id) else {
            println!("{:<26} {:>12} {:>12}  (no golden)", entry.id, "-", "-");
            continue;
        };
        let m = match measure(&path, true) {
            Ok(m) => m,
            Err(e) => {
                println!("{:<26} {e}", entry.id);
                continue;
            }
        };
        let a = &m.golden;
        let row = |field: &str, want: String, got: String| {
            let mark = if want == got { " " } else { "*" };
            println!("{:<26} {want:>12} {got:>12} {mark} {field}", entry.id);
        };
        row(
            "develop",
            format!("{}x{}", g.develop_w, g.develop_h),
            format!("{}x{}", a.develop_w, a.develop_h),
        );
        row(
            "orientation",
            g.orientation.to_string(),
            a.orientation.to_string(),
        );
        row("cfa", g.cfa.clone(), a.cfa.clone());
        for c in 0..3 {
            row(
                &format!("patch_full[{c}]"),
                format!("{:.5}", g.patch_mean_full[c]),
                format!("{:.5}", a.patch_mean_full[c]),
            );
        }
        row(
            "highlight_chroma",
            format!("{:.5}", g.highlight_chroma),
            format!("{:.5}", a.highlight_chroma),
        );
    }
    0
}

fn probe_free(paths: &[PathBuf]) -> i32 {
    for path in paths {
        println!("== {}", path.display());
        match measure(path, true) {
            Ok(m) => {
                let g = &m.golden;
                println!(
                    "  meta: {:?} {:?} orient={} wb={:?}",
                    m.make, m.model, g.orientation, g.wb
                );
                println!(
                    "  develop: {}x{} in {:.2}s cfa={:?} black={:?} white={:?}",
                    g.develop_w, g.develop_h, m.develop_secs, g.cfa, g.black, g.white
                );
                println!(
                    "  patch64={:?} patch32={:?} hl_chroma={:.4}",
                    g.patch_mean_full, g.patch_mean_preview, g.highlight_chroma
                );
                println!(
                    "  thumb: {} bytes, native {:?}, display {:?}, fingerprint={}",
                    m.thumb_len, m.thumb_src, m.thumb_disp, m.fingerprint_some
                );
            }
            Err(e) => println!("  failed: {e}"),
        }
    }
    0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut tier: Option<u8> = None;
    let mut mode = "probe";
    let mut files: Vec<PathBuf> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--record" => mode = "record",
            "--diff" => mode = "diff",
            "--tier" => {
                i += 1;
                tier = args.get(i).and_then(|v| v.parse().ok());
            }
            other => files.push(PathBuf::from(other)),
        }
        i += 1;
    }

    let code = match mode {
        "record" => record(tier),
        "diff" => diff(tier),
        _ => {
            if files.is_empty() {
                files.push(repo_root().join("library/2026/2026-06-06/_55A3947.CR3"));
            }
            probe_free(&files)
        }
    };
    std::process::exit(code);
}

/// A file the manifest calls `unsupported` must fail with a TYPED refusal and must not panic —
/// re-exported here so `tests/corpus.rs` and `--diff` classify it the same way.
pub fn unsupported_outcome(path: &Path) -> Result<(), String> {
    let src = source_from_path(path).map_err(|e| format!("open: {e}"))?;
    match develop_linear(&src) {
        Ok(img) => Err(format!(
            "expected an Unsupported error, but it developed {}x{}",
            img.width, img.height
        )),
        Err(RawError::Unsupported { .. }) => Ok(()),
        Err(other) => Err(format!(
            "expected RawError::Unsupported, got {:?} ({other})",
            other.kind()
        )),
    }
}

/// A file the manifest calls `panics` must not decode successfully, and must not take the process
/// with it. `core-raw` already contains decoder panics ([`core_raw::panic`]) and reports them as
/// `RawError::DecoderPanic`, so BOTH outcomes are accepted: a panic that escapes into this
/// `catch_unwind`, or the typed error the containment turns it into. What is refused is a clean
/// develop — that would mean the manifest is stale and the entry should be `support = "ok"`.
pub fn panics_outcome(path: &Path) -> Result<(), String> {
    let taken = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = std::panic::catch_unwind(|| {
        let src = source_from_path(path)?;
        develop_linear(&src)
    });
    std::panic::set_hook(taken);

    match outcome {
        Err(_) => Ok(()),
        Ok(Err(RawError::DecoderPanic(_))) => Ok(()),
        Ok(Ok(img)) => Err(format!(
            "expected a decoder panic, but it developed {}x{}",
            img.width, img.height
        )),
        Ok(Err(other)) => Err(format!(
            "expected a decoder panic, got {:?} ({other})",
            other.kind()
        )),
    }
}
