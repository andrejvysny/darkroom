//! Plan / unlocked-trash / verified-finalize resolve path, on a real temp SQLite catalog with
//! real files on disk. Removers are injected (never the user's Trash).

use core_db::rusqlite::params;
use core_db::Db;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn scratch_dir(name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "darkroom-dedup-resolve-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn insert(db: &Db, hash: &[u8], path: &str, file_size: i64) -> i64 {
    db.conn
        .execute(
            "INSERT INTO images(content_hash, capture_fingerprint, file_size, path,
                                original_filename, status, imported_at)
             VALUES(?1, NULL, ?2, ?3, ?3, 'present', 0)",
            params![hash, file_size, path],
        )
        .unwrap();
    db.conn.last_insert_rowid()
}

fn write_file(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, b"copy").unwrap();
    p
}

fn row_count(db: &Db, id: i64) -> i64 {
    db.conn
        .query_row(
            "SELECT COUNT(*) FROM images WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap()
}

/// A keeper row that is `present` but whose file is already gone (external delete, unmounted
/// volume) must stop the pass BEFORE any trashing: the victims may be the only copies left.
#[test]
fn absent_keeper_file_trashes_nothing() {
    let db = Db::open_in_memory().unwrap();
    let dir = scratch_dir("absent-keeper");
    let hash = [8u8; 32];
    let keeper = dir.join("keep.cr3"); // deliberately never created
    let v1 = write_file(&dir, "v1.cr3");
    let v2 = write_file(&dir, "v2.cr3");
    let keep_id = insert(&db, &hash, keeper.to_str().unwrap(), 1000);
    let d1 = insert(&db, &hash, v1.to_str().unwrap(), 1000);
    let d2 = insert(&db, &hash, v2.to_str().unwrap(), 1000);

    let plan = core_dedup::plan_resolve(&db.conn, keep_id, &[d1, d2]).unwrap();
    let trashed = core_dedup::trash_victims(&plan, &|p| std::fs::remove_file(p).is_ok());
    assert!(trashed.keeper_lost, "absent keeper must abort the plan");
    assert!(trashed.gone.is_empty(), "nothing may be trashed");
    assert!(v1.exists(), "victim 1 keeps its only copy");
    assert!(v2.exists(), "victim 2 keeps its only copy");

    let res = core_dedup::finalize_resolve(&db.conn, &trashed).unwrap();
    assert_eq!(res.trashed, 0, "no rows removed");
    assert_eq!(res.keeper_lost, 1);
    assert_eq!(row_count(&db, d1), 1, "victim 1 row survives");
    assert_eq!(row_count(&db, d2), 1, "victim 2 row survives");
    assert_eq!(row_count(&db, keep_id), 1, "keeper row survives");
}

/// Plan with 3 victims; the remover trashes victim 1 and, as a side effect, deletes the keeper.
/// Victims 2 and 3 must never be touched, the plan flags `keeper_lost`, and finalize removes only
/// victim 1's row.
#[test]
fn keeper_deleted_mid_resolve_stops_trashing() {
    let db = Db::open_in_memory().unwrap();
    let dir = scratch_dir("keeper-lost");
    let hash = [1u8; 32];
    let keeper = write_file(&dir, "keep.cr3");
    let v1 = write_file(&dir, "v1.cr3");
    let v2 = write_file(&dir, "v2.cr3");
    let v3 = write_file(&dir, "v3.cr3");
    let keep_id = insert(&db, &hash, keeper.to_str().unwrap(), 1000);
    let d1 = insert(&db, &hash, v1.to_str().unwrap(), 1000);
    let d2 = insert(&db, &hash, v2.to_str().unwrap(), 1000);
    let d3 = insert(&db, &hash, v3.to_str().unwrap(), 1000);

    let plan = core_dedup::plan_resolve(&db.conn, keep_id, &[d1, d2, d3]).unwrap();
    assert_eq!(plan.victims.len(), 3);

    let keeper_copy = keeper.clone();
    let v1_copy = v1.clone();
    let removed = RefCell::new(Vec::new());
    let remove = |p: &Path| -> bool {
        let ok = std::fs::remove_file(p).is_ok();
        removed.borrow_mut().push(p.to_path_buf());
        if p == v1_copy {
            let _ = std::fs::remove_file(&keeper_copy); // keeper vanishes mid-pass
        }
        ok
    };

    let trashed = core_dedup::trash_victims(&plan, &remove);
    assert!(trashed.keeper_lost, "keeper loss must abort the plan");
    assert_eq!(trashed.gone.len(), 1, "only victim 1 made it out");
    assert!(!v1.exists());
    assert!(
        v2.exists(),
        "victim 2 must not be trashed after keeper loss"
    );
    assert!(
        v3.exists(),
        "victim 3 must not be trashed after keeper loss"
    );
    assert!(!keeper.exists());

    let res = core_dedup::finalize_resolve(&db.conn, &trashed).unwrap();
    assert_eq!(res.trashed, 1, "only victim 1's row is removed");
    assert_eq!(res.keeper_lost, 1);
    assert_eq!(row_count(&db, d1), 0);
    assert_eq!(
        row_count(&db, d2),
        1,
        "aborted victim keeps its row and its file"
    );
    assert_eq!(row_count(&db, d3), 1);
    assert_eq!(row_count(&db, keep_id), 1);
}

/// A keeper row deleted between trash and finalize (while its file is still on disk, so
/// `trash_victims` saw nothing wrong) means finalize cannot prove a copy survived: it must delete
/// NO victim rows, report `keeper_lost`, and leave the trashed files recoverable from the Trash.
#[test]
fn finalize_keeps_victim_rows_when_keeper_row_vanished() {
    let db = Db::open_in_memory().unwrap();
    let dir = scratch_dir("keeper-row-gone");
    let hash = [9u8; 32];
    let keeper = write_file(&dir, "keep.cr3");
    let v1 = write_file(&dir, "v1.cr3");
    let v2 = write_file(&dir, "v2.cr3");
    let keep_id = insert(&db, &hash, keeper.to_str().unwrap(), 1000);
    let d1 = insert(&db, &hash, v1.to_str().unwrap(), 1000);
    let d2 = insert(&db, &hash, v2.to_str().unwrap(), 1000);

    let plan = core_dedup::plan_resolve(&db.conn, keep_id, &[d1, d2]).unwrap();
    let trashed = core_dedup::trash_victims(&plan, &|p| std::fs::remove_file(p).is_ok());
    assert_eq!(trashed.gone.len(), 2, "both files went to the Trash");
    assert!(!trashed.keeper_lost, "keeper file was on disk throughout");

    // The keeper's ROW vanishes mid-flight (database reset, external prune) while its file stays.
    db.conn
        .execute("DELETE FROM images WHERE id=?1", params![keep_id])
        .unwrap();

    let res = core_dedup::finalize_resolve(&db.conn, &trashed).unwrap();
    assert_eq!(res.trashed, 0, "no victim row may be removed");
    assert_eq!(res.changed, 0, "not a repoint — the keeper simply vanished");
    assert_eq!(res.keeper_lost, 1, "no provably surviving copy");
    assert!(res.trashed_hashes.is_empty());
    assert_eq!(row_count(&db, d1), 1, "victim 1 row stays");
    assert_eq!(row_count(&db, d2), 1, "victim 2 row stays");
    assert_eq!(row_count(&db, keep_id), 0, "the keeper row is really gone");
}

/// A victim row whose path changed between trash and finalize no longer matches the plan: the row
/// is kept and counted in `changed`.
#[test]
fn finalize_skips_repointed_victim() {
    let db = Db::open_in_memory().unwrap();
    let dir = scratch_dir("repointed");
    let hash = [2u8; 32];
    let keeper = write_file(&dir, "keep.cr3");
    let victim = write_file(&dir, "v.cr3");
    let keep_id = insert(&db, &hash, keeper.to_str().unwrap(), 1000);
    let victim_id = insert(&db, &hash, victim.to_str().unwrap(), 1000);

    let plan = core_dedup::plan_resolve(&db.conn, keep_id, &[victim_id]).unwrap();
    let trashed = core_dedup::trash_victims(&plan, &|p| std::fs::remove_file(p).is_ok());
    assert_eq!(trashed.gone.len(), 1);
    assert!(!victim.exists(), "the file really went away");

    // Someone re-pointed the row (e.g. re-imported it elsewhere) before we finalized.
    db.conn
        .execute(
            "UPDATE images SET path='?moved-to-nas.cr3' WHERE id=?1",
            params![victim_id],
        )
        .unwrap();

    let res = core_dedup::finalize_resolve(&db.conn, &trashed).unwrap();
    assert_eq!(res.trashed, 0);
    assert_eq!(
        res.changed, 1,
        "mismatch against the plan counts as changed"
    );
    assert_eq!(
        row_count(&db, victim_id),
        1,
        "a re-pointed row must survive finalize"
    );
}

/// A file that fails to trash keeps its catalog row and is reported in `failed`.
#[test]
fn failed_trash_keeps_victim_row() {
    let db = Db::open_in_memory().unwrap();
    let dir = scratch_dir("failed");
    let hash = [3u8; 32];
    let keeper = write_file(&dir, "keep.cr3");
    let victim = write_file(&dir, "v.cr3");
    let keep_id = insert(&db, &hash, keeper.to_str().unwrap(), 1000);
    let victim_id = insert(&db, &hash, victim.to_str().unwrap(), 1000);

    let res = core_dedup::resolve_with(&db.conn, keep_id, &[victim_id], &|_| false).unwrap();
    assert_eq!(res.trashed, 0);
    assert_eq!(res.failed, 1);
    assert!(victim.exists(), "a failed trash must not remove the file");
    assert_eq!(
        row_count(&db, victim_id),
        1,
        "the catalog never drops the row of a still-present file"
    );
}

/// The victim's `.json` sidecar is trashed alongside it (core-library's `<path>.json` naming).
#[test]
fn sidecar_of_victim_is_trashed() {
    let db = Db::open_in_memory().unwrap();
    let dir = scratch_dir("sidecar");
    let hash = [4u8; 32];
    let keeper = write_file(&dir, "keep.cr3");
    let victim = write_file(&dir, "v.cr3");
    let sidecar = write_file(&dir, "v.cr3.json");
    std::fs::write(&sidecar, "{}").unwrap();
    let keep_id = insert(&db, &hash, keeper.to_str().unwrap(), 1000);
    let victim_id = insert(&db, &hash, victim.to_str().unwrap(), 1000);

    let removed = RefCell::new(Vec::new());
    let remove = |p: &Path| -> bool {
        let ok = std::fs::remove_file(p).is_ok();
        removed.borrow_mut().push(p.to_path_buf());
        ok
    };
    let res = core_dedup::resolve_with(&db.conn, keep_id, &[victim_id], &remove).unwrap();
    assert_eq!(res.trashed, 1);
    assert!(!victim.exists());
    assert!(
        !sidecar.exists(),
        "the orphan sidecar must go with its file"
    );
    assert!(
        removed.borrow().contains(&sidecar),
        "the remover was called for the sidecar"
    );
    assert_eq!(row_count(&db, victim_id), 0);
}

/// An invalid keeper aborts in `plan_resolve` before any file IO: nothing is trashed, all rows stay.
#[test]
fn invalid_keeper_trashes_nothing() {
    let db = Db::open_in_memory().unwrap();
    let dir = scratch_dir("invalid-keeper");
    let hash = [5u8; 32];
    let a1 = write_file(&dir, "a1.cr3");
    let a2 = write_file(&dir, "a2.cr3");
    let id1 = insert(&db, &hash, a1.to_str().unwrap(), 1000);
    let id2 = insert(&db, &hash, a2.to_str().unwrap(), 1000);

    let removed = RefCell::new(Vec::new());
    let remove = |p: &Path| -> bool {
        removed.borrow_mut().push(p.to_path_buf());
        std::fs::remove_file(p).is_ok()
    };

    let stale = id1.max(id2) + 999;
    let err = core_dedup::plan_resolve(&db.conn, stale, &[id1, id2]);
    assert!(
        matches!(err, Err(core_dedup::DedupError::InvalidKeeper(_))),
        "unknown keeper must error"
    );
    let err = core_dedup::resolve_with(&db.conn, stale, &[id1, id2], &remove).unwrap_err();
    assert!(
        matches!(err, core_dedup::DedupError::InvalidKeeper(_)),
        "resolve_with must refuse before any file IO"
    );
    assert!(
        removed.borrow().is_empty(),
        "no file touched for an invalid keeper"
    );
    assert!(a1.exists() && a2.exists());
    assert_eq!(row_count(&db, id1), 1);
    assert_eq!(row_count(&db, id2), 1);

    // A keeper already trashed (row gone) is likewise rejected.
    db.conn
        .execute("DELETE FROM images WHERE id=?1", params![id1])
        .unwrap();
    let err = core_dedup::plan_resolve(&db.conn, id1, &[id2]);
    assert!(
        matches!(err, Err(core_dedup::DedupError::InvalidKeeper(_))),
        "already-resolved keeper must error"
    );
    assert_eq!(row_count(&db, id2), 1, "last copy preserved");
}

/// `plan_auto_resolve_byte_identical` plans every byte-identical group with `pick_keeper`'s choice
/// (largest file, tiebreak lowest id), never listing the keeper as a victim.
#[test]
fn plan_auto_resolve_uses_pick_keeper() {
    let db = Db::open_in_memory().unwrap();
    let dir = scratch_dir("auto-plan");
    let (h1, h2) = ([6u8; 32], [7u8; 32]);
    let g1_small = insert(&db, &h1, dir.join("g1a.cr3").to_str().unwrap(), 900);
    let g1_big = insert(&db, &h1, dir.join("g1b.cr3").to_str().unwrap(), 1100);
    let g1_dup = insert(&db, &h1, dir.join("g1c.cr3").to_str().unwrap(), 900);
    let g2_a = insert(&db, &h2, dir.join("g2a.cr3").to_str().unwrap(), 1000);
    let g2_b = insert(&db, &h2, dir.join("g2b.cr3").to_str().unwrap(), 1000);

    let plans = core_dedup::plan_auto_resolve_byte_identical(&db.conn).unwrap();
    assert_eq!(plans.len(), 2);
    let p1 = plans.iter().find(|p| p.keep_id == g1_big).unwrap();
    assert_eq!(
        p1.victims.iter().map(|v| v.id).collect::<Vec<_>>(),
        vec![g1_small, g1_dup],
        "largest file kept; victims in trash-id order"
    );
    let p2 = plans.iter().find(|p| p.keep_id == g2_a).unwrap();
    assert_eq!(
        p2.victims.iter().map(|v| v.id).collect::<Vec<_>>(),
        vec![g2_b]
    );
    for p in &plans {
        assert!(
            !p.victims.iter().any(|v| v.id == p.keep_id),
            "keeper is never a victim"
        );
    }
}
