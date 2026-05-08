use std::collections::HashMap;
use std::path::PathBuf;

use quiver_ingestion::project_scope;
use quiver_recommender::params::{
    COS_WEIGHT, FTS_CANDIDATES, FTS_WEIGHT, VEC_CANDIDATES, build_fts_query,
};
use quiver_recommender::rerank::{
    DemeritReranker, ProjectScopeReranker, Reranker, SuccessReranker,
};
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

pub async fn run(task: String, json: bool, cwd: Option<PathBuf>) -> anyhow::Result<()> {
    let conn = open(&default_db_path()?)?;

    let embedder = Embedder::new()?;
    let q_emb = embedder.embed_one(&task)?;

    // Resolve project root: explicit --cwd wins, else current_dir().
    let project_root: Option<PathBuf> = cwd.or_else(|| std::env::current_dir().ok());

    // Ingest any per-project skills under `<cwd>/.claude/skills/` before we
    // query the catalog. Best-effort: failures only log and pass through.
    if let Some(ref root) = project_root {
        match project_scope::upsert_project_skills(&conn, &embedder, root) {
            Ok(0) => {},
            Ok(n) => tracing::debug!(project_root = %root.display(), "ingested {n} project skills"),
            Err(err) => tracing::warn!(
                project_root = %root.display(),
                "project skill ingestion failed: {err:#}"
            ),
        }
    }

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

    // Pull a wider candidate set, then run the reranker stack.
    // Order: SuccessReranker → DemeritReranker → ProjectScopeReranker → truncate.
    // (LanguageReranker is hook-only — keeps the CLI idempotent for ad-hoc
    // queries where the user may be probing a project that isn't theirs.)
    let mut hits = search::hybrid_from_score_maps(
        &vec_sims,
        &fts_hits,
        VEC_CANDIDATES.max(FTS_CANDIDATES),
        COS_WEIGHT,
        FTS_WEIGHT,
    );
    SuccessReranker::default().apply(&mut hits, &conn)?;
    DemeritReranker::new(&task).apply(&mut hits, &conn)?;
    ProjectScopeReranker::new(project_root.as_deref()).apply(&mut hits, &conn)?;
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
