use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use quiver_core::tool::{ToolMeta, ToolScope, ToolType};
use rusqlite::{Connection, params};

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

fn scope_to_str(s: ToolScope) -> &'static str {
    match s {
        ToolScope::User => "user",
        ToolScope::Project => "project",
    }
}

fn scope_from_str(s: &str) -> anyhow::Result<ToolScope> {
    Ok(match s {
        "user" => ToolScope::User,
        "project" => ToolScope::Project,
        other => return Err(anyhow!("unknown tool scope {other:?}")),
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
            added_at, last_seen_at, last_used_at, scope, scope_root
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
            last_seen_at     = excluded.last_seen_at,
            scope            = excluded.scope,
            scope_root       = excluded.scope_root",
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
            scope_to_str(m.scope),
            m.scope_root,
        ],
    )?;
    Ok(())
}

pub fn list_all(conn: &Connection) -> anyhow::Result<Vec<ToolMeta>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, name, source_repo, install_path, description, long_description,
                category, triggers, examples, invocation, requires, enabled,
                added_at, last_seen_at, last_used_at, scope, scope_root
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
                row.get::<_, String>(16)?,
                row.get::<_, Option<String>>(17)?,
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
            scope: scope_from_str(&r.16)?,
            scope_root: r.17,
        });
    }
    Ok(out)
}

