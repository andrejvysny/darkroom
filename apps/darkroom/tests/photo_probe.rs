//! `--photo-probe` honesty: the native presentation slice is only as good as its byte-exact
//! GPU proof. Every scenario runs the real binary against a real temp SQLite catalog holding a
//! COPY of the committed CR3 fixture — never the user's catalog under ~/Library. Skips unless a
//! GPU adapter and the fixture exist; `DARKROOM_REQUIRE_FIXTURES=1` makes either a hard failure.

use core_db::Db;
use core_library::{add_root, insert_image, process_file, set_edit, ThumbCache};
use core_pipeline::{GpuContext, PipelineError};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

fn fixture() -> Option<PathBuf> {
    // apps/darkroom -> repo root is ../..; the committed CR3 lives under `library/2026`.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../library/2026")
        .canonicalize()
        .ok()?;
    let mut found = Vec::new();
    let mut stack = vec![dir];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).ok().into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("cr3"))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found.into_iter().next()
}

/// Skip (or fail under `DARKROOM_REQUIRE_FIXTURES`) unless the fixture exists AND a GPU adapter
/// is available — the probe's offscreen comparison is meaningless without both.
fn prerequisites() -> Option<PathBuf> {
    let required = std::env::var_os("DARKROOM_REQUIRE_FIXTURES").is_some();
    let Some(fixture) = fixture() else {
        assert!(
            !required,
            "CR3 fixture missing but DARKROOM_REQUIRE_FIXTURES is set"
        );
        eprintln!("CR3 fixture missing — skipping");
        return None;
    };
    match GpuContext::new() {
        Ok(_) => {}
        Err(PipelineError::NoAdapter)
            if !required && std::env::var_os("DARKROOM_REQUIRE_GPU").is_none() =>
        {
            eprintln!("no GPU adapter — skipping (set DARKROOM_REQUIRE_FIXTURES=1 to require one)");
            return None;
        }
        Err(error) => panic!("GPU initialization failed: {error}; fixtures required"),
    }
    Some(fixture)
}

