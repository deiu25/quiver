//! `toolhub agent` — foreground daily-task agent.
//!
//! Resolves default sessions/hints paths, builds an `AgentConfig`, then hands
//! off to `toolhub_agent::run`. Blocks until Ctrl-C / SIGTERM.

use std::path::PathBuf;

use anyhow::anyhow;
use toolhub_agent::{AgentConfig, run};

use crate::db_path::default_db_path;

pub async fn run_cmd(
    sessions_dir: Option<PathBuf>,
    hints_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let sessions_dir = sessions_dir.map(Ok).unwrap_or_else(default_sessions_dir)?;
    let hints_dir = hints_dir.map(Ok).unwrap_or_else(default_hints_dir)?;
    let cfg = AgentConfig::new(default_db_path()?, sessions_dir, hints_dir);
    run(cfg).await
}

fn default_sessions_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow!("HOME not set"))?;
    Ok(PathBuf::from(home).join(".claude/projects"))
}

fn default_hints_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow!("HOME not set"))?;
    Ok(PathBuf::from(home).join(".claude/hints"))
}
