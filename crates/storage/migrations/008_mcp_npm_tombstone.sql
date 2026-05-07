-- Phase B follow-up (PR-C): tombstone failed npm registry lookups so
-- `quiver sync` doesn't re-hit the network for known-bad packages on
-- every run. A row with `not_found = 1` is a negative cache: the
-- helper layer returns `CacheStatus::NotFound` until the row's TTL
-- expires, at which point it gets re-fetched (in case the package was
-- since published).

ALTER TABLE mcp_npm_cache ADD COLUMN not_found INTEGER NOT NULL DEFAULT 0;