/// Tool ids whose `scope='project' AND scope_root = ?`. Used by the project
/// scope reranker to know which catalog rows belong to the active cwd
/// without scanning every meta.
pub fn list_project_for_root(conn: &Connection, scope_root: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT id FROM tools WHERE scope = 'project' AND scope_root = ? ORDER BY id")?;
    let rows = stmt
        .query_map(params![scope_root], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Delete every tool whose `source_repo` exactly matches `location`.
/// Also clears matching rows from `tool_embeddings` and `tools_vec`.
/// Caller should call `fts::rebuild` afterwards to drop stale FTS rows.
/// Returns the list of deleted tool ids.
pub fn delete_by_source_repo(conn: &Connection, location: &str) -> anyhow::Result<Vec<String>> {
    let ids: Vec<String> = {
        let mut stmt = conn.prepare("SELECT id FROM tools WHERE source_repo = ?")?;
        stmt.query_map(params![location], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    if ids.is_empty() {
        return Ok(ids);
    }
    for id in &ids {
        conn.execute("DELETE FROM tools_vec WHERE tool_id = ?", params![id])?;
        conn.execute("DELETE FROM tool_embeddings WHERE tool_id = ?", params![id])?;
        conn.execute("DELETE FROM tools WHERE id = ?", params![id])?;
    }
    Ok(ids)
}

pub fn get(conn: &Connection, id: &str) -> anyhow::Result<Option<ToolMeta>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, name, source_repo, install_path, description, long_description,
                category, triggers, examples, invocation, requires, enabled,
                added_at, last_seen_at, last_used_at, scope, scope_root
         FROM tools WHERE id = ?",
    )?;
    let mut rows = stmt.query(rusqlite::params![id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let triggers: String = row.get(8)?;
    let examples: String = row.get(9)?;
    let requires: String = row.get(11)?;
    let added_at: String = row.get(13)?;
    let last_seen_at: String = row.get(14)?;
    let last_used_at: Option<String> = row.get(15)?;
    let scope: String = row.get(16)?;
    let scope_root: Option<String> = row.get(17)?;
    Ok(Some(ToolMeta {
        id: row.get(0)?,
        r#type: type_from_str(&row.get::<_, String>(1)?)?,
        name: row.get(2)?,
        source_repo: row.get(3)?,
        install_path: row.get(4)?,
        description: row.get(5)?,
        long_description: row.get(6)?,
        category: row.get(7)?,
        triggers: serde_json::from_str(&triggers)?,
        examples: serde_json::from_str(&examples)?,
        invocation: row.get(10)?,
        requires: serde_json::from_str(&requires)?,
        enabled: row.get::<_, i64>(12)? != 0,
        added_at: parse_ts(&added_at)?,
        last_seen_at: parse_ts(&last_seen_at)?,
        last_used_at: last_used_at.as_deref().map(parse_ts).transpose()?,
        scope: scope_from_str(&scope)?,
        scope_root,
    }))
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
            scope: ToolScope::User,
            scope_root: None,
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

    #[test]
    fn delete_by_source_repo_drops_only_matching_rows() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        let mut a = sample("skill:a", "a");
        a.source_repo = Some("https://github.com/owner/repo1".into());
        let mut b = sample("skill:b", "b");
        b.source_repo = Some("https://github.com/owner/repo1".into());
        let c = sample("skill:c", "c"); // source_repo = None
        upsert(&conn, &a).unwrap();
        upsert(&conn, &b).unwrap();
        upsert(&conn, &c).unwrap();
        // Seed an embedding for one row to confirm cleanup. 384 dims.
        let v = vec![0.1f32; 384];
        crate::embeddings::upsert(&conn, "skill:a", &v).unwrap();

        let deleted = delete_by_source_repo(&conn, "https://github.com/owner/repo1").unwrap();
        assert_eq!(deleted.len(), 2);
        assert!(deleted.contains(&"skill:a".to_string()));
        assert!(deleted.contains(&"skill:b".to_string()));

        let remaining = list_all(&conn).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "skill:c");

        let emb_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tool_embeddings WHERE tool_id = 'skill:a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(emb_count, 0);
    }

    #[test]
    fn delete_by_source_repo_returns_empty_when_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        upsert(&conn, &sample("skill:a", "a")).unwrap();
        let deleted = delete_by_source_repo(&conn, "https://github.com/none/none").unwrap();
        assert!(deleted.is_empty());
        assert_eq!(list_all(&conn).unwrap().len(), 1);
    }

    #[test]
    fn upsert_roundtrips_project_scope() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        let mut m = sample("skill:proj-tdd", "proj-tdd");
        m.scope = ToolScope::Project;
        m.scope_root = Some("/tmp/proj-x".to_string());
        upsert(&conn, &m).unwrap();
        let got = get(&conn, "skill:proj-tdd").unwrap().unwrap();
        assert_eq!(got.scope, ToolScope::Project);
        assert_eq!(got.scope_root.as_deref(), Some("/tmp/proj-x"));
    }

    #[test]
    fn list_project_for_root_filters_by_scope_root() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();

        let mut a = sample("skill:proj-a", "proj-a");
        a.scope = ToolScope::Project;
        a.scope_root = Some("/tmp/p1".into());
        let mut b = sample("skill:proj-b", "proj-b");
        b.scope = ToolScope::Project;
        b.scope_root = Some("/tmp/p2".into());
        let c = sample("skill:user-c", "user-c"); // user-scope, no root
        upsert(&conn, &a).unwrap();
        upsert(&conn, &b).unwrap();
        upsert(&conn, &c).unwrap();

        let p1 = list_project_for_root(&conn, "/tmp/p1").unwrap();
        assert_eq!(p1, vec!["skill:proj-a".to_string()]);
        let p2 = list_project_for_root(&conn, "/tmp/p2").unwrap();
        assert_eq!(p2, vec!["skill:proj-b".to_string()]);
        let none = list_project_for_root(&conn, "/tmp/does-not-exist").unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn user_scope_default_present_for_existing_rows() {
        // Migration 011 sets scope='user' as DEFAULT, so a row inserted via the
        // current upsert path always lands as `User` unless the caller flips it.
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        upsert(&conn, &sample("skill:legacy", "legacy")).unwrap();
        let got = get(&conn, "skill:legacy").unwrap().unwrap();
        assert_eq!(got.scope, ToolScope::User);
        assert!(got.scope_root.is_none());
    }
}
