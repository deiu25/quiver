//! Optional Sonnet-backed task classifier for the agent loop.
//!
//! Each `UserText` event is fed through a [`TaskClassifier`] before the
//! recommender. The classifier returns:
//!
//!   * `is_task=false` ⇒ engine drops the message (no hint, no
//!     `agent_suggestions` row) — filters greetings, acks, status checks.
//!   * `is_task=true` + `query` ⇒ engine embeds `query` (a focused rewrite),
//!     not the raw text, and continues.
//!
//! Two impls ship here:
//!
//!   * [`NoopClassifier`] — passthrough, `is_task=true`, `query=raw`. Used
//!     when the agent is started without `--classify` / `QUIVER_TASK_CLASSIFIER`.
//!   * [`SonnetClassifier`] — calls Anthropic API (`ANTHROPIC_API_KEY`) or local
//!     `claude` CLI. Mirrors the backend selection in
//!     [`quiver_ingestion::llm_extract::ClaudeExtractor`]; same model, same
//!     timeout pattern.
//!
//! The trait method never returns an error: every failure path inside
//! `SonnetClassifier` (timeout, non-2xx, malformed JSON, missing CLI binary)
//! logs a warning and falls back to passthrough so the agent never silently
//! drops a real task because of a transient LLM glitch.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

const RAW_TRUNCATE_BYTES: usize = 2000;
const CLASSIFY_TIMEOUT: Duration = Duration::from_secs(15);
const ANTHROPIC_MODEL: &str = "claude-sonnet-4-6";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";

const SYSTEM_PROMPT: &str = "You triage developer messages for a tool recommender. \
Return ONLY a JSON object (no prose, no code fences) matching: \
{ \"is_task\": bool, \"query\": string }. \
is_task=false when the message is a greeting, ack, status check, or pure chit-chat. \
is_task=true when the user wants code written, changed, explained, or a tool invoked. \
query: a short imperative summary (<=120 chars) of what the user wants done; \
on is_task=false, return the empty string.";

/// Output of one classifier call. `query` equals `raw` for the noop and on
/// any LLM failure, so callers can use it unconditionally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedTask {
    pub is_task: bool,
    pub query: String,
}

impl ClassifiedTask {
    pub fn passthrough(raw: &str) -> Self {
        Self {
            is_task: true,
            query: raw.to_string(),
        }
    }
}

#[async_trait]
pub trait TaskClassifier: Send + Sync {
    async fn classify(&self, raw: &str) -> ClassifiedTask;
}

// ── NoopClassifier ──────────────────────────────────────────────────────────

#[derive(Default, Debug, Clone, Copy)]
pub struct NoopClassifier;

#[async_trait]
impl TaskClassifier for NoopClassifier {
    async fn classify(&self, raw: &str) -> ClassifiedTask {
        ClassifiedTask::passthrough(raw)
    }
}

// ── SonnetClassifier ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ClaudeBackend {
    Api { api_key: String, base_url: String },
    Cli { binary: String },
}

pub struct SonnetClassifier {
    backend: ClaudeBackend,
    timeout: Duration,
}

impl SonnetClassifier {
    pub fn new(backend: ClaudeBackend) -> Self {
        Self {
            backend,
            timeout: CLASSIFY_TIMEOUT,
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
            ClaudeBackend::Api { .. } => "sonnet-api",
            ClaudeBackend::Cli { .. } => "sonnet-cli",
        }
    }

