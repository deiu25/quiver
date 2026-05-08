use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Context;
use chrono::Utc;
use quiver_core::tool::{ToolMeta, ToolType};
use rusqlite::Connection;
use serde::Deserialize;

use crate::mcp_npm::{self, NetworkMode, NpmMetadata};

const KEYWORD_TRIGGER_LIMIT: usize = 16;

#[derive(Debug, Deserialize)]
struct McpServers {
    #[serde(rename = "mcpServers", default)]
    servers: HashMap<String, McpEntry>,
}

#[derive(Debug, Deserialize)]
struct McpEntry {
    command: String,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    env: Option<HashMap<String, String>>,
}

/// Optional npm enrichment for [`parse_mcp_servers`]. When `Some`, the
/// parser tries to derive an npm package name from each entry's
/// `(command, args)` and looks it up via [`mcp_npm::enrich_via_cache`].
pub struct NpmEnrichment<'a> {
    pub conn: &'a Connection,
    pub network: NetworkMode,
    pub registry_base: &'a str,
}

/// Parse `~/.claude/mcp_servers.json`. With `npm = None` the output is
/// the legacy stub-only ToolMeta (matches the v0.1.3 behaviour). When
/// `npm` is provided, MCP entries that resolve to an npm package gain
/// real description/triggers/source_repo from the registry (cached in
/// SQLite, 30-day TTL).
pub async fn parse_mcp_servers(
    path: &Path,
    npm: Option<NpmEnrichment<'_>>,
) -> anyhow::Result<Vec<ToolMeta>> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let parsed: McpServers =
        serde_json::from_str(&raw).with_context(|| format!("parse JSON {}", path.display()))?;

    let now = Utc::now();
    let mut out = Vec::with_capacity(parsed.servers.len());
    for (name, entry) in parsed.servers {
        let args = entry.args.clone().unwrap_or_default();
        let invocation = if args.is_empty() {
            entry.command.clone()
        } else {
            format!("{} {}", entry.command, args.join(" "))
        };
        let env_count = entry.env.as_ref().map(|e| e.len()).unwrap_or(0);
        let stub_long = format!(
            "command: {}\nargs: {:?}\nenv vars: {}",
            entry.command, args, env_count
        );
        let stub_description = format!("MCP server: {name} ({})", entry.command);

        let metadata = match &npm {
            Some(cfg) => match mcp_npm::extract_npm_pkg(&entry.command, &args) {
                Some(pkg) => {
                    match mcp_npm::enrich_via_cache(cfg.conn, cfg.registry_base, &pkg, cfg.network)
                        .await
                    {
                        Ok(m) => m,
                        Err(e) => {
                            tracing::warn!(server = %name, package = %pkg,
                                "npm registry lookup failed, falling back to stub: {e:#}");
                            None
                        },
                    }
                },
                None => None,
            },
            None => None,
        };

        let (description, triggers, source_repo, long_description) =
            merge_with_npm(metadata, name.as_str(), stub_description, stub_long);

        out.push(ToolMeta {
            id: format!("mcp:{name}"),
            r#type: ToolType::Mcp,
            name: name.clone(),
            source_repo,
            install_path: None,
            description: Some(description),
            long_description: Some(long_description),
            category: None,
            triggers,
            examples: Vec::new(),
            invocation: Some(invocation),
            requires: Vec::new(),
            enabled: true,
            added_at: now,
            last_seen_at: now,
            last_used_at: None,
            scope: quiver_core::tool::ToolScope::User,
            scope_root: None,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

fn merge_with_npm(
    meta: Option<NpmMetadata>,
    server_name: &str,
    stub_description: String,
    stub_long: String,
) -> (String, Vec<String>, Option<String>, String) {
    let Some(m) = meta else {
        return (stub_description, Vec::new(), None, stub_long);
    };

    let description = m
        .description
        .filter(|s| !s.trim().is_empty())
        .map(|d| format!("MCP server {server_name} ({}): {d}", m.package))
        .unwrap_or(stub_description);

    let mut triggers = m.keywords;
    triggers.dedup();
    if triggers.len() > KEYWORD_TRIGGER_LIMIT {
        triggers.truncate(KEYWORD_TRIGGER_LIMIT);
    }

    // Prefer the upstream README as long_description so the LLM
    // enrichment pass downstream has rich text to work with. Keep the
    // command/args summary as a trailing footer for diagnostics.
    let long = match m.readme.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(readme) => format!("{readme}\n\n---\n{stub_long}"),
        None => stub_long,
    };

    (description, triggers, m.repository, long)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/mcp_servers.json")
    }

    #[tokio::test]
    async fn parses_two_servers_from_fixture_without_npm() {
        let metas = parse_mcp_servers(&fixture_path(), None).await.unwrap();
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].id, "mcp:context7");
        assert_eq!(metas[0].r#type, ToolType::Mcp);
        assert_eq!(
            metas[0].invocation.as_deref(),
            Some("npx -y @context7/mcp-server")
        );
        assert_eq!(metas[1].id, "mcp:nocmd");
        assert_eq!(metas[1].invocation.as_deref(), Some("echo"));
    }

    #[tokio::test]
    async fn legacy_call_without_npm_keeps_stub_metadata() {
        let metas = parse_mcp_servers(&fixture_path(), None).await.unwrap();
        for m in &metas {
            assert!(m.triggers.is_empty(), "stub should have no triggers");
            assert!(m.source_repo.is_none(), "stub should have no source_repo");
            assert!(
                m.description.as_ref().unwrap().starts_with("MCP server: "),
                "stub description, got {:?}",
                m.description
            );
        }
    }
}
