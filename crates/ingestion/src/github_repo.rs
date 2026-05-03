//! Phase 5 GitHub onboarder.
//!
//! Pipeline:
//!   1. `parse_github_url(url)` → `(owner, repo, canonical_https_url, source_id)`
//!   2. `git clone --depth 1` into a tempdir (`git_clone::shallow_clone`)
//!   3. `git rev-parse HEAD` → `commit_sha`
//!   4. `detect_repo_type(root)` → SkillBundle | PluginMarketplace | McpServer | Cli | Doc
//!   5. `ingest_local(root, canonical_url)` → `Vec<ToolMeta>` (each row gets
//!      `source_repo = canonical_url` so `remove`/`update` can find them)
//!
//! `ingest_local` is split out so tests can exercise the parsing + type-detection
//! path without needing network access. The async `onboard` is the only fn that
//! talks to the network.

use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use chrono::Utc;
use toolhub_core::tool::{ToolMeta, ToolType};

use crate::{git_clone, plugin_json, skill_md, walker};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhRef {
    pub owner: String,
    pub repo: String,
    /// Canonical clone URL: `https://github.com/<owner>/<repo>.git`.
    pub clone_url: String,
    /// Canonical web URL stored on each tool's `source_repo` column:
    /// `https://github.com/<owner>/<repo>` (no `.git` suffix).
    pub web_url: String,
    /// Stable source-table id: `gh:<owner>/<repo>`.
    pub source_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoType {
    SkillBundle,
    PluginMarketplace,
    McpServer,
    Cli,
    Doc,
}

#[derive(Debug)]
pub struct OnboardResult {
    pub source_id: String,
    pub clone_url: String,
    pub web_url: String,
    pub commit_sha: Option<String>,
    pub repo_type: RepoType,
    pub tools: Vec<ToolMeta>,
}

/// Parse any of:
///   * `https://github.com/owner/repo`
///   * `https://github.com/owner/repo.git`
///   * `https://github.com/owner/repo/`
///   * `git@github.com:owner/repo.git`
///   * `gh:owner/repo`
///
/// Rejects anything that isn't a recognisable GitHub form.
pub fn parse_github_url(url: &str) -> anyhow::Result<GhRef> {
    let trimmed = url.trim();
    let body = if let Some(rest) = trimmed.strip_prefix("gh:") {
        rest.to_string()
    } else if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        rest.to_string()
    } else if let Some(rest) = trimmed.strip_prefix("http://github.com/") {
        rest.to_string()
    } else if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        rest.to_string()
    } else {
        return Err(anyhow!("not a recognised GitHub URL: {url:?}"));
    };
    let body = body.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = body.splitn(2, '/');
    let owner = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("missing owner in {url:?}"))?;
    let repo = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("missing repo in {url:?}"))?;
    if owner.contains('/') || repo.contains('/') {
        return Err(anyhow!("malformed GitHub URL: {url:?}"));
    }
    Ok(GhRef {
        owner: owner.to_string(),
        repo: repo.to_string(),
        clone_url: format!("https://github.com/{owner}/{repo}.git"),
        web_url: format!("https://github.com/{owner}/{repo}"),
        source_id: format!("gh:{owner}/{repo}"),
    })
}

/// Most-specific-first repo classification. See PLAN.md §7 Phase 5.
pub fn detect_repo_type(root: &Path) -> RepoType {
    if !walker::discover_skill_dirs(root).is_empty() {
        return RepoType::SkillBundle;
    }
    if root.join("marketplace.json").is_file() || has_plugin_subdir(root) {
        return RepoType::PluginMarketplace;
    }
    if is_mcp_server(root) {
        return RepoType::McpServer;
    }
    if is_cli(root) {
        return RepoType::Cli;
    }
    RepoType::Doc
}

fn has_plugin_subdir(root: &Path) -> bool {
    let plugins = root.join("plugins");
    let Ok(entries) = std::fs::read_dir(&plugins) else {
        return false;
    };
    for entry in entries.flatten() {
        if entry.path().join("plugin.json").is_file() {
            return true;
        }
    }
    false
}

fn is_mcp_server(root: &Path) -> bool {
    let pkg = root.join("package.json");
    if !pkg.is_file() {
        return false;
    }
    let Ok(raw) = std::fs::read_to_string(&pkg) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    if v.get("mcp").is_some() {
        return true;
    }
    match v.get("bin") {
        Some(serde_json::Value::String(s)) => s.contains("mcp"),
        Some(serde_json::Value::Object(map)) => map.keys().any(|k| k.contains("mcp")),
        _ => false,
    }
}

