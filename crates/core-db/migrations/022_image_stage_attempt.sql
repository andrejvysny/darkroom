-- Per-image, per-stage record of the LAST AI-scan attempt: what ran, when, and whether it failed.
--
-- `analysis_results` stays the canonical record of the last *successful* output for a stage
-- (image × analyzer × model_version, `status='ok'`, payload). It deliberately does NOT record
-- failures: it is keyed on model_version and written with INSERT OR REPLACE, so an error row would
-- overwrite a good result and its payload on any forced re-scan.
--
-- This table is the complement: one row per (image, stage) holding the latest ATTEMPT regardless of
-- outcome. That makes three previously-indistinguishable states distinguishable —
--   * no row            => the stage has never been attempted for this image,
--   * status='ok'       => it ran and succeeded at `model_version`,
--   * status='error'    => it ran and failed; `error` says why (and it stays stale, so it retries).
-- A photo whose decode fails produces no `analysis_results` rows at all, so before this table such a
-- photo was silently re-attempted on every scan forever with nothing to show for it.
--
-- `stage_id` is the analyzer id (`object_detection`, `animal_detection`, `presence_probe`,
-- `caption`, `face_detection`) plus the pseudo-stage `panorama`, whose own state lives in
-- `pano_detect_scan`; the read side unions them so one photo shows one list of stages.
-- `model_version` is the version attempted, so a row left behind by an older model reads as
-- "pending" rather than "done" — matching what the scan would actually do next.
CREATE TABLE image_stage_attempt (
  image_id      INTEGER NOT NULL REFERENCES images(id) ON DELETE CASCADE,
  stage_id      TEXT NOT NULL,
  model_version TEXT NOT NULL,
  attempted_at  INTEGER NOT NULL,
  status        TEXT NOT NULL CHECK (status IN ('ok','error')),
  error         TEXT,
  PRIMARY KEY (image_id, stage_id)
) STRICT;

-- "Recently scanned" ordering + the per-photo MAX(attempted_at) lookup behind "last scanned".
CREATE INDEX idx_stage_attempt_time ON image_stage_attempt(attempted_at DESC);

-- Backfill so the readout is populated for libraries scanned before this table existed.
-- Existing successes only — past failures were never recorded and cannot be recovered.
-- A stage with rows at several model versions keeps the newest attempt (MAX(ran_at)); the
-- correlated subquery picks that row's version rather than an arbitrary one.
INSERT OR IGNORE INTO image_stage_attempt
    (image_id, stage_id, model_version, attempted_at, status, error)
SELECT a.image_id,
       a.analyzer_id,
       a.model_version,
       a.ran_at,
       'ok',
       NULL
  FROM analysis_results a
 WHERE a.status = 'ok'
   AND a.ran_at = (SELECT MAX(b.ran_at)
                     FROM analysis_results b
                    WHERE b.image_id = a.image_id
                      AND b.analyzer_id = a.analyzer_id
                      AND b.status = 'ok');

-- Panorama detection keeps its own marker table; project it in as the `panorama` stage.
INSERT OR IGNORE INTO image_stage_attempt
    (image_id, stage_id, model_version, attempted_at, status, error)
SELECT image_id, 'panorama', algo_version, scanned_at, 'ok', NULL
  FROM pano_detect_scan;
