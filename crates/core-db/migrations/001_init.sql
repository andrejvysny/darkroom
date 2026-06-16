-- Darkroom catalog schema v1 (spec §16). STRICT tables; FK enforced at runtime.
-- Connection pragmas (WAL, foreign_keys, synchronous, busy_timeout, cache_size) are set in Rust at open().

CREATE TABLE folders (
  id         INTEGER PRIMARY KEY,
  path       TEXT NOT NULL UNIQUE,
  is_watched INTEGER NOT NULL DEFAULT 1,
  added_at   INTEGER NOT NULL
) STRICT;

CREATE TABLE import_sessions (
  id            INTEGER PRIMARY KEY,
  source_volume TEXT,
  mode          TEXT NOT NULL CHECK (mode IN ('copy','move','reference')),
  started_at    INTEGER NOT NULL,
  finished_at   INTEGER,
  file_count    INTEGER NOT NULL DEFAULT 0,
  skipped_count INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE images (
  id                  INTEGER PRIMARY KEY,
  content_hash        BLOB NOT NULL,
  capture_fingerprint BLOB,
  file_size           INTEGER NOT NULL,
  path                TEXT NOT NULL,
  folder_id           INTEGER REFERENCES folders(id),
  original_filename   TEXT NOT NULL,
  status              TEXT NOT NULL DEFAULT 'present'
                       CHECK (status IN ('present','missing')),
  capture_date        INTEGER,
  camera_make         TEXT,
  camera_model        TEXT,
  body_serial         TEXT,
  lens                TEXT,
  iso                 INTEGER,
  shutter             TEXT,
  aperture            REAL,
  focal_length        REAL,
  width               INTEGER,
  height              INTEGER,
  orientation         INTEGER,
  exif                BLOB,
  imported_at         INTEGER NOT NULL,
  import_session_id   INTEGER REFERENCES import_sessions(id)
) STRICT;

CREATE TABLE edits (
  image_id        INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE,
  process_version INTEGER NOT NULL,
  params          TEXT NOT NULL,
  updated_at      INTEGER NOT NULL
) STRICT;

CREATE TABLE ratings_flags (
  image_id    INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE,
  stars       INTEGER NOT NULL DEFAULT 0 CHECK (stars BETWEEN 0 AND 5),
  flag        TEXT NOT NULL DEFAULT 'none' CHECK (flag IN ('none','pick','reject')),
  color_label TEXT
) STRICT;

CREATE TABLE keywords (
  id        INTEGER PRIMARY KEY,
  name      TEXT NOT NULL,
  parent_id INTEGER REFERENCES keywords(id)
) STRICT;

CREATE TABLE image_keywords (
  image_id   INTEGER REFERENCES images(id) ON DELETE CASCADE,
  keyword_id INTEGER REFERENCES keywords(id) ON DELETE CASCADE,
  PRIMARY KEY (image_id, keyword_id)
) STRICT;

CREATE TABLE collections (
  id       INTEGER PRIMARY KEY,
  name     TEXT NOT NULL,
  is_smart INTEGER NOT NULL DEFAULT 0,
  query    TEXT
) STRICT;

CREATE TABLE collection_images (
  collection_id INTEGER REFERENCES collections(id) ON DELETE CASCADE,
  image_id      INTEGER REFERENCES images(id) ON DELETE CASCADE,
  PRIMARY KEY (collection_id, image_id)
) STRICT;

CREATE TABLE app_meta (
  key   TEXT PRIMARY KEY,
  value TEXT
) STRICT;

CREATE INDEX idx_images_content_hash        ON images(content_hash);
CREATE INDEX idx_images_capture_fingerprint ON images(capture_fingerprint);
CREATE INDEX idx_images_file_size           ON images(file_size);
CREATE INDEX idx_images_folder              ON images(folder_id);
CREATE INDEX idx_images_capture_date        ON images(capture_date);
CREATE INDEX idx_images_camera              ON images(camera_model);
CREATE INDEX idx_rf_stars                   ON ratings_flags(stars);
CREATE INDEX idx_rf_flag                    ON ratings_flags(flag);
CREATE INDEX idx_rf_label                   ON ratings_flags(color_label);