    async fn try_classify(&self, raw: &str) -> Result<ClassifiedTask> {
        let user_msg = format!(
            "Developer message (truncated to {RAW_TRUNCATE_BYTES} chars):\n{body}",
            body = truncate_chars(raw, RAW_TRUNCATE_BYTES)
        );
        let text = match &self.backend {
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
        Ok(parse_classify_json(&text, raw))
    }
}

#[async_trait]
impl TaskClassifier for SonnetClassifier {
    async fn classify(&self, raw: &str) -> ClassifiedTask {
        match self.try_classify(raw).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("sonnet classify failed, passthrough: {e:#}");
                ClassifiedTask::passthrough(raw)
            },
        }
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

async fn call_anthropic_api(base_url: &str, api_key: &str, user_msg: &str) -> Result<String> {
    let body = json!({
        "model": ANTHROPIC_MODEL,
        "max_tokens": 200,
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

async fn call_claude_cli(binary: &str, user_msg: &str) -> Result<String> {
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
    is_task: Option<bool>,
    #[serde(default)]
    query: Option<String>,
}

/// Parse the LLM response into a `ClassifiedTask`. Tolerant of stray prose
/// and code fences. On garbage / missing fields, returns
/// `ClassifiedTask::passthrough(raw)` — never silently drops a message.
pub fn parse_classify_json(text: &str, raw: &str) -> ClassifiedTask {
    let trimmed = strip_code_fence(text.trim()).trim();
    let start = trimmed.find('{');
    let end = trimmed.rfind('}');
    let payload = match (start, end) {
        (Some(a), Some(b)) if b >= a => &trimmed[a..=b],
        _ => return ClassifiedTask::passthrough(raw),
    };
    let parsed: LlmJson = match serde_json::from_str(payload) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("classify json parse failed: {e:#}");
            return ClassifiedTask::passthrough(raw);
        },
    };
    let is_task = parsed.is_task.unwrap_or(true);
    let query_raw = parsed.query.unwrap_or_default();
    let query_trim = query_raw.trim();
    let query = if !is_task {
        String::new()
    } else if query_trim.is_empty() {
        raw.to_string()
    } else {
        query_trim.to_string()
    };
    ClassifiedTask { is_task, query }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_classifier_returns_passthrough() {
        let c = NoopClassifier;
        let out = c.classify("write a tailwind config").await;
        assert!(out.is_task);
        assert_eq!(out.query, "write a tailwind config");
    }

    #[test]
    fn parse_strips_fences_and_extracts_fields() {
        let raw =
            "Sure! Here:\n```json\n{\"is_task\":true,\"query\":\"extract design tokens\"}\n```\n";
        let c = parse_classify_json(raw, "original");
        assert!(c.is_task);
        assert_eq!(c.query, "extract design tokens");
    }

    #[test]
    fn parse_handles_is_task_false() {
        let raw = "{\"is_task\":false,\"query\":\"\"}";
        let c = parse_classify_json(raw, "thanks!");
        assert!(!c.is_task);
        assert_eq!(c.query, "");
    }

    #[test]
    fn parse_falls_back_on_garbage() {
        let c = parse_classify_json("not json at all", "real task");
        assert!(c.is_task);
        assert_eq!(c.query, "real task");
    }

    #[test]
    fn parse_falls_back_on_unbalanced_braces() {
        let c = parse_classify_json("{ broken json", "real task");
        assert!(c.is_task);
        assert_eq!(c.query, "real task");
    }

    #[test]
    fn parse_empty_query_with_is_task_true_uses_raw() {
        // Defensive: model says is_task=true but forgot the query — don't lose
        // the original text.
        let raw = "{\"is_task\":true,\"query\":\"\"}";
        let c = parse_classify_json(raw, "wire up auth middleware");
        assert!(c.is_task);
        assert_eq!(c.query, "wire up auth middleware");
    }

    #[test]
    fn parse_missing_is_task_defaults_to_true() {
        let raw = "{\"query\":\"do x\"}";
        let c = parse_classify_json(raw, "do x literal");
        assert!(c.is_task);
        assert_eq!(c.query, "do x");
    }

    #[test]
    fn truncate_chars_respects_utf8_boundary() {
        let s = "héllo wörld";
        // 11 chars, 13 bytes — truncating to 6 bytes should land on a boundary.
        let out = truncate_chars(s, 6);
        assert!(s.starts_with(&out));
        assert!(out.len() <= 6);
    }
}
