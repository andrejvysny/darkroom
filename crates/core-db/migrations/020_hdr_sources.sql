-- Links a merged-HDR image back to the bracket frames it was blended from. `hdr_image_id` is the
-- synthetic merged image (an ordinary `images` row); `source_image_id` is one of the frames that fed
-- the merge; `position` orders the frames (the same order `core_raw::HdrSourceInfo` used) for a "show
-- source frames" UI; `relative_ev` is the frame's EV offset from the reference (metered) frame — the
-- same value baked into the EXR's `darkroom:sources` attribute (`HdrSourceInfo::relative_ev`), just
-- queryable without decoding the file. Both FKs cascade: deleting the merged image or any source
-- frame drops just the link row — the sibling image rows are untouched.
CREATE TABLE hdr_sources (
  hdr_image_id    INTEGER NOT NULL REFERENCES images(id) ON DELETE CASCADE,
  source_image_id INTEGER NOT NULL REFERENCES images(id) ON DELETE CASCADE,
  position        INTEGER NOT NULL,
  relative_ev     REAL NOT NULL,
  PRIMARY KEY (hdr_image_id, position)
) STRICT;

CREATE INDEX idx_hdr_sources_source ON hdr_sources(source_image_id);
