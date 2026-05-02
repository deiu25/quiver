-- Vector index for embedding-based recall. Requires the `sqlite-vec`
-- extension to be loaded on the connection before this migration runs.
-- See PLAN.md §6 and §3 (fastembed-rs BAAI/bge-small-en-v1.5, 384-dim).

CREATE VIRTUAL TABLE tools_vec USING vec0(
    tool_id   TEXT PRIMARY KEY,
    embedding FLOAT[384]
);
