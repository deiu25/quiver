//! Shared upsert→FTS→embed pipeline used by `sync` and `add`.
//!
//! Given a slice of `ToolMeta`, this:
//!   1. Upserts each row into `tools` (idempotent via `ON CONFLICT(id)`).
//!   2. Rebuilds the FTS5 index.
//!   3. Embeds every row in `tools` (not just `metas`) and writes vectors
//!      into `tool_embeddings` + `tools_vec`. Embedding the full catalog keeps
//!      `recommend` correct even after partial updates from `add`.
//!
//! Caller owns the `Connection` and `Embedder`. Pass `Embedder::new()` lazily
//! — building it loads the fastembed model (~30 MB).

use quiver_core::tool::ToolMeta;
use quiver_recommender::embed::Embedder;
use quiver_storage::{embeddings, fts, tools};
use rusqlite::Connection;

/// Upsert `metas`, rebuild FTS, then re-embed the entire catalog. Returns the
/// number of rows present in `tools` after upsert (i.e. the catalog size).
pub fn persist_tools(
    conn: &Connection,
    embedder: &Embedder,
    metas: &[ToolMeta],
) -> anyhow::Result<usize> {
    for meta in metas {
        tools::upsert(conn, meta)?;
    }
    fts::rebuild(conn)?;

    let catalog = tools::list_all(conn)?;
    if catalog.is_empty() {
        return Ok(0);
    }
    let texts: Vec<String> = catalog.iter().map(embed_text).collect();
    let vectors = embedder.embed_batch(texts)?;
    for (m, v) in catalog.iter().zip(&vectors) {
        embeddings::upsert(conn, &m.id, v)?;
    }
    Ok(catalog.len())
}

/// Concatenate name + description + triggers — the same blend used by Phase 1
/// `sync` so embeddings match what `recommend` expects.
pub fn embed_text(m: &ToolMeta) -> String {
    let desc = m.description.as_deref().unwrap_or("");
    let triggers = m.triggers.join(", ");
    format!("{}\n{}\n{}", m.name, desc, triggers)
}
