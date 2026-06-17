-- Behavioral-signal capture for future on-device AI (dedup / best-shot / lighting / auto-edit).
--
-- `user_events` is an APPEND-ONLY fact log — one immutable row per user decision/label. Never
-- UPDATE/DELETE (the `wipe`/reset action may clear it, but normal operation only inserts). It records
-- the full candidate set + what the system suggested + what the user chose, stamped with time/order +
-- app/process version, so the data stays counterfactually correct and reusable across models.
--
-- `image_features` holds per-image MODEL INPUTS (no history) computed once at index time; overwritten
-- in place. Distinct from the event log (labels) and from AI outputs (`image_detections`).

CREATE TABLE user_events (
  id               INTEGER PRIMARY KEY,         -- monotonic insertion order
  ts               INTEGER NOT NULL,            -- Unix milliseconds
  session_id       TEXT NOT NULL,               -- UUID per app launch
  app_version      TEXT NOT NULL,
  process_version  INTEGER,                     -- develop pipeline version (from edits.process_version)
  suggester_id     TEXT,                        -- NULL until a model suggests; model-version string after
  event_type       TEXT NOT NULL,               -- e.g. 'culling.flag_pick', 'dedup.keeper_chosen'
  image_id         INTEGER REFERENCES images(id) ON DELETE SET NULL,
  group_id         TEXT,                        -- ties burst / dedup / import sets together
  candidate_ids    TEXT,                        -- JSON int array: the FULL choice set shown
  chosen_id        INTEGER,                     -- kept / picked item
  rejected_ids     TEXT,                        -- JSON int array: explicit negatives
  suggestion_id    INTEGER,                     -- system's suggested choice (when suggester_id set)
  suggestion_score REAL,
  params_before    TEXT,                        -- DevelopParams JSON snapshot, pre-change
  params_after     TEXT,                        -- DevelopParams JSON snapshot, post-commit
  scalar_key       TEXT,                        -- single-slider reset events
  scalar_before    REAL,
  scalar_after     REAL,
  stars            INTEGER,
  flag             TEXT,
  color_label      TEXT,
  latency_ms       INTEGER,                     -- decision latency (selection/show -> action)
  touch_count      INTEGER,                     -- slider interactions in the edit session
  is_implicit      INTEGER NOT NULL DEFAULT 0,  -- 1 = weak/implicit signal (weight, not hard label)
  context          TEXT                         -- JSON catch-all for extra fields
) STRICT;

CREATE INDEX idx_ue_ts    ON user_events(ts);
CREATE INDEX idx_ue_image ON user_events(image_id);
CREATE INDEX idx_ue_group ON user_events(group_id);
CREATE INDEX idx_ue_type  ON user_events(event_type);

CREATE TABLE image_features (
  image_id         INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE,
  wb_as_shot_rg    REAL,            -- as-shot white-balance chromaticity (R/G)
  wb_as_shot_bg    REAL,            -- as-shot white-balance chromaticity (B/G)
  hist_logchroma   BLOB,            -- 32x32 f32 log-chroma histogram (AWB feature)
  hist_luma        BLOB,            -- 256 f32 luma histogram
  mean_log_luma    REAL,
  clip_hi          REAL,            -- fraction of pixels clipped high
  clip_lo          REAL,            -- fraction of pixels clipped low
  dynamic_range_ev REAL,
  sharpness        REAL,            -- variance of Laplacian (focus/quality proxy)
  computed_at      INTEGER NOT NULL
) STRICT;
