use std::collections::HashMap;

use toolhub_recommender::{embed::Embedder, search};
use toolhub_storage::{embeddings, open, tools};

use crate::db_path::default_db_path;

pub async fn run(task: String) -> anyhow::Result<()> {
    let conn = open(&default_db_path()?)?;
    let catalog = embeddings::list_all(&conn)?;
    if catalog.is_empty() {
        println!("(empty index — run `toolhub sync` first)");
        return Ok(());
    }

    let embedder = Embedder::new()?;
    let q = embedder.embed_one(&task)?;
    let hits = search::top_k(&q, &catalog, 3);

    let by_id: HashMap<String, _> = tools::list_all(&conn)?
        .into_iter()
        .map(|m| (m.id.clone(), m))
        .collect();

    println!("{:>6}  {:<40}  description", "score", "id");
    println!("{}", "-".repeat(96));
    for h in hits {
        let desc = by_id
            .get(&h.tool_id)
            .and_then(|m| m.description.as_deref())
            .unwrap_or("");
        let desc: String = desc.chars().take(60).collect();
        println!("{:>6.3}  {:<40}  {}", h.score, h.tool_id, desc);
    }
    Ok(())
}
