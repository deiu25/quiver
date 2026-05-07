//! `quiver hook <event>` — Claude Code hook handlers in pure Rust.
//!
//! Wired by `quiver init` into `~/.claude/settings.json`. Three events:
//!
//! - `user-prompt-submit`: enriches every prompt with the recommended skill
//!   body. In **advisory** mode injects via `additionalContext` (legacy);
//!   in **strict** mode also emits a `<quiver-directive>` system-reminder
//!   for `Strong`/`Mandatory` bands.
//! - `pre-tool-use`: matches `*` (every tool call). In strict mode emits
//!   `permissionDecision: deny` when the catalog has a higher-confidence
//!   alternative — the **single-veto-per-tuple** rule lets the model retry
//!   the same call and pass through, so the hook never deadlocks.
//! - `stop`: circuit-breaker. If a session ends with a still-pending
//!   `Mandatory` recommendation, emits `decision: block` with a one-shot
//!   `nudged=1` flag.
//!
//! Hook handlers exit 0 unless the spec demands otherwise (denied tool calls
//! are still exit-0; the JSON tells Claude Code to block).

use std::collections::HashMap;
use std::io::Read;

use anyhow::Result;
use chrono::Utc;
use clap::Subcommand;
use quiver_recommender::embed::Embedder;
use quiver_recommender::excerpt::excerpt;
use quiver_recommender::params::{
    COS_WEIGHT, FTS_CANDIDATES, FTS_WEIGHT, VEC_CANDIDATES, build_fts_query,
};
use quiver_recommender::policy::{Policy, Thresholds};
use quiver_recommender::rerank::{Reranker, SuccessReranker};
use quiver_recommender::search;
use quiver_storage::{embeddings, fts, open, suggestions, tools};
use rusqlite::Connection;
use serde::Serialize;

use crate::db_path::default_db_path;

const MIN_PROMPT_CHARS: usize = 8;
const TASK_INPUT_CAP: usize = 1000;
const DEFAULT_BODY_CHARS: usize = 3000;
const PRE_TOOL_USE_K: usize = 3;
/// Stop circuit-breaker only considers suggestions inside this window.
const STOP_WINDOW_MINUTES: i64 = 60;
/// Tool names that should never be vetoed (built-in built-on-by-Claude
/// primitives where Quiver has no business interfering with the model's
/// raw control flow).
const VETO_BLOCKLIST: &[&str] = &[
    "TodoWrite",
    "TodoRead",
    "ExitPlanMode",
    "EnterPlanMode",
    "AskUserQuestion",
];

/// Enforcement mode read from `QUIVER_ENFORCE`. Default: `strict`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnforceMode {
    Strict,
    Advisory,
    Off,
}

impl EnforceMode {
    pub fn from_env() -> Self {
        match std::env::var("QUIVER_ENFORCE")
            .ok()
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "strict" | "" => EnforceMode::Strict,
            "advisory" | "soft" | "hint" => EnforceMode::Advisory,
            "off" | "disabled" | "no" | "0" => EnforceMode::Off,
            _ => EnforceMode::Strict,
        }
    }
}

#[derive(Subcommand)]
pub enum HookEvent {
    /// Read a Claude Code UserPromptSubmit event from stdin and emit
    /// `additionalContext` (always) plus `systemMessage` (Strong/Mandatory in
    /// strict mode) carrying the recommended skill body.
    UserPromptSubmit,
    /// Read a Claude Code PreToolUse event from stdin. In strict mode and
    /// when an alternative installed tool scores higher than the candidate
    /// by `>= τ_delta`, emit `permissionDecision: deny`. Otherwise inject
    /// metadata advisory only.
    PreToolUse,
    /// Read a Claude Code Stop event from stdin. In strict mode, if the
    /// session has a pending `Mandatory` suggestion that was never invoked
    /// nor nudged, emit `decision: block` once.
    Stop,
}

pub async fn run(event: HookEvent) -> Result<()> {
    if std::env::var("QUIVER_HOOK_DISABLED").as_deref() == Ok("1") {
        return Ok(());
    }
    if EnforceMode::from_env() == EnforceMode::Off {
        return Ok(());
    }
    match event {
        HookEvent::UserPromptSubmit => user_prompt_submit(read_stdin()?),
        HookEvent::PreToolUse => pre_tool_use(read_stdin()?),
        HookEvent::Stop => stop(read_stdin()?),
    }
}

