//! `toolhub dead-weight [--days 30]` — list catalogued tools with no usage
//! recorded in the last N days.

use toolhub_storage::{open, usage};

use crate::db_path::default_db_path;

pub async fn run(days: u32) -> anyhow::Result<()> {
    let db_path = default_db_path()?;
    let conn = open(&db_path)?;
    let rows = usage::dead_weight(&conn, days)?;
    if rows.is_empty() {
        println!(
            "no dead-weight tools — every catalogued tool has usage in the last {days} day(s)"
        );
        return Ok(());
    }
    println!("{} tool(s) unused in the last {days} day(s):", rows.len());
    let header_id = "TOOL_ID";
    let header_name = "NAME";
    println!("{header_id:<40} {header_name:<28} LAST_USED");
    for (id, name, last) in rows {
        println!(
            "{:<40} {:<28} {}",
            truncate_inline(&id, 40),
            truncate_inline(&name, 28),
            last.as_deref().unwrap_or("never")
        );
    }
    Ok(())
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
