-- Phase 9: Auto-Tuner — learn from `false_positive` + `bypassed` signals.
--
-- `agent_suggestions.false_positive` is flipped from the web UI when the user
-- manually flags a recommendation as wrong. `agent_suggestions.bypassed` is
-- flipped by the PreToolUse hook the second time the model retries a call
-- that Quiver vetoed (the single-veto-per-tuple rule lets the second attempt
-- through and records the override as a likely false-positive).
--
-- Until now both signals were inert — `recompute_scores` only read
-- `usage_events`. Migration 010 lets the auto-tuner aggregate FP+bypass into
-- `tool_scores` so the recommender ranking applies a permanent demerit until
-- decay erodes it.
--
-- Columns:
--   demerit_count            — sum of time-decayed FP+bypass weights for the
--                              tool. Half-life env-tunable
--                              (`QUIVER_DEMERIT_HALFLIFE_DAYS`, default 14).
--   demerit_updated_at       — RFC3339 timestamp of the last recompute that
--                              touched the tool's demerit fields.
--   demerit_signatures_json  — JSON `[{"sig": "Bash:cargo build",
--                              "weight": 0.87}, …]`, the top-N most-recent
--                              FP/bypass signatures with their decayed
--                              weights, used by the per-task Jaccard penalty.

ALTER TABLE tool_scores ADD COLUMN demerit_count           REAL NOT NULL DEFAULT 0;
ALTER TABLE tool_scores ADD COLUMN demerit_updated_at      TEXT;
ALTER TABLE tool_scores ADD COLUMN demerit_signatures_json TEXT;
