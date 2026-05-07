//! Optional LLM-assisted metadata enrichment for onboarded GitHub repos.
//!
//! Pipeline: a `MetadataExtractor` reads `(name, README)` and returns
//! `ExtractedMetadata { triggers, examples, category }`. Two production
//! impls ship here:
//!
//!   * [`RegexExtractor`] — pure / sync / no I/O / always available.
//!   * [`ClaudeExtractor`] — calls Anthropic API (if `ANTHROPIC_API_KEY`)
//!     or local `claude` CLI (if on PATH).
//!
//! `CompositeExtractor` runs a primary extractor and merges any empty fields
//! from a fallback. `detect()` builds the right composite for the current
//! environment.
//!
//! Failures never surface — every backend logs and returns empty extraction
//! so onboarding stays deterministic offline.
//!
//! Wired into `github_repo::onboard` via `enrich_with_llm`.

use std::time::Duration;

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

const README_TRUNCATE_BYTES: usize = 4000;
const LLM_TIMEOUT: Duration = Duration::from_secs(15);
const ANTHROPIC_MODEL: &str = "claude-sonnet-4-6";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";

const SYSTEM_PROMPT: &str = "You extract structured metadata for a developer-tool registry. \
Return ONLY a JSON object matching this schema (no prose, no code fences): \
{ \"triggers\": string[], \"examples\": string[], \"category\": string|null }. \
triggers: <=5 short imperative phrases describing when to invoke the tool. \
examples: <=3 verbatim code or command snippets from the README, no surrounding fences. \
category: one of skill, plugin, mcp-server, cli, agent, doc, or null.";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExtractedMetadata {
    pub triggers: Vec<String>,
    pub examples: Vec<Value>,
    pub category: Option<String>,
}

impl ExtractedMetadata {
    pub fn is_empty(&self) -> bool {
        self.triggers.is_empty() && self.examples.is_empty() && self.category.is_none()
    }
}

#[async_trait]
pub trait MetadataExtractor: Send + Sync {
    async fn extract(&self, name: &str, readme: &str) -> anyhow::Result<ExtractedMetadata>;
}

// ── RegexExtractor ──────────────────────────────────────────────────────────

#[derive(Default, Debug, Clone, Copy)]
pub struct RegexExtractor;

#[async_trait]
impl MetadataExtractor for RegexExtractor {
    async fn extract(&self, _name: &str, readme: &str) -> anyhow::Result<ExtractedMetadata> {
        Ok(extract_with_regex(readme))
    }
}

fn extract_with_regex(readme: &str) -> ExtractedMetadata {
    ExtractedMetadata {
        triggers: regex_triggers(readme),
        examples: regex_examples(readme),
        category: regex_category(readme),
    }
}

fn regex_triggers(readme: &str) -> Vec<String> {
    let trigger_headings = [
        "use cases",
        "uses",
        "use",
        "when to use",
        "triggers",
        "trigger",
        "what it does",
    ];
    let mut out: Vec<String> = Vec::new();
    let mut in_section = false;
    for line in readme.lines() {
        let trimmed = line.trim();
        if let Some(heading) = strip_md_heading(trimmed) {
            in_section = trigger_headings.contains(&heading.to_lowercase().as_str());
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(item) = strip_bullet(trimmed) {
            let cleaned = item.trim().trim_end_matches('.').to_string();
            if cleaned.len() > 2 && !out.iter().any(|x| x.eq_ignore_ascii_case(&cleaned)) {
                out.push(cleaned);
                if out.len() >= 5 {
                    break;
                }
            }
        }
    }
    out
}

fn regex_examples(readme: &str) -> Vec<Value> {
    let example_headings = [
        "examples",
        "example",
        "usage",
        "quick start",
        "quickstart",
        "getting started",
    ];
    let mut in_section = false;
    let mut in_fence = false;
    let mut current_lang = String::new();
    let mut current_body = String::new();
    let mut out: Vec<Value> = Vec::new();
    for line in readme.lines() {
        if !in_fence && let Some(heading) = strip_md_heading(line.trim()) {
            in_section = example_headings.contains(&heading.to_lowercase().as_str());
            continue;
        }
        if let Some(rest) = line.strip_prefix("```") {
            if in_fence {
                if in_section && !current_body.trim().is_empty() {
                    let lang = current_lang.clone();
                    out.push(json!({
                        "type": "code",
                        "lang": if lang.is_empty() { Value::Null } else { Value::String(lang) },
                        "body": current_body.trim_end().to_string(),
                    }));
                    if out.len() >= 3 {
                        return out;
                    }
                }
                in_fence = false;
                current_lang.clear();
                current_body.clear();
            } else {
                in_fence = true;
                current_lang = rest.trim().to_string();
                current_body.clear();
            }
            continue;
        }
        if in_fence {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }
    out
}

fn regex_category(readme: &str) -> Option<String> {
    let head: String = readme.chars().take(800).collect::<String>().to_lowercase();
    let table: &[(&str, &[&str])] = &[
        (
            "mcp-server",
            &["mcp server", "model context protocol", "mcp:"],
        ),
        (
            "cli",
            &["command line", "command-line", "cli tool", "binary"],
        ),
        ("skill", &["skill bundle", "skill.md", "claude code skill"]),
        ("plugin", &["plugin marketplace", "plugin.json"]),
        (
            "agent",
            &[
                "agent loop",
                "autonomous agent",
                "subagent",
                "workflow agent",
            ],
        ),
    ];
    for (cat, kws) in table {
        if kws.iter().any(|k| head.contains(k)) {
            return Some((*cat).to_string());
        }
    }
    None
}

fn strip_md_heading(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches('#');
    if rest.len() == line.len() || rest.is_empty() {
        return None;
    }
    Some(rest.trim())
}

fn strip_bullet(line: &str) -> Option<&str> {
    for prefix in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return Some(rest);
        }
    }
    None
}

