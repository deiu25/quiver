//! `quiver update [<source-id>]` — re-pull one or every github source.
//!
//! Compares the current upstream HEAD against `sources.last_commit_sha`. If
//! the sha matches, skips the re-ingest. Otherwise re-runs the same pipeline
//! `add` does, leaving stale tools that disappeared upstream tracked under
//! the same `source_repo` (orphan cleanup is a Phase 6 concern).

use anyhow::{Context, anyhow};
use chrono::Utc;

use quiver_ingestion::github_repo;
use quiver_ingestion::persist::persist_tools;
use quiver_recommender::embed::Embedder;
use quiver_storage::{open, sources};

use crate::db_path::default_db_path;

pub async fn run(source_id: Option<String>) -> anyhow::Result<()> {
    let db_path = default_db_path()?;
    let conn = open(&db_path)?;

    let targets: Vec<sources::SourceRow> = match source_id {
        Some(id) => vec![sources::get(&conn, &id)?.ok_or_else(|| anyhow!("no such source: {id}"))?],
        None => sources::list(&conn)?
            .into_iter()
            .filter(|s| s.r#type == "github")
            .collect(),
    };

    if targets.is_empty() {
        println!("no github sources to update.");
        return Ok(());
    }

    let mut embedder: Option<Embedder> = None;
    for src in targets {
        let result = github_repo::onboard(&src.location)
            .await
            .with_context(|| format!("update {}", src.id))?;

        let unchanged = matches!(
            (&result.commit_sha, &src.last_commit_sha),
            (Some(new), Some(old)) if new == old,
        );
        if unchanged {
            let old = src.last_commit_sha.as_deref().unwrap_or("");
            let short = &old[..7.min(old.len())];
            println!("{}: no changes (HEAD unchanged at {short}…)", src.id);
            continue;
        }

        let n = result.tools.len();
        if n > 0 {
            if embedder.is_none() {
                embedder = Some(Embedder::new()?);
            }
            let emb = embedder.as_ref().expect("embedder just initialised");
            let total = persist_tools(&conn, emb, &result.tools)?;
            println!(
                "{}: re-ingested {n} tool(s); catalog now has {total}.",
                src.id
            );
        } else {
            println!("{}: 0 tools after re-ingest.", src.id);
        }

        sources::upsert_full(
            &conn,
            &result.source_id,
            "github",
            &result.web_url,
            Utc::now(),
            result.commit_sha.as_deref(),
        )?;
    }
    Ok(())
}
