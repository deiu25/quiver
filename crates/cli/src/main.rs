use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod commands {
    pub mod add;
    pub mod agent;
    pub mod dead_weight;
    pub mod digest;
    pub mod list;
    pub mod mcp;
    pub mod recommend;
    pub mod remove;
    pub mod score;
    pub mod serve;
    pub mod stats;
    pub mod sync;
    pub mod tui;
    pub mod update;
}
mod db_path;
mod tui;

#[derive(Parser)]
#[command(
    name = "quiver",
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
        /// Emit JSON (matches the MCP `recommend` response shape).
        #[arg(long)]
        json: bool,
    },
    /// Show details for a tool by id
    Info {
        /// Tool id, e.g. `skill:design-md`
        id: String,
    },
    /// Browse catalogued tools in an interactive TUI
    Tui,
    /// Run the stdio MCP server (so Claude Code can call Quiver mid-session)
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
    /// Onboard a tool source from a GitHub URL (clones repo, registers tools)
    Add {
        /// GitHub URL: https://github.com/owner/repo, gh:owner/repo, git@…
        url: String,
        /// Skip LLM-assisted metadata extraction (regex fallback only).
        /// Equivalent to `QUIVER_LLM_EXTRACT=0`.
        #[arg(long)]
        no_llm: bool,
    },
    /// Re-pull one (or every) registered github source and refresh tools
    Update {
        /// Source id, e.g. `gh:owner/repo`. Omit to update every github source.
        source: Option<String>,
    },
    /// Drop every tool ingested from a source + delete the source row
    Remove {
        /// Source id, e.g. `gh:owner/repo`.
        source: String,
    },
    /// Run the daily-task agent in foreground (Ctrl-C to stop).
    /// Tails session JSONL files and writes hint markdown per session.
    Agent {
        /// Sessions root (default: $HOME/.claude/projects)
        #[arg(long)]
        sessions_dir: Option<PathBuf>,
        /// Where to write `<session>.md` hints (default: $HOME/.claude/hints)
        #[arg(long)]
        hints_dir: Option<PathBuf>,
    },
    /// Generate a markdown digest of recent activity (top tools, dead weight,
    /// suggestion acceptance, new arrivals).
    Digest {
        /// Window size in days
        #[arg(long, default_value_t = 7)]
        days: u32,
        /// Write to this path instead of stdout
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Serve the local web UI on 127.0.0.1 (Ctrl-C to stop).
    /// Read-only against the same SQLite DB the CLI uses.
    Serve {
        /// TCP port to listen on
        #[arg(long, default_value_t = 7777)]
        port: u16,
        /// Bind address. Defaults to loopback only — overriding accepts
        /// non-localhost traffic at your own risk (no auth).
        #[arg(long, default_value_t = IpAddr::V4(Ipv4Addr::LOCALHOST))]
        host: IpAddr,
        /// Open the listen URL in the default browser once the server is up.
        #[arg(long)]
        open: bool,
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
        Cmd::Recommend { task, json } => commands::recommend::run(task, json).await,
        Cmd::Info { id } => {
            println!("info({id:?}): not yet implemented (Phase 1 follow-up)");
            Ok(())
        },
        Cmd::Tui => commands::tui::run().await,
        Cmd::Mcp => commands::mcp::run().await,
        Cmd::Score { sessions_dir } => commands::score::run(sessions_dir).await,
        Cmd::Stats { tool, top, json } => commands::stats::run(tool, top, json).await,
        Cmd::DeadWeight { days } => commands::dead_weight::run(days).await,
        Cmd::Add { url, no_llm } => commands::add::run(url, no_llm).await,
        Cmd::Update { source } => commands::update::run(source).await,
        Cmd::Remove { source } => commands::remove::run(source).await,
        Cmd::Agent {
            sessions_dir,
            hints_dir,
        } => commands::agent::run_cmd(sessions_dir, hints_dir).await,
        Cmd::Digest { days, out } => commands::digest::run_cmd(days, out).await,
        Cmd::Serve { port, host, open } => commands::serve::run(host, port, open).await,
    }
}
