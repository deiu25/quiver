//! End-to-end ingestion tests for plugin manifest enrichment. We feed
//! `parse_installed_plugins` a synthetic `installed_plugins.json` whose
//! entry points at the in-repo `tests/fixtures/plugin_cache/...` tree
//! and verify the resulting ToolMeta carries real description, triggers
//! and source_repo lifted from `.claude-plugin/plugin.json`.

use std::fs;
use std::path::{Path, PathBuf};

use quiver_core::tool::ToolType;
use quiver_ingestion::plugin_json::parse_installed_plugins;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn shared_fixtures() -> PathBuf {
    workspace_root().join("tests/fixtures")
}

fn cache_fixture_root() -> PathBuf {
    shared_fixtures().join("plugin_cache")
}

fn write_installed_plugins_json(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("installed_plugins.json");
    fs::write(&path, body).unwrap();
    path
}

#[test]
fn enriches_plugin_when_cache_root_resolves_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let body = r#"{
      "version": 2,
      "plugins": {
        "test-plugin@test-market": [
          {
            "scope": "user",
            "version": "1.0.0",
            "installedAt": "2026-04-01T10:00:00.000Z",
            "gitCommitSha": "deadbeef"
          }
        ]
      }
    }"#;
    let json_path = write_installed_plugins_json(tmp.path(), body);

    let metas = parse_installed_plugins(&json_path, Some(&cache_fixture_root())).unwrap();
    assert_eq!(metas.len(), 1);
    let meta = &metas[0];
    assert_eq!(meta.id, "plugin:test-plugin@test-market");
    assert_eq!(meta.r#type, ToolType::Plugin);
    assert_eq!(
        meta.description.as_deref(),
        Some("Multi-agent orchestration system for Claude Code")
    );
    assert!(meta.triggers.contains(&"automation".to_string()));
    assert!(meta.triggers.contains(&"testing".to_string()));
    assert_eq!(
        meta.source_repo.as_deref(),
        Some("https://github.com/example/test-plugin")
    );
    let long = meta
        .long_description
        .as_deref()
        .expect("long_description should be the README excerpt");
    assert!(
        long.contains("coordinates multiple agents"),
        "long_description: {long}"
    );
    // category derived from a hint keyword in the manifest.
    assert_eq!(meta.category.as_deref(), Some("testing"));
}

#[test]
fn falls_back_to_stub_when_cache_root_missing_for_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let body = r#"{
      "version": 2,
      "plugins": {
        "ghost@unknown-marketplace": [
          {
            "scope": "user",
            "version": "0.0.1",
            "installedAt": "2026-04-01T10:00:00.000Z"
          }
        ]
      }
    }"#;
    let json_path = write_installed_plugins_json(tmp.path(), body);

    let metas = parse_installed_plugins(&json_path, Some(&cache_fixture_root())).unwrap();
    assert_eq!(metas.len(), 1);
    let meta = &metas[0];
    assert!(meta.triggers.is_empty(), "no manifest = no triggers");
    assert!(meta.source_repo.is_none(), "no manifest = no source_repo");
    assert!(meta.category.is_none());
    let desc = meta.description.as_deref().unwrap();
    assert!(desc.contains("marketplace unknown-marketplace"));
}

#[test]
fn legacy_existing_fixture_remains_stub_only_with_none_cache() {
    let json_path = shared_fixtures().join("installed_plugins.json");
    let metas = parse_installed_plugins(&json_path, None).unwrap();
    assert_eq!(metas.len(), 2);
    for m in &metas {
        assert!(m.triggers.is_empty());
        assert!(m.source_repo.is_none());
        assert!(m.category.is_none());
        assert!(
            m.description.as_ref().unwrap().starts_with("Plugin "),
            "expected stub description, got: {:?}",
            m.description
        );
    }
}
