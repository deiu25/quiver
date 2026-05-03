//! `toolhub stats` — aggregate view over `tool_scores` + `tools`.
//!
//! - `--top N` (default 20): list top tools by `success_rate * ln(sample+1)`.
//! - `--tool <id>`: detail view of one tool, including last 5 events.
//! - `--json`: machine output.

use serde::Serialize;
use toolhub_storage::{open, scores, tools, usage};

use crate::db_path::default_db_path;

#[derive(Serialize)]
struct StatsRow {
    tool_id: String,
    name: Option<String>,
    success_rate: Option<f64>,
    sample_size: Option<i64>,
    avg_cost_usd: Option<f64>,
    median_duration_ms: Option<i64>,
    last_used: Option<String>,
}

#[derive(Serialize)]
struct StatsDetail {
    #[serde(flatten)]
    row: StatsRow,
    recent_events: Vec<EventBrief>,
}

#[derive(Serialize)]
struct EventBrief {
    occurred_at: String,
    outcome: String,
    session_id: Option<String>,
    project: Option<String>,
}

pub async fn run(tool: Option<String>, top: usize, json: bool) -> anyhow::Result<()> {
    let db_path = default_db_path()?;
    let conn = open(&db_path)?;

    if let Some(id) = tool {
        let row = build_row(&conn, &id)?;
        let recent = usage::list_events(&conn, Some(&id), 5)?
            .into_iter()
            .map(|e| EventBrief {
                occurred_at: e.occurred_at,
                outcome: e
                    .outcome
                    .map(|o| o.as_str().to_string())
                    .unwrap_or_else(|| "unknown".into()),
                session_id: e.session_id,
                project: e.project,
            })
            .collect();
        let detail = StatsDetail {
            row,
            recent_events: recent,
        };
        if json {
            println!("{}", serde_json::to_string_pretty(&detail)?);
        } else {
            print_detail(&detail);
        }
        return Ok(());
    }

    let mut rows: Vec<StatsRow> = scores::list(&conn, None)?
        .into_iter()
        .map(|s| {
            let name = tools::get(&conn, &s.tool_id).ok().flatten().map(|m| m.name);
            StatsRow {
                tool_id: s.tool_id,
                name,
                success_rate: s.success_rate,
                sample_size: s.sample_size,
                avg_cost_usd: s.avg_cost_usd,
                median_duration_ms: s.median_duration_ms,
                last_used: None,
            }
        })
        .collect();

    rows.sort_by(|a, b| {
        let av = a.success_rate.unwrap_or(0.0) * ((a.sample_size.unwrap_or(0) as f64) + 1.0).ln();
        let bv = b.success_rate.unwrap_or(0.0) * ((b.sample_size.unwrap_or(0) as f64) + 1.0).ln();
        bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
    });
    rows.truncate(top);

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        print_table(&rows);
    }
    Ok(())
}

fn build_row(conn: &rusqlite::Connection, id: &str) -> anyhow::Result<StatsRow> {
    let s = scores::list(conn, Some(id))?.into_iter().next();
    let name = tools::get(conn, id).ok().flatten().map(|m| m.name);
    let last = usage::last_used(conn, id)?;
    Ok(match s {
        Some(s) => StatsRow {
            tool_id: s.tool_id,
            name,
            success_rate: s.success_rate,
            sample_size: s.sample_size,
            avg_cost_usd: s.avg_cost_usd,
            median_duration_ms: s.median_duration_ms,
            last_used: last,
        },
        None => StatsRow {
            tool_id: id.to_string(),
            name,
            success_rate: None,
            sample_size: None,
            avg_cost_usd: None,
            median_duration_ms: None,
            last_used: last,
        },
    })
}

fn print_table(rows: &[StatsRow]) {
    if rows.is_empty() {
        println!("no tool_scores rows — run `toolhub score` first");
        return;
    }
    let header_id = "TOOL_ID";
    let header_name = "NAME";
    let header_rate = "RATE";
    let header_n = "N";
    let header_med = "MED_MS";
    println!("{header_id:<40} {header_name:<28} {header_rate:>6} {header_n:>8} {header_med:>10}");
    for r in rows {
        let rate = r
            .success_rate
            .map(|v| format!("{v:.2}"))
            .unwrap_or_else(|| "—".into());
        let n = r
            .sample_size
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".into());
        let dur = r
            .median_duration_ms
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".into());
        let name = r.name.as_deref().unwrap_or("(unknown)");
        println!(
            "{:<40} {:<28} {:>6} {:>8} {:>10}",
            truncate_inline(&r.tool_id, 40),
            truncate_inline(name, 28),
            rate,
            n,
            dur
        );
    }
}

fn print_detail(d: &StatsDetail) {
    let r = &d.row;
    println!("tool_id    : {}", r.tool_id);
    println!("name       : {}", r.name.as_deref().unwrap_or("(unknown)"));
    println!(
        "rate       : {}",
        r.success_rate
            .map(|v| format!("{v:.2}"))
            .unwrap_or_else(|| "—".into())
    );
    println!(
        "samples    : {}",
        r.sample_size
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".into())
    );
    println!(
        "avg cost   : {}",
        r.avg_cost_usd
            .map(|v| format!("${v:.4}"))
            .unwrap_or_else(|| "—".into())
    );
    println!(
        "median ms  : {}",
        r.median_duration_ms
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".into())
    );
    println!("last used  : {}", r.last_used.as_deref().unwrap_or("never"));

    if !d.recent_events.is_empty() {
        println!("\nrecent events:");
        for ev in &d.recent_events {
            println!(
                "  {:<32} {:<10} {} / {}",
                ev.occurred_at,
                ev.outcome,
                ev.project.as_deref().unwrap_or("?"),
                ev.session_id.as_deref().unwrap_or("?")
            );
        }
    }
}

fn truncate_inline(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}
