use std::collections::HashSet;
use std::path::{Path, PathBuf};

use toolhub_core::tool::ToolMeta;
use toolhub_ingestion::{mcp_json, plugin_json, skill_md, walker};
use toolhub_recommender::embed::Embedder;
use toolhub_storage::open;

use crate::commands::persist::persist_tools;
use crate::db_path::default_db_path;

pub async fn run() -> anyhow::Result<()> {
    let db_path = default_db_path()?;
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = open(&db_path)?;

    let home = std::env::var("HOME").unwrap_or_default();
    let home_path = PathBuf::from(&home);

    let mut metas: Vec<ToolMeta> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut skipped = 0usize;

    // 1) SKILL.md walker
    for root in skill_roots(&home_path) {
        for dir in walker::discover_skill_dirs(&root) {
            match skill_md::parse_skill_dir(&dir) {
                Ok(meta) => {
                    if seen_ids.insert(meta.id.clone()) {
                        metas.push(meta);
                    }
                },
                Err(err) => {
                    eprintln!("skip {}: {err:#}", dir.display());
                    skipped += 1;
                },
            }
        }
    }

    // 2) Plugins
    let plugin_path = home_path.join(".claude/plugins/installed_plugins.json");
    if plugin_path.exists() {
        match plugin_json::parse_installed_plugins(&plugin_path) {
            Ok(parsed) => {
                for meta in parsed {
                    if seen_ids.insert(meta.id.clone()) {
                        metas.push(meta);
                    }
                }
            },
            Err(err) => eprintln!("skip {}: {err:#}", plugin_path.display()),
        }
    }

    // 3) MCP servers
    let mcp_path = home_path.join(".claude/mcp_servers.json");
    if mcp_path.exists() {
        match mcp_json::parse_mcp_servers(&mcp_path) {
            Ok(parsed) => {
                for meta in parsed {
                    if seen_ids.insert(meta.id.clone()) {
                        metas.push(meta);
                    }
                }
            },
            Err(err) => eprintln!("skip {}: {err:#}", mcp_path.display()),
        }
    }

    let unique = metas.len();
    println!(
        "synced {unique} tool(s){} → {}",
        if skipped > 0 {
            format!(" ({skipped} skipped)")
        } else {
            String::new()
        },
        db_path.display()
    );

    if unique == 0 {
        return Ok(());
    }

    let embedder = Embedder::new()?;
    let total = persist_tools(&conn, &embedder, &metas)?;
    println!("embedded {total} tool(s)");

    Ok(())
}

fn skill_roots(home: &Path) -> Vec<PathBuf> {
    if home.as_os_str().is_empty() {
        return Vec::new();
    }
    vec![
        home.join(".claude/skills"),
        home.join(".agents/skills"),
        home.join(".claude/plugins/cache"),
    ]
}
