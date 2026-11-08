-- Durable sidecar-mirror intent: an edit and the promise to mirror it into the `.json` sidecar
-- beside the RAW are now committed in the SAME transaction.
--
-- Before this, the mirror intent lived only in an in-memory queue in the app layer. A crash, a
-- force-quit, or a machine sleep between the catalog commit and the sidecar write dropped the
-- queue, and the sidecar silently kept the previous develop params forever — nothing remembered
-- that a write was owed, so nothing retried it after restart. The catalog is the only durable
-- store, so the intent belongs there, next to the edit it describes.
--
-- `edits.revision` is a durable save counter (every save bumps it, whatever the clock says);
-- `mirrored_revision` records the revision whose sidecar write actually succeeded. The two being
-- unequal is the whole "a write is owed" condition, and `mirrored_revision` only ever moves
-- forward — a late completion for an older revision must never rewind it.
--
-- `sidecar_outbox` holds at most one row per image (PK image_id): only the newest intent matters,
-- a superseded one is never written. `content_hash` + `path` are the source identity captured when
-- the intent was queued, so a completion can verify the file is still the same file before it
-- claims the mirror is on disk (a renamed/moved/replaced source makes the write meaningless).
-- `attempts` + `last_error` let the retry loop back off and let the UI name a permanently failing
-- image instead of looping silently.
--
-- Existing rows get `revision = 0, mirrored_revision = 0` and no outbox rows: a sidecar written
-- before this migration is assumed in sync with its edit, exactly as the app assumed before.
ALTER TABLE edits ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE edits ADD COLUMN mirrored_revision INTEGER NOT NULL DEFAULT 0;

CREATE TABLE sidecar_outbox (
  image_id      INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE,
  edit_revision INTEGER NOT NULL,
  content_hash  BLOB NOT NULL,
  path          TEXT NOT NULL,
  enqueued_at   INTEGER NOT NULL,
  attempts      INTEGER NOT NULL DEFAULT 0,
  last_error    TEXT
) STRICT;

-- Retry order: oldest intent first, so a long-blocked image is never starved by newer saves.
CREATE INDEX idx_sidecar_outbox_queued ON sidecar_outbox(enqueued_at, image_id);
