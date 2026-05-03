//! `quiver score` — replay every Claude Code session JSONL into
//! `usage_events`, then rebuild `tool_scores`.
//!
//! Idempotent: `usage_events.uuid` is UNIQUE (migration 005) so re-running on
//! the same JSONL files no-ops on already-ingested events.
//!
//! `usage_events.tool_id` has a FK into `tools(id)` and SQLite FK enforcement
//! is active, so events for un-catalogued tools are dropped before insert.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Context;
use quiver_ingestion::session_jsonl;
use quiver_storage::{open, usage};
use walkdir::WalkDir;

use crate::db_path::default_db_path;

pub async fn run(sessions_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let db_path = default_db_path()?;
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut conn = open(&db_path)?;

    let root = sessions_dir.unwrap_or_else(default_sessions_dir);
    if !root.exists() {
        eprintln!("sessions dir not found: {}", root.display());
        return Ok(());
    }

    let catalogue = load_catalogue(&conn)?;

    let mut files = 0usize;
    let mut total_events = 0usize;
    let mut inserted = 0usize;
    let mut deduped = 0usize;
    let mut skipped_unknown = 0usize;
    let mut skipped_files = 0usize;

    for entry in WalkDir::new(&root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        files += 1;
        match session_jsonl::replay(path) {
            Ok(events) => {
                total_events += events.len();
                for ev in events {
                    if !catalogue.contains(&ev.tool_id) {
                        skipped_unknown += 1;
                        continue;
                    }
                    match usage::insert_event(&conn, &ev) {
                        Ok(true) => inserted += 1,
                        Ok(false) => deduped += 1,
                        Err(e) => {
                            eprintln!("insert {} failed: {e}", ev.tool_id);
                        },
                    }
                }
            },
            Err(e) => {
                eprintln!("replay {} failed: {e:#}", path.display());
                skipped_files += 1;
            },
        }
    }

    let scored = usage::recompute_scores(&mut conn).context("recompute_scores")?;

    println!(
        "scanned {files} file(s) ({skipped_files} unreadable); \
         {total_events} event(s) found; {inserted} inserted, \
         {deduped} duplicates, {skipped_unknown} for un-catalogued tools; \
         {scored} tool_scores row(s) rebuilt"
    );
    Ok(())
}

fn default_sessions_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".claude/projects")
}

fn load_catalogue(conn: &rusqlite::Connection) -> anyhow::Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT id FROM tools")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<HashSet<_>, _>>()?;
    Ok(rows)
}
