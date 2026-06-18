-- MobileCLIP-S1 linear-probe presence scores (full-image scene classifier), projected from
-- `analysis_results` (analyzer_id 'presence_probe') for fast facet/filter fusion. `p_person`/
-- `p_animal` are calibrated probabilities in [0,1], compared against the baked max-F1 threshold at
-- query time and OR-fused with the detectors. STRICT; FK ON DELETE CASCADE.

CREATE TABLE image_presence (
  image_id      INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE,
  p_person      REAL NOT NULL,
  p_animal      REAL NOT NULL,
  model_version TEXT NOT NULL
) STRICT;
