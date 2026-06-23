-- Versioned visual features for same-scene / near-duplicate detection. These supersede the legacy
-- images.phash dHash column while keeping it for backward compatibility with older catalogs/code.
CREATE TABLE image_similarity_features (
  image_id        INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE,
  feature_version INTEGER NOT NULL,
  dhash64         INTEGER NOT NULL,
  phash64         INTEGER NOT NULL,
  tiny_luma32     BLOB NOT NULL,
  color_grid4x4   BLOB NOT NULL,
  computed_at     INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_similarity_features_version ON image_similarity_features(feature_version);
