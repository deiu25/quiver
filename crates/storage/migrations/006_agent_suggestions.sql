-- Phase 6: per-suggestion log for the daily-task agent.
-- The agent loop writes one row per top-1 suggestion produced when a new
-- user message arrives in a watched session. When the user actually invokes
-- the suggested tool within an acceptance window, the row is updated to
-- accepted=1, accepted_at=<ts>, giving us a real reinforcement signal that
-- complements `tool_scores` (which only knows tool-result outcomes).

CREATE TABLE agent_suggestions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id      TEXT NOT NULL,
    tool_id         TEXT NOT NULL REFERENCES tools(id),
    task_text       TEXT,
    score           REAL,
    suggested_at    TIMESTAMP NOT NULL,
    accepted        INTEGER NOT NULL DEFAULT 0,
    accepted_at     TIMESTAMP
);

CREATE INDEX idx_suggestions_session ON agent_suggestions(session_id, suggested_at DESC);
CREATE INDEX idx_suggestions_tool    ON agent_suggestions(tool_id, suggested_at DESC);
