//! Score-aware reranking, Phase 4 PLAN §8.3.
//!
//! After the hybrid (cosine + BM25) combine produces candidate `Hit`s, the
//! reranker boosts tools with proven track records. Tools without enough
//! samples or without a `tool_scores` row are passed through unchanged, so
//! Phase 1–3 behaviour is preserved when telemetry is empty.

use std::collections::HashMap;

use rusqlite::Connection;

use crate::search::Hit;

/// Default boost factor — chosen so a tool with 100 % success rate gets a
/// 1.3× score multiplier. PLAN §10 #5: track recommender accuracy on the
/// benchmark set and tune.
pub const SUCCESS_ALPHA: f32 = 0.3;

/// Minimum sample size before we trust a tool's success_rate enough to apply
/// the boost. Below this, the score is statistically noisy.
pub const MIN_SAMPLE_SIZE: i64 = 5;

pub trait Reranker {
    fn apply(&self, hits: &mut [Hit], conn: &Connection) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, Copy)]
pub struct SuccessReranker {
    pub alpha: f32,
    pub min_samples: i64,
}

impl Default for SuccessReranker {
    fn default() -> Self {
        Self {
            alpha: SUCCESS_ALPHA,
            min_samples: MIN_SAMPLE_SIZE,
        }
    }
}

impl Reranker for SuccessReranker {
    fn apply(&self, hits: &mut [Hit], conn: &Connection) -> anyhow::Result<()> {
        if hits.is_empty() {
            return Ok(());
        }
        let scores = load_scores(conn, hits)?;
        for hit in hits.iter_mut() {
            if let Some((rate, samples)) = scores.get(&hit.tool_id)
                && *samples >= self.min_samples
            {
                hit.score *= 1.0 + self.alpha * (*rate as f32);
            }
        }
        // Re-sort — ordering may have changed.
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(())
    }
}

fn load_scores(conn: &Connection, hits: &[Hit]) -> anyhow::Result<HashMap<String, (f64, i64)>> {
    let ids: Vec<&str> = hits.iter().map(|h| h.tool_id.as_str()).collect();
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT tool_id, success_rate, sample_size
         FROM tool_scores
         WHERE tool_id IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::ToSql> = ids.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    let rows = stmt
        .query_map(params.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<f64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = HashMap::with_capacity(rows.len());
    for (id, rate, n) in rows {
        if let (Some(r), Some(n)) = (rate, n) {
            out.insert(id, (r, n));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn open_with_schema() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tool_scores (
                tool_id TEXT PRIMARY KEY,
                success_rate REAL,
                sample_size INTEGER,
                avg_cost_usd REAL,
                median_duration_ms INTEGER,
                score_updated_at TEXT
            );",
        )
        .unwrap();
        conn
    }

    fn seed(conn: &Connection, id: &str, rate: f64, n: i64) {
        conn.execute(
            "INSERT INTO tool_scores VALUES (?, ?, ?, NULL, NULL, '2026-05-03T00:00:00Z')",
            params![id, rate, n],
        )
        .unwrap();
    }

    #[test]
    fn alpha_zero_is_identity() {
        let conn = open_with_schema();
        seed(&conn, "skill:a", 1.0, 100);
        let mut hits = vec![Hit {
            tool_id: "skill:a".into(),
            score: 0.5,
        }];
        let rer = SuccessReranker {
            alpha: 0.0,
            min_samples: 5,
        };
        rer.apply(&mut hits, &conn).unwrap();
        assert!((hits[0].score - 0.5).abs() < 1e-6);
    }

    #[test]
    fn boost_only_when_min_samples_met() {
        let conn = open_with_schema();
        seed(&conn, "skill:trusted", 1.0, 10);
        seed(&conn, "skill:noisy", 1.0, 2);
        let mut hits = vec![
            Hit {
                tool_id: "skill:trusted".into(),
                score: 0.5,
            },
            Hit {
                tool_id: "skill:noisy".into(),
                score: 0.5,
            },
        ];
        let rer = SuccessReranker::default();
        rer.apply(&mut hits, &conn).unwrap();
        let trusted = hits.iter().find(|h| h.tool_id == "skill:trusted").unwrap();
        let noisy = hits.iter().find(|h| h.tool_id == "skill:noisy").unwrap();
        assert!(trusted.score > noisy.score);
        assert!((trusted.score - 0.5 * (1.0 + 0.3)).abs() < 1e-6);
        assert!((noisy.score - 0.5).abs() < 1e-6);
    }

    #[test]
    fn missing_score_passes_through() {
        let conn = open_with_schema();
        let mut hits = vec![Hit {
            tool_id: "skill:no-data".into(),
            score: 0.7,
        }];
        let rer = SuccessReranker::default();
        rer.apply(&mut hits, &conn).unwrap();
        assert!((hits[0].score - 0.7).abs() < 1e-6);
    }

    #[test]
    fn boost_can_change_ordering() {
        let conn = open_with_schema();
        seed(&conn, "skill:underdog", 1.0, 50);
        let mut hits = vec![
            Hit {
                tool_id: "skill:leader".into(),
                score: 0.6,
            },
            Hit {
                tool_id: "skill:underdog".into(),
                score: 0.5,
            },
        ];
        let rer = SuccessReranker::default();
        rer.apply(&mut hits, &conn).unwrap();
        // underdog now 0.5 * 1.3 = 0.65 → wins.
        assert_eq!(hits[0].tool_id, "skill:underdog");
    }

    #[test]
    fn empty_hits_is_noop() {
        let conn = open_with_schema();
        let mut hits: Vec<Hit> = Vec::new();
        SuccessReranker::default().apply(&mut hits, &conn).unwrap();
        assert!(hits.is_empty());
    }
}
