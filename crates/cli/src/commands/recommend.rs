use std::collections::HashMap;

use toolhub_recommender::{embed::Embedder, search};
use toolhub_storage::{embeddings, fts, open, tools};

use crate::db_path::default_db_path;

const FTS_CANDIDATES: usize = 50;
const COS_WEIGHT: f32 = 0.6;
const FTS_WEIGHT: f32 = 0.4;

pub async fn run(task: String) -> anyhow::Result<()> {
    let conn = open(&default_db_path()?)?;
    let catalog = embeddings::list_all(&conn)?;
    if catalog.is_empty() {
        println!("(empty index — run `toolhub sync` first)");
        return Ok(());
    }

    let embedder = Embedder::new()?;
    let q_emb = embedder.embed_one(&task)?;

    let fts_query = build_fts_query(&task);
    let fts_hits: HashMap<String, f32> = if fts_query.is_empty() {
        HashMap::new()
    } else {
        match fts::search(&conn, &fts_query, FTS_CANDIDATES) {
            Ok(rows) => rows.into_iter().collect(),
            Err(err) => {
                eprintln!("fts search failed: {err:#} (falling back to cosine-only)");
                HashMap::new()
            }
        }
    };

    let hits =
        search::hybrid_top_k(&q_emb, &catalog, &fts_hits, 3, COS_WEIGHT, FTS_WEIGHT);

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

/// Tokenise on whitespace, double-quote each token (escaping internal quotes),
/// and OR-join. OR is preferred over implicit AND so a multi-word query still
/// returns hits when only some words match.
fn build_fts_query(task: &str) -> String {
    let toks: Vec<String> = task
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| {
            let cleaned = t.replace('"', "");
            format!("\"{cleaned}\"")
        })
        .collect();
    toks.join(" OR ")
}