// ── ClaudeExtractor ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ClaudeBackend {
    Api { api_key: String, base_url: String },
    Cli { binary: String },
}

pub struct ClaudeExtractor {
    backend: ClaudeBackend,
    timeout: Duration,
}

impl ClaudeExtractor {
    pub fn new(backend: ClaudeBackend) -> Self {
        Self {
            backend,
            timeout: LLM_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    /// API > CLI. Returns `None` when neither is available.
    pub fn detect() -> Option<Self> {
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY")
            && !key.trim().is_empty()
        {
            return Some(Self::new(ClaudeBackend::Api {
                api_key: key,
                base_url: ANTHROPIC_URL.to_string(),
            }));
        }
        if let Some(bin) = which_claude() {
            return Some(Self::new(ClaudeBackend::Cli { binary: bin }));
        }
        None
    }

    pub fn label(&self) -> &'static str {
        match self.backend {
            ClaudeBackend::Api { .. } => "claude-api",
            ClaudeBackend::Cli { .. } => "claude-cli",
        }
    }
}

#[async_trait]
impl MetadataExtractor for ClaudeExtractor {
    async fn extract(&self, name: &str, readme: &str) -> anyhow::Result<ExtractedMetadata> {
        let user_msg = format!(
            "Tool name: {name}\nREADME (truncated to {READ} chars):\n{body}",
            READ = README_TRUNCATE_BYTES,
            body = truncate_chars(readme, README_TRUNCATE_BYTES)
        );

        let raw = match &self.backend {
            ClaudeBackend::Api { api_key, base_url } => tokio::time::timeout(
                self.timeout,
                call_anthropic_api(base_url, api_key, &user_msg),
            )
            .await
            .map_err(|_| anyhow!("anthropic api timeout"))??,
            ClaudeBackend::Cli { binary } => {
                tokio::time::timeout(self.timeout, call_claude_cli(binary, &user_msg))
                    .await
                    .map_err(|_| anyhow!("claude cli timeout"))??
            },
        };

        Ok(parse_llm_json(&raw))
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut out = String::with_capacity(max);
    for ch in s.chars() {
        if out.len() + ch.len_utf8() > max {
            break;
        }
        out.push(ch);
    }
    out
}

#[derive(Deserialize)]
struct ApiResp {
    content: Vec<ApiContentBlock>,
}
#[derive(Deserialize)]
struct ApiContentBlock {
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    text: String,
}

async fn call_anthropic_api(
    base_url: &str,
    api_key: &str,
    user_msg: &str,
) -> anyhow::Result<String> {
    let body = json!({
        "model": ANTHROPIC_MODEL,
        "max_tokens": 800,
        "system": SYSTEM_PROMPT,
        "messages": [{ "role": "user", "content": user_msg }],
    });
    let client = reqwest::Client::builder().build()?;
    let resp = client
        .post(base_url)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("post anthropic /v1/messages")?;
    if !resp.status().is_success() {
        let s = resp.status();
        let t = resp.text().await.unwrap_or_default();
        return Err(anyhow!("anthropic http {s}: {t}"));
    }
    let parsed: ApiResp = resp.json().await.context("decode anthropic response")?;
    let text = parsed
        .content
        .into_iter()
        .filter(|b| b.r#type == "text")
        .map(|b| b.text)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(text)
}

#[derive(Deserialize)]
struct CliResp {
    #[serde(default)]
    result: String,
}

async fn call_claude_cli(binary: &str, user_msg: &str) -> anyhow::Result<String> {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    let prompt = format!("{SYSTEM_PROMPT}\n\n{user_msg}");
    let mut child = Command::new(binary)
        .args(["--print", "--output-format", "json"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("spawn claude cli")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(prompt.as_bytes()).await?;
        stdin.shutdown().await?;
    }
    let out = child.wait_with_output().await.context("wait claude cli")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("claude cli exit {}: {stderr}", out.status));
    }
    let raw = String::from_utf8_lossy(&out.stdout).to_string();
    let parsed: CliResp =
        serde_json::from_str(&raw).context("parse claude --output-format json")?;
    Ok(parsed.result)
}

#[derive(Deserialize)]
struct LlmJson {
    #[serde(default)]
    triggers: Vec<String>,
    #[serde(default)]
    examples: Vec<String>,
    #[serde(default)]
    category: Option<String>,
}

fn parse_llm_json(text: &str) -> ExtractedMetadata {
    let trimmed = strip_code_fence(text.trim()).trim();
    // Find first `{` and matching last `}` to be tolerant of stray prose.
    let start = trimmed.find('{');
    let end = trimmed.rfind('}');
    let payload = match (start, end) {
        (Some(a), Some(b)) if b >= a => &trimmed[a..=b],
        _ => return ExtractedMetadata::default(),
    };
    let parsed: LlmJson = match serde_json::from_str(payload) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("llm json parse failed: {e:#}");
            return ExtractedMetadata::default();
        },
    };
    let triggers: Vec<String> = parsed
        .triggers
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .take(5)
        .collect();
    let examples: Vec<Value> = parsed
        .examples
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .take(3)
        .map(|body| {
            json!({
                "type": "code",
                "lang": Value::Null,
                "body": body,
            })
        })
        .collect();
    let category = parsed.category.and_then(|c| {
        let c = c.trim().to_lowercase();
        if c.is_empty() || c == "null" {
            None
        } else {
            Some(c)
        }
    });
    ExtractedMetadata {
        triggers,
        examples,
        category,
    }
}

