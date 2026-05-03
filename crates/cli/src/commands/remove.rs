//! `quiver remove <source-id>` — drop every tool ingested from one github
//! source, then drop the source row itself.

use anyhow::anyhow;

use quiver_storage::{fts, open, sources, tools};

use crate::db_path::default_db_path;

pub async fn run(source_id: String) -> anyhow::Result<()> {
    let db_path = default_db_path()?;
    let conn = open(&db_path)?;

    let src =
        sources::get(&conn, &source_id)?.ok_or_else(|| anyhow!("no such source: {source_id}"))?;

    let removed = tools::delete_by_source_repo(&conn, &src.location)?;
    if !removed.is_empty() {
        fts::rebuild(&conn)?;
    }
    let n = sources::delete(&conn, &source_id)?;
    println!(
        "{} → removed {} tool(s); source rows deleted: {n}.",
        source_id,
        removed.len()
    );
    Ok(())
}
