//! The `decode_failure` bookkeeping: upsert semantics, and the two rules that decide whether a
//! recorded verdict still stands (same decoder build, unchanged file). No RAW fixture needed —
//! these are catalog rules, not decode behaviour.

use core_db::rusqlite::params;
use core_db::Db;
use core_library::{
    clear_decode_failure, decode_failure_counts, forget_decode_failures, list_decode_failures,
    record_decode_failure, skippable_failures, LibError,
};
use std::path::{Path, PathBuf};

/// A `LibError` carrying a permanently-unsupported RAW, as rawler reports an unknown body.
fn unsupported_err() -> LibError {
    LibError::Raw(core_raw::RawError::Unsupported {
        make: "Nikon".into(),
        model: "Z 9".into(),
        mode: String::new(),
        detail: "NEF compression HighEfficency is not supported".into(),
    })
}

/// A real file on disk (so `skippable_failures` can stat it), removed when the test ends. Mirrors
/// the `std::env::temp_dir()` style of the `core-db` schema-guard test — no extra dev-dependency.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str, bytes: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!("darkroom_df_{}_{name}", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn key(&self) -> String {
        self.0.display().to_string()
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn size_and_mtime(path: &Path) -> (i64, i64) {
    let md = std::fs::metadata(path).unwrap();
    let mtime = md
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    (md.len() as i64, mtime)
}

#[test]
fn recording_twice_bumps_attempts_and_keeps_first_seen() {
    let db = Db::open_in_memory().unwrap();
    let path = Path::new("/cards/DCIM/DSC_0001.NEF");

    assert!(
        record_decode_failure(&db.conn, None, path, &unsupported_err(), 100).unwrap(),
        "an unsupported body must report itself as unsupported"
    );
    record_decode_failure(&db.conn, None, path, &unsupported_err(), 200).unwrap();

    let rows = list_decode_failures(&db.conn, 50).unwrap();
    assert_eq!(rows.len(), 1, "one path = one row");
    let row = &rows[0];
    assert_eq!(row.attempts, 2);
    assert_eq!(row.first_seen, 100, "first_seen must survive a re-attempt");
    assert_eq!(row.last_seen, 200);
    assert_eq!(row.kind, "unsupported");
    assert_eq!(row.model.as_deref(), Some("Z 9"));
    assert_eq!(row.filename, "DSC_0001.NEF");
    assert!(
        row.detail.contains("High Efficiency"),
        "detail should carry the actionable wording, got {:?}",
        row.detail
    );

    let counts = decode_failure_counts(&db.conn).unwrap();
    assert_eq!(
        (counts.unsupported, counts.corrupt, counts.other),
        (1, 0, 0)
    );
    assert_eq!(counts.total(), 1);
}

#[test]
fn a_corrupt_decode_is_not_counted_as_unsupported() {
    let db = Db::open_in_memory().unwrap();
    let err = LibError::Raw(core_raw::RawError::Decode("truncated strip".into()));
    assert!(
        !record_decode_failure(&db.conn, None, Path::new("/cards/a.cr3"), &err, 1).unwrap(),
        "damaged bytes are worth retrying — never reported as unsupported"
    );
    let counts = decode_failure_counts(&db.conn).unwrap();
    assert_eq!((counts.unsupported, counts.corrupt), (0, 1));
}

#[test]
fn a_recorded_failure_is_skippable_until_the_file_changes() {
    let db = Db::open_in_memory().unwrap();
    let file = TempFile::new("changing.NEF", b"not really a raw file");
    record_decode_failure(&db.conn, None, file.path(), &unsupported_err(), 100).unwrap();

    assert!(
        skippable_failures(&db.conn).unwrap().contains(&file.key()),
        "an unchanged file with a current-decoder verdict must not be re-decoded"
    );

    // Rewrite it: a different size (and a fresh mtime) — the verdict no longer applies.
    std::fs::write(file.path(), b"a different, longer set of bytes entirely").unwrap();
    assert!(
        !skippable_failures(&db.conn).unwrap().contains(&file.key()),
        "a changed file must be retried"
    );
}

#[test]
fn a_verdict_from_another_decoder_build_is_never_skippable() {
    let db = Db::open_in_memory().unwrap();
    let file = TempFile::new("stale.NEF", b"bytes");
    let (size, mtime) = size_and_mtime(file.path());

    // Stale row: byte-for-byte current, but written by a decoder this build has moved past.
    db.conn
        .execute(
            "INSERT INTO decode_failure(path, filename, kind, detail, file_size, mtime,
                                        decoder_version, first_seen, last_seen, attempts)
             VALUES (?1, ?2, 'unsupported', 'Unknown camera', ?3, ?4, 'rawler-0.0.0-old', 1, 1, 1)",
            params![file.key(), "stale.NEF", size, mtime],
        )
        .unwrap();

    assert!(
        skippable_failures(&db.conn).unwrap().is_empty(),
        "a decoder upgrade must retry every file the old build refused"
    );
}

#[test]
fn a_missing_file_is_not_skippable() {
    let db = Db::open_in_memory().unwrap();
    let file = TempFile::new("gone.NEF", b"bytes");
    record_decode_failure(&db.conn, None, file.path(), &unsupported_err(), 1).unwrap();
    std::fs::remove_file(file.path()).unwrap();
    assert!(
        skippable_failures(&db.conn).unwrap().is_empty(),
        "a file that is gone gets a fresh attempt if it ever comes back"
    );
}

#[test]
fn clear_and_forget_remove_rows() {
    let db = Db::open_in_memory().unwrap();
    let a = Path::new("/cards/a.nef");
    let b = Path::new("/cards/b.nef");
    record_decode_failure(&db.conn, None, a, &unsupported_err(), 1).unwrap();
    record_decode_failure(&db.conn, None, b, &unsupported_err(), 1).unwrap();

    clear_decode_failure(&db.conn, &a.display().to_string()).unwrap();
    assert_eq!(list_decode_failures(&db.conn, 50).unwrap().len(), 1);

    // "Try again" over one real path and one that was already cleared: only real rows count.
    let removed = forget_decode_failures(
        &db.conn,
        &[b.display().to_string(), a.display().to_string()],
    )
    .unwrap();
    assert_eq!(removed, 1);
    assert!(list_decode_failures(&db.conn, 50).unwrap().is_empty());
}
