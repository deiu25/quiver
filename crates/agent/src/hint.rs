//! Hint markdown writer.
//!
//! On each new user message in a watched session, the engine renders the top
//! recommendations into `<hints_dir>/<session_id>.md` and atomically swaps the
//! file. A caveman-style hook (or any user wrapper) can `cat` the file
//! mid-session to surface the suggestion.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use crate::recommend::RecHit;

/// Render `hits` into a markdown body for `session_id` recommended at `ts`.
pub fn render(
    session_id: &str,
    task_text: Option<&str>,
    ts: DateTime<Utc>,
    hits: &[RecHit],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# ToolHub hint — {session_id}\n\n"));
    out.push_str(&format!("_generated {}_\n\n", ts.to_rfc3339()));
    if let Some(t) = task_text {
        out.push_str(&format!("**Task:** {t}\n\n"));
    }
    if hits.is_empty() {
        out.push_str("_No recommendations — run `toolhub sync` to populate the index._\n");
        return out;
    }
    out.push_str("## Top recommendations\n\n");
    for (i, h) in hits.iter().enumerate() {
        out.push_str(&format!(
            "{n}. **{id}** _(score {score:.3})_\n",
            n = i + 1,
            id = h.tool_id,
            score = h.score
        ));
        if let Some(desc) = &h.description {
            out.push_str(&format!("   - {desc}\n"));
        }
        if let Some(inv) = &h.invocation {
            out.push_str(&format!("   - invoke: `{inv}`\n"));
        }
    }
    out
}

/// Write `<hints_dir>/<session_id>.md` atomically (temp file + rename) and
/// return the final path.
pub fn write_hint(
    hints_dir: &Path,
    session_id: &str,
    task_text: Option<&str>,
    ts: DateTime<Utc>,
    hits: &[RecHit],
) -> Result<PathBuf> {
    fs::create_dir_all(hints_dir)
        .with_context(|| format!("create hints dir {}", hints_dir.display()))?;
    let final_path = hints_dir.join(format!("{session_id}.md"));
    let tmp_path = hints_dir.join(format!(".{session_id}.md.tmp"));
    let body = render(session_id, task_text, ts, hits);
    {
        let mut f = fs::File::create(&tmp_path)
            .with_context(|| format!("create temp {}", tmp_path.display()))?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("rename {} -> {}", tmp_path.display(), final_path.display()))?;
    Ok(final_path)
}

/// Delete hint files older than `max_age_days`. Best-effort — failures
/// logged via `tracing`, never bubble up.
pub fn cleanup_stale(hints_dir: &Path, max_age_days: i64) -> Result<usize> {
    if !hints_dir.exists() {
        return Ok(0);
    }
    let cutoff = Utc::now() - chrono::Duration::days(max_age_days);
    let mut removed = 0usize;
    for entry in fs::read_dir(hints_dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("hints dir entry error: {e}");
                continue;
            },
        };
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| chrono::DateTime::<Utc>::from_timestamp(d.as_secs() as i64, 0));
        if let Some(mt) = mtime
            && mt < cutoff
        {
            if let Err(e) = fs::remove_file(&path) {
                tracing::warn!("rm {} failed: {e}", path.display());
            } else {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn hits_fixture() -> Vec<RecHit> {
        vec![
            RecHit {
                tool_id: "skill:designlang".into(),
                score: 0.872,
                description: Some("extract tokens".into()),
                invocation: Some("/designlang".into()),
            },
            RecHit {
                tool_id: "skill:design-md".into(),
                score: 0.654,
                description: None,
                invocation: None,
            },
        ]
    }

    #[test]
    fn render_includes_session_task_and_top_hits() {
        let ts = Utc.with_ymd_and_hms(2026, 5, 3, 12, 0, 0).unwrap();
        let body = render("smoke-1", Some("extract tokens"), ts, &hits_fixture());
        assert!(body.contains("# ToolHub hint — smoke-1"));
        assert!(body.contains("**Task:** extract tokens"));
        assert!(body.contains("1. **skill:designlang**"));
        assert!(body.contains("score 0.872"));
        assert!(body.contains("`/designlang`"));
        assert!(body.contains("2. **skill:design-md**"));
    }

    #[test]
    fn render_handles_empty_hits() {
        let ts = Utc::now();
        let body = render("s", None, ts, &[]);
        assert!(body.contains("No recommendations"));
    }

    #[test]
    fn write_hint_atomically_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let ts = Utc::now();
        let p = write_hint(dir.path(), "smoke-1", Some("t"), ts, &hits_fixture()).unwrap();
        assert!(p.exists());
        assert_eq!(p.file_name().unwrap(), "smoke-1.md");
        let body = fs::read_to_string(&p).unwrap();
        assert!(body.contains("skill:designlang"));
    }

    #[test]
    fn write_hint_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let ts = Utc::now();
        write_hint(dir.path(), "s", None, ts, &hits_fixture()).unwrap();
        let p = write_hint(dir.path(), "s", Some("new"), ts, &[]).unwrap();
        let body = fs::read_to_string(&p).unwrap();
        assert!(body.contains("**Task:** new"));
        assert!(body.contains("No recommendations"));
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(p.file_name().unwrap(), "s.md");
    }
}