struct Scratch(PathBuf);
impl Scratch {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("dr_photo_probe_{tag}_{}_{}", std::process::id(), n));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn catalog(&self) -> PathBuf {
        self.0.join("catalog.db")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Create a catalog containing a copy of the fixture, indexed like the library tests do.
/// Returns (catalog path, image id).
fn catalog_with_fixture(tag: &str) -> (Scratch, PathBuf, i64) {
    let fixture = prerequisites().expect("prerequisites checked by caller");
    let scratch = Scratch::new(tag);
    let library = scratch.0.join("library");
    fs::create_dir_all(&library).unwrap();
    let copied = library.join("_55A3947.CR3");
    fs::copy(&fixture, &copied).unwrap();
    let catalog = scratch.catalog();
    let id;
    {
        let db = Db::open(&catalog).unwrap();
        let thumbs = ThumbCache::new(scratch.0.join("thumbs")).unwrap();
        let folder = add_root(&db.conn, &library).unwrap();
        let processed = process_file(&copied, &thumbs, 256).expect("process fixture");
        assert!(processed.width > 0 && processed.height > 0);
        id = insert_image(&db.conn, folder, 0, &processed)
            .unwrap()
            .expect("fixture indexed");
        drop(db);
    }
    (scratch, catalog, id)
}

fn run_probe(args: &[&str]) -> (i32, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_darkroom-native"))
        .arg("--photo-probe")
        .args(args)
        .output()
        .expect("run darkroom-native --photo-probe");
    let stdout = String::from_utf8(output.stdout).expect("probe stdout is UTF-8");
    let line = stdout
        .lines()
        .find(|line| line.starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON on stdout: {stdout:?}"));
    let value: Value =
        serde_json::from_str(line).unwrap_or_else(|e| panic!("bad JSON {line:?}: {e}"));
    (output.status.code().expect("exit code"), value)
}

fn store_edit(catalog: &Path, image_id: i64, params_json: &str, updated_at: i64) {
    let db = Db::open(catalog).unwrap();
    // PROCESS_VERSION 6 — the version this build writes (`CURRENT_PROCESS_VERSION`).
    set_edit(&db.conn, image_id, 6, params_json, updated_at).unwrap();
    drop(db);
}

#[test]
fn default_edit_is_byte_identical() {
    if prerequisites().is_none() {
        return;
    }
    let (_scratch, catalog, _id) = catalog_with_fixture("default");
    let (code, json) = run_probe(&[catalog.to_str().unwrap()]);
    assert_eq!(code, 0, "default render must be byte-exact: {json}");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["identical"], true);
    assert_eq!(
        json["egui_presented"], true,
        "the produced frame must survive egui registration + sampling: {json}"
    );
    let width = json["width"].as_u64().unwrap();
    let height = json["height"].as_u64().unwrap();
    assert!(width > 0, "framed width: {json}");
    assert!(height > 0, "framed height: {json}");
    // The fixture is 6960×4640 (3:2) with no crop edit stored, and the probe panel is a
    // square 1024×1024: the fitted frame must be 3:2, never panel-shaped. The probe itself
    // exits 6 on |aspect − expected| > 1/min(w,h); these assertions pin what "expected" is.
    let aspect = json["aspect"].as_f64().unwrap();
    let expected = json["expected_aspect"].as_f64().unwrap();
    assert!(
        (aspect - 1.5).abs() <= 1.0 / width.min(height) as f64,
        "a 3:2 fixture must present at ~1.5 aspect, got {aspect} ({width}×{height}): {json}"
    );
    assert!(
        (expected - 1.5).abs() < 1e-9,
        "expected aspect is the fixture's true 3:2 frame: {json}"
    );
    assert_ne!(
        width, height,
        "a 3:2 frame in a square panel is not square: {json}"
    );
}

#[test]
fn stored_crop_and_rot90_change_the_reported_aspect() {
    if prerequisites().is_none() {
        return;
    }
    let (_scratch, catalog, id) = catalog_with_fixture("aspect");
    let (_, before) = run_probe(&[catalog.to_str().unwrap()]);
    assert!(
        (before["aspect"].as_f64().unwrap() - 1.5).abs() < 0.01,
        "baseline: {before}"
    );

    // Square crop of the 3:2 fixture: half-extents sized so hw·W == hh·H (hw=1/3 of width
    // scale 0.5·2/3 => 4640×4640 output) -> fitted into the square panel is a square frame.
    store_edit(
        &catalog,
        id,
        r#"{"crop":{"cx":0.5,"cy":0.5,"hw":0.33333333,"hh":0.5}}"#,
        1_000,
    );
    let (code, square) = run_probe(&[catalog.to_str().unwrap()]);
    assert_eq!(
        code, 0,
        "square-crop render must succeed and be aspect-checked: {square}"
    );
    assert_eq!(square["identical"], true);
    let w = square["width"].as_u64().unwrap();
    let h = square["height"].as_u64().unwrap();
    assert_eq!(
        w, h,
        "a square crop fits the square panel exactly: {square}"
    );
    assert!(
        (square["aspect"].as_f64().unwrap() - 1.0).abs() <= 1.0 / w.min(h) as f64,
        "square crop must report ~1.0 aspect: {square}"
    );
    assert!(
        (square["expected_aspect"].as_f64().unwrap() - 1.0).abs() < 1e-6,
        "expected aspect follows the stored crop: {square}"
    );

    // rot90 on the full frame swaps the axes: 3:2 becomes 2:3, so the fitted frame is tall.
    store_edit(&catalog, id, r#"{"crop":{"rot90":1}}"#, 2_000);
    let (code, rotated) = run_probe(&[catalog.to_str().unwrap()]);
    assert_eq!(
        code, 0,
        "rot90 render must succeed and be aspect-checked: {rotated}"
    );
    assert_eq!(rotated["identical"], true);
    let w = rotated["width"].as_u64().unwrap();
    let h = rotated["height"].as_u64().unwrap();
    assert!(
        w < h,
        "a rot90'd 3:2 frame fits tall in the square panel: {rotated}"
    );
    assert!(
        (rotated["aspect"].as_f64().unwrap() - 2.0 / 3.0).abs() <= 1.0 / w.min(h) as f64,
        "rot90 must swap the reported aspect to ~2/3: {rotated}"
    );
    assert!(
        (rotated["expected_aspect"].as_f64().unwrap() - 2.0 / 3.0).abs() < 1e-6,
        "expected aspect swaps with rot90: {rotated}"
    );
}

#[test]
fn stored_edit_with_exposure_and_crop_is_byte_identical_and_changes_pixels() {
    if prerequisites().is_none() {
        return;
    }
    let (_scratch, catalog, id) = catalog_with_fixture("edit");
    let (_, before) = run_probe(&[catalog.to_str().unwrap()]);
    assert_eq!(before["identical"], true, "default baseline: {before}");

    store_edit(
        &catalog,
        id,
        r#"{"exposure":1.0,"crop":{"cx":0.5,"cy":0.45,"hw":0.4,"hh":0.3}}"#,
        1_000,
    );
    let (code, after) = run_probe(&[catalog.to_str().unwrap()]);
    assert_eq!(code, 0, "stored edit render must be byte-exact: {after}");
    assert_eq!(after["identical"], true);
    assert_ne!(
        before["mean_rgb"], after["mean_rgb"],
        "+1 EV must move the mean: {before} vs {after}"
    );
}

#[test]
fn unparseable_edit_exits_7_without_touching_the_catalog() {
    if prerequisites().is_none() {
        return;
    }
    let (_scratch, catalog, id) = catalog_with_fixture("corrupt");
    let broken = "greetings, definitely not develop params {{";
    store_edit(&catalog, id, broken, 1_000);
    let before_bytes = fs::read(&catalog).unwrap();
    let (code, json) = run_probe(&[catalog.to_str().unwrap()]);
    assert_eq!(code, 7, "a corrupt document must exit 7: {json}");
    assert_eq!(json["status"], "error");
    assert_eq!(json["kind"], "document");
    assert_eq!(
        json["error"].as_str().unwrap(),
        "document is Corrupt, not Ready",
        "the JSON must name the refused state"
    );
    assert_eq!(
        fs::read(&catalog).unwrap(),
        before_bytes,
        "refusal must not rewrite the catalog"
    );
    let db = Db::open(&catalog).unwrap();
    let stored = core_library::get_edit(&db.conn, id).unwrap();
    assert_eq!(
        stored.as_deref(),
        Some(broken),
        "the preserved edit must survive the probe untouched"
    );
}

#[test]
fn denoise_edit_exits_7_naming_the_feature() {
    if prerequisites().is_none() {
        return;
    }
    let (_scratch, catalog, id) = catalog_with_fixture("denoise");
    store_edit(
        &catalog,
        id,
        r#"{"denoise":{"enabled":true,"amount":50}}"#,
        1_000,
    );
    let (code, json) = run_probe(&[catalog.to_str().unwrap()]);
    assert_eq!(code, 7, "an AI-denoise edit must exit 7: {json}");
    assert_eq!(json["kind"], "document");
    assert!(
        json["error"].as_str().unwrap().contains("AI denoise"),
        "the error must name the unsupported feature: {json}"
    );
}

#[test]
fn explicit_image_id_selects_that_image() {
    if prerequisites().is_none() {
        return;
    }
    let (_scratch, catalog, id) = catalog_with_fixture("byid");
    let (code, json) = run_probe(&[catalog.to_str().unwrap(), "--image", &id.to_string()]);
    assert_eq!(code, 0, "selecting by id must succeed: {json}");
    assert_eq!(json["image_id"], id);
    assert_eq!(json["identical"], true);
}
