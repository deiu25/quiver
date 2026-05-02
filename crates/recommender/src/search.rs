use std::collections::HashMap;

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let dot: f32 = (0..n).map(|i| a[i] * b[i]).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub tool_id: String,
    pub score: f32,
}

pub fn top_k(query: &[f32], catalog: &[(String, Vec<f32>)], k: usize) -> Vec<Hit> {
    let mut hits: Vec<Hit> = catalog
        .iter()
        .map(|(id, v)| Hit {
            tool_id: id.clone(),
            score: cosine(query, v),
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(k);
    hits
}

/// Hybrid FTS5 BM25 + cosine combine, weighted (PLAN §7: 0.4 BM25 / 0.6 cosine).
///
/// `fts_hits` maps `tool_id` to its raw BM25 score (negative — closer to 0 = better).
/// Tools missing from the FTS hits get an FTS contribution of 0.
///
/// Both score lists are min-max normalized to [0, 1] before combining so the
/// disparate scales (cosine ≈ [0, 1], BM25 ≈ [-50, -2]) play nicely.
pub fn hybrid_top_k(
    query_emb: &[f32],
    catalog: &[(String, Vec<f32>)],
    fts_hits: &HashMap<String, f32>,
    k: usize,
    cos_w: f32,
    fts_w: f32,
) -> Vec<Hit> {
    if catalog.is_empty() {
        return Vec::new();
    }

    let cosines: Vec<f32> = catalog.iter().map(|(_, v)| cosine(query_emb, v)).collect();
    let (cmin, cmax) = min_max(&cosines);
    let cspan = (cmax - cmin).max(1e-6);

    // BM25 is negative; use -bm25 as similarity (bigger = better).
    let fts_sims: Vec<f32> = catalog
        .iter()
        .map(|(id, _)| fts_hits.get(id).map(|b| -b).unwrap_or(0.0))
        .collect();
    let present: Vec<f32> = catalog
        .iter()
        .filter_map(|(id, _)| fts_hits.get(id).map(|b| -b))
        .collect();
    let (fmin, fmax) = if present.is_empty() {
        (0.0, 1.0)
    } else {
        min_max(&present)
    };
    let fspan = (fmax - fmin).max(1e-6);

    let mut hits: Vec<Hit> = catalog
        .iter()
        .enumerate()
        .map(|(i, (id, _))| {
            let cos_n = (cosines[i] - cmin) / cspan;
            let fts_n = if fts_hits.contains_key(id) {
                (fts_sims[i] - fmin) / fspan
            } else {
                0.0
            };
            Hit {
                tool_id: id.clone(),
                score: cos_w * cos_n + fts_w * fts_n,
            }
        })
        .collect();

    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(k);
    hits
}

fn min_max(xs: &[f32]) -> (f32, f32) {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for x in xs {
        if *x < lo {
            lo = *x;
        }
        if *x > hi {
            hi = *x;
        }
    }
    (lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = [1.0, 0.0, 0.0];
        let b = [0.0, 1.0, 0.0];
        assert!(cosine(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_identical_is_one() {
        let a = [1.0, 2.0, 3.0];
        assert!((cosine(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_opposite_is_negative_one() {
        let a = [1.0, 0.0];
        let b = [-1.0, 0.0];
        assert!((cosine(&a, &b) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn top_k_orders_by_descending_score_and_truncates() {
        let q = vec![1.0, 0.0];
        let catalog = vec![
            ("c".into(), vec![0.5, 0.5]),
            ("a".into(), vec![1.0, 0.0]),
            ("b".into(), vec![0.0, 1.0]),
            ("d".into(), vec![0.9, 0.05]),
        ];
        let hits = top_k(&q, &catalog, 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].tool_id, "a");
        assert_eq!(hits[1].tool_id, "d");
    }

    #[test]
    fn hybrid_promotes_tool_present_in_both_signals() {
        // q strongly cosine-favours "a" but "b" gets perfect cosine *and* BM25.
        let q = vec![1.0, 0.0];
        let catalog = vec![
            ("a".into(), vec![1.0, 0.0]),
            ("b".into(), vec![0.95, 0.31]),
            ("c".into(), vec![0.0, 1.0]),
        ];
        // SQLite bm25() returns more-negative for better matches; hybrid_top_k
        // negates so the more-negative value normalizes to the highest similarity.
        let mut fts = HashMap::new();
        fts.insert("b".into(), -10.0); // strong match
        fts.insert("c".into(), -2.0); // weak match
        let hits = hybrid_top_k(&q, &catalog, &fts, 3, 0.6, 0.4);
        assert_eq!(hits.len(), 3);
        // "b" should top "a" because BM25 bonus tips the scales.
        assert_eq!(hits[0].tool_id, "b");
    }

    #[test]
    fn hybrid_falls_back_to_cosine_when_fts_empty() {
        let q = vec![1.0, 0.0];
        let catalog = vec![
            ("a".into(), vec![1.0, 0.0]),
            ("b".into(), vec![0.0, 1.0]),
        ];
        let fts = HashMap::new();
        let hits = hybrid_top_k(&q, &catalog, &fts, 2, 0.6, 0.4);
        assert_eq!(hits[0].tool_id, "a");
    }
}
