//! Durable sidecar-mirror intent (the `sidecar_outbox` table, migration 026).
//!
//! An edit is only half-saved when the catalog commit lands: the `.json` sidecar beside the RAW has
//! to carry the same params, and that write can fail — disk full, card unplugged, file locked, app
//! quit mid-write. Before the outbox the "this still needs writing" knowledge lived in an in-memory
//! queue, so a crash lost it and the sidecar kept stale params forever with nothing left to retry.
//! Now the intent is committed in the same transaction as the edit
//! ([`edits::save_edit_with_intent`][crate::edits::save_edit_with_intent]), survives a restart, and
//! is cleared only once a write of THAT revision of THAT exact file has succeeded.
//!
//! Catalog-only by design: this module never touches the filesystem. The host drains
//! [`pending_outbox`], performs the write itself, then reports [`complete_outbox`] or
//! [`fail_outbox`]. Keeping I/O out of here is what makes the queue testable and keeps the catalog
//! lock free while a card is being written.

use crate::error::LibError;
use core_db::rusqlite::{named_params, params, Connection, OptionalExtension, Transaction};

/// One queued "this sidecar is owed a write" intent, as stored by the save that made it necessary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxItem {
    pub image_id: i64,
    /// The `edits.revision` whose params the host must write.
    pub edit_revision: i64,
    /// Hex of the image's `content_hash` when the intent was queued — the source identity.
    pub content_hash_hex: String,
    /// The image's path when the intent was queued.
    pub path: String,
    /// Failed write attempts so far (reset to 0 whenever a new intent supersedes this one).
    pub attempts: i64,
    /// Why the last attempt failed; `None` while nothing has been tried yet.
    pub last_error: Option<String>,
    /// Unix milliseconds before which the host must not attempt this intent again (0 = due now).
    /// Durable, so pacing survives wake-ups and restarts.
    pub next_attempt_at: i64,
}

/// `(next_attempt_at, enqueued_at, image_id)` — the total order [`pending_outbox`] pages by.
pub type OutboxCursor = (i64, i64, i64);

/// What reporting a successful sidecar write did to the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxCompletion {
    /// `mirrored_revision` now equals this revision and the outbox row is gone — the mirror is on
    /// disk and the catalog agrees.
    Completed,
    /// A newer save has queued a newer revision, so this write's success proves nothing about it.
    /// The outbox row was kept for the newer intent's own completion.
    Superseded,
    /// The image row still exists and is `present`, but points at a different path or hash than
    /// when the intent was queued (moved / renamed / replaced mid-write), still at the same
    /// revision. The row owes exactly this write, so the intent was NOT dropped: its path and
    /// content hash were re-pointed at the row's current identity (revision kept, attempts reset)
    /// and the caller should retry the write against the new file now.
    Requeued,
    /// Same revision, different file, and the row is no longer `present`: there is nothing to
    /// mirror right now. The intent is left queued — when the file reappears (relink, remount) the
    /// next drain writes it; deleting here would strand `mirrored_revision < revision` with no
    /// owed write, the exact failure this table exists to prevent.
    SourceChanged,
    /// The image row or the edit row this intent belonged to is gone, so nothing is owed any more.
    /// Any matching outbox row was dropped with it.
    Gone,
}

/// A page of intents that are DUE (`next_attempt_at <= now_ms`), in `(next_attempt_at, enqueued_at,
/// image_id)` order, strictly after the keyset position `after`.
///
/// Eligibility is filtered in SQL so a backed-off row can never be re-selected before its time, and
/// the keyset lets one drain pass walk ALL due rows page by page: a block of unavailable or
/// permission-failing rows pushes themselves to the future (or past the cursor) instead of pinning
/// the head of the queue, so a healthy row behind them is always reached.
pub fn pending_outbox(
    conn: &Connection,
    now_ms: i64,
    after: Option<OutboxCursor>,
    limit: usize,
) -> Result<Vec<OutboxItem>, LibError> {
    Ok(pending_outbox_keyed(conn, now_ms, after, limit)?
        .into_iter()
        .map(|(item, _)| item)
        .collect())
}

