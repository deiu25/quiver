//! Read-only vs mutation intent classifier — gates PreToolUse vetoes.
//!
//! Returns [`IntentVerdict`] with an `is_mutation` flag. The PreToolUse hook
//! consults this verdict before emitting `permissionDecision: "deny"`: a
//! `false` value (the user asked for an explanation, analysis, or read-only
//! investigation) suppresses the veto so Claude can read/grep without being
//! re-routed to a skill.
//!
//! Fail-open default is `is_mutation=true` so the absence of LLM credentials,
//! a timeout, or a malformed model response preserves the existing strict-mode
//! veto behaviour. We never silently flip a mutation prompt to read-only.

use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;

use crate::backend::{
    CLASSIFY_TIMEOUT, ClaudeBackend, RAW_TRUNCATE_BYTES, call_backend, label_for, strip_code_fence,
    truncate_chars,
};

const SYSTEM_PROMPT: &str = "You classify a developer's prompt for a coding agent. \
Return ONLY strict JSON (no prose, no code fences) matching: \
{ \"is_mutation\": bool, \"reason\": string }. \
is_mutation=true ⟺ the user wants the agent to MODIFY code, files, configuration, \
the system, or external state (write, edit, refactor, install, deploy, run a destructive \
command, scaffold a new feature). \
is_mutation=false ⟺ the user wants explanation, analysis, search, listing, status, \
debug-tracing, or any read-only investigation that does NOT change the repo. \
When unsure, prefer is_mutation=true. \
reason: one short clause (<=120 chars) explaining the choice.";

const MAX_TOKENS: u32 = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentVerdict {
    pub is_mutation: bool,
    pub reason: String,
}

impl IntentVerdict {
    /// Fail-open default: assume mutation so existing strict-mode vetoes
    /// still fire when the classifier is unavailable or returns garbage.
    pub fn passthrough() -> Self {
        Self {
            is_mutation: true,
            reason: String::from("fallback: classifier unavailable"),
        }
    }
}

pub struct IntentClassifier {
    backend: ClaudeBackend,
    timeout: Duration,
}

impl IntentClassifier {
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

    pub async fn classify(&self, raw: &str) -> IntentVerdict {
        match self.try_classify(raw).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("intent classify failed, passthrough: {e:#}");
                IntentVerdict::passthrough()
            },
        }
    }

    async fn try_classify(&self, raw: &str) -> Result<IntentVerdict> {
        let user_msg = format!(
            "Developer prompt (truncated to {RAW_TRUNCATE_BYTES} chars):\n{body}",
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
        Ok(parse_intent_json(&text))
    }
}

#[derive(Deserialize)]
struct LlmIntentJson {
    #[serde(default)]
    is_mutation: Option<bool>,
    #[serde(default)]
    reason: Option<String>,
}

/// Tolerant of stray prose and code fences. On garbage / missing
/// `is_mutation`, returns [`IntentVerdict::passthrough`] — fail-open
/// preserves strict vetoes.
pub fn parse_intent_json(text: &str) -> IntentVerdict {
    let trimmed = strip_code_fence(text.trim()).trim();
    let start = trimmed.find('{');
    let end = trimmed.rfind('}');
    let payload = match (start, end) {
        (Some(a), Some(b)) if b >= a => &trimmed[a..=b],
        _ => return IntentVerdict::passthrough(),
    };
    let parsed: LlmIntentJson = match serde_json::from_str(payload) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("intent json parse failed: {e:#}");
            return IntentVerdict::passthrough();
        },
    };
    let is_mutation = parsed.is_mutation.unwrap_or(true);
    let reason = parsed.reason.unwrap_or_default().trim().to_string();
    IntentVerdict {
        is_mutation,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_strips_fences_and_extracts_is_mutation_false() {
        let raw = "```json\n{\"is_mutation\":false,\"reason\":\"asks for explanation\"}\n```";
        let v = parse_intent_json(raw);
        assert!(!v.is_mutation);
        assert_eq!(v.reason, "asks for explanation");
    }

    #[test]
    fn parse_extracts_is_mutation_true() {
        let raw = "{\"is_mutation\":true,\"reason\":\"add retry logic\"}";
        let v = parse_intent_json(raw);
        assert!(v.is_mutation);
        assert_eq!(v.reason, "add retry logic");
    }

    #[test]
    fn parse_falls_back_on_garbage() {
        let v = parse_intent_json("not json at all");
        assert!(
            v.is_mutation,
            "fail-open default must preserve strict vetoes"
        );
    }

    #[test]
    fn parse_falls_back_on_unbalanced_braces() {
        let v = parse_intent_json("{ broken json");
        assert!(v.is_mutation);
    }

    #[test]
    fn parse_missing_field_defaults_to_mutation() {
        let v = parse_intent_json("{\"reason\":\"unsure\"}");
        assert!(v.is_mutation, "missing is_mutation must fail-open to true");
    }

    #[test]
    fn passthrough_is_mutation_true() {
        let v = IntentVerdict::passthrough();
        assert!(v.is_mutation);
    }

    #[test]
    fn parse_with_prose_and_code_fence() {
        let raw =
            "Sure!\n```json\n{\"is_mutation\":false,\"reason\":\"reading auth flow\"}\n```\nDone.";
        let v = parse_intent_json(raw);
        assert!(!v.is_mutation);
        assert_eq!(v.reason, "reading auth flow");
    }

    #[test]
    fn parse_empty_reason_kept_empty() {
        let v = parse_intent_json("{\"is_mutation\":false,\"reason\":\"\"}");
        assert!(!v.is_mutation);
        assert_eq!(v.reason, "");
    }
}
