use std::collections::HashMap;

use toolhub_recommender::params::{
    COS_WEIGHT, FTS_CANDIDATES, FTS_WEIGHT, VEC_CANDIDATES, build_fts_query,
};
use toolhub_recommender::{embed::Embedder, search};
use toolhub_storage::{embeddings, fts, open, tools};

use crate::db_path::default_db_path;

pub async fn run(task: String) -> anyhow::Result<()> {
    let conn = open(&default_db_path()?)?;

    let embedder = Embedder::new()?;
    let q_emb = embedder.embed_one(&task)?;

    // sqlite-vec returns cosine *distance* in [0, 2]; convert to similarity.
    let vec_sims: HashMap<String, f32> = embeddings::vec_search(&conn, &q_emb, VEC_CANDIDATES)?
        .into_iter()
        .map(|(id, dist)| (id, 1.0 - dist))
        .collect();

    if vec_sims.is_empty() {
        println!("(empty index — run `toolhub sync` first)");
        return Ok(());
    }

    let fts_query = build_fts_query(&task);
    let fts_hits: HashMap<String, f32> = if fts_query.is_empty() {
        HashMap::new()
    } else {
        match fts::search(&conn, &fts_query, FTS_CANDIDATES) {
            Ok(rows) => rows.into_iter().collect(),
            Err(err) => {
                eprintln!("fts search failed: {err:#} (falling back to vec-only)");
                HashMap::new()
            }
        }
    };

    let hits = search::hybrid_from_score_maps(&vec_sims, &fts_hits, 3, COS_WEIGHT, FTS_WEIGHT);

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
