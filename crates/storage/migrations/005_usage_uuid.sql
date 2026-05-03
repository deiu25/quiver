-- Phase 4: dedupe key for replayed tool_use events.
-- `uuid` mirrors the `id` field on the assistant `tool_use` content block in
-- Claude Code session JSONL, letting `quiver score` re-run idempotently.

ALTER TABLE usage_events ADD COLUMN uuid TEXT;

CREATE UNIQUE INDEX idx_usage_uuid     ON usage_events(uuid)        WHERE uuid IS NOT NULL;
CREATE        INDEX idx_usage_session  ON usage_events(session_id);
CREATE        INDEX idx_usage_occurred ON usage_events(occurred_at DESC);
