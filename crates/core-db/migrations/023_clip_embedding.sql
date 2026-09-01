-- 023_clip_embedding.sql — per-image MobileCLIP scene embedding.
--
-- The foundation for pick/reject suggestions: one 512-d L2-normalized vector per image, the same
-- feature the presence probe already scores, kept so a downstream model can be trained/served
-- without re-running CLIP over the library.
--
-- Shaped exactly like `face_embedding` (010_faces.sql): `dim` + a little-endian f32 BLOB + a
-- `model_tag` so swapping the encoder invalidates every stored vector rather than silently mixing
-- two embedding spaces. One row per image (PRIMARY KEY on image_id) — unlike faces there is nothing
-- to key on but the photo itself.
--
-- Deliberately NOT `analysis_results.payload`: the scan writes only a `{"dim":512}` marker there.
-- 512 floats as JSON is ~6 kB/image, i.e. ~300 MB across a 50k library, for a value nothing reads
-- as text.
CREATE TABLE image_embedding (
  image_id    INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE,
  dim         INTEGER NOT NULL,                          -- 512 (MobileCLIP-S1)
  vector      BLOB NOT NULL,                             -- L2-normalized f32[dim], little-endian
  model_tag   TEXT NOT NULL,                             -- swap → re-embed
  computed_at INTEGER NOT NULL
) STRICT;

-- Tick the new stage for installations that already saved a scan selection. The stored list is a
-- whitelist of enabled stages, so a stage introduced after it was written is indistinguishable from
-- one the user unticked — without this, every existing library would silently never embed.
-- Guarded on `json_valid` so a hand-corrupted value is left alone (`scan_stages()` already falls
-- back to the defaults for anything unparseable), and on the substring so re-running is a no-op.
UPDATE app_meta
   SET value = json_insert(value, '$[#]', 'embeddings')
 WHERE key = 'scan_stages'
   AND json_valid(value)
   AND json_type(value) = 'array'
   AND value NOT LIKE '%embeddings%';
