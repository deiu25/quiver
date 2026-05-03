use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Context;
use chrono::{DateTime, Utc};
use quiver_core::tool::{ToolMeta, ToolType};
use serde::Deserialize;

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

pub fn parse_installed_plugins(path: &Path) -> anyhow::Result<Vec<ToolMeta>> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let parsed: InstalledPlugins =
        serde_json::from_str(&raw).with_context(|| format!("parse JSON {}", path.display()))?;

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

        let description = if version.is_empty() {
            format!("Plugin {name} from marketplace {marketplace}")
        } else {
            format!("Plugin {name} from marketplace {marketplace}, version {version}")
        };

        out.push(ToolMeta {
            id: format!("plugin:{name}@{marketplace}"),
            r#type: ToolType::Plugin,
            name: format!("{name} ({marketplace})"),
            source_repo: None,
            install_path: None,
            description: Some(description),
            long_description: head
                .and_then(|e| e.git_commit_sha.clone())
                .map(|sha| format!("scope: {scope}\nversion: {version}\ngitCommitSha: {sha}")),
            category: None,
            triggers: Vec::new(),
            examples: Vec::new(),
            invocation: None,
            requires: Vec::new(),
            enabled: !entries.is_empty(),
            added_at: added,
            last_seen_at: now,
            last_used_at: None,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
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
        let metas = parse_installed_plugins(&fixture_path()).unwrap();
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
}
