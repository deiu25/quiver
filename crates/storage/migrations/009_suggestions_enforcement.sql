-- Phase 8 v3: enforcement signals on the per-suggestion log.
--
-- Strict mode (`QUIVER_ENFORCE=strict`) extends the agent loop with three
-- coercion layers (UserPromptSubmit imperative + PreToolUse veto + Stop
-- circuit-breaker). Every veto / bypass / nudge writes back to this table so
-- later analytics (and an opt-in auto-tuner) can score Quiver's own
-- recommendations.
--
-- Columns:
--   level           — 'hint' | 'strong' | 'mandatory' (band the suggestion
--                     was emitted at; null for legacy rows).
--   task_signature  — short, stable digest of (tool_name, salient input)
--                     used by the PreToolUse single-veto-per-tuple rule.
--                     Null for UserPromptSubmit-origin rows.
--   vetoed          — 1 once Quiver has emitted a `permissionDecision: deny`
--                     against a competing tool call for this row's tuple.
--   bypassed        — 1 if the model re-invoked the same tool after the
--                     veto (the second attempt is allowed). Flags a likely
--                     false-positive.
--   nudged          — 1 once the Stop hook has emitted `decision: block`
--                     citing this row. Prevents loops.
--   false_positive  — 1 when the user (web UI / `quiver stats`) marks the
--                     suggestion as wrong. Feeds future auto-tuner.

ALTER TABLE agent_suggestions ADD COLUMN level           TEXT;
ALTER TABLE agent_suggestions ADD COLUMN task_signature  TEXT;
ALTER TABLE agent_suggestions ADD COLUMN vetoed          INTEGER NOT NULL DEFAULT 0;
ALTER TABLE agent_suggestions ADD COLUMN bypassed        INTEGER NOT NULL DEFAULT 0;
ALTER TABLE agent_suggestions ADD COLUMN nudged          INTEGER NOT NULL DEFAULT 0;
ALTER TABLE agent_suggestions ADD COLUMN false_positive  INTEGER NOT NULL DEFAULT 0;

CREATE INDEX idx_suggestions_signature
    ON agent_suggestions(session_id, task_signature);
CREATE INDEX idx_suggestions_pending_mandatory
    ON agent_suggestions(session_id, level, accepted, nudged, suggested_at DESC);
