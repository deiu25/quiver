use anyhow::Context;
use rusqlite::Connection;

/// Rebuild the FTS5 index from the underlying `tools` table. Cheap at <1k rows.
pub fn rebuild(conn: &Connection) -> anyhow::Result<()> {
    conn.execute("INSERT INTO tools_fts(tools_fts) VALUES('rebuild')", [])
        .context("rebuild tools_fts")?;
    Ok(())
}

/// Run an FTS5 MATCH query and return up to `limit` hits as
/// `(tool_id, bm25_score)`. BM25 is negative — closer to 0 = better match.
/// FTS5 column weights (in declaration order from migration 002):
///   name, description, long_description, triggers, examples, category
/// We boost name (5x) and triggers (3x) since they encode the strongest
/// task-matching signal.
pub fn search(conn: &Connection, query: &str, limit: usize) -> anyhow::Result<Vec<(String, f32)>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, bm25(tools_fts, 5.0, 2.0, 1.0, 3.0, 1.0, 1.0) AS rank
         FROM tools_fts
         JOIN tools t ON t.rowid = tools_fts.rowid
         WHERE tools_fts MATCH ?1
         ORDER BY rank
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![query, limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)? as f32))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{open, tools};
    use chrono::Utc;
    use quiver_core::tool::{ToolMeta, ToolType};

    fn sample(id: &str, name: &str, desc: &str) -> ToolMeta {
        let now = Utc::now();
        ToolMeta {
            id: id.into(),
            r#type: ToolType::Skill,
            name: name.into(),
            source_repo: None,
            install_path: None,
            description: Some(desc.into()),
            long_description: None,
            category: None,
            triggers: vec![],
            examples: vec![],
            invocation: None,
            requires: vec![],
            enabled: true,
            added_at: now,
            last_seen_at: now,
            last_used_at: None,
        }
    }

    #[test]
    fn rebuild_then_search_finds_tool() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("f.sqlite")).unwrap();
        tools::upsert(
            &conn,
            &sample("skill:design-md", "design-md", "extract design tokens"),
        )
        .unwrap();
        tools::upsert(
            &conn,
            &sample(
                "skill:caveman",
                "caveman",
                "compress markdown caveman speak",
            ),
        )
        .unwrap();
        rebuild(&conn).unwrap();

        let hits = search(&conn, "design", 10).unwrap();
        assert!(hits.iter().any(|(id, _)| id == "skill:design-md"));

        let hits = search(&conn, "caveman", 10).unwrap();
        assert!(hits.iter().any(|(id, _)| id == "skill:caveman"));
    }
}
