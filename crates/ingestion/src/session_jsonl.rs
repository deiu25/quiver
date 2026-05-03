//! Claude Code session JSONL replay → `UsageEvent` stream. Phase 4.
//!
//! Each line of `~/.claude/projects/<dir>/<sessionId>.jsonl` is one event.
//! We care about three shapes:
//!
//! - `assistant` messages whose `message.content[]` contains `tool_use` blocks
//!   (`name`, `id`, `input`).
//! - `user` messages whose `message.content[]` contains `tool_result` blocks
//!   (`tool_use_id`, optional `is_error`).
//! - `user` messages whose `message.content[]` is plain `text` — these are the
//!   prompts we associate with the next `tool_use` as `task_text`.
//!
//! All other event types (queue-operation, hook_*, attachment, …) are ignored.
//! Built-in tools (Bash, Read, Edit, Write, Grep, Glob, TodoWrite, ToolSearch,
//! ExitPlanMode, NotebookEdit, WebFetch, WebSearch) are NOT catalogued by
//! ToolHub; we skip their events. Only `Skill` invocations and `mcp__*__*`
//! tool calls are mapped to `tool_id` and emitted.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde_json::Value;
use toolhub_core::usage::{Outcome, UsageEvent};

const TASK_TEXT_MAX: usize = 500;

/// Tool names that Claude Code provides directly — not catalogued.
const BUILTIN_TOOLS: &[&str] = &[
    "Bash",
    "Read",
    "Edit",
    "Write",
    "Grep",
    "Glob",
    "TodoWrite",
    "ToolSearch",
    "ExitPlanMode",
    "EnterPlanMode",
    "NotebookEdit",
    "WebFetch",
    "WebSearch",
    "Agent",
    "Task",
    "MultiEdit",
    "ListMcpResourcesTool",
    "ReadMcpResourceTool",
    "AskUserQuestion",
    "ScheduleWakeup",
    "Monitor",
    "PushNotification",
    "RemoteTrigger",
    "TaskOutput",
    "TaskStop",
    "EnterWorktree",
    "ExitWorktree",
    "CronCreate",
    "CronDelete",
    "CronList",
];

/// Map a JSONL `tool_use.name` (+ `input` for `Skill`) to a ToolHub `tool_id`.
/// Returns `None` for built-ins or shapes we don't understand.
pub fn map_tool_id(name: &str, input: &Value) -> Option<String> {
    if BUILTIN_TOOLS.contains(&name) {
        return None;
    }
    if name == "Skill" {
        let skill = input.get("skill").and_then(|v| v.as_str())?;
        return Some(if let Some((plugin, cmd)) = skill.split_once(':') {
            format!("plugin:{plugin}@{cmd}")
        } else {
            format!("skill:{skill}")
        });
    }
    if let Some(rest) = name.strip_prefix("mcp__") {
        // `mcp__<server>__<tool>` — collapse to server-level so it joins the
        // `mcp:<server>` rows that mcp_json.rs ingests.
        let server = rest.split("__").next()?;
        return Some(format!("mcp:{server}"));
    }
    None
}

/// Project name = parent directory of the JSONL file with `~/.claude/projects/`
/// path encoding stripped (leading `-` removed, last segment lowercased).
pub fn project_from_path(p: &Path) -> Option<String> {
    let parent = p.parent()?.file_name()?.to_str()?;
    let stripped = parent.strip_prefix('-').unwrap_or(parent);
    stripped.rsplit('-').next().map(|s| s.to_lowercase())
}

#[derive(Debug)]
struct PendingToolUse {
    uuid: String,
    tool_id: String,
    task_text: Option<String>,
    session_id: Option<String>,
    project: Option<String>,
    occurred_at: DateTime<Utc>,
}

