//! Read `.claude-plugin/plugin.json` + nearby README for offline plugin
//! enrichment. The Claude Code plugin cache lives under
//! `~/.claude/plugins/cache/<marketplace>/<plugin>/<version>/` and every
//! installed plugin ships a manifest with a real description, keywords,
//! repository URL, etc. We use that to give the recommender semantic
//! signal it can actually rank against the much richer SKILL.md corpus.
//!
//! Failures are absorbed: if the manifest is missing or malformed we
//! return `None` so the caller falls back to the legacy stub ToolMeta.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

const README_EXCERPT_LIMIT: usize = 600;
const README_PARAGRAPH_MIN_CHARS: usize = 80;

/// Subset of `.claude-plugin/plugin.json` we care about for ToolMeta
/// enrichment. All fields are optional; a missing field is just absent
/// signal, not an error.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PluginManifest {
    pub description: Option<String>,
    pub keywords: Vec<String>,
    pub repository: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub author: Option<String>,
    pub readme_excerpt: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    repository: Option<RepositoryField>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    author: Option<AuthorField>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RepositoryField {
    Url(String),
    Object {
        #[serde(default)]
        url: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AuthorField {
    Name(String),
    Object {
        #[serde(default)]
        name: Option<String>,
    },
}

/// Build the cache directory path for a given (marketplace, plugin,
/// version) triple. Pure path arithmetic, no I/O.
pub fn cache_dir_for(plugin_root: &Path, marketplace: &str, name: &str, version: &str) -> PathBuf {
    plugin_root.join(marketplace).join(name).join(version)
}

/// Read `<cache_dir>/.claude-plugin/plugin.json` + `<cache_dir>/README.md`.
/// Returns `None` if the manifest itself is missing or unparseable; the
/// README is a soft optional and failures there leave `readme_excerpt` as
/// `None` while still returning the parsed manifest.
pub fn read_plugin_manifest(cache_dir: &Path) -> Option<PluginManifest> {
    let manifest_path = cache_dir.join(".claude-plugin").join("plugin.json");
    let raw = fs::read_to_string(&manifest_path).ok()?;
    let parsed: RawManifest = serde_json::from_str(&raw).ok()?;

    let repository = parsed.repository.and_then(|r| match r {
        RepositoryField::Url(s) => Some(s),
        RepositoryField::Object { url } => url,
    });
    let author = parsed.author.and_then(|a| match a {
        AuthorField::Name(s) => Some(s),
        AuthorField::Object { name } => name,
    });

    let readme_excerpt = read_readme_excerpt(cache_dir);

    Some(PluginManifest {
        description: parsed.description.filter(|s| !s.trim().is_empty()),
        keywords: parsed
            .keywords
            .into_iter()
            .map(|k| k.trim().to_ascii_lowercase())
            .filter(|k| !k.is_empty())
            .collect(),
        repository: repository.map(normalize_repo_url),
        homepage: parsed.homepage.filter(|s| !s.trim().is_empty()),
        license: parsed.license.filter(|s| !s.trim().is_empty()),
        author: author.filter(|s| !s.trim().is_empty()),
        readme_excerpt,
    })
}

/// `git+https://github.com/foo/bar.git` → `https://github.com/foo/bar`.
/// Leaves anything we don't recognise untouched.
pub fn normalize_repo_url(raw: String) -> String {
    let trimmed = raw.trim();
    let stripped = trimmed.strip_prefix("git+").unwrap_or(trimmed);
    let no_git = stripped.strip_suffix(".git").unwrap_or(stripped);
    no_git.to_string()
}

fn read_readme_excerpt(cache_dir: &Path) -> Option<String> {
    let readme_path = cache_dir.join("README.md");
    let raw = fs::read_to_string(&readme_path).ok()?;
    extract_excerpt(&raw)
}

/// Walk a README looking for the first prose paragraph: skip HTML chrome,
/// badges, blockquotes, and any leading `# Title` heading. Returns up to
/// `README_EXCERPT_LIMIT` chars with a `…` suffix on truncation.
fn extract_excerpt(readme: &str) -> Option<String> {
    let mut buf = String::new();
    let mut seen_blank_after_title = false;

    for raw_line in readme.lines() {
        let line = raw_line.trim();

        if line.is_empty() {
            if !buf.is_empty() {
                // End of candidate paragraph. Decide whether to keep it.
                if buf.chars().count() >= README_PARAGRAPH_MIN_CHARS {
                    return Some(clip(buf.trim(), README_EXCERPT_LIMIT));
                }
                buf.clear();
            }
            seen_blank_after_title = true;
            continue;
        }

        if is_chrome_line(line) {
            buf.clear();
            continue;
        }

        if let Some(rest) = line.strip_prefix('#') {
            // `# Title` / `## Heading`. If we've already collected real
            // text, treat the heading as a paragraph terminator.
            if buf.chars().count() >= README_PARAGRAPH_MIN_CHARS {
                return Some(clip(buf.trim(), README_EXCERPT_LIMIT));
            }
            // Otherwise reset and keep walking — next prose paragraph wins.
            let _ = rest;
            buf.clear();
            seen_blank_after_title = true;
            continue;
        }

        if !seen_blank_after_title && buf.is_empty() {
            // Title-adjacent line (e.g. the tagline directly under a
            // centered title). Skip until the first blank-separated block.
            continue;
        }

        if !buf.is_empty() {
            buf.push(' ');
        }
        buf.push_str(line);
    }

    if buf.chars().count() >= README_PARAGRAPH_MIN_CHARS {
        Some(clip(buf.trim(), README_EXCERPT_LIMIT))
    } else {
        None
    }
}

fn is_chrome_line(line: &str) -> bool {
    let l = line.trim_start();
    l.starts_with("<p")
        || l.starts_with("</p")
        || l.starts_with("<img")
        || l.starts_with("<div")
        || l.starts_with("</div")
        || l.starts_with("<h1")
        || l.starts_with("</h1")
        || l.starts_with("<a ")
        || l.starts_with("![")
        || l.starts_with("[!")
        || l.starts_with("> ")
        || l.starts_with("---")
        || l == ">"
}

fn clip(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/plugin_cache")
    }

    #[test]
    fn cache_dir_for_joins_components() {
        let p = cache_dir_for(Path::new("/cache"), "mkt", "plug", "1.0");
        assert_eq!(p, PathBuf::from("/cache/mkt/plug/1.0"));
    }

    #[test]
    fn read_full_fixture_populates_every_field() {
        let dir = cache_dir_for(&fixture_root(), "test-market", "test-plugin", "1.0.0");
        let m = read_plugin_manifest(&dir).expect("manifest present");
        assert_eq!(
            m.description.as_deref(),
            Some("Multi-agent orchestration system for Claude Code")
        );
        assert!(m.keywords.contains(&"automation".to_string()));
        assert!(m.keywords.contains(&"testing".to_string()));
        assert_eq!(
            m.repository.as_deref(),
            Some("https://github.com/example/test-plugin")
        );
        assert_eq!(
            m.homepage.as_deref(),
            Some("https://github.com/example/test-plugin")
        );
        assert_eq!(m.license.as_deref(), Some("MIT"));
        assert_eq!(m.author.as_deref(), Some("test-plugin contributors"));
        let excerpt = m.readme_excerpt.expect("readme excerpt present");
        assert!(
            excerpt.contains("coordinates multiple agents"),
            "excerpt: {excerpt}"
        );
        assert!(!excerpt.contains("<p"));
        assert!(!excerpt.contains("![Stars"));
        assert!(!excerpt.starts_with('>'));
    }

    #[test]
    fn read_missing_manifest_returns_none() {
        let dir = fixture_root().join("does-not-exist/plug/9.9");
        assert!(read_plugin_manifest(&dir).is_none());
    }

    #[test]
    fn malformed_manifest_returns_none() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let manifest_dir = tmp.path().join(".claude-plugin");
        fs::create_dir_all(&manifest_dir)?;
        fs::write(manifest_dir.join("plugin.json"), "{ not valid json")?;
        assert!(read_plugin_manifest(tmp.path()).is_none());
        Ok(())
    }

    #[test]
    fn manifest_without_readme_yields_none_excerpt()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let manifest_dir = tmp.path().join(".claude-plugin");
        fs::create_dir_all(&manifest_dir)?;
        fs::write(
            manifest_dir.join("plugin.json"),
            r#"{"name":"x","description":"hello","keywords":["a","b"]}"#,
        )?;
        let m = read_plugin_manifest(tmp.path()).expect("manifest");
        assert_eq!(m.description.as_deref(), Some("hello"));
        assert_eq!(m.keywords, vec!["a".to_string(), "b".to_string()]);
        assert!(m.readme_excerpt.is_none());
        Ok(())
    }

    #[test]
    fn repository_object_form_is_normalized() -> std::result::Result<(), Box<dyn std::error::Error>>
    {
        let tmp = tempfile::tempdir()?;
        let manifest_dir = tmp.path().join(".claude-plugin");
        fs::create_dir_all(&manifest_dir)?;
        fs::write(
            manifest_dir.join("plugin.json"),
            r#"{
              "name":"x",
              "repository": { "type":"git", "url":"git+https://github.com/foo/bar.git" }
            }"#,
        )?;
        let m = read_plugin_manifest(tmp.path()).expect("manifest");
        assert_eq!(m.repository.as_deref(), Some("https://github.com/foo/bar"));
        Ok(())
    }

    #[test]
    fn keywords_are_lowercased_and_trimmed() -> std::result::Result<(), Box<dyn std::error::Error>>
    {
        let tmp = tempfile::tempdir()?;
        let manifest_dir = tmp.path().join(".claude-plugin");
        fs::create_dir_all(&manifest_dir)?;
        fs::write(
            manifest_dir.join("plugin.json"),
            r#"{"name":"x","keywords":["  Alpha ","BETA","gamma","",""]}"#,
        )?;
        let m = read_plugin_manifest(tmp.path()).expect("manifest");
        assert_eq!(m.keywords, vec!["alpha", "beta", "gamma"]);
        Ok(())
    }

    #[test]
    fn excerpt_clipping_appends_ellipsis() {
        let long: String = "Lorem ipsum dolor sit amet ".repeat(50);
        let body = format!("# Title\n\n{long}\n");
        let out = extract_excerpt(&body).expect("excerpt");
        assert!(out.chars().count() <= README_EXCERPT_LIMIT + 1);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn excerpt_skips_short_taglines() {
        let body = "# Title\n\n> Quote line.\n\nShort tagline.\n\nThis is the real paragraph that explains what the plugin does in enough detail to clear the eighty character minimum bar comfortably.\n";
        let out = extract_excerpt(body).expect("excerpt");
        assert!(out.starts_with("This is the real paragraph"));
    }
}
