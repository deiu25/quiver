-- Per-session intent classification cache. Written by the detached
-- `quiver hook classify-intent` subprocess after UserPromptSubmit; read
-- by `quiver hook pre-tool-use` to suppress vetoes when the user asked
-- for a read-only investigation. Last-write-wins per
-- (session_id, prompt_hash).

CREATE TABLE turn_intents (
    session_id     TEXT NOT NULL,
    prompt_hash    TEXT NOT NULL,
    is_mutation    INTEGER NOT NULL,
    classifier     TEXT NOT NULL,
    reason         TEXT,
    classified_at  INTEGER NOT NULL,
    PRIMARY KEY (session_id, prompt_hash)
);

CREATE INDEX IF NOT EXISTS idx_turn_intents_session_recent
    ON turn_intents(session_id, classified_at DESC);
