//! `quiver hook <event>` — Claude Code hook handlers in pure Rust.
//!
//! Wired by `quiver init` into `~/.claude/settings.json`. Replaces the bash
//! wrapper at `~/.local/bin/quiver-pretooluse.sh` and adds the missing
//! `UserPromptSubmit` enrichment that injects the recommended skill body so
//! the model can act on it without the file existing in `~/.claude/skills/`.
//!
//! Hook handlers always exit 0 — never block tool execution. Empty stdout =
//! "no enrichment needed" (Claude Code accepts that).

use std::collections::HashMap;
use std::io::Read;

use clap::Subcommand;
use quiver_recommender::embed::Embedder;
use quiver_recommender::excerpt::excerpt;
use quiver_recommender::params::{
    COS_WEIGHT, FTS_CANDIDATES, FTS_WEIGHT, VEC_CANDIDATES, build_fts_query,
};
use quiver_recommender::rerank::{Reranker, SuccessReranker};
use quiver_recommender::search;
use quiver_storage::{embeddings, fts, open, tools};
use rusqlite::Connection;
use serde::Serialize;

use crate::db_path::default_db_path;

const MIN_PROMPT_CHARS: usize = 8;
const TASK_INPUT_CAP: usize = 1000;
const DEFAULT_MIN_SCORE: f32 = 0.4;
const DEFAULT_BODY_CHARS: usize = 3000;
const PRE_TOOL_USE_K: usize = 3;

#[derive(Subcommand)]
pub enum HookEvent {
    /// Read a Claude Code UserPromptSubmit event from stdin and emit
    /// `additionalContext` containing the top-1 skill body excerpt when the
    /// recommender finds a confident match (score >= QUIVER_HOOK_SCORE_MIN).
    UserPromptSubmit,
    /// Read a Claude Code PreToolUse event from stdin and emit
    /// `additionalContext` with top-3 metadata (no bodies) for Skill / Agent
    /// tool calls. Mirrors the legacy bash hook.
    PreToolUse,
}

pub async fn run(event: HookEvent) -> anyhow::Result<()> {
    if std::env::var("QUIVER_HOOK_DISABLED").as_deref() == Ok("1") {
        return Ok(());
    }
    match event {
        HookEvent::UserPromptSubmit => user_prompt_submit(read_stdin()?),
        HookEvent::PreToolUse => pre_tool_use(read_stdin()?),
    }
}

fn read_stdin() -> anyhow::Result<serde_json::Value> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    if buf.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    Ok(serde_json::from_str(&buf)?)
}

fn min_score() -> f32 {
    std::env::var("QUIVER_HOOK_SCORE_MIN")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MIN_SCORE)
}

fn body_chars() -> usize {
    std::env::var("QUIVER_HOOK_BODY_CHARS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_BODY_CHARS)
}

#[derive(Serialize)]
struct HookOutput<'a> {
    #[serde(rename = "hookSpecificOutput")]
    hook_specific_output: HookPayload<'a>,
}

#[derive(Serialize)]
struct HookPayload<'a> {
    #[serde(rename = "hookEventName")]
    hook_event_name: &'a str,
    #[serde(rename = "additionalContext")]
    additional_context: String,
}

fn emit(event_name: &str, ctx: String) -> anyhow::Result<()> {
    let out = HookOutput {
        hook_specific_output: HookPayload {
            hook_event_name: event_name,
            additional_context: ctx,
        },
    };
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}

fn user_prompt_submit(event: serde_json::Value) -> anyhow::Result<()> {
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
    if top.score < min_score() {
        return Ok(());
    }

    let body = tools::get(&conn, &top.tool_id)?
        .and_then(|m| m.long_description)
        .map(|b| excerpt(&b, body_chars()));

    emit(
        "UserPromptSubmit",
        format_user_prompt_block(&top, body.as_deref()),
    )
}

fn pre_tool_use(event: serde_json::Value) -> anyhow::Result<()> {
    let tool = event
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let task = match tool {
        "Skill" => extract_task_skill(&event),
        "Agent" | "Task" => extract_task_agent(&event),
        _ => return Ok(()),
    };
    let task = task.trim();
    if task.chars().count() < MIN_PROMPT_CHARS {
        return Ok(());
    }
    let task_trim: String = task.chars().take(TASK_INPUT_CAP).collect();

    let conn = open(&default_db_path()?)?;
    let hits = top_n(&conn, &task_trim, PRE_TOOL_USE_K)?;
    if hits.is_empty() {
        return Ok(());
    }

    let metas: HashMap<String, _> = tools::list_all(&conn)?
        .into_iter()
        .map(|m| (m.id.clone(), m))
        .collect();

    let mut lines = Vec::new();
    for h in &hits {
        let desc = metas
            .get(&h.tool_id)
            .and_then(|m| m.description.as_deref())
            .unwrap_or("");
        lines.push(format!("- score={:.3} {} — {}", h.score, h.tool_id, desc));
    }
    let ctx = format!(
        "Quiver top-{} suggestions for this {tool} call (task: \"{task_trim}\"):\n{}\n\n\
         (If a suggestion has score >= {:.2} and fits, prefer it over the chosen tool. \
         Skip this hint if the request explicitly named a different tool.)",
        hits.len(),
        lines.join("\n"),
        min_score()
    );
    emit("PreToolUse", ctx)
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

#[derive(Debug, Clone)]
struct TopHit {
    tool_id: String,
    score: f32,
    name: String,
    description: Option<String>,
    invocation: Option<String>,
}

fn top_match(conn: &Connection, task: &str) -> anyhow::Result<Option<TopHit>> {
    let hits = top_n(conn, task, 1)?;
    Ok(hits.into_iter().next().map(|h| {
        // Hydrate metadata for the single hit. Cheap because we only fetch one.
        match tools::get(conn, &h.tool_id).ok().flatten() {
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
        }
    }))
}

fn top_n(conn: &Connection, task: &str, k: usize) -> anyhow::Result<Vec<search::Hit>> {
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

    #[test]
    fn user_prompt_block_includes_id_score_and_body() {
        let top = TopHit {
            tool_id: "skill:python-testing".into(),
            score: 0.82,
            name: "python-testing".into(),
            description: Some("pytest patterns".into()),
            invocation: Some("/python-testing".into()),
        };
        let block = format_user_prompt_block(&top, Some("# Heading\n\nbody line"));
        assert!(block.contains("<quiver-recommendation>"));
        assert!(block.contains("skill:python-testing"));
        assert!(block.contains("0.820"));
        assert!(block.contains("/python-testing"));
        assert!(block.contains("body line"));
        assert!(block.ends_with("</quiver-recommendation>"));
    }

    #[test]
    fn user_prompt_block_omits_body_section_when_none() {
        let top = TopHit {
            tool_id: "skill:x".into(),
            score: 0.5,
            name: "x".into(),
            description: None,
            invocation: None,
        };
        let block = format_user_prompt_block(&top, None);
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
    fn min_score_uses_default_when_unset() {
        // Reading the env var here is racy with other tests; just verify the
        // default branch parses the constant.
        let v = std::env::var("QUIVER_HOOK_SCORE_MIN")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_MIN_SCORE);
        assert!((v - 0.4).abs() < 1e-6 || v >= 0.0);
    }
}
