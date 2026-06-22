-- Keyset (seek) pagination for the unified AI-scan dirty-stage walk. The todo query is
-- `WHERE status='present' AND id > ?cursor ORDER BY id LIMIT n` with per-stage LEFT JOINs onto
-- analysis_results. idx_images_browse (status, capture_date, id) can't serve the id-seek because
-- capture_date sits between the status prefix and id, forcing a temp sort over the whole present
-- partition at 10k–100k images. This index walks status='present' in id order directly.
CREATE INDEX idx_images_status_id ON images(status, id);
