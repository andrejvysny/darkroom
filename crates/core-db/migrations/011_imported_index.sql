-- Keyset (seek) pagination for the import-date sorts. Mirrors idx_images_browse (003) but keys on
-- imported_at, so `WHERE status='present' AND (imported_at,id) <seek> ORDER BY imported_at,id` walks
-- the index forward (asc) / backward (desc) instead of scanning the whole present partition.
CREATE INDEX idx_images_imported
  ON images(status, imported_at, id);
