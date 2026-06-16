-- Scale tuning for medium libraries (10k–50k images): composite indexes matching the Library
-- grid's hot-path queries (status filter + capture-date ordering + id tiebreak), so paginated
-- browsing (ORDER BY … LIMIT/OFFSET) walks an index instead of sorting the whole filtered set.
--
-- One index serves both sort directions: SQLite scans it forward for *_asc and backward for *_desc
-- (capture_date and id flip together in both the asc and desc orderings used by query.rs).

-- Default browse: WHERE status='present' ORDER BY capture_date, id.
CREATE INDEX idx_images_browse
  ON images(status, capture_date, id);

-- Folder-filtered browse (left nav): WHERE status='present' AND folder_id=? ORDER BY capture_date, id.
CREATE INDEX idx_images_folder_browse
  ON images(status, folder_id, capture_date, id);
