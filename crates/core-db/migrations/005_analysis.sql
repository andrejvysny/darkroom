-- AI scan analysis results (object detection + captioning). STRICT; FK ON DELETE CASCADE.
-- `analysis_results` is the canonical, idempotent, version-gated record (one row per
-- image × analyzer × model_version). `image_detections` / `image_captions` are denormalized
-- projections for fast filtering and display.

CREATE TABLE analysis_results (
  image_id      INTEGER NOT NULL REFERENCES images(id) ON DELETE CASCADE,
  analyzer_id   TEXT NOT NULL,
  model_version TEXT NOT NULL,
  ran_at        INTEGER NOT NULL,
  status        TEXT NOT NULL DEFAULT 'ok' CHECK (status IN ('ok','error')),
  payload       TEXT NOT NULL,
  PRIMARY KEY (image_id, analyzer_id, model_version)
) STRICT;

CREATE INDEX idx_ar_lookup ON analysis_results(image_id, analyzer_id);

CREATE TABLE image_detections (
  id            INTEGER PRIMARY KEY,
  image_id      INTEGER NOT NULL REFERENCES images(id) ON DELETE CASCADE,
  label         TEXT NOT NULL,
  category      TEXT NOT NULL,   -- People | Animals | Vehicles
  confidence    REAL NOT NULL,
  bbox_x0       REAL NOT NULL,
  bbox_y0       REAL NOT NULL,
  bbox_x1       REAL NOT NULL,
  bbox_y1       REAL NOT NULL,
  model_version TEXT NOT NULL
) STRICT;

CREATE INDEX idx_det_image    ON image_detections(image_id);
CREATE INDEX idx_det_category ON image_detections(category);
CREATE INDEX idx_det_label    ON image_detections(label);

CREATE TABLE image_captions (
  image_id      INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE,
  caption       TEXT NOT NULL,
  keywords      TEXT NOT NULL,   -- JSON array of strings
  model_version TEXT NOT NULL,
  generated_at  INTEGER NOT NULL
) STRICT;
