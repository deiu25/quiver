//! Filesystem rescan: discover every catalogued tool from `$HOME` and (when
//! requested) persist them through `persist_tools`.
//!
//! Split into two layers so callers can compose:
//!
//! * [`discover_all`] is pure I/O — walks skill roots, parses installed
//!   plugins / MCP servers, returns a [`DiscoverReport`] without touching the
//!   DB or the embedder. Cheap to call from any context.
//! * [`run_sync`] composes `discover_all` + [`persist_tools`] for the common
//!   case (CLI `quiver sync`, web `POST /api/sources/sync`).

use std::path::{Path, PathBuf};

use quiver_core::tool::ToolMeta;
use quiver_recommender::embed::Embedder;
use rusqlite::Connection;

use crate::persist::persist_tools;
use crate::{mcp_json, plugin_json, skill_md, walker};

/// One source that failed to parse. We never abort a sync on a single bad
/// file — we just collect the failures and let the caller surface them.
#[derive(Debug)]
pub struct DiscoverSkip {
    pub path: PathBuf,
    pub error: String,
}

/// Result of a filesystem rescan, before anything is written to the DB.
#[derive(Debug, Default)]
pub struct DiscoverReport {
    pub metas: Vec<ToolMeta>,
    pub skipped: Vec<DiscoverSkip>,
}

/// Result of a full sync (discover + persist).
#[derive(Debug, Default)]
pub struct SyncReport {
    pub unique: usize,
    pub skipped: usize,
    pub catalog_total: usize,
    pub skipped_paths: Vec<DiscoverSkip>,
}

/// Walk every known source under `$HOME` and return what we found. No DB
/// writes, no embeddings. Duplicates by `tool.id` are dropped (first wins).
pub fn discover_all(home: &Path) -> DiscoverReport {
    let mut report = DiscoverReport::default();
    let mut seen_ids = std::collections::HashSet::<String>::new();

    if home.as_os_str().is_empty() {
        return report;
    }

    // 1) SKILL.md walker
    for root in skill_roots(home) {
        for dir in walker::discover_skill_dirs(&root) {
            match skill_md::parse_skill_dir(&dir) {
                Ok(meta) => {
                    if seen_ids.insert(meta.id.clone()) {
                        report.metas.push(meta);
                    }
                },
                Err(err) => {
                    report.skipped.push(DiscoverSkip {
                        path: dir,
                        error: format!("{err:#}"),
                    });
                },
            }
        }
    }

    // 2) Plugins
    let plugin_path = home.join(".claude/plugins/installed_plugins.json");
    if plugin_path.exists() {
        match plugin_json::parse_installed_plugins(&plugin_path) {
            Ok(parsed) => {
                for meta in parsed {
                    if seen_ids.insert(meta.id.clone()) {
                        report.metas.push(meta);
                    }
                }
            },
            Err(err) => report.skipped.push(DiscoverSkip {
                path: plugin_path,
                error: format!("{err:#}"),
            }),
        }
    }

    // 3) MCP servers
    let mcp_path = home.join(".claude/mcp_servers.json");
    if mcp_path.exists() {
        match mcp_json::parse_mcp_servers(&mcp_path) {
            Ok(parsed) => {
                for meta in parsed {
                    if seen_ids.insert(meta.id.clone()) {
                        report.metas.push(meta);
                    }
                }
            },
            Err(err) => report.skipped.push(DiscoverSkip {
                path: mcp_path,
                error: format!("{err:#}"),
            }),
        }
    }

    report
}

/// Discover every tool under `home` and persist (upsert + FTS + re-embed).
/// `embedder` is only consulted when `discover_all` returned ≥ 1 metas.
pub fn run_sync(conn: &Connection, embedder: &Embedder, home: &Path) -> anyhow::Result<SyncReport> {
    let DiscoverReport { metas, skipped } = discover_all(home);
    let unique = metas.len();

    if metas.is_empty() {
        return Ok(SyncReport {
            unique: 0,
            skipped: skipped.len(),
            catalog_total: 0,
            skipped_paths: skipped,
        });
    }

    let total = persist_tools(conn, embedder, &metas)?;
    Ok(SyncReport {
        unique,
        skipped: skipped.len(),
        catalog_total: total,
        skipped_paths: skipped,
    })
}

fn skill_roots(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".claude/skills"),
        home.join(".agents/skills"),
        home.join(".claude/plugins/cache"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_on_empty_home_returns_empty_report() {
        let dir = tempfile::tempdir().unwrap();
        let report = discover_all(dir.path());
        assert!(report.metas.is_empty());
        assert!(report.skipped.is_empty());
    }

    #[test]
    fn discover_on_default_constructed_path_is_safe() {
        let report = discover_all(Path::new(""));
        assert!(report.metas.is_empty());
        assert!(report.skipped.is_empty());
    }
}