fn strip_code_fence(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix("```json").or_else(|| s.strip_prefix("```"))
        && let Some(end) = rest.rfind("```")
    {
        return &rest[..end];
    }
    s
}

fn which_claude() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("claude");
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

// ── CompositeExtractor ─────────────────────────────────────────────────────

pub struct CompositeExtractor {
    pub primary: Box<dyn MetadataExtractor>,
    pub fallback: Box<dyn MetadataExtractor>,
}

impl CompositeExtractor {
    pub fn new(primary: Box<dyn MetadataExtractor>, fallback: Box<dyn MetadataExtractor>) -> Self {
        Self { primary, fallback }
    }
}

#[async_trait]
impl MetadataExtractor for CompositeExtractor {
    async fn extract(&self, name: &str, readme: &str) -> anyhow::Result<ExtractedMetadata> {
        let mut merged = match self.primary.extract(name, readme).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("primary extractor failed: {e:#}");
                ExtractedMetadata::default()
            },
        };
        if !merged.triggers.is_empty() && !merged.examples.is_empty() && merged.category.is_some() {
            return Ok(merged);
        }
        match self.fallback.extract(name, readme).await {
            Ok(fb) => {
                if merged.triggers.is_empty() {
                    merged.triggers = fb.triggers;
                }
                if merged.examples.is_empty() {
                    merged.examples = fb.examples;
                }
                if merged.category.is_none() {
                    merged.category = fb.category;
                }
            },
            Err(e) => tracing::warn!("fallback extractor failed: {e:#}"),
        }
        Ok(merged)
    }
}