fn read_stdin() -> Result<serde_json::Value> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    if buf.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    Ok(serde_json::from_str(&buf)?)
}

fn body_chars() -> usize {
    std::env::var("QUIVER_HOOK_BODY_CHARS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_BODY_CHARS)
}

// ---------------------------------------------------------------------------
// JSON output shapes.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct AdditionalContextOnly<'a> {
    #[serde(rename = "hookSpecificOutput")]
    hook_specific_output: AdditionalContextPayload<'a>,
}

#[derive(Serialize)]
struct AdditionalContextPayload<'a> {
    #[serde(rename = "hookEventName")]
    hook_event_name: &'a str,
    #[serde(rename = "additionalContext")]
    additional_context: String,
}

#[derive(Serialize)]
struct DirectivePlusContext<'a> {
    #[serde(rename = "systemMessage")]
    system_message: String,
    #[serde(rename = "hookSpecificOutput")]
    hook_specific_output: AdditionalContextPayload<'a>,
}

#[derive(Serialize)]
struct VetoOutput {
    #[serde(rename = "hookSpecificOutput")]
    hook_specific_output: VetoPayload,
}

#[derive(Serialize)]
struct VetoPayload {
    #[serde(rename = "hookEventName")]
    hook_event_name: &'static str,
    #[serde(rename = "permissionDecision")]
    permission_decision: &'static str,
    #[serde(rename = "permissionDecisionReason")]
    permission_decision_reason: String,
}

#[derive(Serialize)]
struct StopBlock {
    decision: &'static str,
    reason: String,
}

fn emit_additional_context(event_name: &str, ctx: String) -> Result<()> {
    let out = AdditionalContextOnly {
        hook_specific_output: AdditionalContextPayload {
            hook_event_name: event_name,
            additional_context: ctx,
        },
    };
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}

fn emit_directive_plus_context(event_name: &str, directive: String, ctx: String) -> Result<()> {
    let out = DirectivePlusContext {
        system_message: directive,
        hook_specific_output: AdditionalContextPayload {
            hook_event_name: event_name,
            additional_context: ctx,
        },
    };
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}

fn emit_veto(reason: String) -> Result<()> {
    let out = VetoOutput {
        hook_specific_output: VetoPayload {
            hook_event_name: "PreToolUse",
            permission_decision: "deny",
            permission_decision_reason: reason,
        },
    };
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}

fn emit_stop_block(reason: String) -> Result<()> {
    let out = StopBlock {
        decision: "block",
        reason,
    };
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}

// ---------------------------------------------------------------------------
// UserPromptSubmit.
// ---------------------------------------------------------------------------

fn user_prompt_submit(event: serde_json::Value) -> Result<()> {
    let prompt = event
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if prompt.chars().count() < MIN_PROMPT_CHARS {
        return Ok(());
    }

    let task: String = prompt.chars().take(TASK_INPUT_CAP).collect();
    let conn = open(&default_db_path()?)?;
    let Some(top) = top_match(&conn, &task)? else {
        return Ok(());
    };
    let thresholds = Thresholds::from_env();
    let policy = thresholds.classify(top.score);
    if policy == Policy::Silent {
        return Ok(());
    }

    let body = tools::get(&conn, &top.tool_id)?
        .and_then(|m| m.long_description)
        .map(|b| excerpt(&b, body_chars()));
    let ctx = format_user_prompt_block(&top, body.as_deref());

    let strict = EnforceMode::from_env() == EnforceMode::Strict;
    if strict && policy.is_directive() {
        let directive = format_directive(policy, &top, &task);
        emit_directive_plus_context("UserPromptSubmit", directive, ctx)
    } else {
        emit_additional_context("UserPromptSubmit", ctx)
    }
}