/// Parse one JSONL session file → ordered list of `UsageEvent`s.
///
/// Outcome heuristic (matches PLAN §7 Phase 4 #2):
///   1. `tool_result.is_error == true`            → Failure
///   2. no matching `tool_result` before EOF       → Abandoned
///   3. otherwise                                  → Success
///
/// "Same tool re-invoked within N turns" and "negative-keyword next message"
/// are deferred — they fire too eagerly on this format. PLAN §12 risk row
/// "outcome scoring noisy" already accepts the lossy v1.
pub fn replay(path: &Path) -> anyhow::Result<Vec<UsageEvent>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let reader = BufReader::new(file);
    let project = project_from_path(path);

    let mut last_user_text: Option<String> = None;
    let mut pending: HashMap<String, PendingToolUse> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut results: HashMap<String, Option<bool>> = HashMap::new();

    for (line_idx, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!(
                    "session_jsonl: {} line {} read error: {e}",
                    path.display(),
                    line_idx + 1
                );
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "session_jsonl: {} line {} parse error: {e}",
                    path.display(),
                    line_idx + 1
                );
                continue;
            }
        };

        let kind = v.get("type").and_then(|s| s.as_str()).unwrap_or("");
        let session_id = v
            .get("sessionId")
            .and_then(|s| s.as_str())
            .map(str::to_string);

        match kind {
            "user" => {
                let content = v
                    .pointer("/message/content")
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default();
                for block in content {
                    let bt = block.get("type").and_then(|s| s.as_str()).unwrap_or("");
                    match bt {
                        "text" => {
                            if let Some(t) = block.get("text").and_then(|s| s.as_str()) {
                                last_user_text = Some(truncate(t, TASK_TEXT_MAX));
                            }
                        }
                        "tool_result" => {
                            if let Some(id) = block.get("tool_use_id").and_then(|s| s.as_str()) {
                                let is_err = block.get("is_error").and_then(|b| b.as_bool());
                                results.insert(id.to_string(), is_err);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "assistant" => {
                let ts = v
                    .get("timestamp")
                    .and_then(|s| s.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now);
                let content = v
                    .pointer("/message/content")
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default();
                for block in content {
                    if block.get("type").and_then(|s| s.as_str()) != Some("tool_use") {
                        continue;
                    }
                    let id = match block.get("id").and_then(|s| s.as_str()) {
                        Some(s) => s.to_string(),
                        None => continue,
                    };
                    let name = block.get("name").and_then(|s| s.as_str()).unwrap_or("");
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    let Some(tool_id) = map_tool_id(name, &input) else {
                        continue;
                    };
                    let pu = PendingToolUse {
                        uuid: id.clone(),
                        tool_id,
                        task_text: last_user_text.clone(),
                        session_id: session_id.clone(),
                        project: project.clone(),
                        occurred_at: ts,
                    };
                    pending.insert(id.clone(), pu);
                    order.push(id);
                }
            }
            _ => {}
        }
    }

    let mut out = Vec::with_capacity(order.len());
    for id in order {
        let Some(pu) = pending.remove(&id) else {
            continue;
        };
        let outcome = match results.get(&id) {
            Some(Some(true)) => Outcome::Failure,
            Some(_) => Outcome::Success,
            None => Outcome::Abandoned,
        };
        out.push(UsageEvent {
            uuid: Some(pu.uuid),
            tool_id: pu.tool_id,
            session_id: pu.session_id,
            project: pu.project,
            task_text: pu.task_text,
            outcome,
            duration_ms: None,
            cost_usd: None,
            occurred_at: pu.occurred_at,
        });
    }
    Ok(out)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        // Snap to char boundary to avoid splitting a UTF-8 codepoint.
        let mut idx = max;
        while idx > 0 && !s.is_char_boundary(idx) {
            idx -= 1;
        }
        format!("{}…", &s[..idx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn map_skill_plain() {
        let id = map_tool_id("Skill", &json!({"skill": "designlang"}));
        assert_eq!(id.as_deref(), Some("skill:designlang"));
    }

    #[test]
    fn map_skill_with_plugin_prefix() {
        let id = map_tool_id("Skill", &json!({"skill": "caveman:caveman"}));
        assert_eq!(id.as_deref(), Some("plugin:caveman@caveman"));
    }

    #[test]
    fn map_mcp_collapses_to_server() {
        let id = map_tool_id("mcp__ruflo__search", &json!({}));
        assert_eq!(id.as_deref(), Some("mcp:ruflo"));
    }

    #[test]
    fn map_builtin_returns_none() {
        for n in ["Bash", "Read", "Edit", "TodoWrite"] {
            assert!(
                map_tool_id(n, &json!({})).is_none(),
                "{n} should be filtered"
            );
        }
    }

    #[test]
    fn project_strips_dash_prefix_and_lowercases() {
        let p = Path::new(
            "/home/x/.claude/projects/-home-deiu-Documents-Programming-Quiver/abc.jsonl",
        );
        assert_eq!(project_from_path(p).as_deref(), Some("quiver"));
    }

    fn write_fixture(lines: &[Value]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let body: Vec<String> = lines.iter().map(|v| v.to_string()).collect();
        std::fs::write(&path, body.join("\n")).unwrap();
        (dir, path)
    }

    #[test]
    fn replay_emits_success_failure_abandoned() {
        let (_d, path) = write_fixture(&[
            json!({
                "type": "user",
                "message": {"role": "user", "content": [{"type": "text", "text": "do the thing"}]}
            }),
            json!({
                "type": "assistant",
                "timestamp": "2026-05-03T12:00:00Z",
                "sessionId": "sess-1",
                "message": {"model": "claude-opus-4-7", "content": [
                    {"type": "tool_use", "id": "toolu_a", "name": "Skill", "input": {"skill": "caveman"}}
                ]}
            }),
            json!({
                "type": "user",
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_a", "content": "ok"}
                ]}
            }),
            json!({
                "type": "assistant",
                "timestamp": "2026-05-03T12:01:00Z",
                "sessionId": "sess-1",
                "message": {"content": [
                    {"type": "tool_use", "id": "toolu_b", "name": "mcp__ruflo__search", "input": {"q": "x"}}
                ]}
            }),
            json!({
                "type": "user",
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_b", "content": "boom", "is_error": true}
                ]}
            }),
            json!({
                "type": "assistant",
                "timestamp": "2026-05-03T12:02:00Z",
                "sessionId": "sess-1",
                "message": {"content": [
                    {"type": "tool_use", "id": "toolu_c", "name": "Skill", "input": {"skill": "designlang"}}
                ]}
            }),
            // Built-in must be ignored
            json!({
                "type": "assistant",
                "timestamp": "2026-05-03T12:03:00Z",
                "sessionId": "sess-1",
                "message": {"content": [
                    {"type": "tool_use", "id": "toolu_d", "name": "Bash", "input": {"cmd": "ls"}}
                ]}
            }),
        ]);
        let events = replay(&path).unwrap();
        assert_eq!(events.len(), 3, "Bash must be filtered out");

        assert_eq!(events[0].tool_id, "skill:caveman");
        assert_eq!(events[0].outcome, Outcome::Success);
        assert_eq!(events[0].session_id.as_deref(), Some("sess-1"));
        assert_eq!(events[0].task_text.as_deref(), Some("do the thing"));

        assert_eq!(events[1].tool_id, "mcp:ruflo");
        assert_eq!(events[1].outcome, Outcome::Failure);

        assert_eq!(events[2].tool_id, "skill:designlang");
        assert_eq!(events[2].outcome, Outcome::Abandoned);
    }

    #[test]
    fn replay_tolerates_garbage_lines() {
        let (_d, path) = write_fixture(&[
            json!({"type": "queue-operation", "operation": "enqueue"}),
            json!({"type": "assistant", "timestamp": "2026-05-03T12:00:00Z",
                   "message": {"content": [
                       {"type": "tool_use", "id": "x", "name": "Skill", "input": {"skill": "y"}}
                   ]}}),
        ]);
        std::fs::write(
            &path,
            format!("{}\nNOT JSON\n", std::fs::read_to_string(&path).unwrap()),
        )
        .unwrap();
        let events = replay(&path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].outcome, Outcome::Abandoned);
    }

    #[test]
    fn truncate_respects_char_boundary() {
        let s = "héllo wörld with non-ascii 🚀 and more";
        let t = truncate(s, 10);
        assert!(t.ends_with('…'));
        assert!(t.len() <= 14);
    }
}
