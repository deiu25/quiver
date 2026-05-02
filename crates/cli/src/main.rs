use clap::{Parser, Subcommand};

mod commands {
    pub mod list;
}

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
    /// Re-scan filesystem and refresh DB
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::List => commands::list::run().await,
        Cmd::Sync => {
            println!("sync: not yet implemented (Phase 1 follow-up)");
            Ok(())
        }
        Cmd::Recommend { task } => {
            println!("recommend({task:?}): not yet implemented (Phase 1 follow-up)");
            Ok(())
        }
        Cmd::Info { id } => {
            println!("info({id:?}): not yet implemented (Phase 1 follow-up)");
            Ok(())
        }
    }
}
