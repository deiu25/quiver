use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Context;
use chrono::{DateTime, Utc};
use quiver_core::tool::{ToolMeta, ToolType};
use serde::Deserialize;

use crate::plugin_manifest::{self, PluginManifest};

const KEYWORD_TRIGGER_LIMIT: usize = 16;
const CATEGORY_HINTS: &[&str] = &["security", "testing", "git", "ui", "mcp", "agent", "docs"];

#[derive(Debug, Deserialize)]
struct InstalledPlugins {
    #[serde(default)]
    plugins: HashMap<String, Vec<Entry>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct Entry {
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    install_path: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    installed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    last_updated: Option<DateTime<Utc>>,
    #[serde(default, rename = "gitCommitSha")]
    git_commit_sha: Option<String>,
}

/// Parse `~/.claude/plugins/installed_plugins.json`. When `cache_root` is
/// `Some`, each entry is enriched with metadata from
/// `<cache_root>/<marketplace>/<name>/<version>/.claude-plugin/plugin.json`
/// and the adjacent README. With `cache_root = None` we keep the legacy
/// stub-only output (used by the existing fixture-based regression test).
pub fn parse_installed_plugins(
    json_path: &Path,
    cache_root: Option<&Path>,
) -> anyhow::Result<Vec<ToolMeta>> {
    let raw =
        fs::read_to_string(json_path).with_context(|| format!("read {}", json_path.display()))?;
    let parsed: InstalledPlugins = serde_json::from_str(&raw)
        .with_context(|| format!("parse JSON {}", json_path.display()))?;

    let now = Utc::now();
    let mut out = Vec::with_capacity(parsed.plugins.len());
    for (key, entries) in parsed.plugins {
        let (name, marketplace) = key
            .split_once('@')
            .map(|(n, m)| (n.to_string(), m.to_string()))
            .unwrap_or((key.clone(), "unknown".to_string()));

        let head = entries.first();
        let version = head.and_then(|e| e.version.clone()).unwrap_or_default();
        let scope = head.and_then(|e| e.scope.clone()).unwrap_or_default();
        let added = entries
            .iter()
            .filter_map(|e| e.installed_at)
            .min()
            .unwrap_or(now);

        let stub_description = if version.is_empty() {
            format!("Plugin {name} from marketplace {marketplace}")
        } else {
            format!("Plugin {name} from marketplace {marketplace}, version {version}")
        };
        let stub_long = head
            .and_then(|e| e.git_commit_sha.clone())
            .map(|sha| format!("scope: {scope}\nversion: {version}\ngitCommitSha: {sha}"));

        let manifest = cache_root.and_then(|root| {
            let dir = plugin_manifest::cache_dir_for(root, &marketplace, &name, &version);
            plugin_manifest::read_plugin_manifest(&dir)
        });

        let (description, triggers, source_repo, long_description, category) =
            merge_with_manifest(manifest, stub_description, stub_long);

        out.push(ToolMeta {
            id: format!("plugin:{name}@{marketplace}"),
            r#type: ToolType::Plugin,
            name: format!("{name} ({marketplace})"),
            source_repo,
            install_path: None,
            description: Some(description),
            long_description,
            category,
            triggers,
            examples: Vec::new(),
            invocation: None,
            requires: Vec::new(),
            enabled: !entries.is_empty(),
            added_at: added,
            last_seen_at: now,
            last_used_at: None,
            scope: quiver_core::tool::ToolScope::User,
            scope_root: None,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

fn merge_with_manifest(
    manifest: Option<PluginManifest>,
    stub_description: String,
    stub_long: Option<String>,
) -> (
    String,
    Vec<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let Some(m) = manifest else {
        return (stub_description, Vec::new(), None, stub_long, None);
    };

    let description = m
        .description
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(stub_description);

    let mut triggers: Vec<String> = m.keywords;
    triggers.dedup();
    if triggers.len() > KEYWORD_TRIGGER_LIMIT {
        triggers.truncate(KEYWORD_TRIGGER_LIMIT);
    }

    let category = triggers
        .iter()
        .find(|kw| CATEGORY_HINTS.contains(&kw.as_str()))
        .cloned();

    let long_description = m.readme_excerpt.or(stub_long);

    (
        description,
        triggers,
        m.repository,
        long_description,
        category,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/installed_plugins.json")
    }

    #[test]
    fn parses_two_plugins_from_fixture() {
        let metas = parse_installed_plugins(&fixture_path(), None).unwrap();
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].id, "plugin:caveman@caveman");
        assert_eq!(metas[0].r#type, ToolType::Plugin);
        assert!(
            metas[0]
                .description
                .as_ref()
                .unwrap()
                .contains("marketplace caveman")
        );
        assert_eq!(metas[1].id, "plugin:context7@claude-plugins-official");
    }

    #[test]
    fn legacy_call_without_cache_keeps_stub_metadata() {
        let metas = parse_installed_plugins(&fixture_path(), None).unwrap();
        for m in &metas {
            assert!(m.triggers.is_empty(), "stub should have no triggers");
            assert!(m.source_repo.is_none(), "stub should have no source_repo");
            assert!(m.category.is_none(), "stub should have no category");
        }
    }
}
