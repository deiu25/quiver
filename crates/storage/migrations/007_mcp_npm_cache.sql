-- Phase B: cache for npm registry lookups during MCP server enrichment.
-- We hit https://registry.npmjs.org/<pkg>/latest at most once per (package,
-- TTL window). Rows are pruned by `fetched_at` age in the helper layer
-- (default TTL 30 days) — no SQL trigger, so a stale row simply gets
-- overwritten on the next miss.

CREATE TABLE mcp_npm_cache (
    package         TEXT PRIMARY KEY,
    fetched_at      TIMESTAMP NOT NULL,
    description     TEXT,
    keywords_json   TEXT,
    repository      TEXT,
    homepage        TEXT,
    readme          TEXT
);

CREATE INDEX idx_mcp_npm_fetched ON mcp_npm_cache(fetched_at);
