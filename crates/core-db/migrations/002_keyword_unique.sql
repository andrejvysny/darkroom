-- Keywords are flat tags in v1 (hierarchy column kept for the future). Enforce a single row per
-- name, case-insensitively, so create-or-get never produces duplicates and lookups are indexed.
CREATE UNIQUE INDEX idx_keywords_name ON keywords(name COLLATE NOCASE);
