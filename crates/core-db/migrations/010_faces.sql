-- 010_faces.sql — face recognition + People.
--
-- `person`  : an identity (named, or NULL name = an unnamed auto-cluster shown under "Suggested").
-- `face`     : one detected face on one image, soft-linked to a person with a lifecycle `status`.
-- `face_embedding` : the L2-normalized ArcFace vector for a face (split out so it can later move to a
--                    vector index without touching `face`); `model_tag` lets a model swap invalidate it.
-- `face_rejection` : "not this person" — pairs the clustering must never re-suggest.
--
-- Note the intentional circular reference (person.thumbnail_face_id ↔ face.person_id): SQLite resolves
-- foreign keys lazily, so creating `person` before `face` is fine.

CREATE TABLE person (
  id                INTEGER PRIMARY KEY,
  name              TEXT,                                    -- NULL = unnamed "Suggested" cluster
  hidden            INTEGER NOT NULL DEFAULT 0,              -- excluded from sidebar + search
  thumbnail_face_id INTEGER REFERENCES face(id) ON DELETE SET NULL,  -- key photo
  created_at        INTEGER NOT NULL,
  updated_at        INTEGER NOT NULL
) STRICT;

CREATE TABLE face (
  id            INTEGER PRIMARY KEY,
  asset_id      INTEGER NOT NULL REFERENCES images(id) ON DELETE CASCADE,
  person_id     INTEGER REFERENCES person(id) ON DELETE SET NULL,
  bbox_x1       REAL NOT NULL,                              -- normalized [0,1] top-left
  bbox_y1       REAL NOT NULL,
  bbox_x2       REAL NOT NULL,                              -- normalized [0,1] bottom-right
  bbox_y2       REAL NOT NULL,
  kps           BLOB NOT NULL,                              -- 10 f32 (5 landmark pairs), source px
  quality_score REAL NOT NULL,                              -- face area × sharpness (key-photo/thumb)
  det_score     REAL NOT NULL,                              -- detector confidence [0,1]
  source        TEXT NOT NULL DEFAULT 'ml' CHECK (source IN ('ml','manual')),
  status        TEXT NOT NULL DEFAULT 'unconfirmed'
                  CHECK (status IN ('unconfirmed','confirmed','rejected','ignored')),
  deferred      INTEGER NOT NULL DEFAULT 0,                 -- 1 = pending the next clustering sweep
  model_version TEXT NOT NULL,
  created_at    INTEGER NOT NULL
) STRICT;

CREATE TABLE face_embedding (
  face_id   INTEGER PRIMARY KEY REFERENCES face(id) ON DELETE CASCADE,
  dim       INTEGER NOT NULL,                               -- 512 (ArcFace)
  vector    BLOB NOT NULL,                                  -- L2-normalized f32[dim], little-endian
  model_tag TEXT NOT NULL                                   -- swap → re-embed + re-cluster
) STRICT;

CREATE TABLE face_rejection (
  face_id   INTEGER NOT NULL REFERENCES face(id) ON DELETE CASCADE,
  person_id INTEGER NOT NULL REFERENCES person(id) ON DELETE CASCADE,
  PRIMARY KEY (face_id, person_id)
) STRICT;

CREATE INDEX idx_face_person ON face(person_id) WHERE person_id IS NOT NULL;
CREATE INDEX idx_face_status ON face(status);
CREATE INDEX idx_face_asset  ON face(asset_id);
