-- 024_suggestions.sql — on-device pick/reject preference model + its per-image scores.
--
-- `suggestion_model` is an append-only history of every fit: the serialized `core-suggest` head plus
-- the numbers it was judged by. Nothing is ever overwritten, so a promotion that turns out badly can
-- be traced (and rolled back) against the exact weights that produced it. Which row is LIVE is not a
-- column here but a pointer in `app_meta` (`suggest_current_model_id`) — one authoritative place,
-- so two rows can never both claim to be current. `promoted` records that a row was live at some
-- point (an audit bit, not the pointer).
--
-- `feature_version` + `embedding_model_tag` are stored next to the weights because both invalidate
-- them: a reordered feature vector or a swapped image encoder makes position-indexed weights read
-- garbage rather than fail. Scoring compares them and refuses instead of coercing.
--
-- `image_suggestion` holds the latest score per image (one row, replaced by each scoring pass) plus
-- the model that produced it, so a stale row from a superseded model is recognisable and cleanable.
-- `withheld` marks a small deterministic slice whose badge the UI hides: those images keep an
-- unprompted label, which is the only kind that can honestly measure the model later.
CREATE TABLE suggestion_model (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  created_at  INTEGER NOT NULL,
  model_json  TEXT NOT NULL,            -- core-suggest Model serde JSON
  feature_version INTEGER NOT NULL,
  embedding_model_tag TEXT NOT NULL,
  n_pos INTEGER NOT NULL, n_neg INTEGER NOT NULL,
  cv_auc REAL NOT NULL, cv_auprc REAL NOT NULL,
  top1_agreement REAL,                  -- NULL when no burst had both a pick and a reject
  promoted    INTEGER NOT NULL DEFAULT 0  -- 1 = was promoted to current at some point
) STRICT;

CREATE TABLE image_suggestion (
  image_id  INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE,
  model_id  INTEGER NOT NULL REFERENCES suggestion_model(id),
  score     REAL NOT NULL,
  suggested TEXT NOT NULL CHECK (suggested IN ('none','pick','reject')),
  withheld  INTEGER NOT NULL DEFAULT 0,  -- score computed but badge hidden (unbiased eval bucket)
  scored_at INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_sugg_suggested ON image_suggestion(suggested);
