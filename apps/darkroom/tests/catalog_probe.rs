//! `--catalog-probe` honesty: the probe must only print `status: "ok"` (exit 0) when the
//! catalog genuinely opened, the selection genuinely landed, and the runtime genuinely shut
//! down. Every scenario runs the real binary against a real temp SQLite catalog — never the
//! user's catalog under ~/Library.

use core_db::rusqlite::params;
use core_db::{Db, LATEST_SCHEMA_VERSION};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

struct Scratch(PathBuf);
impl Scratch {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "dr_catalog_probe_{tag}_{}_{}",
            std::process::id(),
            n
        ));
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

/// Create (and migrate) a catalog the way the app does, then close it.
fn create_catalog(path: &Path) -> Vec<PathBuf> {
    let db = Db::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let dir = path.parent().unwrap().to_path_buf();
    drop(db);
    // WAL sidecars left behind by the creating connection would confound the corrupt test.
    [dir.join("catalog.db-wal"), dir.join("catalog.db-shm")]
        .into_iter()
        .filter(|p| p.exists())
        .collect()
}

/// `images.content_hash` holds the 32 raw BLAKE3 bytes; readers hex-encode them.
fn insert_image(
    conn: &core_db::rusqlite::Connection,
    hash: &[u8; 32],
    path: &str,
    filename: &str,
    capture: i64,
) -> i64 {
    conn.execute(
        "INSERT INTO images(content_hash, file_size, path, original_filename, status,
                            capture_date, imported_at)
         VALUES (?1, 64, ?2, ?3, 'present', ?4, 0)",
        params![hash.to_vec(), path, filename, capture],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn run_probe(args: &[&str]) -> (i32, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_darkroom-native"))
        .arg("--catalog-probe")
        .args(args)
        .output()
        .expect("run darkroom-native --catalog-probe");
    let stdout = String::from_utf8(output.stdout).expect("probe stdout is UTF-8");
    let line = stdout
        .lines()
        .find(|line| line.starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON on stdout: {stdout:?}"));
    let value: Value =
        serde_json::from_str(line).unwrap_or_else(|e| panic!("bad JSON {line:?}: {e}"));
    (output.status.code().expect("exit code"), value)
}

#[test]
fn a_valid_empty_catalog_is_ok_and_marked_empty() {
    let scratch = Scratch::new("empty");
    create_catalog(&scratch.catalog());
    let (code, json) = run_probe(&[scratch.catalog().to_str().unwrap()]);
    assert_eq!(code, 0, "a valid empty catalog succeeds: {json}");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["count"], 0);
    assert_eq!(
        json["empty"], true,
        "empty must be distinct from failure: {json}"
    );
    assert_eq!(json["selected"], Value::Null);
    assert_eq!(json["rows"].as_array().unwrap().len(), 0);
}

#[test]
fn a_populated_catalog_selects_the_first_row_with_matching_metadata() {
    let scratch = Scratch::new("populated");
    let path = scratch.catalog();
    let (first_id, second_id, first_hash_hex);
    {
        let db = Db::open(&path).unwrap();
        first_id = insert_image(&db.conn, &[0xab; 32], "/lib/first.cr3", "first.cr3", 2000);
        second_id = insert_image(&db.conn, &[0xcd; 32], "/lib/second.cr3", "second.cr3", 1000);
        first_hash_hex = "ab".repeat(32);
        drop(db);
    }
    let (code, json) = run_probe(&[path.to_str().unwrap()]);
    assert_eq!(code, 0, "populated probe must succeed: {json}");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["count"], 2);
    assert_eq!(json["empty"], false);
    let rows = json["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    // Default sort is capture_date DESC ⇒ the row captured at 2000 leads.
    assert_eq!(rows[0]["id"], first_id);
    assert_eq!(rows[1]["id"], second_id);
    let selected = &json["selected"];
    assert_eq!(
        selected["id"], first_id,
        "selected must be the first row: {selected}"
    );
    assert_eq!(selected["filename"], "first.cr3");
    assert_eq!(selected["contentHash"], first_hash_hex);
}

#[test]
fn no_select_reports_the_rows_but_selects_nothing() {
    let scratch = Scratch::new("noselect");
    let path = scratch.catalog();
    {
        let db = Db::open(&path).unwrap();
        insert_image(&db.conn, &[0xab; 32], "/lib/first.cr3", "first.cr3", 2000);
        insert_image(&db.conn, &[0xcd; 32], "/lib/second.cr3", "second.cr3", 1000);
        drop(db);
    }
    let (code, json) = run_probe(&[path.to_str().unwrap(), "--no-select"]);
    assert_eq!(code, 0, "--no-select must succeed: {json}");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["count"], 2);
    assert_eq!(json["selected"], Value::Null);
}

#[test]
fn a_catalog_under_a_file_fails_with_an_error_json() {
    let scratch = Scratch::new("underfile");
    let blocker = scratch.0.join("blocker");
    fs::write(&blocker, b"not a directory").unwrap();
    let path = blocker.join("subdir").join("catalog.db");
    let (code, json) = run_probe(&[path.to_str().unwrap()]);
    assert_ne!(code, 0, "an unopenable path must not exit 0: {json}");
    assert_eq!(json["status"], "error");
    assert_eq!(code, 2, "open failures exit 2: {json}");
}

#[test]
fn a_corrupt_catalog_exits_2_with_kind_corrupt() {
    let scratch = Scratch::new("corrupt");
    let path = scratch.catalog();
    for sidecar in create_catalog(&path) {
        fs::remove_file(sidecar).unwrap();
    }
    fs::write(
        &path,
        b"greetings, this is definitely not a sqlite database file",
    )
    .unwrap();
    let (code, json) = run_probe(&[path.to_str().unwrap()]);
    assert_eq!(code, 2, "corrupt catalog must exit 2: {json}");
    assert_eq!(json["status"], "error");
    assert_eq!(json["kind"], "corrupt");
}

#[test]
fn a_newer_schema_exits_2_with_kind_schema_too_new_and_never_touches_the_file() {
    let scratch = Scratch::new("toonew");
    let path = scratch.catalog();
    create_catalog(&path);
    {
        let db = Db::open(&path).unwrap();
        // Simulate a catalog written by a newer build.
        db.conn
            .execute_batch(&format!(
                "PRAGMA user_version = {};",
                LATEST_SCHEMA_VERSION + 1
            ))
            .unwrap();
        drop(db);
    }
    let before_bytes = fs::read(&path).unwrap();
    let before_mtime = fs::metadata(&path).unwrap().modified().unwrap();
    let (code, json) = run_probe(&[path.to_str().unwrap()]);
    assert_eq!(code, 2, "a newer schema must exit 2: {json}");
    assert_eq!(json["status"], "error");
    assert_eq!(json["kind"], "schema_too_new");
    assert_eq!(
        fs::read(&path).unwrap(),
        before_bytes,
        "refusal must not write"
    );
    assert_eq!(
        fs::metadata(&path).unwrap().modified().unwrap(),
        before_mtime,
        "refusal must not touch the file's mtime"
    );
}
