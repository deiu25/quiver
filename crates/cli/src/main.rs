use clap::{Parser, Subcommand};

mod commands {
    pub mod list;
    pub mod recommend;
    pub mod sync;
    pub mod tui;
}
mod db_path;
mod tui;

#[derive(Parser)]
#[command(name = "toolhub", version, about = "Claude Code tool registry & recommender")]
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
        }
        Cmd::Tui => commands::tui::run().await,
    }
}
