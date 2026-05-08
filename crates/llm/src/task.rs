//! Task triage classifier — drives the daily-agent loop.
//!
//! Returns [`ClassifiedTask`]:
//!
//!   * `is_task=false` ⇒ engine drops the message (no hint, no
//!     `agent_suggestions` row) — filters greetings, acks, status checks.
//!   * `is_task=true` + `query` ⇒ engine embeds `query` (a focused rewrite),
//!     not the raw text, and continues.
//!
//! Two impls ship here: [`NoopClassifier`] (passthrough) and
//! [`SonnetClassifier`] (Claude-backed). Failure modes inside the Sonnet
//! variant log a warning and fall back to passthrough so the agent never
//! silently drops a real task because of a transient LLM glitch.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;

use crate::backend::{
    CLASSIFY_TIMEOUT, ClaudeBackend, RAW_TRUNCATE_BYTES, call_backend, label_for, strip_code_fence,
    truncate_chars,
};

const SYSTEM_PROMPT: &str = "You triage developer messages for a tool recommender. \
Return ONLY a JSON object (no prose, no code fences) matching: \
{ \"is_task\": bool, \"query\": string }. \
is_task=false when the message is a greeting, ack, status check, or pure chit-chat. \
is_task=true when the user wants code written, changed, explained, or a tool invoked. \
query: a short imperative summary (<=120 chars) of what the user wants done; \
on is_task=false, return the empty string.";

const MAX_TOKENS: u32 = 200;

/// Output of one triage call. `query` equals `raw` for the noop and on any
/// LLM failure, so callers can use it unconditionally.
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

#[derive(Default, Debug, Clone, Copy)]
pub struct NoopClassifier;

#[async_trait]
impl TaskClassifier for NoopClassifier {
    async fn classify(&self, raw: &str) -> ClassifiedTask {
        ClassifiedTask::passthrough(raw)
    }
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

    /// API > CLI. `None` if no credentials and no `claude` binary on `PATH`.
    pub fn detect() -> Option<Self> {
        crate::backend::detect_backend().map(Self::new)
    }

    pub fn label(&self) -> &'static str {
        label_for(&self.backend)
    }

    async fn try_classify(&self, raw: &str) -> Result<ClassifiedTask> {
        let user_msg = format!(
            "Developer message (truncated to {RAW_TRUNCATE_BYTES} chars):\n{body}",
            body = truncate_chars(raw, RAW_TRUNCATE_BYTES)
        );
        let text = call_backend(
            &self.backend,
            SYSTEM_PROMPT,
            &user_msg,
            MAX_TOKENS,
            self.timeout,
        )
        .await?;
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

#[derive(Deserialize)]
struct LlmJson {
    #[serde(default)]
    is_task: Option<bool>,
    #[serde(default)]
    query: Option<String>,
}

/// Tolerant of stray prose and code fences. On garbage / missing fields,
/// returns `ClassifiedTask::passthrough(raw)` — never silently drops a
/// message.
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
}
