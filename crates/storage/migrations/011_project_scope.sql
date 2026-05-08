-- Per-project skill scoping. `scope='project'` rows are ingested on-the-fly
-- from `<cwd>/.claude/skills/` whenever a recommend is issued from that cwd.
-- `scope_root` is the canonicalised project directory (only set when
-- scope='project'). The partial index makes the per-cwd lookup in the
-- ProjectScopeReranker O(log n) regardless of catalog size.

ALTER TABLE tools ADD COLUMN scope      TEXT NOT NULL DEFAULT 'user';
ALTER TABLE tools ADD COLUMN scope_root TEXT;

CREATE INDEX IF NOT EXISTS idx_tools_scope_root
    ON tools(scope_root)
    WHERE scope = 'project';
