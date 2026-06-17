-- Manual ground-truth labels set by the user (distinct from AI `image_detections`, so an analyze
-- pass never overwrites human truth). Doubles as the labeled dataset for the detection eval harness.
-- Tri-state per field: NULL = unlabeled, 0 = absent, 1 = present. STRICT; FK ON DELETE CASCADE.

CREATE TABLE image_user_labels (
  image_id        INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE,
  contains_person INTEGER CHECK (contains_person IN (0, 1)),
  contains_animal INTEGER CHECK (contains_animal IN (0, 1)),
  updated_at      INTEGER NOT NULL
) STRICT;
