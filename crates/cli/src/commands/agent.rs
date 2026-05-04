//! `quiver agent` — foreground daily-task agent.
//!
//! Resolves default sessions/hints paths, builds an `AgentConfig`, then hands
//! off to `quiver_agent::run`. Blocks until Ctrl-C / SIGTERM.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, anyhow};
use quiver_agent::{AgentConfig, HaikuClassifier, run};

use crate::db_path::default_db_path;

pub async fn run_cmd(
    sessions_dir: Option<PathBuf>,
    hints_dir: Option<PathBuf>,
    classify_flag: bool,
) -> anyhow::Result<()> {
    let sessions_dir = sessions_dir.map(Ok).unwrap_or_else(default_sessions_dir)?;
    let hints_dir = hints_dir.map(Ok).unwrap_or_else(default_hints_dir)?;
    let mut cfg = AgentConfig::new(default_db_path()?, sessions_dir, hints_dir);

    if classify_flag || env_classify_enabled() {
        let classifier = HaikuClassifier::detect().context(
            "--classify requested but no ANTHROPIC_API_KEY set and no `claude` CLI on PATH",
        )?;
        tracing::info!(
            backend = classifier.label(),
            "haiku task classifier enabled"
        );
        cfg = cfg.with_classifier(Arc::new(classifier));
    }

    run(cfg).await
}

fn env_classify_enabled() -> bool {
    match std::env::var("QUIVER_TASK_CLASSIFIER") {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            !v.is_empty() && v != "0" && v != "off" && v != "false" && v != "none"
        },
        Err(_) => false,
    }
}

fn default_sessions_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow!("HOME not set"))?;
    Ok(PathBuf::from(home).join(".claude/projects"))
}

fn default_hints_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow!("HOME not set"))?;
    Ok(PathBuf::from(home).join(".claude/hints"))
}
