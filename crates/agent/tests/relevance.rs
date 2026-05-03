//! Recommender relevance benchmark — PLAN §10 success criterion #2.
//!
//! Loads `benches/tasks.json` (50 synthetic tools + 50 paraphrased queries),
//! ingests them through the same `persist_tools` pipeline `quiver sync` uses,
//! then runs the shared `agent::recommend::top_k` pipeline for every task.
//! Acceptance: ≥80% top-3 hit rate.
//!
//! Cost: first run downloads BAAI/bge-small-en-v1.5 (~30 MB) into
//! `$XDG_CACHE_HOME/fastembed/`. CI caches that path; local runs reuse it.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use chrono::Utc;
use quiver_agent::recommend::top_k;
use quiver_core::tool::{ToolMeta, ToolType};
use quiver_ingestion::persist::persist_tools;
use quiver_recommender::embed::Embedder;
use quiver_storage::open;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Bench {
    tools: Vec<BenchTool>,
    tasks: Vec<BenchTask>,
}

#[derive(Debug, Deserialize)]
struct BenchTool {
    id: String,
    name: String,
    description: String,
}

#[derive(Debug, Deserialize)]
struct BenchTask {
    task: String,
    expected: String,
}

const HIT_RATE_THRESHOLD: f32 = 0.80;
const TOP_K: usize = 3;

fn benchmark_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../benches/tasks.json")
        .canonicalize()
        .expect("benches/tasks.json must exist at workspace root")
}

fn load_benchmark() -> Bench {
    let bytes = fs::read(benchmark_path()).expect("read benches/tasks.json");
    serde_json::from_slice(&bytes).expect("parse benches/tasks.json")
}

fn synth_meta(t: &BenchTool) -> ToolMeta {
    let now = Utc::now();
    ToolMeta {
        id: t.id.clone(),
        r#type: ToolType::Skill,
        name: t.name.clone(),
        source_repo: None,
        install_path: None,
        description: Some(t.description.clone()),
        long_description: Some(t.description.clone()),
        category: None,
        triggers: vec![],
        examples: vec![],
        invocation: Some(format!("/{}", t.name)),
        requires: vec![],
        enabled: true,
        added_at: now,
        last_seen_at: now,
        last_used_at: None,
    }
}

#[test]
fn recommender_top3_hit_rate_meets_80_percent() {
    let bench = load_benchmark();
    assert_eq!(
        bench.tools.len(),
        50,
        "benchmark must have 50 tools (current: {})",
        bench.tools.len()
    );
    assert_eq!(
        bench.tasks.len(),
        50,
        "benchmark must have 50 tasks (current: {})",
        bench.tasks.len()
    );

    let known_ids: HashSet<&str> = bench.tools.iter().map(|t| t.id.as_str()).collect();
    for task in &bench.tasks {
        assert!(
            known_ids.contains(task.expected.as_str()),
            "task '{}' expects unknown tool id '{}'",
            task.task,
            task.expected
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let conn = open(&dir.path().join("bench.sqlite")).unwrap();
    let embedder = Embedder::new().expect("init fastembed (BAAI/bge-small-en-v1.5)");

    let metas: Vec<ToolMeta> = bench.tools.iter().map(synth_meta).collect();
    let count = persist_tools(&conn, &embedder, &metas).unwrap();
    assert_eq!(count, 50);

    let mut hits = 0usize;
    let mut misses: Vec<String> = Vec::new();
    for task in &bench.tasks {
        let results = top_k(&conn, &embedder, &task.task, TOP_K).unwrap();
        let in_top_k = results.iter().any(|h| h.tool_id == task.expected);
        if in_top_k {
            hits += 1;
        } else {
            let got: Vec<String> = results.iter().map(|h| h.tool_id.clone()).collect();
            misses.push(format!(
                "  - task: {:?}\n    expected: {}\n    got: {:?}",
                task.task, task.expected, got
            ));
        }
    }

    let total = bench.tasks.len();
    let rate = hits as f32 / total as f32;
    println!("\nrelevance: {hits}/{total} = {:.1}%", rate * 100.0);
    if !misses.is_empty() {
        println!("misses ({}):\n{}", misses.len(), misses.join("\n"));
    }

    assert!(
        rate >= HIT_RATE_THRESHOLD,
        "top-{TOP_K} hit rate {:.1}% < {:.0}% gate (hits={hits}/{total})",
        rate * 100.0,
        HIT_RATE_THRESHOLD * 100.0
    );
}
