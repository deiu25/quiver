//! Filesystem rescan: discover every catalogued tool from `$HOME` and (when
//! requested) persist them through `persist_tools`.
//!
//! Split into two layers so callers can compose:
//!
//! * [`discover_all`] is pure I/O + (optionally) network — walks skill
//!   roots, parses installed plugins, parses MCP servers (with optional
//!   npm registry enrichment), and runs the LLM enrichment pass when
//!   enabled. Returns a [`DiscoverReport`] without touching the
//!   embeddings table or running any persist.
//! * [`run_sync`] composes `discover_all` + [`persist_tools`] for the
//!   common case (CLI `quiver sync`, web `POST /api/sources/sync`).

use std::path::{Path, PathBuf};

use quiver_core::tool::ToolMeta;
use quiver_recommender::embed::Embedder;
use rusqlite::Connection;

use crate::llm_extract::{self, MetadataExtractor};
use crate::mcp_json::NpmEnrichment;
use crate::mcp_npm::NetworkMode;
use crate::persist::persist_tools;
use crate::{mcp_json, plugin_json, skill_md, walker};

/// Toggles applied to a single discover/sync run. Constructed by the
/// caller; defaults via [`DiscoverOpts::for_home`].
pub struct DiscoverOpts<'a> {
    pub home: &'a Path,
    /// Optional SQLite connection used by the npm registry cache. When
    /// `None`, MCP servers fall back to the legacy stub metadata.
    pub mcp_npm_conn: Option<&'a Connection>,
    pub network: NetworkMode,
    /// When `true`, run [`crate::llm_extract`] over each ToolMeta with an
    /// empty `triggers`/`examples`/`category` to fill those gaps from the
    /// `long_description` body.
    pub llm_enabled: bool,
    pub registry_base: &'a str,
}

impl<'a> DiscoverOpts<'a> {
    /// Default sync configuration for an interactive run: online network,
    /// LLM enrichment honouring `QUIVER_LLM_EXTRACT`, real npm registry.
    pub fn for_home(home: &'a Path) -> Self {
        Self {
            home,
            mcp_npm_conn: None,
            network: NetworkMode::Online,
            llm_enabled: env_llm_enabled(),
            registry_base: crate::mcp_npm::REGISTRY_BASE,
        }
    }
}

/// `QUIVER_LLM_EXTRACT=0` opts out of LLM enrichment for this run.
pub fn env_llm_enabled() -> bool {
    !matches!(
        std::env::var("QUIVER_LLM_EXTRACT").as_deref(),
        Ok("0") | Ok("false") | Ok("no")
    )
}

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
/// writes (other than the npm registry cache table when `mcp_npm_conn` is
/// passed), no embeddings. Duplicates by `tool.id` are dropped (first
/// wins).
pub async fn discover_all(opts: DiscoverOpts<'_>) -> DiscoverReport {
    let mut report = DiscoverReport::default();
    let mut seen_ids = std::collections::HashSet::<String>::new();

    if opts.home.as_os_str().is_empty() {
        return report;
    }

    // 1) SKILL.md walker
    for root in skill_roots(opts.home) {
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
    let plugin_path = opts.home.join(".claude/plugins/installed_plugins.json");
    let plugin_cache_root = opts.home.join(".claude/plugins/cache");
    let plugin_cache = if plugin_cache_root.is_dir() {
        Some(plugin_cache_root.as_path())
    } else {
        None
    };
    if plugin_path.exists() {
        match plugin_json::parse_installed_plugins(&plugin_path, plugin_cache) {
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
    let mcp_path = opts.home.join(".claude/mcp_servers.json");
    if mcp_path.exists() {
        let npm = opts.mcp_npm_conn.map(|conn| NpmEnrichment {
            conn,
            network: opts.network,
            registry_base: opts.registry_base,
        });
        match mcp_json::parse_mcp_servers(&mcp_path, npm).await {
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

    // 4) Optional LLM enrichment for empty triggers/examples/category.
    if opts.llm_enabled && !report.metas.is_empty() {
        let (extractor, label) = llm_extract::build_default(false);
        tracing::debug!("llm enrichment via {label}");
        enrich_in_place(&mut report.metas, extractor.as_ref()).await;
    }

    report
}

/// Discover every tool under `home` and persist (upsert + FTS + re-embed).
/// `embedder` is only consulted when `discover_all` returned ≥ 1 metas.
pub async fn run_sync(
    conn: &Connection,
    embedder: &Embedder,
    home: &Path,
) -> anyhow::Result<SyncReport> {
    let opts = DiscoverOpts {
        mcp_npm_conn: Some(conn),
        ..DiscoverOpts::for_home(home)
    };
    let DiscoverReport { metas, skipped } = discover_all(opts).await;
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

async fn enrich_in_place(tools: &mut [ToolMeta], extractor: &dyn MetadataExtractor) {
    for tool in tools.iter_mut() {
        if !tool.triggers.is_empty() && !tool.examples.is_empty() && tool.category.is_some() {
            continue;
        }
        let readme = tool.long_description.as_deref().unwrap_or("");
        if readme.trim().is_empty() {
            continue;
        }
        match extractor.extract(&tool.name, readme).await {
            Ok(m) => {
                if tool.triggers.is_empty() {
                    tool.triggers = m.triggers;
                }
                if tool.examples.is_empty() {
                    tool.examples = m.examples;
                }
                if tool.category.is_none() {
                    tool.category = m.category;
                }
            },
            Err(e) => {
                tracing::warn!(tool = %tool.name, "llm extract failed: {e:#}");
            },
        }
    }
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

    #[tokio::test]
    async fn discover_on_empty_home_returns_empty_report() {
        let dir = tempfile::tempdir().unwrap();
        let opts = DiscoverOpts {
            llm_enabled: false,
            ..DiscoverOpts::for_home(dir.path())
        };
        let report = discover_all(opts).await;
        assert!(report.metas.is_empty());
        assert!(report.skipped.is_empty());
    }

    #[tokio::test]
    async fn discover_on_default_constructed_path_is_safe() {
        let opts = DiscoverOpts {
            llm_enabled: false,
            ..DiscoverOpts::for_home(Path::new(""))
        };
        let report = discover_all(opts).await;
        assert!(report.metas.is_empty());
        assert!(report.skipped.is_empty());
    }
}
