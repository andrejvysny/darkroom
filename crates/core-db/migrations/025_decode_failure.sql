-- Why a RAW file is NOT in the catalog — one row per path that failed to decode.
--
-- Before this table a file that rawler cannot decode was invisible: `IndexStats.failed` counted it,
-- nothing recorded it, and every scan / watcher wake re-read + re-decoded the same unknown body
-- forever. Three things need the record:
--   * the UI, to name the bodies it cannot open ("Nikon Z 9 — High Efficiency NEF") instead of
--     silently dropping files,
--   * the scanner, to STOP re-decoding a permanently-unsupported file (`kind='unsupported'`),
--   * the retry rule: `decoder_version` + `(file_size, mtime)` say whether the verdict still holds.
--     A decoder upgrade changes `decoder_version`, so every row becomes retryable automatically; a
--     re-copied/edited file changes size or mtime, so it is retried too.
--
-- `path` (not an image id) is the key: these files have no `images` row by definition. `folder_id`
-- is the watched root it was found under, NULL for an import source (a card, not a library folder)
-- — and ON DELETE SET NULL so removing a root never deletes the record of what it could not read.
-- `kind` mirrors `core_raw::FailureKind`; `make`/`model` are rawler's own body identification, so
-- the UI can group by camera without a second (impossible) metadata read.
CREATE TABLE decode_failure (
  path            TEXT PRIMARY KEY,
  folder_id       INTEGER REFERENCES folders(id) ON DELETE SET NULL,
  filename        TEXT NOT NULL,
  kind            TEXT NOT NULL CHECK (kind IN ('unsupported','corrupt','io','panic','other')),
  make            TEXT,
  model           TEXT,
  detail          TEXT NOT NULL,
  file_size       INTEGER NOT NULL DEFAULT 0,
  mtime           INTEGER,
  decoder_version TEXT NOT NULL,
  first_seen      INTEGER NOT NULL,
  last_seen       INTEGER NOT NULL,
  attempts        INTEGER NOT NULL DEFAULT 1
) STRICT;

-- "Most recently seen first" — the order the Unsupported list renders in.
CREATE INDEX idx_decode_failure_seen ON decode_failure(last_seen DESC);
-- Group-by-camera for the Unsupported list's per-body rows.
CREATE INDEX idx_decode_failure_model ON decode_failure(kind, make, model);
