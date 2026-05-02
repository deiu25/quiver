use anyhow::{Context, anyhow};
use rusqlite::{Connection, params};

const F32_BYTES: usize = 4;

fn to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * F32_BYTES);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

fn from_blob(b: &[u8]) -> anyhow::Result<Vec<f32>> {
    if b.len() % F32_BYTES != 0 {
        return Err(anyhow!(
            "embedding blob length {} is not a multiple of 4",
            b.len()
        ));
    }
    let mut out = Vec::with_capacity(b.len() / F32_BYTES);
    for chunk in b.chunks_exact(F32_BYTES) {
        let arr: [u8; 4] = chunk.try_into().expect("chunks_exact gives 4 bytes");
        out.push(f32::from_le_bytes(arr));
    }
    Ok(out)
}

pub fn upsert(conn: &Connection, tool_id: &str, vector: &[f32]) -> anyhow::Result<()> {
    let blob = to_blob(vector);
    conn.execute(
        "INSERT INTO tool_embeddings (tool_id, embedding) VALUES (?, ?)
         ON CONFLICT(tool_id) DO UPDATE SET embedding = excluded.embedding",
        params![tool_id, blob],
    )
    .with_context(|| format!("upsert embedding for {tool_id}"))?;
    Ok(())
}

pub fn list_all(conn: &Connection) -> anyhow::Result<Vec<(String, Vec<f32>)>> {
    let mut stmt = conn.prepare("SELECT tool_id, embedding FROM tool_embeddings")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut out = Vec::with_capacity(rows.len());
    for (id, blob) in rows {
        out.push((id, from_blob(&blob)?));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open;

    fn ensure_parent_tool(conn: &Connection, id: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO tools (id, type, name, triggers, examples, requires,
                                enabled, added_at, last_seen_at)
             VALUES (?, 'skill', ?, '[]', '[]', '[]', 1, ?, ?)",
            rusqlite::params![id, id, now, now],
        )
        .unwrap();
    }

    #[test]
    fn upsert_then_list_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("e.sqlite")).unwrap();
        ensure_parent_tool(&conn, "skill:a");
        let v = (0..384).map(|i| i as f32 / 384.0).collect::<Vec<_>>();
        upsert(&conn, "skill:a", &v).unwrap();
        let all = list_all(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "skill:a");
        assert_eq!(all[0].1, v);
    }

    #[test]
    fn upsert_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("e.sqlite")).unwrap();
        ensure_parent_tool(&conn, "skill:x");
        upsert(&conn, "skill:x", &[1.0, 2.0, 3.0]).unwrap();
        upsert(&conn, "skill:x", &[9.0, 8.0]).unwrap();
        let all = list_all(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].1, vec![9.0, 8.0]);
    }
}