fn is_cli(root: &Path) -> bool {
    if root.join("Cargo.toml").is_file() {
        let Ok(raw) = std::fs::read_to_string(root.join("Cargo.toml")) else {
            return false;
        };
        if raw.contains("[[bin]]") {
            return true;
        }
        if root.join("src").join("main.rs").is_file() {
            return true;
        }
    }
    let pkg = root.join("package.json");
    if pkg.is_file()
        && let Ok(raw) = std::fs::read_to_string(&pkg)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw)
        && v.get("bin").is_some()
    {
        return true;
    }
    false
}

/// Walk a cloned repo on disk and return tool rows. Pure / sync so tests can
/// run offline against a fixture directory.
pub fn ingest_local(root: &Path, source_url: &str) -> anyhow::Result<Vec<ToolMeta>> {
    let kind = detect_repo_type(root);
    match kind {
        RepoType::SkillBundle => ingest_skill_bundle(root, source_url),
        RepoType::PluginMarketplace => ingest_plugin_marketplace(root, source_url),
        RepoType::McpServer => Ok(vec![ingest_doc_or_codebase(
            root,
            source_url,
            ToolType::Mcp,
        )?]),
        RepoType::Cli => Ok(vec![ingest_doc_or_codebase(
            root,
            source_url,
            ToolType::Cli,
        )?]),
        RepoType::Doc => Ok(vec![ingest_doc_or_codebase(
            root,
            source_url,
            ToolType::Doc,
        )?]),
    }
}

fn ingest_skill_bundle(root: &Path, source_url: &str) -> anyhow::Result<Vec<ToolMeta>> {
    let mut out = Vec::new();
    for dir in walker::discover_skill_dirs(root) {
        match skill_md::parse_skill_dir(&dir) {
            Ok(mut meta) => {
                meta.source_repo = Some(source_url.to_string());
                out.push(meta);
            },
            Err(err) => {
                tracing::warn!(skill_dir = %dir.display(), "skip skill: {err:#}");
            },
        }
    }
    Ok(out)
}

fn ingest_plugin_marketplace(root: &Path, source_url: &str) -> anyhow::Result<Vec<ToolMeta>> {
    // Reuse `parse_installed_plugins` only when marketplace.json matches its
    // schema (key: { plugins: { "name@market": [{...}] } }). Most upstream
    // marketplace.json files use a different schema, so fall back to a
    // single doc-style row in that case.
    let market = root.join("marketplace.json");
    if market.is_file()
        && let Ok(metas) = plugin_json::parse_installed_plugins(&market)
        && !metas.is_empty()
    {
        return Ok(metas
            .into_iter()
            .map(|mut m| {
                m.source_repo = Some(source_url.to_string());
                m
            })
            .collect());
    }
    // Walk plugins/<name>/plugin.json — minimal name-only ingest.
    let mut out = Vec::new();
    let plugins = root.join("plugins");
    if let Ok(entries) = std::fs::read_dir(&plugins) {
        for entry in entries.flatten() {
            let dir = entry.path();
            let pj = dir.join("plugin.json");
            if !pj.is_file() {
                continue;
            }
            let name = dir
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            let now = Utc::now();
            out.push(ToolMeta {
                id: format!("plugin:{name}"),
                r#type: ToolType::Plugin,
                name: name.clone(),
                source_repo: Some(source_url.to_string()),
                install_path: None,
                description: Some(format!("Plugin {name} from {source_url}")),
                long_description: std::fs::read_to_string(&pj).ok(),
                category: None,
                triggers: Vec::new(),
                examples: Vec::new(),
                invocation: None,
                requires: Vec::new(),
                enabled: true,
                added_at: now,
                last_seen_at: now,
                last_used_at: None,
            });
        }
    }
    if out.is_empty() {
        return Ok(vec![ingest_doc_or_codebase(
            root,
            source_url,
            ToolType::Doc,
        )?]);
    }
    Ok(out)
}

fn ingest_doc_or_codebase(
    root: &Path,
    source_url: &str,
    kind: ToolType,
) -> anyhow::Result<ToolMeta> {
    let name = repo_basename(root, source_url);
    let readme = read_first_readme(root);
    let description = readme
        .as_deref()
        .and_then(|body| {
            body.lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with('#'))
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| format!("{name} from {source_url}"));
    let now = Utc::now();
    let prefix = match kind {
        ToolType::Mcp => "mcp",
        ToolType::Cli => "cli",
        ToolType::Doc => "doc",
        ToolType::Skill => "skill",
        ToolType::Plugin => "plugin",
    };
    Ok(ToolMeta {
        id: format!("{prefix}:{name}"),
        r#type: kind,
        name,
        source_repo: Some(source_url.to_string()),
        install_path: None,
        description: Some(description),
        long_description: readme,
        category: None,
        triggers: Vec::new(),
        examples: Vec::new(),
        invocation: None,
        requires: Vec::new(),
        enabled: true,
        added_at: now,
        last_seen_at: now,
        last_used_at: None,
    })
}

