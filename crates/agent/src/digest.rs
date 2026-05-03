//! Weekly markdown digest generator.
//!
//! Sections (in this order):
//! 1. **Top tools** — most-invoked in the window, with success rate.
//! 2. **Suggestion acceptance** — agent_suggestions counts since cutoff.
//! 3. **Dead weight** — tools with zero usage in the window.
//! 4. **New arrivals** — tools added in the window.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use quiver_storage::{open, scores, suggestions, tools, usage};
use rusqlite::Connection;

/// Generate a digest for the last `days` of activity. Writes the markdown to
/// `out_path` if `Some`, else returns it as a `String` for the caller to
/// print.
pub fn digest(db_path: &Path, days: u32, out_path: Option<&Path>) -> Result<String> {
    let conn = open(db_path)?;
    let now = Utc::now();
    let cutoff = now - Duration::days(days as i64);
    let body = render(&conn, now, cutoff, days)?;

    if let Some(p) = out_path {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(p, &body)?;
    }
    Ok(body)
}

fn render(
    conn: &Connection,
    now: DateTime<Utc>,
    cutoff: DateTime<Utc>,
    days: u32,
) -> Result<String> {
    let mut out = String::new();
    out.push_str(&format!("# Quiver digest — last {days} day(s)\n\n"));
    out.push_str(&format!("_generated {}_\n\n", now.to_rfc3339()));

    out.push_str("## Top tools\n\n");
    let top = top_tools(conn, &cutoff)?;
    if top.is_empty() {
        out.push_str("_No usage events in the window._\n\n");
    } else {
        out.push_str("| tool_id | events | success_rate | sample_size |\n");
        out.push_str("|---|---:|---:|---:|\n");
        for r in &top {
            let sr = r
                .success_rate
                .map(|x| format!("{:.0}%", x * 100.0))
                .unwrap_or_else(|| "—".into());
            let n = r
                .sample_size
                .map(|n| n.to_string())
                .unwrap_or_else(|| "—".into());
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                r.tool_id, r.event_count, sr, n
            ));
        }
        out.push('\n');
    }

    out.push_str("## Suggestion acceptance\n\n");
    let (suggested, accepted) = suggestions::acceptance_stats(conn, cutoff)?;
    if suggested == 0 {
        out.push_str("_No suggestions in the window._\n\n");
    } else {
        let pct = 100.0 * (accepted as f64) / (suggested as f64);
        out.push_str(&format!(
            "Suggested **{suggested}** tool(s); user invoked the suggestion in **{accepted}** \
             case(s) ({pct:.1}%).\n\n"
        ));
    }

    out.push_str("## Dead weight\n\n");
    let dead = usage::dead_weight(conn, days)?;
    if dead.is_empty() {
        out.push_str("_Every catalogued tool has been used recently — clean slate._\n\n");
    } else {
        for (id, name, last) in dead.iter().take(20) {
            let last = last.as_deref().unwrap_or("never");
            out.push_str(&format!("- `{id}` ({name}) — last seen {last}\n"));
        }
        if dead.len() > 20 {
            out.push_str(&format!("- _…and {} more._\n", dead.len() - 20));
        }
        out.push('\n');
    }

    out.push_str("## New arrivals\n\n");
    let arrivals = tools::list_all(conn)?
        .into_iter()
        .filter(|m| m.added_at >= cutoff)
        .collect::<Vec<_>>();
    if arrivals.is_empty() {
        out.push_str("_No new tools onboarded._\n\n");
    } else {
        for m in &arrivals {
            let desc = m.description.as_deref().unwrap_or("");
            out.push_str(&format!("- `{}` — {}\n", m.id, desc));
        }
        out.push('\n');
    }

    Ok(out)
}

#[derive(Debug, Clone)]
struct TopRow {
    tool_id: String,
    event_count: i64,
    success_rate: Option<f64>,
    sample_size: Option<i64>,
}