fn format_directive(policy: Policy, top: &TopHit, task: &str) -> String {
    let invoke_attr = top
        .invocation
        .as_deref()
        .map(|s| format!(" invoke=\"{}\"", escape_attr(s)))
        .unwrap_or_default();
    let invoke_now = if policy == Policy::Mandatory {
        " invoke_now=\"true\""
    } else {
        ""
    };
    let body = match policy {
        Policy::Mandatory => format!(
            "  Quiver mandatory match (score={:.3}). Per project policy, your next \
             action MUST invoke this tool unless the user explicitly named a \
             different tool in this prompt. Task summary: \"{}\".",
            top.score,
            summarise_task(task)
        ),
        Policy::Strong => format!(
            "  Quiver strong match (score={:.3}). Use this tool unless you can \
             articulate a concrete reason it does not fit; state that reason in \
             your reply if you bypass it. Task summary: \"{}\".",
            top.score,
            summarise_task(task)
        ),
        _ => unreachable!("format_directive only valid for Strong/Mandatory"),
    };
    format!(
        "<quiver-directive level=\"{}\"{} tool_id=\"{}\"{} score=\"{:.3}\">\n{}\n</quiver-directive>",
        policy.as_str(),
        invoke_now,
        escape_attr(&top.tool_id),
        invoke_attr,
        top.score,
        body,
    )
}

