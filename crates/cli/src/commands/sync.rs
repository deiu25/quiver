//! `quiver sync` — re-scan filesystem and refresh DB.
//!
//! Thin CLI wrapper over [`quiver_ingestion::sync::discover_all`] +
//! [`quiver_ingestion::persist::persist_tools`]. The discover/embed pipeline
//! lives in `quiver_ingestion::sync` so the web layer can reuse it.

use std::path::PathBuf;

use clap::Args;
use quiver_ingestion::mcp_npm::NetworkMode;
use quiver_ingestion::sync::{DiscoverOpts, DiscoverReport};
use quiver_recommender::embed::Embedder;
use quiver_storage::open;

use crate::db_path::default_db_path;

#[derive(Args, Debug, Clone, Default)]
pub struct SyncArgs {
    /// Skip outgoing HTTP (npm registry, LLM API). Cache hits still
    /// apply. Equivalent to setting `QUIVER_NO_NETWORK=1`.
    #[arg(long)]
    pub no_network: bool,
    /// Skip the LLM-assisted trigger/example/category extraction pass.
    /// Equivalent to setting `QUIVER_LLM_EXTRACT=0`.
    #[arg(long)]
    pub no_llm: bool,
}

pub async fn run(args: SyncArgs) -> anyhow::Result<()> {
    let db_path = default_db_path()?;
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = open(&db_path)?;

    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());

    let env_no_network = matches!(
        std::env::var("QUIVER_NO_NETWORK").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    );
    let no_network = args.no_network || env_no_network;
    let llm_enabled = !args.no_llm && quiver_ingestion::sync::env_llm_enabled();

    let opts = DiscoverOpts {
        home: &home,
        mcp_npm_conn: Some(&conn),
        network: if no_network {
            NetworkMode::Offline
        } else {
            NetworkMode::Online
        },
        llm_enabled,
        registry_base: quiver_ingestion::mcp_npm::REGISTRY_BASE,
    };
    let DiscoverReport { metas, skipped } = quiver_ingestion::sync::discover_all(opts).await;
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
    let total = quiver_ingestion::persist::persist_tools(&conn, &embedder, &metas)?;
    println!("embedded {total} tool(s)");

    Ok(())
}
