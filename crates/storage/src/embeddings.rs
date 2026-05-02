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
    if !b.len().is_multiple_of(F32_BYTES) {
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
        params![tool_id, &blob],
    )
    .with_context(|| format!("upsert embedding for {tool_id}"))?;
    // Mirror into vec0 virtual table for fast nearest-neighbour search.
    conn.execute("DELETE FROM tools_vec WHERE tool_id = ?", params![tool_id])
        .with_context(|| format!("clear vec row for {tool_id}"))?;
    conn.execute(
        "INSERT INTO tools_vec (tool_id, embedding) VALUES (?, ?)",
        params![tool_id, &blob],
    )
    .with_context(|| format!("insert vec row for {tool_id}"))?;
    Ok(())
}

/// Approximate nearest-neighbour search over `tools_vec` (sqlite-vec).
/// Returns up to `k` `(tool_id, distance)` pairs ordered best-first.
/// `distance` is the metric configured on the column (cosine: 0 = identical,
/// 2 = opposite). Caller converts to similarity with `1.0 - distance`.
pub fn vec_search(
    conn: &Connection,
    query: &[f32],
    k: usize,
) -> anyhow::Result<Vec<(String, f32)>> {
    let blob = to_blob(query);
    let mut stmt = conn.prepare(
        "SELECT tool_id, distance
         FROM tools_vec
         WHERE embedding MATCH ?1 AND k = ?2
         ORDER BY distance",
    )?;
    let rows = stmt
        .query_map(params![&blob, k as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)? as f32))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
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
    fn vec_search_returns_nearest() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("v.sqlite")).unwrap();
        ensure_parent_tool(&conn, "skill:a");
        ensure_parent_tool(&conn, "skill:b");
        // Two unit vectors in a 384-dim space. a aligned with axis 0; b with axis 1.
        let mut va = vec![0.0f32; 384];
        va[0] = 1.0;
        let mut vb = vec![0.0f32; 384];
        vb[1] = 1.0;
        upsert(&conn, "skill:a", &va).unwrap();
        upsert(&conn, "skill:b", &vb).unwrap();
        // Query close to a -> a should rank first.
        let hits = vec_search(&conn, &va, 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0, "skill:a");
        assert!(hits[0].1 < hits[1].1, "distance must be ascending");
    }

    #[test]
    fn upsert_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("e.sqlite")).unwrap();
        ensure_parent_tool(&conn, "skill:x");
        // tools_vec demands 384-dim vectors per migration 003.
        let v1 = vec![0.1f32; 384];
        let mut v2 = vec![0.0f32; 384];
        v2[0] = 1.0;
        upsert(&conn, "skill:x", &v1).unwrap();
        upsert(&conn, "skill:x", &v2).unwrap();
        let all = list_all(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].1, v2);
    }
}
