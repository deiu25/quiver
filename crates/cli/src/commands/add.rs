//! `quiver add <github-url>` — Phase 5 onboarding entry point.
//!
//! Clones a GitHub repo (shallow), classifies it, parses any tool metadata
//! it contains, persists rows + embeddings, and records the source for
//! later `update`/`remove`.

use chrono::Utc;

use quiver_ingestion::github_repo;
use quiver_ingestion::llm_extract;
use quiver_ingestion::persist::persist_tools;
use quiver_recommender::embed::Embedder;
use quiver_storage::{open, sources};

use crate::db_path::default_db_path;

pub async fn run(url: String, no_llm: bool) -> anyhow::Result<()> {
    let db_path = default_db_path()?;
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = open(&db_path)?;

    let force_regex = no_llm
        || std::env::var("QUIVER_LLM_EXTRACT")
            .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
            .unwrap_or(false);
    let (extractor, label) = llm_extract::build_default(force_regex);
    tracing::info!(target: "quiver::onboard", "extractor: {label}");

    let result = github_repo::onboard(&url, extractor.as_ref()).await?;
    let n = result.tools.len();
    let kind = format!("{:?}", result.repo_type);

    if n == 0 {
        println!(
            "{} → 0 tools registered (repo classified as {kind}, no metadata found).",
            result.source_id
        );
    } else {
        let embedder = Embedder::new()?;
        let total = persist_tools(&conn, &embedder, &result.tools)?;
        println!(
            "{} → {n} tool(s) registered ({kind}); catalog now has {total}.",
            result.source_id
        );
    }

    sources::upsert_full(
        &conn,
        &result.source_id,
        "github",
        &result.web_url,
        Utc::now(),
        result.commit_sha.as_deref(),
    )?;
    if let Some(sha) = &result.commit_sha {
        let short = &sha[..7.min(sha.len())];
        println!("source recorded at {} ({short}…)", result.web_url);
    }

    Ok(())
}
