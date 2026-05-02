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
}
