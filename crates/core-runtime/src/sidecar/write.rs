//! The per-image sidecar write decision, kept free of `AppHandle` so the identity rules are unit
//! testable against a real catalog; `sidecar_queue` only schedules when this runs.
//!
//! Three defects this module exists to rule out:
//! - An *unrelated* catalog mutation (the watcher inserting image B) must not cost image A its
//!   sidecar. Reading the gate's catalog-wide `sequence` up front did exactly that: any mutation made
//!   A's token uncallable and the id was dropped as "source identity stale".
//! - A row that is deleted, goes `missing`, or is repointed at other bytes mid-flight must not get a
//!   sidecar written at all.
//! - A filesystem failure (read-only volume, full disk) must not be reported as done: every outcome
//!   that is not `Written`/`Gone` returns the id to the dirty set, so the intent survives until the
//!   sidecar really matches the catalog.
//!
//! Lock order matches the rest of the app: publication gate before catalog. A catalog guard never
//! outlives a catalog read. A write is three steps, only the last under the gate:
//! 1. brief catalog read (no gate): row identity + sidecar snapshot, then capture a publication token;
//! 2. STAGE with no gate and no catalog lock: serialize the JSON and write it to a unique hidden
//!    temp file beside the destination (`create_new`, `.<name>.<pid>-<n>.tmp`);
//! 3. PUBLISH under the gate (`with_current`: token still valid) with a brief catalog re-read that
//!    must show the same path + hash + snapshot, then a single `rename(temp, final)`. Anything stale
//!    removes the temp and returns `Retry`/`Gone`/`Unavailable` — nothing unverified ever lands.

use crate::publication::{PublicationGate, SourceKey};
use core_db::{rusqlite::Connection, Db};
use core_library::{LibError, SidecarWrite};
use core_model::identity::ImageId;
use std::sync::Mutex;

const POISONED: &str = "sidecar catalog lock poisoned";

/// What one sidecar attempt decided. The queue maps this onto the dirty set: `Written`/`Gone` are
/// finished, everything else goes back for another pass.
#[derive(Debug)]
pub enum SidecarOutcome {
    /// File written and matches the catalog snapshot.
    Written,
    /// Row deleted / repointed to another file: nothing to mirror any more.
    Gone,
    /// The row exists but is not `present` right now (card unplugged, file missing). Nothing can be
    /// written, but the intent is still owed: the outbox worker keeps it and retries when the
    /// source returns; the coalescing queue treats it as settled (the durable outbox remembers).
    Unavailable,
    /// Identity or snapshot moved while we worked; re-snapshot and try again.
    Retry,
    /// Filesystem write failed; keep the intent and retry later. Carries the error text.
    Failed(String),
}

/// Write `image_id`'s sidecar iff the catalog still says what it said when the snapshot was gathered.
///
/// `write` STAGES the sidecar: it is handed a copy of the snapshot whose `path` is a unique temp
/// file beside the destination and must put the bytes there (no gate, no catalog lock held). The
/// final `rename` is done here, under the gate. `before_publish` is a test seam run after staging and
/// before the fenced re-read: it models the worker being paused mid-write. Production passes
/// `&|| {}`. `Err` is reserved for a poisoned catalog lock — every other problem is an outcome.
pub fn write_one(
    gate: &PublicationGate,
    db: &Mutex<Db>,
    image_id: i64,
    write: &dyn Fn(&SidecarWrite) -> std::io::Result<()>,
    before_publish: &dyn Fn(),
) -> Result<SidecarOutcome, String> {
    // Deliberately no catalog-wide `sequence` pre-read: the real check is the in-fence re-read below,
    // which compares *this* image's identity and snapshot, so an unrelated image cannot invalidate it.
    let (key, snapshot) = match read_state(db, image_id)? {
        State::Live(key, snapshot) => (key, snapshot),
        State::Gone => return Ok(SidecarOutcome::Gone),
        State::Unavailable => return Ok(SidecarOutcome::Unavailable),
        State::Unknown => return Ok(SidecarOutcome::Retry),
    };
    let Ok(token) = gate.capture(ImageId(image_id), key.clone()) else {
        return Ok(SidecarOutcome::Retry);
    };
    // Stage with NO lock held: serialization and the temp-file write are the slow part.
    let staged = SidecarWrite {
        path: core_library::staging_path(&snapshot.path),
        sidecar: snapshot.sidecar.clone(),
    };
    if let Err(error) = write(&staged) {
        let _ = std::fs::remove_file(&staged.path);
        return Ok(SidecarOutcome::Failed(error.to_string()));
    }
    before_publish();
    let fenced = gate.with_current(&token, || -> Result<SidecarOutcome, String> {
        match decide(read_state(db, image_id)?, &key, &snapshot) {
            Decision::Gone => Ok(SidecarOutcome::Gone),
            Decision::Unavailable => Ok(SidecarOutcome::Unavailable),
            Decision::Retry => Ok(SidecarOutcome::Retry),
            // The only I/O under the gate is the rename itself; the catalog guard from the re-read
            // is already released.
            Decision::Publish => Ok(match std::fs::rename(&staged.path, &snapshot.path) {
                Ok(()) => SidecarOutcome::Written,
                Err(error) => SidecarOutcome::Failed(error.to_string()),
            }),
        }
    });
    let outcome = match fenced {
        Ok(outcome) => outcome,
        // The fence moved between capture and publish. Only a row that is really gone justifies
        // dropping the intent; anything else re-snapshots.
        Err(_) => settle_stale(db, image_id),
    };
    if !matches!(outcome, Ok(SidecarOutcome::Written)) {
        // Not published: the staged bytes are unverified — never leave them behind.
        let _ = std::fs::remove_file(&staged.path);
    }
    outcome
}

