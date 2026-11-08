-- Per-intent retry eligibility for the sidecar outbox (extends 026).
--
-- 026 kept backoff only in the worker's memory, and `pending_outbox` had no eligibility filter:
-- every wake-up (poll tick, notify) re-attempted every row regardless of its backoff, and the same
-- oldest 64 unavailable rows were re-selected forever, starving a healthy 65th. `next_attempt_at`
-- (unix milliseconds) makes eligibility durable: a row is attempted only once `next_attempt_at <=
-- now`, the drain pages through ALL due rows by keyset, and a restart simply resets the column to 0
-- (attempts / last_error are preserved). Existing rows default to 0 = due immediately.
ALTER TABLE sidecar_outbox ADD COLUMN next_attempt_at INTEGER NOT NULL DEFAULT 0;

CREATE INDEX idx_sidecar_outbox_due ON sidecar_outbox(next_attempt_at, enqueued_at, image_id);