/// [`pending_outbox`] plus each row's full keyset position (needed to continue the page walk).
pub fn pending_outbox_keyed(
    conn: &Connection,
    now_ms: i64,
    after: Option<OutboxCursor>,
    limit: usize,
) -> Result<Vec<(OutboxItem, OutboxCursor)>, LibError> {
    // Row-value comparison against a floor tuple when there is no cursor: every real row is above it.
    let (a, e, i) = after.unwrap_or((i64::MIN, i64::MIN, i64::MIN));
    let mut stmt = conn.prepare(
        "SELECT image_id, edit_revision, content_hash, path, attempts, last_error,
                next_attempt_at, enqueued_at
           FROM sidecar_outbox
          WHERE next_attempt_at <= :now
            AND (next_attempt_at, enqueued_at, image_id) > (:after_next, :after_enq, :after_id)
          ORDER BY next_attempt_at, enqueued_at, image_id
          LIMIT :limit",
    )?;
    let rows = stmt.query_map(
        named_params! {
            ":now": now_ms,
            ":after_next": a,
            ":after_enq": e,
            ":after_id": i,
            ":limit": limit as i64,
        },
        |r| {
            let item = OutboxItem {
                image_id: r.get(0)?,
                edit_revision: r.get(1)?,
                // Stored as the 32 raw BLAKE3 bytes; the queue hands out the hex form every other
                // sidecar/cache key uses.
                content_hash_hex: hex_or_raw(&r.get::<_, Vec<u8>>(2)?),
                path: r.get(3)?,
                attempts: r.get(4)?,
                last_error: r.get(5)?,
                next_attempt_at: r.get(6)?,
            };
            let cursor = (item.next_attempt_at, r.get::<_, i64>(7)?, item.image_id);
            Ok((item, cursor))
        },
    )?;
    // A row that cannot be read must fail the drain: silently dropping an intent is exactly the
    // "lost write" this table exists to prevent.
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Earliest `next_attempt_at` among all queued intents (0 = something is due now), or `None` when
/// the queue is empty. The worker compares it against its clock to decide whether to drain or sleep.
pub fn next_outbox_due(conn: &Connection) -> Result<Option<i64>, LibError> {
    Ok(
        conn.query_row("SELECT MIN(next_attempt_at) FROM sidecar_outbox", [], |r| {
            r.get::<_, Option<i64>>(0)
        })?,
    )
}

/// Record a successful sidecar write for `item`.
///
/// One transaction, so the mirror can never be marked done in the catalog while the intent that
/// proves it is still queued (or vice versa). The write is accepted only while the catalog still
/// describes exactly what was written: the edits row is still at `item.edit_revision` and the image
/// row still has the path + content hash captured with the intent. Anything else returns the reason
/// without marking anything (see [`OutboxCompletion`]) — and without deleting, either: only a gone
/// image/edit deletes an intent. A present-but-moved row gets its intent re-pointed at the row's
/// current identity ([`OutboxCompletion::Requeued`]) so the queue never silently loses an owed
/// write.
pub fn complete_outbox(conn: &Connection, item: &OutboxItem) -> Result<OutboxCompletion, LibError> {
    let tx = conn.unchecked_transaction()?;
    // One read of the current state decides everything; the guards below compare against it rather
    // than trusting `item`, which is a snapshot of how things looked when the write started.
    let current = tx
        .query_row(
            "SELECT i.path, i.content_hash, i.status, e.revision
               FROM images i LEFT JOIN edits e ON e.image_id = i.id
              WHERE i.id = ?1",
            params![item.image_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .optional()?;
    let (path, content_hash, status, revision) = match current {
        Some(c) => c,
        // Image deleted: its outbox row cascaded away with it. Rollback (nothing was written).
        None => return Ok(OutboxCompletion::Gone),
    };
    let revision = match revision {
        Some(revision) => revision,
        None => return drop_intent(tx, item, OutboxCompletion::Gone),
    };
    if revision > item.edit_revision {
        // A newer save is queued. Keeping its row untouched is the whole point: the older write
        // succeeding says nothing about the newer params.
        return Ok(OutboxCompletion::Superseded);
    }
    if revision < item.edit_revision {
        // The edits row is YOUNGER than this intent, so the row it belonged to was deleted and a
        // new one created. The described edit is gone; the newer row's own intent (if any) is
        // protected by the `edit_revision` guard on the delete.
        return drop_intent(tx, item, OutboxCompletion::Gone);
    }
    if path != item.path || hex_or_raw(&content_hash) != item.content_hash_hex {
        // Same revision, different file: a rename/move/replacement happened while the write was
        // in flight. Whatever was written is not this image's sidecar, and the path in the intent
        // is wrong, so retrying it as-is would write to the wrong place forever.
        if status == "present" {
            // The row still exists, is still present, and still owes exactly this revision — the
            // relink path (`relink_missing_image`, renames adopted by the watcher) keeps the id
            // and its edit. Dropping the intent here would strand `mirrored_revision < revision`
            // with nothing left to retry: relink never re-queues. Re-point the intent at the row's
            // CURRENT identity instead (revision kept — it is still the edit owed; attempts reset
            // — the old path's failures say nothing about the new one) and tell the caller to
            // retry now.
            tx.execute(
                "UPDATE sidecar_outbox
                    SET path = ?2, content_hash = ?3, attempts = 0, last_error = NULL,
                        next_attempt_at = 0
                  WHERE image_id = ?1 AND edit_revision = ?4",
                params![item.image_id, path, content_hash, item.edit_revision],
            )?;
            tx.commit()?;
            return Ok(OutboxCompletion::Requeued);
        }
        // The row went `missing` mid-write: there is no file on disk whose sidecar this could be,
        // but the row may reappear (remount, relink). Leave the intent queued — only a deleted row
        // (or a vanished edit) ever deletes an intent; if the file never comes back, the row's
        // eventual deletion cascades the intent away with it.
        return Ok(OutboxCompletion::SourceChanged);
    }
    // `MAX` is what makes `mirrored_revision` monotonic: a late completion can never rewind it.
    // The `revision` predicate repeats the check inside the write lock, so a save that landed
    // between the read above and this statement still wins.
    tx.execute(
        "UPDATE edits SET mirrored_revision = MAX(mirrored_revision, ?2)
          WHERE image_id = ?1 AND revision = ?2",
        params![item.image_id, item.edit_revision],
    )?;
    tx.execute(
        "DELETE FROM sidecar_outbox WHERE image_id = ?1 AND edit_revision = ?2",
        params![item.image_id, item.edit_revision],
    )?;
    tx.commit()?;
    Ok(OutboxCompletion::Completed)
}

/// Note a failed sidecar write, so the retry loop can back off and the UI can name the image
/// instead of looping silently. Only the intent that was attempted is touched: a newer save has
/// already reset the counters, and blaming it for the older attempt would be wrong.
/// `next_attempt_at` (unix ms) is when the host may try again; it is persisted so wake-ups and
/// restarts honour it.
pub fn fail_outbox(
    conn: &Connection,
    image_id: i64,
    edit_revision: i64,
    error: &str,
    next_attempt_at: i64,
) -> Result<(), LibError> {
    conn.execute(
        "UPDATE sidecar_outbox
            SET attempts = attempts + 1, last_error = :error, next_attempt_at = :next
          WHERE image_id = :id AND edit_revision = :rev",
        named_params! {
            ":error": error,
            ":next": next_attempt_at,
            ":id": image_id,
            ":rev": edit_revision,
        },
    )?;
    Ok(())
}

/// Postpone an intent WITHOUT charging an attempt: the catalog moved under the write, or the source
/// is not present right now. The intent stays queued (only a deleted row/edit ever deletes one);
/// `reason` is recorded as `last_error` when given. Guarded on `edit_revision` like [`fail_outbox`].
pub fn defer_outbox(
    conn: &Connection,
    image_id: i64,
    edit_revision: i64,
    next_attempt_at: i64,
    reason: Option<&str>,
) -> Result<(), LibError> {
    conn.execute(
        "UPDATE sidecar_outbox
            SET next_attempt_at = :next, last_error = COALESCE(:reason, last_error)
          WHERE image_id = :id AND edit_revision = :rev",
        named_params! {
            ":next": next_attempt_at,
            ":reason": reason,
            ":id": image_id,
            ":rev": edit_revision,
        },
    )?;
    Ok(())
}

/// Make intents due now: all rows (`None` — worker start, so a restart retries everything) or one
/// image's (`Some` — relink / remount / status flipped back to `present`). `attempts` and
/// `last_error` are kept so the UI still names a persistently failing image. Returns rows touched.
pub fn reset_outbox_schedule(conn: &Connection, image_id: Option<i64>) -> Result<usize, LibError> {
    Ok(match image_id {
        Some(id) => conn.execute(
            "UPDATE sidecar_outbox SET next_attempt_at = 0
              WHERE image_id = :id AND next_attempt_at <> 0",
            named_params! { ":id": id },
        )?,
        None => conn.execute(
            "UPDATE sidecar_outbox SET next_attempt_at = 0 WHERE next_attempt_at <> 0",
            [],
        )?,
    })
}

/// The image's durable save counter and the revision actually mirrored on disk, or `None` when the
/// image has no edit row. `mirrored != revision` is the "a write is owed" condition; `mirrored`
/// never exceeds `revision` and never moves backwards.
pub fn mirrored_revision(conn: &Connection, image_id: i64) -> Result<Option<(i64, i64)>, LibError> {
    Ok(conn
        .query_row(
            "SELECT revision, mirrored_revision FROM edits WHERE image_id = ?1",
            params![image_id],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?)
}

/// Durable mirror state of one image's edit, for display: the save counter, the revision actually
/// on disk, and the failure of the outbox intent for THIS revision (if any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorState {
    pub revision: i64,
    pub mirrored_revision: i64,
    /// `last_error` of the queued intent for exactly `revision`; `None` before any failed attempt.
    pub last_error: Option<String>,
}

/// `None` when the image has no edit row.
pub fn mirror_state(conn: &Connection, image_id: i64) -> Result<Option<MirrorState>, LibError> {
    Ok(conn
        .query_row(
            "SELECT e.revision, e.mirrored_revision, o.last_error
               FROM edits e
          LEFT JOIN sidecar_outbox o
                 ON o.image_id = e.image_id AND o.edit_revision = e.revision
              WHERE e.image_id = :image_id",
            named_params! { ":image_id": image_id },
            |r| {
                Ok(MirrorState {
                    revision: r.get(0)?,
                    mirrored_revision: r.get(1)?,
                    last_error: r.get(2)?,
                })
            },
        )
        .optional()?)
}

/// Delete a stale intent (guarded on `edit_revision`, so a newer save's row survives) and report
/// `outcome`. Commits: dropping a dead intent is a real state change, not a no-op.
fn drop_intent(
    tx: Transaction<'_>,
    item: &OutboxItem,
    outcome: OutboxCompletion,
) -> Result<OutboxCompletion, LibError> {
    tx.execute(
        "DELETE FROM sidecar_outbox WHERE image_id = ?1 AND edit_revision = ?2",
        params![item.image_id, item.edit_revision],
    )?;
    tx.commit()?;
    Ok(outcome)
}

/// Hex for the 32-byte digests this catalog stores; anything unexpected is handed back verbatim
/// (debug-printable) rather than failing the whole drain of the queue.
fn hex_or_raw(bytes: &[u8]) -> String {
    match <[u8; 32]>::try_from(bytes) {
        Ok(digest) => core_raw::hex(&digest),
        Err(_) => format!("<{} bytes>", bytes.len()),
    }
}
