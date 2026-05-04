//! `tool_scores` table accessors.
//!
//! Phase 3: read-only access — `tool_scores` is empty until Phase 4 lands
//! the heuristic outcome scorer.

use chrono::{DateTime, Utc};
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

#[derive(Debug, Clone, Serialize)]
pub struct TopSpendRow {
    pub tool_id: String,
    pub total_cost_usd: f64,
    pub samples: i64,
}

/// Top tools by total `cost_usd` over events with `occurred_at >= since`.
/// Excludes events with NULL `cost_usd` (older rows pre-cost-extraction;
/// re-run `quiver score` to backfill).
pub fn top_by_cost(
    conn: &Connection,
    since: DateTime<Utc>,
    limit: i64,
) -> rusqlite::Result<Vec<TopSpendRow>> {
    let mut stmt = conn.prepare(
        "SELECT tool_id, SUM(cost_usd) AS total, COUNT(*) AS n
         FROM usage_events
         WHERE cost_usd IS NOT NULL AND occurred_at >= ?1
         GROUP BY tool_id
         ORDER BY total DESC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![since.to_rfc3339(), limit], |row| {
            Ok(TopSpendRow {
                tool_id: row.get(0)?,
                total_cost_usd: row.get(1)?,
                samples: row.get(2)?,
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

    #[test]
    fn top_by_cost_aggregates_and_orders_desc() {
        use chrono::Duration;

        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        for id in ["skill:a", "skill:b", "skill:c"] {
            conn.execute(
                "INSERT INTO tools (id, type, name, triggers, examples, requires,
                                    enabled, added_at, last_seen_at)
                 VALUES (?, 'skill', ?, '[]', '[]', '[]', 1,
                         '2026-05-03T00:00:00+00:00', '2026-05-03T00:00:00+00:00')",
                params![id, id],
            )
            .unwrap();
        }
        let now = Utc::now();
        let recent = now - Duration::hours(1);
        let too_old = now - Duration::days(60);
        // skill:a → 2 events of $0.10 + $0.20 = $0.30
        // skill:b → 1 event of $1.00
        // skill:c → 1 event but cost NULL (excluded)
        // skill:a → 1 too-old event $5 (excluded)
        for (id, ts, cost) in [
            ("skill:a", recent, Some(0.10)),
            ("skill:a", recent, Some(0.20)),
            ("skill:a", too_old, Some(5.0)),
            ("skill:b", recent, Some(1.00)),
            ("skill:c", recent, None),
        ] {
            conn.execute(
                "INSERT INTO usage_events
                 (tool_id, session_id, project, task_text, outcome,
                  duration_ms, cost_usd, occurred_at)
                 VALUES (?, 's', 'p', NULL, 'success', NULL, ?, ?)",
                params![id, cost, ts.to_rfc3339()],
            )
            .unwrap();
        }
        let cutoff = now - Duration::days(7);
        let rows = top_by_cost(&conn, cutoff, 10).unwrap();
        assert_eq!(rows.len(), 2, "skill:c excluded (NULL cost)");
        assert_eq!(rows[0].tool_id, "skill:b");
        assert!((rows[0].total_cost_usd - 1.0).abs() < 1e-9);
        assert_eq!(rows[0].samples, 1);
        assert_eq!(rows[1].tool_id, "skill:a");
        assert!((rows[1].total_cost_usd - 0.30).abs() < 1e-9);
        assert_eq!(rows[1].samples, 2);
    }
}
