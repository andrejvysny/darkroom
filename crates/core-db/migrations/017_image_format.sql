-- Source format bucket per image ('raw' | 'jpeg' | 'png'), to drive the by-filetype filters in the
-- Import dialog and the Library query. Nullable + backfilled: every existing row predates JPEG/PNG
-- support, so it is necessarily RAW. New rows set it at insert time from the file extension
-- (core-library::image_kind). Indexed for cheap WHERE format = :format filtering.
ALTER TABLE images ADD COLUMN format TEXT;
UPDATE images SET format = 'raw' WHERE format IS NULL;
CREATE INDEX IF NOT EXISTS idx_images_format ON images(format);
