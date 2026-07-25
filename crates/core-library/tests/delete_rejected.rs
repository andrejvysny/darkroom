//! "Delete all rejected" — the destructive path. Every test asserts the same invariant from a
//! different angle: **only rejected photos (and the camera companions riding on a rejected RAW) may
//! ever be removed.** Real files are used via an injected remover so the OS Trash is never touched.

use core_db::rusqlite::{params, Connection};
use core_db::Db;
use core_library::{
    delete_rejected_with, link_pair, rejected_ids, summarize_rejected, trash::trash_images_with,
    QueryParams,
};
use std::path::{Path, PathBuf};

/// Insert an image row pointing at a real file under `dir`; returns its id.
fn insert_file(conn: &Connection, dir: &Path, tag: u8, filename: &str) -> i64 {
    let path = dir.join(filename);
    std::fs::write(&path, vec![tag; 64]).unwrap();
    conn.execute(
        "INSERT INTO images(content_hash, file_size, path, original_filename, status,
                            capture_date, imported_at)
         VALUES (?1, 64, ?2, ?3, 'present', 1000, 0)",
        params![vec![tag; 32], path.display().to_string(), filename],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn set_flag(conn: &Connection, id: i64, flag: &str) {
    core_library::set_flag(conn, id, flag).unwrap();
}

fn present_ids(conn: &Connection) -> Vec<i64> {
    let mut stmt = conn
        .prepare("SELECT id FROM images WHERE status='present' ORDER BY id")
        .unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).unwrap();
    rows.map(Result::unwrap).collect()
}

