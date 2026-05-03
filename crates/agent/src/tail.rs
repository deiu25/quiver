//! Incremental JSONL tail reader.
//!
//! `TailReader` tracks an in-memory byte offset per file. On each `poll`, it
//! re-opens the file, seeks to the offset, parses any new whole lines, and
//! advances the offset to the new EOF (or to the start of any partial trailing
//! line). The whole-file replay logic in
//! [`quiver_ingestion::session_jsonl::replay`] handles ingestion-time
//! aggregation; the agent loop wants per-line streaming, so this module
//! re-uses the field-level parsing patterns but emits one `TailEvent` per
//! recognised content block.
//!
//! Truncation handling: if the file shrunk below the saved offset, reset to 0
//! (the JSONL was rotated/cleared).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use quiver_ingestion::session_jsonl::map_tool_id;
use serde_json::Value;

const TASK_TEXT_MAX: usize = 500;

/// One parsed event lifted out of a JSONL line.
#[derive(Debug, Clone, PartialEq)]
pub enum TailEvent {
    /// A new user prompt — the trigger to recommend.
    UserText {
        session_id: String,
        text: String,
        ts: DateTime<Utc>,
    },
    /// Assistant invoked a non-builtin tool.
    ToolUse {
        session_id: String,
        uuid: String,
        tool_id: String,
        ts: DateTime<Utc>,
    },
    /// User-side `tool_result` — used to mark outcomes.
    ToolResult {
        session_id: String,
        uuid: String,
        is_error: Option<bool>,
        ts: DateTime<Utc>,
    },
}

#[derive(Debug)]
pub struct TailReader {
    pub path: PathBuf,
    offset: u64,
}

impl TailReader {
    /// Open at end-of-file (caller wants only events from now on). Used for
    /// existing JSONL files at agent startup so we don't re-suggest history.
    pub fn at_eof(path: &Path) -> Result<Self> {
        let len = std::fs::metadata(path)
            .with_context(|| format!("stat {}", path.display()))?
            .len();
        Ok(Self {
            path: path.to_path_buf(),
            offset: len,
        })
    }

    /// Open at offset 0 — used when we want every event in a brand-new file.
    pub fn at_start(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            offset: 0,
        }
    }

    /// Read every whole line that has appeared since the last `poll`,
    /// advance the offset, return ordered events. Partial trailing lines
    /// (no `\n`) are NOT consumed — we wait for them to be terminated.
    pub fn poll(&mut self) -> Result<Vec<TailEvent>> {
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("open {}", self.path.display())),
        };
        let len = file.metadata()?.len();
        if len < self.offset {
            self.offset = 0;
        }
        if len == self.offset {
            return Ok(Vec::new());
        }

        file.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::with_capacity((len - self.offset) as usize);
        file.read_to_end(&mut buf)?;

        let trailing_partial = !buf.ends_with(b"\n");
        let mut lines: Vec<&[u8]> = buf.split(|&b| b == b'\n').collect();
        let leftover_bytes: u64 = if trailing_partial {
            lines.pop().map(|s| s.len() as u64).unwrap_or(0)
        } else {
            lines.pop();
            0
        };

        let mut events = Vec::new();
        for raw in lines {
            if raw.is_empty() {
                continue;
            }
            let line = match std::str::from_utf8(raw) {
                Ok(s) => s,
                Err(_) => continue,
            };
            events.extend(parse_line(line));
        }

        self.offset = len - leftover_bytes;
        Ok(events)
    }
}

