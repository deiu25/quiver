use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod commands {
    pub mod dead_weight;
    pub mod list;
    pub mod mcp;
    pub mod recommend;
    pub mod score;
    pub mod stats;
    pub mod sync;
    pub mod tui;
}
mod db_path;
mod tui;

#[derive(Parser)]
#[command(
    name = "toolhub",
    version,
    about = "Claude Code tool registry & recommender"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List catalogued tools
    List,
    /// Re-scan filesystem and refresh DB (also re-embeds every tool)
    Sync,
    /// Recommend tools for a task
    Recommend {
        /// Free-text task description
        task: String,
    },
    /// Show details for a tool by id
    Info {
        /// Tool id, e.g. `skill:design-md`
        id: String,
    },
    /// Browse catalogued tools in an interactive TUI
    Tui,
    /// Run the stdio MCP server (so Claude Code can call ToolHub mid-session)
    Mcp,
    /// Replay Claude Code session JSONL into usage_events + rebuild tool_scores
    Score {
        /// Sessions root (default: $HOME/.claude/projects)
        #[arg(long)]
        sessions_dir: Option<PathBuf>,
    },
    /// Show usage stats from tool_scores
    Stats {
        /// Detail view for one tool id, e.g. `skill:caveman`
        #[arg(long)]
        tool: Option<String>,
        /// How many rows to show in list mode
        #[arg(long, default_value_t = 20)]
        top: usize,
        /// Emit JSON instead of a table
        #[arg(long)]
        json: bool,
    },
    /// List catalogued tools with no usage in the last N days
    DeadWeight {
        #[arg(long, default_value_t = 30)]
        days: u32,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,refinery_core=warn".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::List => commands::list::run().await,
        Cmd::Sync => commands::sync::run().await,
        Cmd::Recommend { task } => commands::recommend::run(task).await,
        Cmd::Info { id } => {
            println!("info({id:?}): not yet implemented (Phase 1 follow-up)");
            Ok(())
        },
        Cmd::Tui => commands::tui::run().await,
        Cmd::Mcp => commands::mcp::run().await,
        Cmd::Score { sessions_dir } => commands::score::run(sessions_dir).await,
        Cmd::Stats { tool, top, json } => commands::stats::run(tool, top, json).await,
        Cmd::DeadWeight { days } => commands::dead_weight::run(days).await,
    }
}
