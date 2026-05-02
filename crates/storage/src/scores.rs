//! `tool_scores` table accessors.
//!
//! Phase 3: read-only access — `tool_scores` is empty until Phase 4 lands
//! the heuristic outcome scorer.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreRow {
    pub tool_id: String,
    pub success_rate: Option<f64>,
    pub sample_size: Option<i64>,
    pub avg_cost_usd: Option<f64>,
    pub median_duration_ms: Option<i64>,
    pub score_updated_at: Option<String>,
}

pub fn list(conn: &Connection, tool_id: Option<&str>) -> anyhow::Result<Vec<ScoreRow>> {
    let (sql, params) = match tool_id {
        Some(id) => (
            "SELECT tool_id, success_rate, sample_size, avg_cost_usd,
                    median_duration_ms, score_updated_at
             FROM tool_scores WHERE tool_id = ?",
            vec![id.to_string()],
        ),
        None => (
            "SELECT tool_id, success_rate, sample_size, avg_cost_usd,
                    median_duration_ms, score_updated_at
             FROM tool_scores ORDER BY tool_id",
            vec![],
        ),
    };
    let mut stmt = conn.prepare(sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(ScoreRow {
                tool_id: row.get(0)?,
                success_rate: row.get(1)?,
                sample_size: row.get(2)?,
                avg_cost_usd: row.get(3)?,
                median_duration_ms: row.get(4)?,
                score_updated_at: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open;
    use rusqlite::params;

    #[test]
    fn list_empty_table_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        let rows = list(&conn, None).unwrap();
        assert!(rows.is_empty());
        let rows = list(&conn, Some("skill:nonexistent")).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn list_returns_inserted_rows_filtered_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        // tool_scores has FK to tools(id); insert a tool first.
        conn.execute(
            "INSERT INTO tools (id, type, name, triggers, examples, requires,
                                enabled, added_at, last_seen_at)
             VALUES (?, 'skill', 'X', '[]', '[]', '[]', 1,
                     '2026-05-03T00:00:00+00:00', '2026-05-03T00:00:00+00:00')",
            params!["skill:x"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tool_scores
             (tool_id, success_rate, sample_size, avg_cost_usd,
              median_duration_ms, score_updated_at)
             VALUES (?, 0.8, 10, 0.04, 250, '2026-05-03T00:00:00+00:00')",
            params!["skill:x"],
        )
        .unwrap();
        let rows = list(&conn, Some("skill:x")).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sample_size, Some(10));
    }
}