/// Parse one JSONL line → 0..N `TailEvent`s. A single user message can carry
/// both a `text` and a `tool_result` block, hence the vec.
fn parse_line(line: &str) -> Vec<TailEvent> {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let kind = v.get("type").and_then(|s| s.as_str()).unwrap_or("");
    let session_id = match v.get("sessionId").and_then(|s| s.as_str()) {
        Some(s) => s.to_string(),
        None => return Vec::new(),
    };
    let ts = v
        .get("timestamp")
        .and_then(|s| s.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    let blocks = v
        .pointer("/message/content")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    match kind {
        "user" => {
            for block in blocks {
                let bt = block.get("type").and_then(|s| s.as_str()).unwrap_or("");
                match bt {
                    "text" => {
                        if let Some(t) = block.get("text").and_then(|s| s.as_str()) {
                            out.push(TailEvent::UserText {
                                session_id: session_id.clone(),
                                text: truncate(t, TASK_TEXT_MAX),
                                ts,
                            });
                        }
                    },
                    "tool_result" => {
                        if let Some(uuid) = block.get("tool_use_id").and_then(|s| s.as_str()) {
                            out.push(TailEvent::ToolResult {
                                session_id: session_id.clone(),
                                uuid: uuid.to_string(),
                                is_error: block.get("is_error").and_then(|b| b.as_bool()),
                                ts,
                            });
                        }
                    },
                    _ => {},
                }
            }
        },
        "assistant" => {
            for block in blocks {
                if block.get("type").and_then(|s| s.as_str()) != Some("tool_use") {
                    continue;
                }
                let Some(uuid) = block.get("id").and_then(|s| s.as_str()) else {
                    continue;
                };
                let name = block.get("name").and_then(|s| s.as_str()).unwrap_or("");
                let input = block.get("input").cloned().unwrap_or(Value::Null);
                let Some(tool_id) = map_tool_id(name, &input) else {
                    continue;
                };
                out.push(TailEvent::ToolUse {
                    session_id: session_id.clone(),
                    uuid: uuid.to_string(),
                    tool_id,
                    ts,
                });
            }
        },
        _ => {},
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut idx = max;
        while idx > 0 && !s.is_char_boundary(idx) {
            idx -= 1;
        }
        format!("{}…", &s[..idx])
    }
}

/// Walk `root` and return every JSONL file path. Used by the engine on
/// startup to seed offsets and by the watcher recovery path.
pub fn walk_jsonl(root: &Path) -> Vec<PathBuf> {
    use walkdir::WalkDir;
    WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .collect()
}

/// Read `Path::file_stem()` as the session id (Claude Code names files
/// `<sessionId>.jsonl`). Falls back to a stringified path on weird inputs.
pub fn session_id_from_path(p: &Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn write_lines(path: &Path, lines: &[Value]) {
        let body: Vec<String> = lines.iter().map(|v| v.to_string()).collect();
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        if !body.is_empty() {
            f.write_all(body.join("\n").as_bytes()).unwrap();
            f.write_all(b"\n").unwrap();
        }
    }

    fn user_text(text: &str) -> Value {
        json!({
            "type": "user",
            "sessionId": "sess-1",
            "timestamp": "2026-05-03T12:00:00Z",
            "message": {"role": "user", "content": [{"type": "text", "text": text}]}
        })
    }

    fn tool_use(uuid: &str, skill: &str) -> Value {
        json!({
            "type": "assistant",
            "sessionId": "sess-1",
            "timestamp": "2026-05-03T12:01:00Z",
            "message": {"content": [
                {"type": "tool_use", "id": uuid, "name": "Skill", "input": {"skill": skill}}
            ]}
        })
    }

    fn tool_result(uuid: &str, is_err: bool) -> Value {
        json!({
            "type": "user",
            "sessionId": "sess-1",
            "timestamp": "2026-05-03T12:02:00Z",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": uuid, "is_error": is_err}
            ]}
        })
    }

    #[test]
    fn at_eof_emits_no_events_for_pre_existing_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        write_lines(&path, &[user_text("old prompt")]);
        let mut t = TailReader::at_eof(&path).unwrap();
        let evs = t.poll().unwrap();
        assert!(evs.is_empty());
    }

    #[test]
    fn picks_up_new_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        write_lines(&path, &[user_text("old")]);
        let mut t = TailReader::at_eof(&path).unwrap();
        write_lines(
            &path,
            &[user_text("new prompt"), tool_use("u1", "designlang")],
        );
        let evs = t.poll().unwrap();
        assert_eq!(evs.len(), 2);
        match &evs[0] {
            TailEvent::UserText { text, .. } => assert_eq!(text, "new prompt"),
            other => panic!("expected UserText, got {other:?}"),
        }
        match &evs[1] {
            TailEvent::ToolUse { tool_id, uuid, .. } => {
                assert_eq!(tool_id, "skill:designlang");
                assert_eq!(uuid, "u1");
            },
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn truncation_resets_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        // Write something longer first so the replacement is strictly shorter,
        // forcing the truncation branch (len < offset) in `poll`.
        write_lines(
            &path,
            &[user_text("a-fairly-long-prompt-that-makes-the-file-large")],
        );
        let mut t = TailReader::at_eof(&path).unwrap();
        std::fs::write(&path, format!("{}\n", user_text("ok"))).unwrap();
        let evs = t.poll().unwrap();
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            TailEvent::UserText { text, .. } => assert_eq!(text, "ok"),
            _ => panic!(),
        }
    }

    #[test]
    fn partial_trailing_line_held_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut t = TailReader::at_start(&path);
        write_lines(&path, &[user_text("done")]);
        let partial = user_text("partial").to_string();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(partial.as_bytes()).unwrap();

        let evs = t.poll().unwrap();
        assert_eq!(evs.len(), 1);

        f.write_all(b"\n").unwrap();
        let evs2 = t.poll().unwrap();
        assert_eq!(evs2.len(), 1);
    }

    #[test]
    fn tool_result_carries_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut t = TailReader::at_start(&path);
        write_lines(&path, &[tool_result("u1", true)]);
        let evs = t.poll().unwrap();
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            TailEvent::ToolResult {
                uuid,
                is_error: Some(true),
                ..
            } => assert_eq!(uuid, "u1"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn builtin_tools_are_filtered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let bash = json!({
            "type": "assistant",
            "sessionId": "sess-1",
            "timestamp": "2026-05-03T12:00:00Z",
            "message": {"content": [
                {"type": "tool_use", "id": "u9", "name": "Bash", "input": {"cmd": "ls"}}
            ]}
        });
        let mut t = TailReader::at_start(&path);
        write_lines(&path, &[bash]);
        let evs = t.poll().unwrap();
        assert!(evs.is_empty());
    }

    #[test]
    fn session_id_from_path_uses_stem() {
        let p = Path::new("/x/y/abc-123.jsonl");
        assert_eq!(session_id_from_path(p), "abc-123");
    }

    #[test]
    fn walk_jsonl_finds_only_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("proj-a");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("s1.jsonl"), b"").unwrap();
        std::fs::write(sub.join("ignore.txt"), b"").unwrap();
        let paths = walk_jsonl(dir.path());
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].file_name().unwrap(), "s1.jsonl");
    }
}
