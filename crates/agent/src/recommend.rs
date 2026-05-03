//! Shared `top_k` recommendation pipeline.
//!
//! Same hybrid (sqlite-vec cosine + FTS5 BM25) → SuccessReranker pipeline
//! the CLI `toolhub recommend` command uses (`crates/cli/src/commands/recommend.rs`).
//! Lifted into the agent crate so both the CLI command and the long-running
//! agent loop share one code path.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Connection;
use toolhub_recommender::{
    embed::Embedder,
    params::{COS_WEIGHT, FTS_CANDIDATES, FTS_WEIGHT, VEC_CANDIDATES, build_fts_query},
    rerank::{Reranker, SuccessReranker},
    search,
};
use toolhub_storage::{embeddings, fts, tools};

#[derive(Debug, Clone)]
pub struct RecHit {
    pub tool_id: String,
    pub score: f32,
    pub description: Option<String>,
    pub invocation: Option<String>,
}

/// Run the full hybrid + rerank pipeline for `task` and return the top `k`
/// hits with each hit's description + invocation joined in. Returns an empty
/// vec when the vector index is empty (caller's signal to advise `toolhub sync`).
pub fn top_k(conn: &Connection, embedder: &Embedder, task: &str, k: usize) -> Result<Vec<RecHit>> {
    let q_emb = embedder.embed_one(task)?;

    let vec_sims: HashMap<String, f32> = embeddings::vec_search(conn, &q_emb, VEC_CANDIDATES)?
        .into_iter()
        .map(|(id, dist)| (id, 1.0 - dist))
        .collect();
    if vec_sims.is_empty() {
        return Ok(Vec::new());
    }

    let fts_query = build_fts_query(task);
    let fts_hits: HashMap<String, f32> = if fts_query.is_empty() {
        HashMap::new()
    } else {
        fts::search(conn, &fts_query, FTS_CANDIDATES)
            .map(|rows| rows.into_iter().collect())
            .unwrap_or_default()
    };

    let mut hits = search::hybrid_from_score_maps(
        &vec_sims,
        &fts_hits,
        VEC_CANDIDATES.max(FTS_CANDIDATES),
        COS_WEIGHT,
        FTS_WEIGHT,
    );
    SuccessReranker::default().apply(&mut hits, conn)?;
    hits.truncate(k);

    let by_id: HashMap<String, _> = tools::list_all(conn)?
        .into_iter()
        .map(|m| (m.id.clone(), m))
        .collect();

    Ok(hits
        .into_iter()
        .map(|h| {
            let meta = by_id.get(&h.tool_id);
            RecHit {
                tool_id: h.tool_id,
                score: h.score,
                description: meta.and_then(|m| m.description.clone()),
                invocation: meta.and_then(|m| m.invocation.clone()),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use toolhub_core::tool::{ToolMeta, ToolType};
    use toolhub_storage::open;

    fn seed_tool_with_emb(conn: &Connection, id: &str, desc: &str, emb: &[f32]) {
        let now = Utc::now();
        let meta = ToolMeta {
            id: id.into(),
            r#type: ToolType::Skill,
            name: id.into(),
            source_repo: None,
            install_path: None,
            description: Some(desc.into()),
            long_description: Some(desc.into()),
            category: None,
            triggers: vec![],
            examples: vec![],
            invocation: Some(format!("/{id}")),
            requires: vec![],
            enabled: true,
            added_at: now,
            last_seen_at: now,
            last_used_at: None,
        };
        tools::upsert(conn, &meta).unwrap();
        embeddings::upsert(conn, id, emb).unwrap();
    }

    #[test]
    fn empty_index_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        let embedder = Embedder::new().unwrap();
        let hits = top_k(&conn, &embedder, "anything", 3).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn top_k_returns_metadata_joined_hits() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        let embedder = Embedder::new().unwrap();
        let q = embedder.embed_one("design tokens from a website").unwrap();
        seed_tool_with_emb(&conn, "skill:designlang", "extract design tokens", &q);
        let mut far = vec![0.0f32; q.len()];
        far[0] = 1.0;
        seed_tool_with_emb(&conn, "skill:caveman", "be terse", &far);

        let hits = top_k(&conn, &embedder, "design tokens from a website", 3).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].tool_id, "skill:designlang");
        assert_eq!(hits[0].invocation.as_deref(), Some("/skill:designlang"));
        assert_eq!(
            hits[0].description.as_deref(),
            Some("extract design tokens")
        );
    }
}
