-- Persistent named develop snapshots (the saved side of the hybrid history model). Each row is a
-- FULL DevelopParams JSON for an image at a save-point, with a user name. Session undo/redo lives
-- only in the frontend (in-memory); these snapshots survive restart. Restoring a snapshot applies its
-- params as a normal edit. `ON DELETE CASCADE` cleans up when an image is removed.
CREATE TABLE develop_snapshots (
  id              INTEGER PRIMARY KEY,
  image_id        INTEGER NOT NULL REFERENCES images(id) ON DELETE CASCADE,
  name            TEXT NOT NULL,
  params          TEXT NOT NULL,                -- full DevelopParams JSON
  process_version INTEGER NOT NULL,
  created_at      INTEGER NOT NULL,
  UNIQUE (image_id, name)
) STRICT;

CREATE INDEX idx_snapshots_image ON develop_snapshots(image_id, created_at);
