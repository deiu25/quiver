use quiver_storage::{open, tools};

use crate::db_path::default_db_path;

pub async fn run() -> anyhow::Result<()> {
    let db_path = default_db_path()?;
    let conn = open(&db_path)?;
    let metas = tools::list_all(&conn)?;

    println!("{:<32} {:<8} description", "id", "type");
    println!("{}", "-".repeat(96));
    if metas.is_empty() {
        println!("(empty — run `quiver sync` to populate)");
        return Ok(());
    }
    for m in metas {
        let t = format!("{:?}", m.r#type).to_lowercase();
        let desc_full = m.description.unwrap_or_default();
        let desc: String = desc_full.chars().take(80).collect();
        println!("{:<32} {:<8} {}", m.id, t, desc);
    }
    Ok(())
}
