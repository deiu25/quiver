use std::path::PathBuf;

use toolhub_ingestion::{skill_md, walker};
use toolhub_storage::{open, tools};

use crate::db_path::default_db_path;

pub async fn run() -> anyhow::Result<()> {
    let db_path = default_db_path()?;
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = open(&db_path)?;

    let mut ok = 0usize;
    let mut skipped = 0usize;
    for root in skill_roots() {
        for dir in walker::discover_skill_dirs(&root) {
            match skill_md::parse_skill_dir(&dir) {
                Ok(meta) => {
                    tools::upsert(&conn, &meta)?;
                    ok += 1;
                }
                Err(err) => {
                    eprintln!("skip {}: {err:#}", dir.display());
                    skipped += 1;
                }
            }
        }
    }
    println!(
        "synced {ok} tool(s){} → {}",
        if skipped > 0 {
            format!(" ({skipped} skipped)")
        } else {
            String::new()
        },
        db_path.display()
    );
    Ok(())
}

fn skill_roots() -> Vec<PathBuf> {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return Vec::new(),
    };
    vec![
        PathBuf::from(&home).join(".claude/skills"),
        PathBuf::from(&home).join(".agents/skills"),
    ]
}
