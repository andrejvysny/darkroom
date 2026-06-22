-- Invalidate suspect legacy face-scan markers. Before the unified pipeline, a face inference error
-- was swallowed and recorded as a SUCCESSFUL zero-face result, so a `face_detection` marker whose
-- payload is exactly {"faces":0} is either a genuine no-face image OR a masked failure — untrusted.
-- Deleting only those rows makes the next scan re-run exactly the suspect set under the new
-- error-retry + reconcile semantics (reconcile preserves any user edits, so this is safe). Markers
-- with faces>0 found real faces and stay. `json_extract` (SQLite JSON1, bundled) reads the count
-- structurally — robust to any payload whitespace / key-order / extra-field drift.
DELETE FROM analysis_results
 WHERE analyzer_id = 'face_detection' AND json_extract(payload, '$.faces') = 0;
