//! `quiver sync` — re-scan filesystem and refresh DB.
//!
//! Thin CLI wrapper over [`quiver_ingestion::sync::run_sync`]. The CLI's job
//! is just `default_db_path` + open + print; the discover/embed/persist
//! pipeline lives in `quiver_ingestion::sync` so the web layer can reuse it.

use std::path::PathBuf;

use quiver_ingestion::sync::{DiscoverReport, SyncReport, discover_all};
use quiver_recommender::embed::Embedder;
use quiver_storage::open;

use crate::db_path::default_db_path;

pub async fn run() -> anyhow::Result<()> {
    let db_path = default_db_path()?;
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = open(&db_path)?;

    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());

    // Discover first so we can print per-skip diagnostics on stderr in the
    // CLI's idiomatic style. The web layer skips the prints and reads the
    // returned vec instead.
    let DiscoverReport { metas, skipped } = discover_all(&home);
    for skip in &skipped {
        eprintln!("skip {}: {}", skip.path.display(), skip.error);
    }

    let unique = metas.len();
    println!(
        "synced {unique} tool(s){} → {}",
        if !skipped.is_empty() {
            format!(" ({} skipped)", skipped.len())
        } else {
            String::new()
        },
        db_path.display()
    );

    if unique == 0 {
        return Ok(());
    }

    let embedder = Embedder::new()?;
    let report = SyncReport {
        unique,
        skipped: skipped.len(),
        catalog_total: quiver_ingestion::persist::persist_tools(&conn, &embedder, &metas)?,
        skipped_paths: skipped,
    };
    println!("embedded {} tool(s)", report.catalog_total);

    Ok(())
}