/// Build the extractor for the current environment, honouring opt-out flags.
///
/// `force_regex_only`: caller passes `true` for `--no-llm` or
/// `QUIVER_LLM_EXTRACT=0`. Returns a logging label for INFO output.
pub fn build_default(force_regex_only: bool) -> (Box<dyn MetadataExtractor>, &'static str) {
    if force_regex_only {
        return (Box::new(RegexExtractor), "regex-only");
    }
    match ClaudeExtractor::detect() {
        Some(claude) => {
            let label = claude.label();
            let composite = CompositeExtractor::new(Box::new(claude), Box::new(RegexExtractor));
            (Box::new(composite), label)
        },
        None => (Box::new(RegexExtractor), "regex-only"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_readme() -> &'static str {
        r#"# fake-tool

Some intro paragraph about the fake tool.

## When to use

- Generate boilerplate React components
- Audit Tailwind config drift
- Convert Stitch designs to JSX
- Lint a design token sheet

## Examples

```bash
fake-tool gen button
```

```ts
import { fakeTool } from 'fake-tool'
fakeTool.generate({ name: 'Card' })
```

## Notes

This is a CLI tool, also exposed as an MCP server.
"#
    }

    #[tokio::test]
    async fn regex_extracts_triggers_from_headed_bullets() {
        let m = RegexExtractor
            .extract("fake-tool", sample_readme())
            .await
            .unwrap();
        assert_eq!(m.triggers.len(), 4);
        assert!(m.triggers[0].starts_with("Generate"));
    }

    #[tokio::test]
    async fn regex_extracts_first_2_code_fences_under_examples() {
        let m = RegexExtractor
            .extract("fake-tool", sample_readme())
            .await
            .unwrap();
        assert_eq!(m.examples.len(), 2);
        assert_eq!(m.examples[0]["lang"], json!("bash"));
        assert!(
            m.examples[0]["body"]
                .as_str()
                .unwrap()
                .contains("fake-tool")
        );
        assert_eq!(m.examples[1]["lang"], json!("ts"));
    }

    #[tokio::test]
    async fn regex_caps_triggers_and_examples() {
        // 7 triggers, 5 fences → cap at 5 / 3.
        let r = "## When to use\n- alpha trigger\n- beta trigger\n- gamma trigger\n- delta trigger\n- epsilon trigger\n- zeta trigger\n- eta trigger\n\n## Examples\n```\n1\n```\n```\n2\n```\n```\n3\n```\n```\n4\n```\n```\n5\n```\n";
        let m = RegexExtractor.extract("x", r).await.unwrap();
        assert_eq!(m.triggers.len(), 5);
        assert_eq!(m.examples.len(), 3);
    }

    #[tokio::test]
    async fn regex_category_from_keywords() {
        let r = "# t\n\nA command-line tool.\n";
        let m = RegexExtractor.extract("t", r).await.unwrap();
        assert_eq!(m.category.as_deref(), Some("cli"));
    }

    #[tokio::test]
    async fn regex_category_none_without_keywords() {
        let r = "# t\n\nJust some random helper.\n";
        let m = RegexExtractor.extract("t", r).await.unwrap();
        assert_eq!(m.category, None);
    }

    #[test]
    fn parse_llm_json_strips_prose_and_fences() {
        let raw = "Sure! Here you go:\n```json\n{\"triggers\":[\"x\"],\"examples\":[\"echo hi\"],\"category\":\"cli\"}\n```\n";
        let m = parse_llm_json(raw);
        assert_eq!(m.triggers, vec!["x".to_string()]);
        assert_eq!(m.examples.len(), 1);
        assert_eq!(m.examples[0]["body"], json!("echo hi"));
        assert_eq!(m.category.as_deref(), Some("cli"));
    }

    #[test]
    fn parse_llm_json_returns_default_on_garbage() {
        assert!(parse_llm_json("not json at all").is_empty());
        assert!(parse_llm_json("{ bad json").is_empty());
    }

    struct Mock {
        out: ExtractedMetadata,
        err: bool,
    }
    #[async_trait]
    impl MetadataExtractor for Mock {
        async fn extract(&self, _: &str, _: &str) -> anyhow::Result<ExtractedMetadata> {
            if self.err {
                Err(anyhow!("boom"))
            } else {
                Ok(self.out.clone())
            }
        }
    }

    #[tokio::test]
    async fn composite_falls_back_when_primary_errors() {
        let comp = CompositeExtractor::new(
            Box::new(Mock {
                out: ExtractedMetadata::default(),
                err: true,
            }),
            Box::new(RegexExtractor),
        );
        let m = comp.extract("x", sample_readme()).await.unwrap();
        assert!(!m.triggers.is_empty());
    }

    #[tokio::test]
    async fn composite_merges_only_empty_fields() {
        let primary_out = ExtractedMetadata {
            triggers: vec!["primary trigger".into()],
            examples: vec![],
            category: None,
        };
        let fallback_out = ExtractedMetadata {
            triggers: vec!["fallback trigger".into()],
            examples: vec![json!({ "type": "code", "lang": Value::Null, "body": "fb" })],
            category: Some("cli".into()),
        };
        let comp = CompositeExtractor::new(
            Box::new(Mock {
                out: primary_out,
                err: false,
            }),
            Box::new(Mock {
                out: fallback_out,
                err: false,
            }),
        );
        let m = comp.extract("x", "anything").await.unwrap();
        assert_eq!(m.triggers, vec!["primary trigger".to_string()]);
        assert_eq!(m.examples.len(), 1);
        assert_eq!(m.category.as_deref(), Some("cli"));
    }

    #[tokio::test]
    async fn build_default_force_regex_only_picks_regex() {
        let (_, label) = build_default(true);
        assert_eq!(label, "regex-only");
    }
}