fn top_tools(conn: &Connection, cutoff: &DateTime<Utc>) -> Result<Vec<TopRow>> {
    let cutoff_str = cutoff.to_rfc3339();
    let mut stmt = conn.prepare(
        "SELECT tool_id, COUNT(*) AS n
         FROM usage_events
         WHERE occurred_at >= ?
         GROUP BY tool_id
         ORDER BY n DESC
         LIMIT 20",
    )?;
    let rows = stmt
        .query_map([cutoff_str], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let score_map: HashMap<String, _> = scores::list(conn, None)?
        .into_iter()
        .map(|s| (s.tool_id.clone(), s))
        .collect();

    Ok(rows
        .into_iter()
        .map(|(id, n)| {
            let (sr, samples) = score_map
                .get(&id)
                .map(|s| (s.success_rate, s.sample_size))
                .unwrap_or((None, None));
            TopRow {
                tool_id: id,
                event_count: n,
                success_rate: sr,
                sample_size: samples,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use quiver_core::usage::{Outcome, UsageEvent};
    use rusqlite::params;

    fn seed_tool(conn: &Connection, id: &str, added: DateTime<Utc>) {
        conn.execute(
            "INSERT OR IGNORE INTO tools (id, type, name, description, triggers, examples,
                                          requires, enabled, added_at, last_seen_at)
             VALUES (?, 'skill', ?, 'desc', '[]', '[]', '[]', 1, ?, ?)",
            params![id, id, added.to_rfc3339(), added.to_rfc3339()],
        )
        .unwrap();
    }

    fn evt(uuid: &str, tool: &str, outcome: Outcome, when: DateTime<Utc>) -> UsageEvent {
        UsageEvent {
            uuid: Some(uuid.into()),
            tool_id: tool.into(),
            session_id: Some("sess".into()),
            project: Some("p".into()),
            task_text: None,
            outcome,
            duration_ms: None,
            cost_usd: None,
            occurred_at: when,
        }
    }

    fn open_tmp() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sqlite");
        let _conn = open(&path).unwrap();
        (dir, path)
    }

    #[test]
    fn empty_db_renders_all_sections() {
        let (_d, path) = open_tmp();
        let body = digest(&path, 7, None).unwrap();
        for header in [
            "## Top tools",
            "## Suggestion acceptance",
            "## Dead weight",
            "## New arrivals",
        ] {
            assert!(body.contains(header), "missing {header} in:\n{body}");
        }
        assert!(body.contains("No usage events"));
        assert!(body.contains("No suggestions"));
        assert!(body.contains("No new tools onboarded"));
    }

    #[test]
    fn populated_db_reports_top_tool_and_acceptance() {
        let (_d, path) = open_tmp();
        let mut conn = open(&path).unwrap();
        let now = Utc::now();
        let recent = now - Duration::days(1);

        seed_tool(&conn, "skill:caveman", recent);
        seed_tool(&conn, "skill:designlang", now);

        usage::insert_event(&conn, &evt("u1", "skill:caveman", Outcome::Success, recent)).unwrap();
        usage::insert_event(&conn, &evt("u2", "skill:caveman", Outcome::Success, recent)).unwrap();
        usage::insert_event(&conn, &evt("u3", "skill:caveman", Outcome::Failure, recent)).unwrap();
        usage::insert_event(
            &conn,
            &evt("u4", "skill:designlang", Outcome::Success, recent),
        )
        .unwrap();
        usage::recompute_scores(&mut conn).unwrap();

        suggestions::record(&conn, "s1", "skill:caveman", None, Some(0.9), recent).unwrap();
        suggestions::record(&conn, "s1", "skill:designlang", None, Some(0.7), recent).unwrap();
        suggestions::mark_accepted(&conn, "s1", "skill:caveman", recent, 60).unwrap();

        let body = digest(&path, 7, None).unwrap();
        assert!(body.contains("skill:caveman"));
        assert!(body.contains("skill:designlang"));
        let cave = body.find("skill:caveman").unwrap();
        let dl = body.find("skill:designlang").unwrap();
        assert!(cave < dl, "caveman should appear above designlang");
        assert!(body.contains("Suggested **2**"));
        assert!(body.contains("**1**"));
    }

    #[test]
    fn writes_to_out_path_when_provided() {
        let (_d, path) = open_tmp();
        let outdir = tempfile::tempdir().unwrap();
        let out = outdir.path().join("d.md");
        let body = digest(&path, 7, Some(&out)).unwrap();
        assert!(out.exists());
        let on_disk = std::fs::read_to_string(&out).unwrap();
        assert_eq!(on_disk, body);
    }

    #[test]
    fn old_arrivals_are_excluded() {
        let (_d, path) = open_tmp();
        let conn = open(&path).unwrap();
        let long_ago = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        seed_tool(&conn, "skill:ancient", long_ago);
        drop(conn);
        let body = digest(&path, 7, None).unwrap();
        assert!(body.contains("No new tools onboarded"));
    }
}
