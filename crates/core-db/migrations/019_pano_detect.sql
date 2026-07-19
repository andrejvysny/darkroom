-- Panorama-detection suggestions. `pano_detect_groups` is one candidate stitch group found by the
-- background scan (keyed by `member_key`, a blake3 hash of the sorted member content hashes, so
-- rescans/re-imports upsert the same row instead of duplicating it — dismissed/merged status
-- survives). `pano_detect_members` lists the frames in a group, ordered by `position` (capture
-- time); deleting a group or a member image drops just the link row. `pano_detect_scan` marks the
-- last algo version each image was scanned at, so incremental scans can skip unchanged images.
CREATE TABLE pano_detect_groups (
  id               INTEGER PRIMARY KEY,
  member_key       TEXT NOT NULL UNIQUE,
  algo_version     TEXT NOT NULL,
  confidence       REAL NOT NULL,
  detected_at      INTEGER NOT NULL,
  status           TEXT NOT NULL DEFAULT 'suggested'
                    CHECK (status IN ('suggested','dismissed','merged')),
  merged_image_id  INTEGER REFERENCES images(id) ON DELETE SET NULL
) STRICT;

CREATE TABLE pano_detect_members (
  group_id INTEGER NOT NULL REFERENCES pano_detect_groups(id) ON DELETE CASCADE,
  image_id INTEGER NOT NULL REFERENCES images(id) ON DELETE CASCADE,
  position INTEGER NOT NULL,
  PRIMARY KEY (group_id, image_id)
) STRICT;

CREATE INDEX idx_pano_detect_members_image ON pano_detect_members(image_id);

CREATE TABLE pano_detect_scan (
  image_id     INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE,
  algo_version TEXT NOT NULL,
  scanned_at   INTEGER NOT NULL
) STRICT;
