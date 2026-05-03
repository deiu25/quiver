//! `toolhub add <github-url>` — Phase 5 onboarding entry point.
//!
//! Clones a GitHub repo (shallow), classifies it, parses any tool metadata
//! it contains, persists rows + embeddings, and records the source for
//! later `update`/`remove`.

use chrono::Utc;

use toolhub_ingestion::github_repo;
use toolhub_ingestion::persist::persist_tools;
use toolhub_recommender::embed::Embedder;
use toolhub_storage::{open, sources};

use crate::db_path::default_db_path;

pub async fn run(url: String) -> anyhow::Result<()> {
    let db_path = default_db_path()?;
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = open(&db_path)?;

    let result = github_repo::onboard(&url).await?;
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
