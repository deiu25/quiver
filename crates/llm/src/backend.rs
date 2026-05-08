//! Claude API + CLI backend abstraction shared by every classifier.
//!
//! Two transports:
//!
//!   * [`ClaudeBackend::Api`] — POST `/v1/messages` with `x-api-key`. Driven
//!     by `ANTHROPIC_API_KEY`. Same model + version as the rest of the
//!     ecosystem.
//!   * [`ClaudeBackend::Cli`] — pipe the prompt to a local `claude --print
//!     --output-format json` binary. Detected via `PATH` lookup.
//!
//! Both paths run through [`call_backend`], which applies a single
//! `tokio::time::timeout` and returns the raw text the model emitted. Caller
//! is responsible for parsing — every classifier in this crate expects
//! strict-JSON output and falls back to a passthrough verdict on parse error.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::json;

pub const RAW_TRUNCATE_BYTES: usize = 2000;
pub const CLASSIFY_TIMEOUT: Duration = Duration::from_secs(15);
pub const ANTHROPIC_MODEL: &str = "claude-sonnet-4-6";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
pub const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";

#[derive(Debug, Clone)]
pub enum ClaudeBackend {
    Api { api_key: String, base_url: String },
    Cli { binary: String },
}

/// API > CLI. `None` when neither is available.
pub fn detect_backend() -> Option<ClaudeBackend> {
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY")
        && !key.trim().is_empty()
    {
        return Some(ClaudeBackend::Api {
            api_key: key,
            base_url: ANTHROPIC_URL.to_string(),
        });
    }
    if let Some(bin) = which_claude() {
        return Some(ClaudeBackend::Cli { binary: bin });
    }
    None
}

pub fn label_for(b: &ClaudeBackend) -> &'static str {
    match b {
        ClaudeBackend::Api { .. } => "sonnet-api",
        ClaudeBackend::Cli { .. } => "sonnet-cli",
    }
}

/// Run a single classifier prompt against the configured backend and return
/// the raw text the model emitted. Times out after `timeout`.
pub async fn call_backend(
    backend: &ClaudeBackend,
    system: &str,
    user: &str,
    max_tokens: u32,
    timeout: Duration,
) -> Result<String> {
    match backend {
        ClaudeBackend::Api { api_key, base_url } => tokio::time::timeout(
            timeout,
            call_anthropic_api(base_url, api_key, system, user, max_tokens),
        )
        .await
        .map_err(|_| anyhow!("anthropic api timeout"))?,
        ClaudeBackend::Cli { binary } => {
            tokio::time::timeout(timeout, call_claude_cli(binary, system, user))
                .await
                .map_err(|_| anyhow!("claude cli timeout"))?
        },
    }
}

pub fn truncate_chars(s: &str, max: usize) -> String {
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

pub fn strip_code_fence(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix("```json").or_else(|| s.strip_prefix("```"))
        && let Some(end) = rest.rfind("```")
    {
        return &rest[..end];
    }
    s
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
    system: &str,
    user_msg: &str,
    max_tokens: u32,
) -> Result<String> {
    let body = json!({
        "model": ANTHROPIC_MODEL,
        "max_tokens": max_tokens,
        "system": system,
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

async fn call_claude_cli(binary: &str, system: &str, user_msg: &str) -> Result<String> {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    let prompt = format!("{system}\n\n{user_msg}");
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

    #[test]
    fn truncate_chars_respects_utf8_boundary() {
        let s = "héllo wörld";
        let out = truncate_chars(s, 6);
        assert!(s.starts_with(&out));
        assert!(out.len() <= 6);
    }

    #[test]
    fn strip_fence_keeps_payload() {
        let s = "```json\n{\"x\":1}\n```";
        let out = strip_code_fence(s.trim()).trim();
        assert!(out.contains("{\"x\":1}"));
    }

    #[test]
    fn strip_fence_passthrough_when_no_fence() {
        let s = "{\"x\":1}";
        assert_eq!(strip_code_fence(s), s);
    }

    #[test]
    fn label_for_api_and_cli() {
        let api = ClaudeBackend::Api {
            api_key: "k".into(),
            base_url: "u".into(),
        };
        let cli = ClaudeBackend::Cli {
            binary: "claude".into(),
        };
        assert_eq!(label_for(&api), "sonnet-api");
        assert_eq!(label_for(&cli), "sonnet-cli");
    }
}