/// Verdict for a publication that went stale: gone / not-present rows are reported as such, anything
/// else (identity moved, epoch bumped) re-snapshots.
fn settle_stale(db: &Mutex<Db>, image_id: i64) -> Result<SidecarOutcome, String> {
    let db = db.lock().map_err(|_| POISONED.to_string())?;
    Ok(match core_library::image_by_id(&db.conn, image_id) {
        Ok(None) => SidecarOutcome::Gone,
        Ok(Some(_)) => match is_present(&db.conn, image_id) {
            Some(false) => SidecarOutcome::Unavailable,
            _ => SidecarOutcome::Retry,
        },
        // A catalog we cannot read is not evidence that the image is gone.
        Err(_) => SidecarOutcome::Retry,
    })
}

/// The production staging `write`: core-library's catalog-free serialize + `create_new` temp file.
/// Keeping the mapping here lets the tests exercise the same call the worker makes; the `io`
/// variant is preserved so a real `PermissionDenied` kind survives into `SidecarOutcome::Failed`.
pub fn production_write(snapshot: &SidecarWrite) -> std::io::Result<()> {
    match core_library::write_staged(snapshot) {
        Ok(()) => Ok(()),
        Err(LibError::Io(error)) => Err(error),
        Err(error) => Err(std::io::Error::other(error.to_string())),
    }
}

/// Log-safe summary of a write failure: the message with any path-shaped token masked. Same
/// no-user-data policy as `logging::safe_error` (which only ever logged a type name), but it keeps
/// the OS message that actually explains a read-only volume or a full disk.
pub fn safe_write_error(error: &str) -> String {
    error
        .split_whitespace()
        .map(|token| {
            if token.contains('/') || token.contains('\\') {
                "[path]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect()
}

/// What one catalog read said about an image.
enum State {
    /// Present row: its source identity plus the sidecar gathered from it (boxed so the variant
    /// stays small; the snapshot is the only bulk payload and it is read once per attempt).
    Live(SourceKey, Box<SidecarWrite>),
    /// No row (or no sidecar can be built for it): there is nothing to mirror.
    Gone,
    /// The row exists but is not `present`: nothing to write now, the intent is still owed.
    Unavailable,
    /// The catalog could not be read — undecidable, so the intent must not be dropped.
    Unknown,
}

/// In-fence verdict for a freshly read state against what we captured.
enum Decision {
    Publish,
    Gone,
    Unavailable,
    Retry,
}

/// Read identity + sidecar snapshot under a catalog guard that is dropped before returning.
fn read_state(db: &Mutex<Db>, image_id: i64) -> Result<State, String> {
    let db = db.lock().map_err(|_| POISONED.to_string())?;
    let row = match core_library::image_by_id(&db.conn, image_id) {
        Ok(Some(row)) => row,
        Ok(None) => return Ok(State::Gone),
        Err(_) => return Ok(State::Unknown),
    };
    match is_present(&db.conn, image_id) {
        Some(true) => {}
        // Row went `missing` between queueing and now — nothing to mirror right now. The outbox
        // intent is left queued (`complete_outbox` only deletes on `Gone`), so a relink
        // (`relink_missing_image`) or a watcher flip back to `present` gets it mirrored.
        Some(false) => return Ok(State::Unavailable),
        None => return Ok(State::Unknown),
    }
    match core_library::sidecar_snapshot(&db.conn, image_id) {
        Ok(Some(snapshot)) => Ok(State::Live(
            SourceKey {
                hash: row.content_hash,
                path: row.path,
            },
            Box::new(snapshot),
        )),
        Ok(None) => Ok(State::Gone),
        Err(_) => Ok(State::Unknown),
    }
}

fn decide(state: State, key: &SourceKey, snapshot: &SidecarWrite) -> Decision {
    match state {
        State::Gone => Decision::Gone,
        State::Unavailable => Decision::Unavailable,
        State::Unknown => Decision::Retry,
        State::Live(current_key, current) => {
            if &current_key != key {
                // Bytes or path moved under us: what we gathered describes a file that no longer
                // exists as far as this row is concerned.
                Decision::Gone
            } else if same_snapshot(&current, snapshot) {
                Decision::Publish
            } else {
                Decision::Retry
            }
        }
    }
}

/// Same destination and same sidecar content? `updated_at` is stamped at gather time, so two
/// snapshots of an unchanged image differ only there — normalize it before comparing.
fn same_snapshot(current: &SidecarWrite, captured: &SidecarWrite) -> bool {
    let mut latest = current.clone();
    latest.sidecar.updated_at = captured.sidecar.updated_at;
    latest.path == captured.path && latest.sidecar == captured.sidecar
}

/// `Some(true)`/`Some(false)` for a row that is / is not `present`; `None` when the catalog could not
/// answer, which callers treat as "undecidable" rather than evidence the image is gone.
fn is_present(conn: &Connection, image_id: i64) -> Option<bool> {
    conn.query_row(
        "SELECT status = 'present' FROM images WHERE id = ?1",
        [image_id],
        |r| r.get::<_, bool>(0),
    )
    .ok()
}

/// Real-catalog tests for [`write_one`], split out to keep this file short. With `test-support`
/// on but not `test` (a host building us as its dev-dependency), the `#[test]` functions are
/// stripped and their helpers/`Fix` methods look unused — that is expected, hence the allow.
#[cfg(any(test, feature = "test-support"))]
#[allow(dead_code)]
#[path = "write/tests.rs"]
pub mod tests;