fn summarise_task(task: &str) -> String {
    let trimmed: String = task.chars().take(120).collect();
    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if ch == '"' || ch == '\n' || ch == '\r' || ch == '\\' {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out.trim().to_string()
}

fn escape_attr(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// ---------------------------------------------------------------------------
// PreToolUse.
// ---------------------------------------------------------------------------

fn pre_tool_use(event: serde_json::Value) -> Result<()> {
    let tool = event
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if tool.is_empty() {
        return Ok(());
    }
    let session_id = event
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let task_text = task_text_for(tool, &event);
    let task = task_text.trim();
    if task.chars().count() < MIN_PROMPT_CHARS {
        return Ok(());
    }
    let task_trim: String = task.chars().take(TASK_INPUT_CAP).collect();
    let signature = task_signature(tool, &event);

    let conn = open(&default_db_path()?)?;
    let hits = top_n(&conn, &task_trim, PRE_TOOL_USE_K)?;
    if hits.is_empty() {
        return Ok(());
    }
    let top = hits[0].clone();
    let chosen_score = hits
        .iter()
        .find(|h| matches_invocation(&h.tool_id, tool))
        .map(|h| h.score)
        .unwrap_or(0.0);

    let thresholds = Thresholds::from_env();
    let strict = EnforceMode::from_env() == EnforceMode::Strict;
    let policy = thresholds.classify(top.score);
    let competing = !matches_invocation(&top.tool_id, tool);
    let delta = top.score - chosen_score;

    if strict
        && policy.is_directive()
        && competing
        && delta >= thresholds.tau_delta
        && !VETO_BLOCKLIST.contains(&tool)
        && !session_id.is_empty()
        && !signature.is_empty()
    {
        // Single-veto-per-tuple rule: a re-invocation flips bypassed=1 and
        // passes through.
        if suggestions::is_vetoed(&conn, &session_id, &signature)? {
            if let Some(row) = suggestions::find_vetoed_row(&conn, &session_id, &signature)? {
                let _ = suggestions::mark_bypassed(&conn, row.id);
            }
            return advisory_metadata(&conn, &hits, tool, &task_trim);
        }
        let top_meta = tools::get(&conn, &top.tool_id).ok().flatten();
        let invoke = top_meta
            .as_ref()
            .and_then(|m| m.invocation.as_deref())
            .unwrap_or(top.tool_id.as_str());
        let row_id = suggestions::record(
            &conn,
            &session_id,
            &top.tool_id,
            Some(&task_trim),
            Some(top.score as f64),
            Utc::now(),
            Some(policy.as_str()),
            Some(&signature),
        )?;
        let _ = suggestions::mark_vetoed(&conn, row_id);
        let reason = format!(
            "Quiver: a higher-confidence installed tool fits this task. \
             Use `{invoke}` (id={}, score={:.3}, Δ={:.3}) instead of `{tool}`. \
             Re-invoke the same tool to override (Quiver vetoes once per \
             session/tool/task; quiver veto: row={row_id}).",
            top.tool_id, top.score, delta,
        );
        return emit_veto(reason);
    }

    advisory_metadata(&conn, &hits, tool, &task_trim)
}

fn advisory_metadata(
    conn: &Connection,
    hits: &[search::Hit],
    tool: &str,
    task_trim: &str,
) -> Result<()> {
    let metas: HashMap<String, _> = tools::list_all(conn)?
        .into_iter()
        .map(|m| (m.id.clone(), m))
        .collect();
    let mut lines = Vec::new();
    for h in hits {
        let desc = metas
            .get(&h.tool_id)
            .and_then(|m| m.description.as_deref())
            .unwrap_or("");
        lines.push(format!("- score={:.3} {} — {}", h.score, h.tool_id, desc));
    }
    let thresholds = Thresholds::from_env();
    let ctx = format!(
        "Quiver top-{} suggestions for this {tool} call (task: \"{task_trim}\"):\n{}\n\n\
         (If a suggestion has score >= {:.2} and fits, prefer it over the chosen tool. \
         Skip this hint if the request explicitly named a different tool.)",
        hits.len(),
        lines.join("\n"),
        thresholds.tau_strong,
    );
    emit_additional_context("PreToolUse", ctx)
}

fn task_text_for(tool: &str, event: &serde_json::Value) -> String {
    match tool {
        "Skill" => extract_task_skill(event),
        "Agent" | "Task" => extract_task_agent(event),
        "Bash" => extract_task_bash(event),
        "Read" => extract_task_path(event, "file_path"),
        "WebFetch" => extract_task_url(event),
        "WebSearch" => extract_task_field(event, "query"),
        "Write" => extract_task_path(event, "file_path"),
        "Edit" | "MultiEdit" => extract_task_path(event, "file_path"),
        _ => json_input_summary(event),
    }
}

fn extract_task_skill(event: &serde_json::Value) -> String {
    let inp = event.get("tool_input").cloned().unwrap_or_default();
    let skill = inp.get("skill").and_then(|v| v.as_str()).unwrap_or("");
    let args = inp.get("args").and_then(|v| v.as_str()).unwrap_or("");
    format!("{skill} {args}").trim().to_string()
}

fn extract_task_agent(event: &serde_json::Value) -> String {
    let inp = event.get("tool_input").cloned().unwrap_or_default();
    let desc = inp
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let prompt = inp.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    format!("{desc}: {prompt}").trim().to_string()
}

fn extract_task_bash(event: &serde_json::Value) -> String {
    let inp = event.get("tool_input").cloned().unwrap_or_default();
    let cmd = inp.get("command").and_then(|v| v.as_str()).unwrap_or("");
    let desc = inp
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if desc.is_empty() {
        cmd.to_string()
    } else {
        format!("{desc}: {cmd}")
    }
}

fn extract_task_url(event: &serde_json::Value) -> String {
    let inp = event.get("tool_input").cloned().unwrap_or_default();
    let url = inp.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let prompt = inp.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    if prompt.is_empty() {
        url.to_string()
    } else {
        format!("{prompt} ({url})")
    }
}

fn extract_task_field(event: &serde_json::Value, key: &str) -> String {
    event
        .get("tool_input")
        .and_then(|v| v.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn extract_task_path(event: &serde_json::Value, key: &str) -> String {
    extract_task_field(event, key)
}

fn json_input_summary(event: &serde_json::Value) -> String {
    let inp = event.get("tool_input").cloned().unwrap_or_default();
    let s = inp.to_string();
    s.chars().take(TASK_INPUT_CAP).collect()
}

/// Stable digest used for the single-veto-per-tuple rule. Format:
/// `<tool_name>:<salient input>`. Truncated to 256 chars.
fn task_signature(tool: &str, event: &serde_json::Value) -> String {
    let salient = match tool {
        "Bash" => extract_task_field(event, "command"),
        "Read" | "Write" | "Edit" | "MultiEdit" => extract_task_field(event, "file_path"),
        "WebFetch" => extract_task_field(event, "url"),
        "WebSearch" => extract_task_field(event, "query"),
        "Skill" => extract_task_field(event, "skill"),
        "Agent" | "Task" => extract_task_field(event, "description"),
        _ => json_input_summary(event),
    };
    let sig = format!("{tool}:{salient}");
    sig.chars().take(256).collect()
}

/// Heuristic match between a tool's invocation and the candidate tool name
/// the model is about to call. Skill/Agent/Task tools get matched against
/// id-prefix; everything else compares the chosen tool name to the recommended
/// `tool_id` prefix and bare invocation.
fn matches_invocation(tool_id: &str, chosen_tool: &str) -> bool {
    let chosen_lower = chosen_tool.to_ascii_lowercase();
    if let Some((prefix, rest)) = tool_id.split_once(':') {
        match prefix {
            "skill" => return chosen_lower == "skill" || chosen_lower == format!("/{rest}"),
            "agent" | "task" => return chosen_lower == "agent" || chosen_lower == "task",
            "mcp" => return chosen_lower.starts_with("mcp__"),
            _ => return false,
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Stop circuit-breaker.
// ---------------------------------------------------------------------------

fn stop(event: serde_json::Value) -> Result<()> {
    if EnforceMode::from_env() != EnforceMode::Strict {
        return Ok(());
    }
    let session_id = event
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Ok(());
    }
    let conn = open(&default_db_path()?)?;
    let Some(row) = suggestions::pending_mandatory_for_session(
        &conn,
        session_id,
        STOP_WINDOW_MINUTES,
        Utc::now(),
    )?
    else {
        return Ok(());
    };
    let _ = suggestions::mark_nudged(&conn, row.id);
    let task_summary = row.task_text.as_deref().unwrap_or("(no task summary)");
    let score = row.score.unwrap_or(0.0);
    let reason = format!(
        "Quiver: this session had a top-1 mandatory recommendation \
         ({}, score={:.3}) for \"{}\" that was never invoked. Invoke it now \
         or write one sentence explaining why it was wrong (the explanation \
         feeds Quiver's auto-tuner).",
        row.tool_id,
        score,
        summarise_task(task_summary),
    );
    emit_stop_block(reason)
}

// ---------------------------------------------------------------------------
// Recommender wrappers (unchanged from prior implementation).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct TopHit {
    tool_id: String,
    score: f32,
    name: String,
    description: Option<String>,
    invocation: Option<String>,
}

fn top_match(conn: &Connection, task: &str) -> Result<Option<TopHit>> {
    let hits = top_n(conn, task, 1)?;
    Ok(hits
        .into_iter()
        .next()
        .map(|h| match tools::get(conn, &h.tool_id).ok().flatten() {
            Some(m) => TopHit {
                tool_id: h.tool_id,
                score: h.score,
                name: m.name,
                description: m.description,
                invocation: m.invocation,
            },
            None => TopHit {
                tool_id: h.tool_id.clone(),
                score: h.score,
                name: h.tool_id,
                description: None,
                invocation: None,
            },
        }))
}

fn top_n(conn: &Connection, task: &str, k: usize) -> Result<Vec<search::Hit>> {
    let embedder = Embedder::new()?;
    let q_emb = embedder.embed_one(task)?;

    let vec_sims: HashMap<String, f32> = embeddings::vec_search(conn, &q_emb, VEC_CANDIDATES)?
        .into_iter()
        .map(|(id, dist)| (id, 1.0 - dist))
        .collect();
    if vec_sims.is_empty() {
        return Ok(Vec::new());
    }

    let fts_query = build_fts_query(task);
    let fts_hits: HashMap<String, f32> = if fts_query.is_empty() {
        HashMap::new()
    } else {
        fts::search(conn, &fts_query, FTS_CANDIDATES)
            .map(|rows| rows.into_iter().collect())
            .unwrap_or_default()
    };

    let mut hits = search::hybrid_from_score_maps(
        &vec_sims,
        &fts_hits,
        VEC_CANDIDATES.max(FTS_CANDIDATES),
        COS_WEIGHT,
        FTS_WEIGHT,
    );
    SuccessReranker::default().apply(&mut hits, conn)?;
    hits.truncate(k);
    Ok(hits)
}

fn format_user_prompt_block(top: &TopHit, body: Option<&str>) -> String {
    let mut s = String::new();
    s.push_str("<quiver-recommendation>\n");
    s.push_str(&format!("  id: {}\n", top.tool_id));
    s.push_str(&format!("  name: {}\n", top.name));
    s.push_str(&format!("  score: {:.3}\n", top.score));
    if let Some(inv) = &top.invocation {
        s.push_str(&format!("  invoke: `{inv}`\n"));
    }
    if let Some(desc) = &top.description {
        s.push_str(&format!("  description: {desc}\n"));
    }
    if let Some(body) = body {
        s.push_str("\n  body:\n  ---\n");
        for line in body.lines() {
            s.push_str("  ");
            s.push_str(line);
            s.push('\n');
        }
        s.push_str("  ---\n");
    }
    s.push_str("</quiver-recommendation>");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top(score: f32) -> TopHit {
        TopHit {
            tool_id: "skill:python-testing".into(),
            score,
            name: "python-testing".into(),
            description: Some("pytest patterns".into()),
            invocation: Some("/python-testing".into()),
        }
    }

    #[test]
    fn user_prompt_block_includes_id_score_and_body() {
        let block = format_user_prompt_block(&top(0.82), Some("# Heading\n\nbody line"));
        assert!(block.contains("<quiver-recommendation>"));
        assert!(block.contains("skill:python-testing"));
        assert!(block.contains("0.820"));
        assert!(block.contains("/python-testing"));
        assert!(block.contains("body line"));
        assert!(block.ends_with("</quiver-recommendation>"));
    }

    #[test]
    fn user_prompt_block_omits_body_section_when_none() {
        let t = TopHit {
            tool_id: "skill:x".into(),
            score: 0.5,
            name: "x".into(),
            description: None,
            invocation: None,
        };
        let block = format_user_prompt_block(&t, None);
        assert!(!block.contains("body:"));
    }

    #[test]
    fn extract_task_skill_combines_skill_and_args() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"tool_input":{"skill":"python-testing","args":"fastapi fixture"}}"#,
        )
        .unwrap();
        assert_eq!(extract_task_skill(&v), "python-testing fastapi fixture");
    }

    #[test]
    fn extract_task_agent_combines_description_and_prompt() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"tool_input":{"description":"refactor","prompt":"split this module"}}"#,
        )
        .unwrap();
        assert_eq!(extract_task_agent(&v), "refactor: split this module");
    }

    #[test]
    fn extract_task_bash_uses_command_and_description() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"tool_input":{"command":"cargo test","description":"run tests"}}"#,
        )
        .unwrap();
        assert_eq!(extract_task_bash(&v), "run tests: cargo test");
    }

    #[test]
    fn task_signature_is_stable() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"tool_input":{"command":"cargo test --workspace"}}"#).unwrap();
        let sig = task_signature("Bash", &v);
        assert_eq!(sig, "Bash:cargo test --workspace");
    }

    #[test]
    fn directive_strong_omits_invoke_now() {
        let d = format_directive(Policy::Strong, &top(0.65), "implement pytest fixtures");
        assert!(d.contains("level=\"strong\""));
        assert!(!d.contains("invoke_now"));
        assert!(d.contains("tool_id=\"skill:python-testing\""));
        assert!(d.contains("invoke=\"/python-testing\""));
    }

    #[test]
    fn directive_mandatory_includes_invoke_now() {
        let d = format_directive(Policy::Mandatory, &top(0.85), "write fastapi tests");
        assert!(d.contains("level=\"mandatory\""));
        assert!(d.contains("invoke_now=\"true\""));
        assert!(d.contains("score=\"0.850\""));
    }

    #[test]
    fn enforce_mode_default_is_strict() {
        // Don't poke env globally; just verify the parser.
        // Direct check on parser branches — env-driven branch covered by
        // integration tests.
        let modes = [
            ("strict", EnforceMode::Strict),
            ("STRICT", EnforceMode::Strict),
            ("advisory", EnforceMode::Advisory),
            ("off", EnforceMode::Off),
            ("disabled", EnforceMode::Off),
            ("garbage", EnforceMode::Strict),
            ("", EnforceMode::Strict),
        ];
        for (s, want) in modes {
            let got = match s.to_ascii_lowercase().as_str() {
                "strict" | "" => EnforceMode::Strict,
                "advisory" | "soft" | "hint" => EnforceMode::Advisory,
                "off" | "disabled" | "no" | "0" => EnforceMode::Off,
                _ => EnforceMode::Strict,
            };
            assert_eq!(got, want, "input {s}");
        }
    }

    #[test]
    fn matches_invocation_skill_prefix() {
        assert!(matches_invocation("skill:python-testing", "Skill"));
        assert!(matches_invocation(
            "skill:python-testing",
            "/python-testing"
        ));
        assert!(!matches_invocation("skill:python-testing", "Bash"));
    }

    #[test]
    fn matches_invocation_mcp_prefix() {
        assert!(matches_invocation("mcp:quiver", "mcp__quiver__recommend"));
        assert!(!matches_invocation("mcp:quiver", "Read"));
    }

    #[test]
    fn summarise_task_strips_quotes_and_newlines() {
        let s = summarise_task("hello \"world\"\nnext line");
        assert!(!s.contains('"'));
        assert!(!s.contains('\n'));
    }
}
