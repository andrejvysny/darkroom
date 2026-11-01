//! Publication fence for merge inputs: HDR/panorama provenance rows must attach to the exact
//! files the job read, not to a rowid that SQLite handed out again after a cull/dedup delete.
//!
//! A merge reads every source field it uses (path, status, format, hash, folder) ONCE, in
//! [`snapshot_sources`], and the publication tokens certify exactly those fields. No other read of
//! the source rows may feed the merge, or the tokens could vouch for a different file.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use core_db::Db;
use core_model::identity::ImageId;
use core_runtime::publication::{
    PublicationGate, PublicationToken, SourceKey, SourceMutationGuard,
};

const STALE_CAPTURE: &str = "sources changed while preparing the merge; try again";
const STALE_PUBLISH: &str = "a source photo changed during the merge; nothing was published";

/// One source row as read in the snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRecord {
    pub id: i64,
    pub path: PathBuf,
    pub status: String,
    /// `IFNULL(format,'raw')`.
    pub format: String,
    pub content_hash_hex: String,
    /// `None` when the row has no folder (legacy rows).
    pub folder_id: Option<i64>,
}

/// Source rows and the tokens certifying exactly those rows (same order as the requested ids).
#[derive(Debug)]
pub struct ProcessingSnapshot {
    sources: Vec<SourceRecord>,
    tokens: Vec<PublicationToken>,
}

impl ProcessingSnapshot {
    pub fn sources(&self) -> &[SourceRecord] {
        &self.sources
    }

    pub fn tokens(&self) -> &[PublicationToken] {
        &self.tokens
    }

