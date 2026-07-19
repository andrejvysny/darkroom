-- Links a generated panorama image back to the source frames it was stitched from. `pano_image_id`
-- is the synthetic merged image (an ordinary `images` row); `source_image_id` is one of the frames
-- that fed the stitch; `position` orders the frames (capture/left-to-right order) for re-stitch or
-- a "show source frames" UI. Both FKs cascade: deleting the pano image or any source frame drops
-- just the link row — the sibling image rows are untouched.
CREATE TABLE panorama_sources (
  pano_image_id   INTEGER NOT NULL REFERENCES images(id) ON DELETE CASCADE,
  source_image_id INTEGER NOT NULL REFERENCES images(id) ON DELETE CASCADE,
  position        INTEGER NOT NULL,
  PRIMARY KEY (pano_image_id, source_image_id)
) STRICT;

CREATE INDEX idx_panorama_sources_source ON panorama_sources(source_image_id);
