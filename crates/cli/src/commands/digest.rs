//! `toolhub digest` — markdown report over a sliding window of activity.

use std::path::PathBuf;

use toolhub_agent::digest;

use crate::db_path::default_db_path;

pub async fn run_cmd(days: u32, out: Option<PathBuf>) -> anyhow::Result<()> {
    let db = default_db_path()?;
    let body = digest(&db, days, out.as_deref())?;
    match out {
        Some(p) => println!("wrote digest ({days} days) to {}", p.display()),
        None => print!("{body}"),
    }
    Ok(())
}
