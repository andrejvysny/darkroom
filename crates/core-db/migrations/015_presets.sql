-- Develop presets (user + built-in). A preset is a SPARSE subset of DevelopParams: `params` holds
-- only the touched top-level fields (a camelCase JSON object) and `field_keys` lists those keys.
-- Applying a preset overlays only those keys onto the current edit (see
-- core-preset::apply::apply_sparse), so untouched modules are never disturbed. Presets are GLOBAL
-- (not per-image); applying writes the merged result into the image's `edits` row + sidecar via the
-- normal develop_set_edit path. Built-ins (`builtin = 1`) are shipped read-only (no delete/rename).
CREATE TABLE presets (
  id              INTEGER PRIMARY KEY,
  name            TEXT NOT NULL,
  group_name      TEXT NOT NULL DEFAULT 'My Presets',
  builtin         INTEGER NOT NULL DEFAULT 0,
  is_favorite     INTEGER NOT NULL DEFAULT 0,
  field_keys      TEXT NOT NULL,                       -- JSON array of touched top-level field keys
  params          TEXT NOT NULL,                       -- sparse DevelopParams JSON (only those keys)
  process_version INTEGER NOT NULL,
  sort_order      INTEGER NOT NULL DEFAULT 0,
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL,
  UNIQUE (group_name, name)
) STRICT;

CREATE INDEX idx_presets_group ON presets(group_name, sort_order);
