-- ToolHub schema, base tables. See PLAN.md §6.
-- TIMESTAMP columns hold SQLite ISO 8601 strings (UTC).

CREATE TABLE tools (
    id               TEXT PRIMARY KEY,
    type             TEXT NOT NULL,
    name             TEXT NOT NULL,
    source_repo      TEXT,
    install_path     TEXT,
    description      TEXT,
    long_description TEXT,
    category         TEXT,
    triggers         TEXT,
    examples         TEXT,
    invocation       TEXT,
    requires         TEXT,
    enabled          INTEGER NOT NULL DEFAULT 1,
    added_at         TIMESTAMP NOT NULL,
    last_seen_at     TIMESTAMP NOT NULL,
    last_used_at     TIMESTAMP
);

CREATE INDEX idx_tools_type     ON tools(type);
CREATE INDEX idx_tools_category ON tools(category);

CREATE TABLE usage_events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    tool_id      TEXT NOT NULL REFERENCES tools(id),
    session_id   TEXT,
    project      TEXT,
    task_text    TEXT,
    outcome      TEXT,
    duration_ms  INTEGER,
    cost_usd     REAL,
    occurred_at  TIMESTAMP NOT NULL
);

CREATE INDEX idx_usage_tool ON usage_events(tool_id, occurred_at DESC);

CREATE TABLE tool_scores (
    tool_id            TEXT PRIMARY KEY REFERENCES tools(id),
    success_rate       REAL,
    sample_size        INTEGER,
    avg_cost_usd       REAL,
    median_duration_ms INTEGER,
    score_updated_at   TIMESTAMP
);

CREATE TABLE sources (
    id              TEXT PRIMARY KEY,
    type            TEXT NOT NULL,
    location        TEXT NOT NULL,
    last_pulled_at  TIMESTAMP,
    last_commit_sha TEXT
);
