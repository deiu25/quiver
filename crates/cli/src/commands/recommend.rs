use std::collections::HashMap;

use quiver_recommender::params::{
    COS_WEIGHT, FTS_CANDIDATES, FTS_WEIGHT, VEC_CANDIDATES, build_fts_query,
};
use quiver_recommender::rerank::{Reranker, SuccessReranker};
use quiver_recommender::{embed::Embedder, search};
use quiver_storage::{embeddings, fts, open, tools};
use serde::Serialize;

use crate::db_path::default_db_path;

/// Mirror of `quiver_mcp_server::schema::RecommendHit` so a `--json` consumer
/// (e.g. PreToolUse hook) gets the same shape the MCP server returns.
#[derive(Debug, Serialize)]
struct RecommendHit {
    tool_id: String,
    score: f32,
    name: String,
    description: Option<String>,
    invocation: Option<String>,
    install_path: Option<String>,
}

pub async fn run(task: String, json: bool) -> anyhow::Result<()> {
    let conn = open(&default_db_path()?)?;

    let embedder = Embedder::new()?;
    let q_emb = embedder.embed_one(&task)?;

    // sqlite-vec returns cosine *distance* in [0, 2]; convert to similarity.
    let vec_sims: HashMap<String, f32> = embeddings::vec_search(&conn, &q_emb, VEC_CANDIDATES)?
        .into_iter()
        .map(|(id, dist)| (id, 1.0 - dist))
        .collect();

    if vec_sims.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("(empty index — run `quiver sync` first)");
        }
        return Ok(());
    }

    let fts_query = build_fts_query(&task);
    let fts_hits: HashMap<String, f32> = if fts_query.is_empty() {
        HashMap::new()
    } else {
        match fts::search(&conn, &fts_query, FTS_CANDIDATES) {
            Ok(rows) => rows.into_iter().collect(),
            Err(err) => {
                eprintln!("fts search failed: {err:#} (falling back to vec-only)");
                HashMap::new()
            },
        }
    };

    // Pull a wider candidate set, then rerank by historical success_rate (PLAN
    // §8.3). Reranker is a no-op when tool_scores is empty, so Phase 1–3
    // behaviour is preserved.
    let mut hits = search::hybrid_from_score_maps(
        &vec_sims,
        &fts_hits,
        VEC_CANDIDATES.max(FTS_CANDIDATES),
        COS_WEIGHT,
        FTS_WEIGHT,
    );
    SuccessReranker::default().apply(&mut hits, &conn)?;
    hits.truncate(3);

    let by_id: HashMap<String, _> = tools::list_all(&conn)?
        .into_iter()
        .map(|m| (m.id.clone(), m))
        .collect();

    if json {
        let payload: Vec<RecommendHit> = hits
            .into_iter()
            .map(|h| {
                let meta = by_id.get(&h.tool_id);
                RecommendHit {
                    tool_id: h.tool_id.clone(),
                    score: h.score,
                    name: meta
                        .map(|m| m.name.clone())
                        .unwrap_or_else(|| h.tool_id.clone()),
                    description: meta.and_then(|m| m.description.clone()),
                    invocation: meta.and_then(|m| m.invocation.clone()),
                    install_path: meta.and_then(|m| m.install_path.clone()),
                }
            })
            .collect();
        println!("{}", serde_json::to_string(&payload)?);
        return Ok(());
    }

    println!("{:>6}  {:<40}  description", "score", "id");
    println!("{}", "-".repeat(96));
    for h in hits {
        let desc = by_id
            .get(&h.tool_id)
            .and_then(|m| m.description.as_deref())
            .unwrap_or("");
        let desc: String = desc.chars().take(60).collect();
        println!("{:>6.3}  {:<40}  {}", h.score, h.tool_id, desc);
    }
    Ok(())
}
