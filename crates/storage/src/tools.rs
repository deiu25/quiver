use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use toolhub_core::tool::{ToolMeta, ToolType};

fn type_to_str(t: ToolType) -> &'static str {
    match t {
        ToolType::Skill => "skill",
        ToolType::Plugin => "plugin",
        ToolType::Mcp => "mcp",
        ToolType::Cli => "cli",
        ToolType::Doc => "doc",
    }
}

fn type_from_str(s: &str) -> anyhow::Result<ToolType> {
    Ok(match s {
        "skill" => ToolType::Skill,
        "plugin" => ToolType::Plugin,
        "mcp" => ToolType::Mcp,
        "cli" => ToolType::Cli,
        "doc" => ToolType::Doc,
        other => return Err(anyhow!("unknown tool type {other:?}")),
    })
}

fn parse_ts(s: &str) -> anyhow::Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(s)
        .with_context(|| format!("parse RFC3339 timestamp {s:?}"))?
        .with_timezone(&Utc))
}

pub fn upsert(conn: &Connection, m: &ToolMeta) -> anyhow::Result<()> {
    let triggers = serde_json::to_string(&m.triggers)?;
    let examples = serde_json::to_string(&m.examples)?;
    let requires = serde_json::to_string(&m.requires)?;
    let added = m.added_at.to_rfc3339();
    let seen = m.last_seen_at.to_rfc3339();
    let last_used = m.last_used_at.map(|d| d.to_rfc3339());
    conn.execute(
        "INSERT INTO tools (
            id, type, name, source_repo, install_path, description, long_description,
            category, triggers, examples, invocation, requires, enabled,
            added_at, last_seen_at, last_used_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
            type             = excluded.type,
            name             = excluded.name,
            source_repo      = excluded.source_repo,
            install_path     = excluded.install_path,
            description      = excluded.description,
            long_description = excluded.long_description,
            category         = excluded.category,
            triggers         = excluded.triggers,
            examples         = excluded.examples,
            invocation       = excluded.invocation,
            requires         = excluded.requires,
            enabled          = excluded.enabled,
            last_seen_at     = excluded.last_seen_at",
        params![
            m.id,
            type_to_str(m.r#type),
            m.name,
            m.source_repo,
            m.install_path,
            m.description,
            m.long_description,
            m.category,
            triggers,
            examples,
            m.invocation,
            requires,
            m.enabled as i64,
            added,
            seen,
            last_used,
        ],
    )?;
    Ok(())
}

pub fn list_all(conn: &Connection) -> anyhow::Result<Vec<ToolMeta>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, name, source_repo, install_path, description, long_description,
                category, triggers, examples, invocation, requires, enabled,
                added_at, last_seen_at, last_used_at
         FROM tools ORDER BY type, name",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, i64>(12)?,
                row.get::<_, String>(13)?,
                row.get::<_, String>(14)?,
                row.get::<_, Option<String>>(15)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(ToolMeta {
            id: r.0,
            r#type: type_from_str(&r.1)?,
            name: r.2,
            source_repo: r.3,
            install_path: r.4,
            description: r.5,
            long_description: r.6,
            category: r.7,
            triggers: serde_json::from_str(&r.8)?,
            examples: serde_json::from_str(&r.9)?,
            invocation: r.10,
            requires: serde_json::from_str(&r.11)?,
            enabled: r.12 != 0,
            added_at: parse_ts(&r.13)?,
            last_seen_at: parse_ts(&r.14)?,
            last_used_at: r.15.as_deref().map(parse_ts).transpose()?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open;
    use chrono::Utc;

    fn sample(id: &str, name: &str) -> ToolMeta {
        let now = Utc::now();
        ToolMeta {
            id: id.to_string(),
            r#type: ToolType::Skill,
            name: name.to_string(),
            source_repo: None,
            install_path: Some("/tmp/x".into()),
            description: Some("desc".into()),
            long_description: Some("body".into()),
            category: None,
            triggers: vec!["a".into(), "b".into()],
            examples: vec![],
            invocation: None,
            requires: vec!["dep:foo".into()],
            enabled: true,
            added_at: now,
            last_seen_at: now,
            last_used_at: None,
        }
    }

    #[test]
    fn upsert_then_list_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        upsert(&conn, &sample("skill:a", "a")).unwrap();
        upsert(&conn, &sample("skill:b", "b")).unwrap();
        let metas = list_all(&conn).unwrap();
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].id, "skill:a");
        assert_eq!(metas[1].id, "skill:b");
        assert_eq!(metas[0].triggers, vec!["a", "b"]);
        assert_eq!(metas[0].requires, vec!["dep:foo"]);
    }

    #[test]
    fn upsert_updates_existing_row() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        let mut m = sample("skill:x", "x");
        upsert(&conn, &m).unwrap();
        m.description = Some("changed".into());
        upsert(&conn, &m).unwrap();
        let metas = list_all(&conn).unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].description.as_deref(), Some("changed"));
    }
}