fn repo_basename(root: &Path, source_url: &str) -> String {
    if let Some(name) = root.file_name().and_then(|s| s.to_str()) {
        return name.to_string();
    }
    source_url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("repo")
        .to_string()
}

fn read_first_readme(root: &Path) -> Option<String> {
    for candidate in [
        "README.md",
        "Readme.md",
        "readme.md",
        "README",
        "README.rst",
    ] {
        let p: PathBuf = root.join(candidate);
        if let Ok(s) = std::fs::read_to_string(&p) {
            return Some(s);
        }
    }
    None
}

/// Async end-to-end onboarding: clone, classify, parse, return rows.
pub async fn onboard(url: &str) -> anyhow::Result<OnboardResult> {
    let gh = parse_github_url(url)?;
    let tmp = tempfile::tempdir().context("create tempdir for git clone")?;
    let dest = tmp.path().join("repo");
    git_clone::shallow_clone(&gh.clone_url, &dest)
        .await
        .with_context(|| format!("clone {}", gh.clone_url))?;
    let commit_sha = git_clone::head_sha(&dest).await?;
    let repo_type = detect_repo_type(&dest);
    let tools = ingest_local(&dest, &gh.web_url)?;
    Ok(OnboardResult {
        source_id: gh.source_id,
        clone_url: gh.clone_url,
        web_url: gh.web_url,
        commit_sha,
        repo_type,
        tools,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixtures_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/github_repos")
    }

    #[test]
    fn parse_https_url_no_suffix() {
        let r = parse_github_url("https://github.com/foo/bar").unwrap();
        assert_eq!(r.owner, "foo");
        assert_eq!(r.repo, "bar");
        assert_eq!(r.clone_url, "https://github.com/foo/bar.git");
        assert_eq!(r.web_url, "https://github.com/foo/bar");
        assert_eq!(r.source_id, "gh:foo/bar");
    }

    #[test]
    fn parse_https_url_with_dot_git_and_trailing_slash() {
        let r = parse_github_url("https://github.com/foo/bar.git/").unwrap();
        assert_eq!(r.source_id, "gh:foo/bar");
    }

    #[test]
    fn parse_ssh_url() {
        let r = parse_github_url("git@github.com:foo/bar.git").unwrap();
        assert_eq!(r.source_id, "gh:foo/bar");
        assert_eq!(r.clone_url, "https://github.com/foo/bar.git");
    }

    #[test]
    fn parse_short_form() {
        let r = parse_github_url("gh:foo/bar").unwrap();
        assert_eq!(r.source_id, "gh:foo/bar");
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_github_url("https://gitlab.com/foo/bar").is_err());
        assert!(parse_github_url("https://github.com/foo").is_err());
        assert!(parse_github_url("not a url").is_err());
        assert!(parse_github_url("").is_err());
    }

    #[test]
    fn detect_skill_bundle_fixture() {
        let root = fixtures_root().join("skill_bundle");
        assert_eq!(detect_repo_type(&root), RepoType::SkillBundle);
    }

    #[test]
    fn detect_plugin_marketplace_fixture() {
        let root = fixtures_root().join("plugin_marketplace");
        assert_eq!(detect_repo_type(&root), RepoType::PluginMarketplace);
    }

    #[test]
    fn detect_doc_only_fixture() {
        let root = fixtures_root().join("doc_only");
        assert_eq!(detect_repo_type(&root), RepoType::Doc);
    }

    #[test]
    fn ingest_skill_bundle_returns_two_tools_with_source_repo() {
        let root = fixtures_root().join("skill_bundle");
        let url = "https://github.com/example/skill-bundle";
        let tools = ingest_local(&root, url).unwrap();
        assert_eq!(tools.len(), 2, "tools: {tools:?}");
        for t in &tools {
            assert_eq!(t.r#type, ToolType::Skill);
            assert_eq!(t.source_repo.as_deref(), Some(url));
        }
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"alpha-skill"));
        assert!(names.contains(&"beta-skill"));
    }

    #[test]
    fn ingest_doc_only_returns_one_doc_row() {
        let root = fixtures_root().join("doc_only");
        let url = "https://github.com/example/doc-only";
        let tools = ingest_local(&root, url).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].r#type, ToolType::Doc);
        assert_eq!(tools[0].source_repo.as_deref(), Some(url));
        assert!(tools[0].long_description.is_some());
    }
}