/// A remover that really deletes, so the file/row consistency rules are exercised without the Trash.
fn fs_remover() -> impl Fn(&Path) -> bool {
    |p: &Path| std::fs::remove_file(p).is_ok()
}

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("darkroom-del-rejected-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn deletes_only_rejected_rows_and_files() {
    let dir = tempdir("basic");
    let db = Db::open_in_memory().unwrap();

    let keep_none = insert_file(&db.conn, &dir, 1, "unflagged.cr3");
    let keep_pick = insert_file(&db.conn, &dir, 2, "picked.cr3");
    let drop_a = insert_file(&db.conn, &dir, 3, "rejected-a.cr3");
    let drop_b = insert_file(&db.conn, &dir, 4, "rejected-b.cr3");
    set_flag(&db.conn, keep_pick, "pick");
    set_flag(&db.conn, drop_a, "reject");
    set_flag(&db.conn, drop_b, "reject");

    // The target set is derived from the flag, not from the caller.
    let p = QueryParams::default();
    assert_eq!(rejected_ids(&db.conn, &p).unwrap(), vec![drop_a, drop_b]);

    let summary = summarize_rejected(&db.conn, &p).unwrap();
    assert_eq!(summary.images, 2);
    assert_eq!(summary.companions, 0);
    assert_eq!(summary.bytes, 128, "2 × 64-byte files");

    let res = trash_images_with(&db.conn, &[drop_a, drop_b], &fs_remover()).unwrap();
    assert_eq!(res.trashed, 2);
    assert_eq!(res.failed, 0);

    assert_eq!(present_ids(&db.conn), vec![keep_none, keep_pick]);
    assert!(dir.join("unflagged.cr3").exists());
    assert!(dir.join("picked.cr3").exists());
    assert!(!dir.join("rejected-a.cr3").exists());
    assert!(!dir.join("rejected-b.cr3").exists());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn caller_supplied_flag_cannot_widen_the_set() {
    let dir = tempdir("no-widen");
    let db = Db::open_in_memory().unwrap();
    let picked = insert_file(&db.conn, &dir, 1, "picked.cr3");
    let rejected = insert_file(&db.conn, &dir, 2, "rejected.cr3");
    set_flag(&db.conn, picked, "pick");
    set_flag(&db.conn, rejected, "reject");

    // Every one of these asks for something other than "reject"; all must resolve to the rejects.
    for flag in [None, Some("pick"), Some("none"), Some("bogus")] {
        let p = QueryParams {
            flag: flag.map(str::to_string),
            ..Default::default()
        };
        assert_eq!(
            rejected_ids(&db.conn, &p).unwrap(),
            vec![rejected],
            "flag={flag:?} must not widen the delete set"
        );
    }

    let res = delete_rejected_with(&db.conn, &QueryParams::default(), &fs_remover()).unwrap();
    assert_eq!(res.trashed, 1);
    assert_eq!(present_ids(&db.conn), vec![picked]);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn paging_params_never_truncate_the_delete_set() {
    let dir = tempdir("paging");
    let db = Db::open_in_memory().unwrap();
    let ids: Vec<i64> = (1..=5u8)
        .map(|t| insert_file(&db.conn, &dir, t, &format!("r{t}.cr3")))
        .collect();
    for &id in &ids {
        set_flag(&db.conn, id, "reject");
    }

    // A grid-shaped params object carries limit/offset/cursor — a destructive op must ignore them.
    let p = QueryParams {
        limit: Some(2),
        offset: Some(3),
        seek: Some(true),
        cursor_id: Some(ids[1]),
        ..Default::default()
    };
    assert_eq!(rejected_ids(&db.conn, &p).unwrap(), ids);
    assert_eq!(summarize_rejected(&db.conn, &p).unwrap().images, 5);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn honours_the_active_filter() {
    let dir = tempdir("filter");
    let db = Db::open_in_memory().unwrap();
    let a = insert_file(&db.conn, &dir, 1, "a.cr3");
    let b = insert_file(&db.conn, &dir, 2, "b.jpg");
    set_flag(&db.conn, a, "reject");
    set_flag(&db.conn, b, "reject");
    db.conn
        .execute("UPDATE images SET format='raw' WHERE id=?1", params![a])
        .unwrap();
    db.conn
        .execute("UPDATE images SET format='jpeg' WHERE id=?1", params![b])
        .unwrap();

    let raws_only = QueryParams {
        format: Some("raw".into()),
        ..Default::default()
    };
    assert_eq!(rejected_ids(&db.conn, &raws_only).unwrap(), vec![a]);

    let res = delete_rejected_with(&db.conn, &raws_only, &fs_remover()).unwrap();
    assert_eq!(res.trashed, 1);
    assert_eq!(
        present_ids(&db.conn),
        vec![b],
        "the filtered-out reject stays"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn rejected_raw_takes_its_camera_companion() {
    let dir = tempdir("pairs");
    let db = Db::open_in_memory().unwrap();
    let raw = insert_file(&db.conn, &dir, 1, "IMG_1.cr3");
    let jpg = insert_file(&db.conn, &dir, 2, "IMG_1.jpg");
    let other_raw = insert_file(&db.conn, &dir, 3, "IMG_2.cr3");
    let other_jpg = insert_file(&db.conn, &dir, 4, "IMG_2.jpg");
    link_pair(&db.conn, raw, jpg).unwrap();
    link_pair(&db.conn, other_raw, other_jpg).unwrap();
    set_flag(&db.conn, raw, "reject");

    let p = QueryParams::default();
    let summary = summarize_rejected(&db.conn, &p).unwrap();
    assert_eq!(summary.images, 1);
    assert_eq!(
        summary.companions, 1,
        "the paired JPEG is reported, not hidden"
    );
    assert_eq!(summary.bytes, 128);

    let res = delete_rejected_with(&db.conn, &p, &fs_remover()).unwrap();
    assert_eq!(res.trashed, 2);
    assert_eq!(res.companions, 1);
    assert_eq!(
        present_ids(&db.conn),
        vec![other_raw, other_jpg],
        "the unrejected pair is untouched"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn rejected_companion_alone_does_not_take_its_raw() {
    let dir = tempdir("companion-only");
    let db = Db::open_in_memory().unwrap();
    let raw = insert_file(&db.conn, &dir, 1, "IMG_1.cr3");
    let jpg = insert_file(&db.conn, &dir, 2, "IMG_1.jpg");
    link_pair(&db.conn, raw, jpg).unwrap();
    set_flag(&db.conn, jpg, "reject");

    // Companions are hidden from the default grid query, but a reject on one still counts.
    let p = QueryParams::default();
    assert_eq!(rejected_ids(&db.conn, &p).unwrap(), vec![jpg]);

    let res = delete_rejected_with(&db.conn, &p, &fs_remover()).unwrap();
    assert_eq!(res.trashed, 1);
    assert_eq!(res.companions, 0);
    assert_eq!(present_ids(&db.conn), vec![raw]);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn keeps_the_row_when_the_file_cannot_be_trashed() {
    let dir = tempdir("failed");
    let db = Db::open_in_memory().unwrap();
    let stuck = insert_file(&db.conn, &dir, 1, "stuck.cr3");
    let ok = insert_file(&db.conn, &dir, 2, "ok.cr3");
    set_flag(&db.conn, stuck, "reject");
    set_flag(&db.conn, ok, "reject");

    let out = trash_images_with(&db.conn, &[stuck, ok], &|p: &Path| {
        !p.ends_with("stuck.cr3") && std::fs::remove_file(p).is_ok()
    })
    .unwrap();

    assert_eq!(out.trashed, 1);
    assert_eq!(out.failed, 1);
    assert_eq!(
        present_ids(&db.conn),
        vec![stuck],
        "a row whose file survived must survive too"
    );
    assert!(dir.join("stuck.cr3").exists());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn removes_the_sidecar_and_cascades_dependents() {
    let dir = tempdir("sidecar");
    let db = Db::open_in_memory().unwrap();
    let id = insert_file(&db.conn, &dir, 1, "shot.cr3");
    set_flag(&db.conn, id, "reject");
    core_library::write_sidecar(&db.conn, id).unwrap();
    let sidecar = dir.join("shot.cr3.json");
    assert!(sidecar.exists(), "precondition: sidecar written");

    trash_images_with(&db.conn, &[id], &fs_remover()).unwrap();

    assert!(!sidecar.exists(), "orphan sidecar removed with its image");
    let flags: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM ratings_flags", [], |r| r.get(0))
        .unwrap();
    assert_eq!(flags, 0, "dependent rows cascade away");

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn empty_reject_set_is_a_no_op() {
    let dir = tempdir("empty");
    let db = Db::open_in_memory().unwrap();
    let a = insert_file(&db.conn, &dir, 1, "a.cr3");
    set_flag(&db.conn, a, "pick");

    let res = delete_rejected_with(&db.conn, &QueryParams::default(), &fs_remover()).unwrap();
    assert_eq!(res.trashed, 0);
    assert_eq!(present_ids(&db.conn), vec![a]);
    assert!(dir.join("a.cr3").exists());

    std::fs::remove_dir_all(&dir).unwrap();
}
