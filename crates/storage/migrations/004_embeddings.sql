-- Tool embedding vectors. BLOB = packed little-endian f32,
-- 384 floats (1536 bytes) per row for BAAI/bge-small-en-v1.5.
-- See PLAN.md §6 (vec0 alternative when sqlite-vec extension is unavailable).

CREATE TABLE tool_embeddings (
    tool_id   TEXT PRIMARY KEY REFERENCES tools(id) ON DELETE CASCADE,
    embedding BLOB NOT NULL
);
