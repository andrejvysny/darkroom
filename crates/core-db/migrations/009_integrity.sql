-- Catalog scale schema (additive). Forward-looking infrastructure for the 10k–50k target; the
-- features that consume these land in a later scale pass (see CURRENT_STATE / TODO).
--
-- NOTE on content_hash uniqueness: a UNIQUE index is deliberately NOT added. `insert_image` already
-- rejects a byte-identical hash before inserting (single-connection serialized — no race to guard),
-- and the byte-identical dedup feature (`core-dedup::find_byte_identical`) groups rows by shared
-- content_hash, so a UNIQUE constraint would both be redundant and break that feature. The non-unique
-- `idx_images_content_hash` from 001 stays.

-- RESERVED: indexed path lookup for a future per-file rescan skip (today `existing_paths` loads a
-- HashSet, which is fine at ≤50k). Also speeds any path-keyed lookup / reconcile.
CREATE INDEX IF NOT EXISTS idx_images_path ON images(path);

-- RESERVED: DB-tracked thumbnail recency for a future LRU that doesn't depend on filesystem atime
-- (atime is reliable on macOS APFS today, so the current atime-based eviction still works).
ALTER TABLE images ADD COLUMN last_access INTEGER;
