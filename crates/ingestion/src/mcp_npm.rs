//! npm registry lookups for MCP server enrichment.
//!
//! Pipeline:
//! 1. [`extract_npm_pkg`] inspects `(command, args)` from `mcp_servers.json`
//!    to derive the npm package name (e.g. `npx -y @context7/mcp-server` →
//!    `@context7/mcp-server`).
//! 2. [`fetch_npm_metadata`] hits `https://registry.npmjs.org/<pkg>/latest`
//!    with a 5s timeout. Failures (network, 4xx/5xx, parse) bubble up as
//!    `Err`; the caller logs and falls back to the existing stub.
//! 3. [`enrich_via_cache`] glues the storage cache (`mcp_npm_cache` table,
//!    30-day TTL) to the network fetch, honouring [`NetworkMode`] so we
//!    never go online when the user passes `--no-network` /
//!    `QUIVER_NO_NETWORK=1`.

use std::time::Duration;

use anyhow::{Context, anyhow};
use chrono::Utc;
use rusqlite::Connection;
use serde::Deserialize;

use quiver_storage::mcp_npm::{self, DEFAULT_TTL_DAYS, NpmCacheRow};

/// Default npm registry root. Tests inject a mock; production callers use
/// this constant.
pub const REGISTRY_BASE: &str = "https://registry.npmjs.org";
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Network policy applied to a single sync run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMode {
    /// Allow npm registry HTTP calls on cache miss.
    Online,
    /// Disable all outgoing HTTP — cache hits still apply.
    Offline,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NpmMetadata {
    pub package: String,
    pub description: Option<String>,
    pub keywords: Vec<String>,
    pub repository: Option<String>,
    pub homepage: Option<String>,
    pub readme: Option<String>,
}

impl From<NpmCacheRow> for NpmMetadata {
    fn from(r: NpmCacheRow) -> Self {
        NpmMetadata {
            package: r.package,
            description: r.description,
            keywords: r.keywords,
            repository: r.repository,
            homepage: r.homepage,
            readme: r.readme,
        }
    }
}

impl NpmMetadata {
    fn into_cache_row(self, fetched_at: chrono::DateTime<Utc>) -> NpmCacheRow {
        NpmCacheRow {
            package: self.package,
            fetched_at,
            description: self.description,
            keywords: self.keywords,
            repository: self.repository,
            homepage: self.homepage,
            readme: self.readme,
        }
    }
}

/// Inspect `(command, args)` and return the npm package the MCP server is
/// invoked through, when one is recognisable. Handles `npx`, `bunx`,
/// `bun x`, `pnpx`, and `pnpm dlx`.
pub fn extract_npm_pkg(command: &str, args: &[String]) -> Option<String> {
    let cmd = command.trim();
    let bin = cmd.rsplit('/').next().unwrap_or(cmd).to_ascii_lowercase();

    let mut iter = args.iter().map(|s| s.as_str());

    // Skip a sub-command for runners that need one (e.g. `bun x`, `pnpm dlx`).
    let needs_subcommand = matches!(bin.as_str(), "bun" | "pnpm" | "yarn");
    if needs_subcommand {
        let sub = iter.next()?;
        match (bin.as_str(), sub) {
            ("bun", "x") | ("pnpm", "dlx") | ("yarn", "dlx") => {},
            _ => return None,
        }
    } else if !matches!(bin.as_str(), "npx" | "bunx" | "pnpx") {
        // We only know how to interpret JS package runners. `node script.js`
        // / arbitrary binaries / `uvx` get None — caller falls back to stub.
        return None;
    }

    for arg in iter {
        if arg.starts_with('-') {
            continue;
        }
        if arg == "--" {
            continue;
        }
        // env-style assignment (FOO=bar) → not a package.
        if !arg.starts_with('@') && arg.contains('=') {
            continue;
        }
        return Some(arg.to_string());
    }
    None
}

#[derive(Deserialize)]
struct RegistryResp {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    repository: Option<RepositoryField>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    readme: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RepositoryField {
    Url(String),
    Object {
        #[serde(default)]
        url: Option<String>,
    },
}

/// HTTP fetch + parse. No caching, no fallback — the orchestration layer
/// owns those concerns.
pub async fn fetch_npm_metadata(base_url: &str, pkg: &str) -> anyhow::Result<NpmMetadata> {
    let url = format!("{base_url}/{pkg}/latest");
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .user_agent("quiver-ingestion/0.1")
        .build()?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("get {url}"))?;
    if !resp.status().is_success() {
        let s = resp.status();
        let t = resp.text().await.unwrap_or_default();
        return Err(anyhow!("npm registry http {s}: {t}"));
    }
    let parsed: RegistryResp = resp.json().await.context("decode npm registry response")?;

    let repo_url = parsed.repository.and_then(|r| match r {
        RepositoryField::Url(s) => Some(s),
        RepositoryField::Object { url } => url,
    });
    let keywords = parsed
        .keywords
        .into_iter()
        .map(|k| k.trim().to_ascii_lowercase())
        .filter(|k| !k.is_empty())
        .collect();

    Ok(NpmMetadata {
        package: pkg.to_string(),
        description: parsed.description.filter(|s| !s.trim().is_empty()),
        keywords,
        repository: repo_url.map(normalize_repo_url),
        homepage: parsed.homepage.filter(|s| !s.trim().is_empty()),
        readme: parsed.readme.filter(|s| !s.trim().is_empty()),
    })
}

/// `git+https://github.com/foo/bar.git` → `https://github.com/foo/bar`.
/// Mirrors the helper in `plugin_manifest::normalize_repo_url`.
pub fn normalize_repo_url(raw: String) -> String {
    let trimmed = raw.trim();
    let stripped = trimmed.strip_prefix("git+").unwrap_or(trimmed);
    let no_git = stripped.strip_suffix(".git").unwrap_or(stripped);
    no_git.to_string()
}

/// Cache-aware enrichment: returns metadata from the local SQLite cache
/// when fresh; on miss + `NetworkMode::Online` performs a registry fetch
/// and persists the response. Network failures are returned to the caller
/// as `Err` so they can decide whether to degrade silently.
pub async fn enrich_via_cache(
    conn: &Connection,
    base_url: &str,
    pkg: &str,
    network: NetworkMode,
) -> anyhow::Result<Option<NpmMetadata>> {
    if let Some(row) = mcp_npm::get(conn, pkg, DEFAULT_TTL_DAYS)? {
        return Ok(Some(row.into()));
    }
    if network == NetworkMode::Offline {
        return Ok(None);
    }
    let meta = fetch_npm_metadata(base_url, pkg).await?;
    mcp_npm::upsert(conn, &meta.clone().into_cache_row(Utc::now()))?;
    Ok(Some(meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(slice: &[&str]) -> Vec<String> {
        slice.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn extract_npx_y_scoped_package() {
        assert_eq!(
            extract_npm_pkg("npx", &args(&["-y", "@context7/mcp-server"])),
            Some("@context7/mcp-server".to_string())
        );
    }

    #[test]
    fn extract_npx_unscoped() {
        assert_eq!(
            extract_npm_pkg("npx", &args(&["pkg-name"])),
            Some("pkg-name".to_string())
        );
    }

    #[test]
    fn extract_npx_with_absolute_path_command() {
        assert_eq!(
            extract_npm_pkg("/usr/bin/npx", &args(&["-y", "pkg"])),
            Some("pkg".to_string())
        );
    }

    #[test]
    fn extract_bunx() {
        assert_eq!(
            extract_npm_pkg("bunx", &args(&["pkg"])),
            Some("pkg".to_string())
        );
    }

    #[test]
    fn extract_bun_x_subcommand() {
        assert_eq!(
            extract_npm_pkg("bun", &args(&["x", "pkg"])),
            Some("pkg".to_string())
        );
    }

    #[test]
    fn extract_pnpm_dlx_subcommand() {
        assert_eq!(
            extract_npm_pkg("pnpm", &args(&["dlx", "@scope/pkg"])),
            Some("@scope/pkg".to_string())
        );
    }

    #[test]
    fn extract_pnpx() {
        assert_eq!(
            extract_npm_pkg("pnpx", &args(&["-y", "pkg"])),
            Some("pkg".to_string())
        );
    }

    #[test]
    fn extract_returns_none_for_node_script() {
        assert_eq!(extract_npm_pkg("node", &args(&["./local/script.js"])), None);
    }

    #[test]
    fn extract_returns_none_for_uvx() {
        assert_eq!(extract_npm_pkg("uvx", &args(&["python-pkg"])), None);
    }

    #[test]
    fn extract_skips_env_assignment_args() {
        assert_eq!(
            extract_npm_pkg("npx", &args(&["FOO=bar", "@scope/pkg"])),
            Some("@scope/pkg".to_string())
        );
    }

    #[test]
    fn extract_returns_none_for_bun_with_unknown_subcommand() {
        assert_eq!(extract_npm_pkg("bun", &args(&["run", "build"])), None);
    }

    #[test]
    fn normalize_repo_url_strips_git_prefix_and_dot_git() {
        assert_eq!(
            normalize_repo_url("git+https://github.com/foo/bar.git".into()),
            "https://github.com/foo/bar"
        );
    }

    #[test]
    fn normalize_repo_url_leaves_clean_urls_alone() {
        assert_eq!(
            normalize_repo_url("https://github.com/foo/bar".into()),
            "https://github.com/foo/bar"
        );
    }
}