    /// Every source must be on disk; a missing one needs a relink first.
    pub fn require_present(&self) -> Result<(), String> {
        match self.sources.iter().find(|s| s.status != "present") {
            Some(s) => Err(format!(
                "{}: file is missing — relink it first",
                s.path.display()
            )),
            None => Ok(()),
        }
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn read_record(db: &Db, id: i64) -> Result<SourceRecord, String> {
    let (path, status, format, hash, folder_id): (String, String, String, Vec<u8>, Option<i64>) =
        db.conn
            .query_row(
                "SELECT path, status, IFNULL(format,'raw'), content_hash, folder_id
                 FROM images WHERE id = ?1",
                core_db::rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .map_err(|_| format!("image {id} not found"))?;
    Ok(SourceRecord {
        id,
        path: PathBuf::from(path),
        status,
        format,
        content_hash_hex: hex_bytes(&hash),
        folder_id,
    })
}

/// Reads all source rows under one catalog lock and captures tokens for exactly those rows.
pub fn snapshot_sources(
    publications: &PublicationGate,
    db: &Mutex<Db>,
    ids: &[i64],
) -> Result<ProcessingSnapshot, String> {
    snapshot_sources_with(publications, db, ids, || ())
}

/// [`snapshot_sources`] with a hook between the row read and the capture (test seam).
fn snapshot_sources_with(
    publications: &PublicationGate,
    db: &Mutex<Db>,
    ids: &[i64],
    after_read: impl FnOnce(),
) -> Result<ProcessingSnapshot, String> {
    // Sequence BEFORE the rows: any source mutation after this point fails the capture instead
    // of vouching for a replaced file or a deleted/reused rowid.
    let sequence = publications.sequence();
    let sources = {
        let db = db.lock().map_err(|e| e.to_string())?;
        ids.iter()
            .map(|&id| read_record(&db, id))
            .collect::<Result<Vec<_>, String>>()?
    };
    after_read();
    let tokens = sources
        .iter()
        .map(|s| {
            let key = SourceKey {
                hash: s.content_hash_hex.clone(),
                path: s.path.to_string_lossy().into_owned(),
            };
            publications
                .capture_at(ImageId(s.id), key, sequence)
                .map_err(|_| STALE_CAPTURE.to_string())
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(ProcessingSnapshot { sources, tokens })
}

/// Validates `tokens` under the publication gate and runs `commit` while holding it (gate before
/// catalog lock). On a stale token or a commit error, removes `owned_output` — a file this job
/// created — so nothing is left orphaned and no catalog row is written.
pub fn publish_owned<T>(
    publications: &PublicationGate,
    tokens: &[PublicationToken],
    owned_output: &Path,
    commit: impl FnOnce(&mut SourceMutationGuard<'_>) -> Result<T, String>,
) -> Result<T, String> {
    publications
        .tokens_guard(tokens)
        .map_err(|_| STALE_PUBLISH.to_string())
        .and_then(|mut guard| commit(&mut guard))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(owned_output);
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Mutex<Db> {
        let db = Db::open_in_memory().unwrap();
        db.conn
            .execute(
                "INSERT INTO folders(path, added_at) VALUES ('/lib', 0)",
                core_db::rusqlite::params![],
            )
            .unwrap();
        Mutex::new(db)
    }

    fn insert(db: &Mutex<Db>, id: i64, path: &str, hash: u8, status: &str) {
        db.lock()
            .unwrap()
            .conn
            .execute(
                "INSERT INTO images(id, content_hash, file_size, path, folder_id,
                    original_filename, status, width, height, exif, imported_at)
                 VALUES (?1, ?2, 1, ?3, 1, 'x', ?4, 1, 1, x'7b7d', 0)",
                core_db::rusqlite::params![id, vec![hash; 32], path, status],
            )
            .unwrap();
    }

    fn key(hash: u8, path: &str) -> SourceKey {
        SourceKey {
            hash: hex_bytes(&[hash; 32]),
            path: path.into(),
        }
    }

    fn setup() -> (PublicationGate, Mutex<Db>) {
        let gate = PublicationGate::default();
        let db = test_db();
        insert(&db, 1, "/lib/a.cr3", 0xaa, "present");
        insert(&db, 2, "/lib/b.cr3", 0xbb, "present");
        (gate, db)
    }

    /// Replace source 2 with different bytes at a new path, in catalog and gate.
    fn replace_2(gate: &PublicationGate, db: &Mutex<Db>) {
        let mut guard = gate.source_guard();
        db.lock()
            .unwrap()
            .conn
            .execute(
                "UPDATE images SET path='/lib/b2.cr3', content_hash=?1 WHERE id=2",
                core_db::rusqlite::params![vec![0xccu8; 32]],
            )
            .unwrap();
        guard.replace_source(ImageId(2), key(0xcc, "/lib/b2.cr3"));
    }

    fn reset(gate: &PublicationGate, _db: &Mutex<Db>) {
        gate.source_guard().invalidate_all();
    }

    fn reuse_id(gate: &PublicationGate, db: &Mutex<Db>) {
        gate.source_guard().retire(ImageId(2));
        db.lock()
            .unwrap()
            .conn
            .execute("DELETE FROM images WHERE id=2", [])
            .unwrap();
        insert(db, 2, "/lib/other.cr3", 0xdd, "present");
        gate.source_guard()
            .replace_source(ImageId(2), key(0xdd, "/lib/other.cr3"));
    }

    type Mutation = fn(&PublicationGate, &Mutex<Db>);
    const MUTATIONS: [(&str, Mutation); 3] = [
        ("replace", replace_2),
        ("reset", reset),
        ("reuse", reuse_id),
    ];

    #[test]
    fn happy_path_fields_match_rows_and_tokens_validate() {
        let (gate, db) = setup();
        let snap = snapshot_sources(&gate, &db, &[2, 1]).unwrap();
        snap.require_present().unwrap();
        let s = snap.sources();
        assert_eq!(s[0].id, 2);
        assert_eq!(s[0].path, PathBuf::from("/lib/b.cr3"));
        assert_eq!(s[0].status, "present");
        assert_eq!(s[0].format, "raw");
        assert_eq!(s[0].content_hash_hex, hex_bytes(&[0xbb; 32]));
        assert_eq!(s[0].folder_id, Some(1));
        assert_eq!(s[1].content_hash_hex, hex_bytes(&[0xaa; 32]));
        assert!(gate.with_tokens(snap.tokens(), || ()).is_ok());
    }

    #[test]
    fn missing_source_is_rejected_by_require_present() {
        let (gate, db) = setup();
        insert(&db, 3, "/lib/c.cr3", 0xee, "missing");
        let snap = snapshot_sources(&gate, &db, &[1, 3]).unwrap();
        assert!(snap.require_present().unwrap_err().contains("relink"));
    }

    #[test]
    fn mutation_between_read_and_capture_is_stale() {
        for (name, mutate) in MUTATIONS {
            let (gate, db) = setup();
            let err =
                snapshot_sources_with(&gate, &db, &[1, 2], || mutate(&gate, &db)).expect_err(name);
            assert_eq!(err, STALE_CAPTURE, "{name}");
        }
    }

    #[test]
    fn mutation_after_capture_rejects_publish_and_removes_owned_output_only() {
        for (name, mutate) in MUTATIONS {
            let (gate, db) = setup();
            let snap = snapshot_sources(&gate, &db, &[1, 2]).unwrap();
            let dir = tempfile::tempdir().unwrap();
            let owned = dir.path().join("out_HDR.exr");
            let other = dir.path().join("unrelated.exr");
            std::fs::write(&owned, b"x").unwrap();
            std::fs::write(&other, b"y").unwrap();
            mutate(&gate, &db);
            let committed = std::cell::Cell::new(false);
            let err = publish_owned(&gate, snap.tokens(), &owned, |_| {
                committed.set(true);
                Ok(())
            })
            .expect_err(name);
            assert_eq!(err, STALE_PUBLISH, "{name}");
            assert!(!committed.get(), "{name}: commit ran");
            assert!(!owned.exists(), "{name}: owned output kept");
            assert!(other.exists(), "{name}: unrelated file removed");
        }
    }

    #[test]
    fn commit_error_removes_owned_output() {
        let (gate, db) = setup();
        let snap = snapshot_sources(&gate, &db, &[1]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let owned = dir.path().join("out.dng");
        std::fs::write(&owned, b"x").unwrap();
        let err =
            publish_owned::<()>(&gate, snap.tokens(), &owned, |_| Err("boom".into())).unwrap_err();
        assert_eq!(err, "boom");
        assert!(!owned.exists());
    }

    #[test]
    fn publish_keeps_output_on_success() {
        let (gate, db) = setup();
        let snap = snapshot_sources(&gate, &db, &[1]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let owned = dir.path().join("out.dng");
        std::fs::write(&owned, b"x").unwrap();
        publish_owned(&gate, snap.tokens(), &owned, |_| Ok(7)).unwrap();
        assert!(owned.exists());
    }
}
