use std::path::PathBuf;

use toolhub_core::tool::ToolMeta;
use toolhub_ingestion::{mcp_json, plugin_json, skill_md, walker};
use toolhub_recommender::embed::Embedder;
use toolhub_storage::{embeddings, fts, open, tools};

use crate::db_path::default_db_path;

pub async fn run() -> anyhow::Result<()> {
    let db_path = default_db_path()?;
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = open(&db_path)?;

    let home = std::env::var("HOME").unwrap_or_default();
    let home_path = PathBuf::from(&home);

    let mut ok = 0usize;
    let mut skipped = 0usize;

    // 1) SKILL.md walker
    for root in skill_roots(&home_path) {
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

    // 2) Plugins
    let plugin_path = home_path.join(".claude/plugins/installed_plugins.json");
    if plugin_path.exists() {
        match plugin_json::parse_installed_plugins(&plugin_path) {
            Ok(metas) => {
                for meta in metas {
                    tools::upsert(&conn, &meta)?;
                    ok += 1;
                }
            }
            Err(err) => eprintln!("skip {}: {err:#}", plugin_path.display()),
        }
    }

    // 3) MCP servers
    let mcp_path = home_path.join(".claude/mcp_servers.json");
    if mcp_path.exists() {
        match mcp_json::parse_mcp_servers(&mcp_path) {
            Ok(metas) => {
                for meta in metas {
                    tools::upsert(&conn, &meta)?;
                    ok += 1;
                }
            }
            Err(err) => eprintln!("skip {}: {err:#}", mcp_path.display()),
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

    if ok == 0 {
        return Ok(());
    }

    fts::rebuild(&conn)?;

    let metas = tools::list_all(&conn)?;
    let texts: Vec<String> = metas.iter().map(embed_text).collect();
    let embedder = Embedder::new()?;
    let vectors = embedder.embed_batch(texts)?;
    for (m, v) in metas.iter().zip(&vectors) {
        embeddings::upsert(&conn, &m.id, v)?;
    }
    println!("embedded {} tool(s)", vectors.len());

    Ok(())
}

fn embed_text(m: &ToolMeta) -> String {
    let desc = m.description.as_deref().unwrap_or("");
    let triggers = m.triggers.join(", ");
    format!("{}\n{}\n{}", m.name, desc, triggers)
}

fn skill_roots(home: &PathBuf) -> Vec<PathBuf> {
    if home.as_os_str().is_empty() {
        return Vec::new();
    }
    vec![
        home.join(".claude/skills"),
        home.join(".agents/skills"),
        home.join(".claude/plugins/cache"),
    ]
}
