-- Links a camera-generated companion file (the JPEG or HEIF a body writes alongside a RAW in
-- "RAW+JPEG" / "RAW+HEIF" mode) to the RAW it was captured with. Both files remain ordinary `images`
-- rows — nothing is hidden from the catalog, only from the default Library query, which skips any
-- row appearing here as a `secondary_image_id` (see `core-library::query`). The RAW is always the
-- primary; a shot may carry several companions (CR3 + JPG + HIF), hence `secondary_image_id` — not
-- the pair — is the primary key: a companion belongs to exactly one RAW.
--
-- Both FKs cascade: deleting either file drops just the link row, never its partner. Pairs are made
-- at import time (opt-in, per import) and can be broken by hand (`image_pair_unlink`), which is why
-- the link is a separate table rather than a column on `images`.
CREATE TABLE image_pairs (
  secondary_image_id INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE,
  primary_image_id   INTEGER NOT NULL REFERENCES images(id) ON DELETE CASCADE,
  created_at         INTEGER NOT NULL,
  CHECK (primary_image_id <> secondary_image_id)
) STRICT;

CREATE INDEX idx_image_pairs_primary ON image_pairs(primary_image_id);
