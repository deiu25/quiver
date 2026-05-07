//! Score-aware reranking, Phase 4 PLAN §8.3.
//!
//! After the hybrid (cosine + BM25) combine produces candidate `Hit`s, the
//! reranker boosts tools with proven track records. Tools without enough
//! samples or without a `tool_scores` row are passed through unchanged, so
//! Phase 1–3 behaviour is preserved when telemetry is empty.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use crate::project::{Language, skill_language};
use crate::search::Hit;
use quiver_core::tool::ToolMeta;
use quiver_storage::tools;

/// Default boost factor — chosen so a tool with 100 % success rate gets a
/// 1.3× score multiplier. PLAN §10 #5: track recommender accuracy on the
/// benchmark set and tune.
pub const SUCCESS_ALPHA: f32 = 0.3;

/// Minimum sample size before we trust a tool's success_rate enough to apply
/// the boost. Below this, the score is statistically noisy.
pub const MIN_SAMPLE_SIZE: i64 = 5;

pub trait Reranker {
    fn apply(&self, hits: &mut [Hit], conn: &Connection) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, Copy)]
pub struct SuccessReranker {
    pub alpha: f32,
    pub min_samples: i64,
}

impl Default for SuccessReranker {
    fn default() -> Self {
        Self {
            alpha: SUCCESS_ALPHA,
            min_samples: MIN_SAMPLE_SIZE,
        }
    }
}

impl Reranker for SuccessReranker {
    fn apply(&self, hits: &mut [Hit], conn: &Connection) -> anyhow::Result<()> {
        if hits.is_empty() {
            return Ok(());
        }
        let scores = load_scores(conn, hits)?;
        for hit in hits.iter_mut() {
            if let Some((rate, samples)) = scores.get(&hit.tool_id)
                && *samples >= self.min_samples
            {
                hit.score *= 1.0 + self.alpha * (*rate as f32);
            }
        }
        // Re-sort — ordering may have changed.
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(())
    }
}

/// Default flat penalty applied to a hit whose `skill_language` is foreign
/// to the project. 0.30 is calibrated against the score-band ladder: it
/// pushes a perfect 1.0 to 0.7 (Strong → not Mandatory), a 0.85 to 0.55
/// (Hint), and a 0.70 to 0.40 (edge of Silent). Big enough to neutralise
/// Mandatory, small enough to keep the result in the list as a hint.
pub const LANGUAGE_PENALTY: f32 = 0.30;

/// Reranker that demotes language-tagged skills which don't match any of
/// the project's detected languages. Language-agnostic skills (`None` from
/// [`skill_language`]) and skills matching at least one project language
/// pass through unchanged. Empty `project_langs` means "language unknown"
/// and the reranker becomes a no-op.
#[derive(Debug, Clone)]
pub struct LanguageReranker {
    pub project_langs: HashSet<Language>,
    pub penalty: f32,
}

impl LanguageReranker {
    pub fn new(project_langs: HashSet<Language>, penalty: f32) -> Self {
        Self {
            project_langs,
            penalty,
        }
    }

    /// Read penalty override from `QUIVER_LANG_PENALTY` (default 0.30).
    /// Negative values clamp to 0 (no-op), values above 1.0 also clamp to 1.0
    /// — anything bigger than 1 would zero every foreign skill which is
    /// rarely what an operator wants.
    pub fn penalty_from_env() -> f32 {
        let v = std::env::var("QUIVER_LANG_PENALTY")
            .ok()
            .and_then(|s| s.trim().parse::<f32>().ok())
            .unwrap_or(LANGUAGE_PENALTY);
        v.clamp(0.0, 1.0)
    }
}

impl Reranker for LanguageReranker {
    fn apply(&self, hits: &mut [Hit], conn: &Connection) -> anyhow::Result<()> {
        if hits.is_empty() || self.project_langs.is_empty() || self.penalty <= 0.0 {
            return Ok(());
        }
        let metas = load_metas(conn, hits)?;
        for hit in hits.iter_mut() {
            let Some(meta) = metas.get(&hit.tool_id) else {
                continue;
            };
            let Some(lang) = skill_language(meta) else {
                continue;
            };
            if self.project_langs.contains(&lang) {
                continue;
            }
            hit.score = (hit.score - self.penalty).max(0.0);
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Phase 9 — auto-tuner DemeritReranker
// ---------------------------------------------------------------------------

/// Default haircut applied for the global per-tool demerit. The factor used
/// at runtime is `alpha_global * count / (count + K_sat)`, so the haircut
/// asymptotes to `alpha_global` as the tool accumulates negative feedback.
pub const DEMERIT_ALPHA_GLOBAL: f32 = 0.20;
/// Per-task haircut applied multiplicatively on top of the global one when
/// the current task's tokens overlap a stored demerit signature.
pub const DEMERIT_ALPHA_TASK: f32 = 0.40;
/// Saturation constant — `K=3` means three demerits ≈ 50 % saturation.
pub const DEMERIT_K_SAT: f32 = 3.0;
/// Minimum Jaccard overlap before the per-task penalty fires.
pub const DEMERIT_JACCARD_MIN: f32 = 0.4;

/// Token set for Jaccard similarity. Strips a leading `<tool>:` prefix when
/// present (so `Bash:cargo build` and the user's prompt `cargo build the
/// release` overlap on `{cargo, build}` regardless of the wrapping tool).
pub fn tokenize_signature(s: &str) -> std::collections::HashSet<String> {
    let salient = match s.split_once(':') {
        Some((_tool, rest)) if !rest.is_empty() => rest,
        _ => s,
    };
    salient
        .split(|c: char| !c.is_alphanumeric())
        .filter_map(|tok| {
            let lc = tok.to_ascii_lowercase();
            if lc.chars().count() >= 2 {
                Some(lc)
            } else {
                None
            }
        })
        .collect()
}

fn jaccard(a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    let union = a.len() + b.len() - inter;
    if union == 0 {
        0.0
    } else {
        inter as f32 / union as f32
    }
}

/// Reranker that demotes tools accumulating `false_positive` / `bypassed`
/// signals in `agent_suggestions`. Combines a saturating per-tool haircut
/// with an optional per-task Jaccard penalty when the current prompt's
/// tokens overlap any stored demerit signature for that tool.
///
/// Disabled entirely when `QUIVER_DEMERIT_DISABLED=1`.
#[derive(Debug, Clone)]
pub struct DemeritReranker {
    pub task_text: String,
    pub alpha_global: f32,
    pub alpha_task: f32,
    pub k_sat: f32,
    pub jaccard_min: f32,
    pub disabled: bool,
}

impl DemeritReranker {
    /// Construct with all knobs read from env at construction time. Reading
    /// once avoids races where concurrent tests mutate env between the
    /// disabled check and the apply call.
    pub fn new(task_text: impl Into<String>) -> Self {
        Self {
            task_text: task_text.into(),
            alpha_global: env_f32("QUIVER_DEMERIT_ALPHA", DEMERIT_ALPHA_GLOBAL),
            alpha_task: env_f32("QUIVER_DEMERIT_TASK_ALPHA", DEMERIT_ALPHA_TASK),
            k_sat: env_f32("QUIVER_DEMERIT_K", DEMERIT_K_SAT).max(0.001),
            jaccard_min: env_f32("QUIVER_DEMERIT_JACCARD_MIN", DEMERIT_JACCARD_MIN),
            disabled: read_demerit_disabled(),
        }
    }

    /// Force the reranker on/off regardless of env. Mainly for tests.
    pub fn with_disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

fn read_demerit_disabled() -> bool {
    std::env::var("QUIVER_DEMERIT_DISABLED")
        .ok()
        .map(|s| {
            let t = s.trim();
            t == "1" || t.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

fn env_f32(key: &str, default: f32) -> f32 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .map(|v| v.clamp(0.0, 1.0))
        .unwrap_or(default)
}

impl Reranker for DemeritReranker {
    fn apply(&self, hits: &mut [Hit], conn: &Connection) -> anyhow::Result<()> {
        if hits.is_empty() || self.disabled {
            return Ok(());
        }
        let tool_ids: Vec<&str> = hits.iter().map(|h| h.tool_id.as_str()).collect();
        let demerits = quiver_storage::scores::demerits_for(conn, &tool_ids)?;
        if demerits.is_empty() {
            return Ok(());
        }
        let task_tokens = tokenize_signature(&self.task_text);
        for hit in hits.iter_mut() {
            let Some((count, sigs)) = demerits.get(&hit.tool_id) else {
                continue;
            };
            // Global saturating haircut.
            let count_f = *count as f32;
            let global_haircut = self.alpha_global * count_f / (count_f + self.k_sat);
            hit.score *= 1.0 - global_haircut;

            // Per-task Jaccard penalty: max overlap across stored sigs.
            if !task_tokens.is_empty() && self.alpha_task > 0.0 {
                let mut max_j: f32 = 0.0;
                for (sig, _w) in sigs {
                    let sig_toks = tokenize_signature(sig);
                    let j = jaccard(&task_tokens, &sig_toks);
                    if j > max_j {
                        max_j = j;
                    }
                }
                if max_j >= self.jaccard_min {
                    hit.score *= 1.0 - self.alpha_task * max_j;
                }
            }
            if hit.score < 0.0 {
                hit.score = 0.0;
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(())
    }
}

fn load_metas(conn: &Connection, hits: &[Hit]) -> anyhow::Result<HashMap<String, ToolMeta>> {
    let mut out = HashMap::with_capacity(hits.len());
    for h in hits {
        if let Some(m) = tools::get(conn, &h.tool_id)? {
            out.insert(h.tool_id.clone(), m);
        }
    }
    Ok(out)
}

fn load_scores(conn: &Connection, hits: &[Hit]) -> anyhow::Result<HashMap<String, (f64, i64)>> {
    let ids: Vec<&str> = hits.iter().map(|h| h.tool_id.as_str()).collect();
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT tool_id, success_rate, sample_size
         FROM tool_scores
         WHERE tool_id IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::ToSql> = ids.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    let rows = stmt
        .query_map(params.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<f64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = HashMap::with_capacity(rows.len());
    for (id, rate, n) in rows {
        if let (Some(r), Some(n)) = (rate, n) {
            out.insert(id, (r, n));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn open_with_schema() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tool_scores (
                tool_id TEXT PRIMARY KEY,
                success_rate REAL,
                sample_size INTEGER,
                avg_cost_usd REAL,
                median_duration_ms INTEGER,
                score_updated_at TEXT
            );",
        )
        .unwrap();
        conn
    }

    fn seed(conn: &Connection, id: &str, rate: f64, n: i64) {
        conn.execute(
            "INSERT INTO tool_scores VALUES (?, ?, ?, NULL, NULL, '2026-05-03T00:00:00Z')",
            params![id, rate, n],
        )
        .unwrap();
    }

    #[test]
    fn alpha_zero_is_identity() {
        let conn = open_with_schema();
        seed(&conn, "skill:a", 1.0, 100);
        let mut hits = vec![Hit {
            tool_id: "skill:a".into(),
            score: 0.5,
        }];
        let rer = SuccessReranker {
            alpha: 0.0,
            min_samples: 5,
        };
        rer.apply(&mut hits, &conn).unwrap();
        assert!((hits[0].score - 0.5).abs() < 1e-6);
    }

    #[test]
    fn boost_only_when_min_samples_met() {
        let conn = open_with_schema();
        seed(&conn, "skill:trusted", 1.0, 10);
        seed(&conn, "skill:noisy", 1.0, 2);
        let mut hits = vec![
            Hit {
                tool_id: "skill:trusted".into(),
                score: 0.5,
            },
            Hit {
                tool_id: "skill:noisy".into(),
                score: 0.5,
            },
        ];
        let rer = SuccessReranker::default();
        rer.apply(&mut hits, &conn).unwrap();
        let trusted = hits.iter().find(|h| h.tool_id == "skill:trusted").unwrap();
        let noisy = hits.iter().find(|h| h.tool_id == "skill:noisy").unwrap();
        assert!(trusted.score > noisy.score);
        assert!((trusted.score - 0.5 * (1.0 + 0.3)).abs() < 1e-6);
        assert!((noisy.score - 0.5).abs() < 1e-6);
    }

    #[test]
    fn missing_score_passes_through() {
        let conn = open_with_schema();
        let mut hits = vec![Hit {
            tool_id: "skill:no-data".into(),
            score: 0.7,
        }];
        let rer = SuccessReranker::default();
        rer.apply(&mut hits, &conn).unwrap();
        assert!((hits[0].score - 0.7).abs() < 1e-6);
    }

    #[test]
    fn boost_can_change_ordering() {
        let conn = open_with_schema();
        seed(&conn, "skill:underdog", 1.0, 50);
        let mut hits = vec![
            Hit {
                tool_id: "skill:leader".into(),
                score: 0.6,
            },
            Hit {
                tool_id: "skill:underdog".into(),
                score: 0.5,
            },
        ];
        let rer = SuccessReranker::default();
        rer.apply(&mut hits, &conn).unwrap();
        // underdog now 0.5 * 1.3 = 0.65 → wins.
        assert_eq!(hits[0].tool_id, "skill:underdog");
    }

    #[test]
    fn empty_hits_is_noop() {
        let conn = open_with_schema();
        let mut hits: Vec<Hit> = Vec::new();
        SuccessReranker::default().apply(&mut hits, &conn).unwrap();
        assert!(hits.is_empty());
    }
}

#[cfg(test)]
mod language_reranker_tests {
    use super::*;
    use chrono::Utc;
    use quiver_core::tool::{ToolMeta, ToolType};
    use quiver_storage::open;

    fn open_with_tools_and_seed(metas: &[ToolMeta]) -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.sqlite");
        let conn = open(&path).unwrap();
        // Keep tempdir alive by leaking — only inside this helper for tests.
        std::mem::forget(dir);
        for m in metas {
            quiver_storage::tools::upsert(&conn, m).unwrap();
        }
        conn
    }

    fn make_meta(id: &str, name: &str, category: Option<&str>, triggers: &[&str]) -> ToolMeta {
        ToolMeta {
            id: id.to_string(),
            r#type: ToolType::Skill,
            name: name.to_string(),
            source_repo: None,
            install_path: None,
            description: None,
            long_description: None,
            category: category.map(str::to_string),
            triggers: triggers.iter().map(|s| s.to_string()).collect(),
            examples: vec![],
            invocation: None,
            requires: vec![],
            enabled: true,
            added_at: Utc::now(),
            last_seen_at: Utc::now(),
            last_used_at: None,
        }
    }

    #[test]
    fn demotes_golang_skill_in_rust_project() {
        let go_skill = make_meta("skill:golang-patterns", "golang-patterns", None, &[]);
        let rust_skill = make_meta("skill:rust-patterns", "rust-patterns", None, &[]);
        let conn = open_with_tools_and_seed(&[go_skill, rust_skill]);

        let mut hits = vec![
            Hit {
                tool_id: "skill:golang-patterns".into(),
                score: 0.85,
            },
            Hit {
                tool_id: "skill:rust-patterns".into(),
                score: 0.70,
            },
        ];
        let mut langs = HashSet::new();
        langs.insert(Language::Rust);
        let rer = LanguageReranker::new(langs, LANGUAGE_PENALTY);
        rer.apply(&mut hits, &conn).unwrap();

        // golang demoted to 0.55, rust unchanged at 0.70 → rust now leads.
        assert_eq!(hits[0].tool_id, "skill:rust-patterns");
        assert!((hits[0].score - 0.70).abs() < 1e-6);
        let go = hits
            .iter()
            .find(|h| h.tool_id == "skill:golang-patterns")
            .unwrap();
        assert!((go.score - 0.55).abs() < 1e-6);
    }

    #[test]
    fn no_op_when_project_langs_empty() {
        let go_skill = make_meta("skill:golang-patterns", "golang-patterns", None, &[]);
        let conn = open_with_tools_and_seed(&[go_skill]);

        let mut hits = vec![Hit {
            tool_id: "skill:golang-patterns".into(),
            score: 0.85,
        }];
        let rer = LanguageReranker::new(HashSet::new(), LANGUAGE_PENALTY);
        rer.apply(&mut hits, &conn).unwrap();
        assert!((hits[0].score - 0.85).abs() < 1e-6);
    }

    #[test]
    fn no_op_for_language_agnostic_skill() {
        let agnostic = make_meta("skill:git-workflow", "git-workflow", None, &[]);
        let conn = open_with_tools_and_seed(&[agnostic]);

        let mut hits = vec![Hit {
            tool_id: "skill:git-workflow".into(),
            score: 0.90,
        }];
        let mut langs = HashSet::new();
        langs.insert(Language::Rust);
        let rer = LanguageReranker::new(langs, LANGUAGE_PENALTY);
        rer.apply(&mut hits, &conn).unwrap();
        assert!((hits[0].score - 0.90).abs() < 1e-6);
    }

    #[test]
    fn matching_language_passes_through() {
        let rust_skill = make_meta("skill:rust-reviewer", "rust-reviewer", None, &[]);
        let conn = open_with_tools_and_seed(&[rust_skill]);

        let mut hits = vec![Hit {
            tool_id: "skill:rust-reviewer".into(),
            score: 0.92,
        }];
        let mut langs = HashSet::new();
        langs.insert(Language::Rust);
        let rer = LanguageReranker::new(langs, LANGUAGE_PENALTY);
        rer.apply(&mut hits, &conn).unwrap();
        assert!((hits[0].score - 0.92).abs() < 1e-6);
    }

    #[test]
    fn clamps_score_at_zero() {
        let go_skill = make_meta("skill:golang-tiny", "golang-tiny", None, &[]);
        let conn = open_with_tools_and_seed(&[go_skill]);

        let mut hits = vec![Hit {
            tool_id: "skill:golang-tiny".into(),
            score: 0.10,
        }];
        let mut langs = HashSet::new();
        langs.insert(Language::Rust);
        let rer = LanguageReranker::new(langs, LANGUAGE_PENALTY);
        rer.apply(&mut hits, &conn).unwrap();
        assert_eq!(hits[0].score, 0.0);
    }

    #[test]
    fn polyglot_repo_keeps_both_languages() {
        let go_skill = make_meta("skill:golang-patterns", "golang-patterns", None, &[]);
        let rust_skill = make_meta("skill:rust-patterns", "rust-patterns", None, &[]);
        let conn = open_with_tools_and_seed(&[go_skill, rust_skill]);

        let mut hits = vec![
            Hit {
                tool_id: "skill:golang-patterns".into(),
                score: 0.80,
            },
            Hit {
                tool_id: "skill:rust-patterns".into(),
                score: 0.80,
            },
        ];
        let mut langs = HashSet::new();
        langs.insert(Language::Rust);
        langs.insert(Language::Go);
        let rer = LanguageReranker::new(langs, LANGUAGE_PENALTY);
        rer.apply(&mut hits, &conn).unwrap();

        for h in &hits {
            assert!((h.score - 0.80).abs() < 1e-6);
        }
    }

    #[test]
    fn penalty_zero_disables_reranker() {
        let go_skill = make_meta("skill:golang-patterns", "golang-patterns", None, &[]);
        let conn = open_with_tools_and_seed(&[go_skill]);

        let mut hits = vec![Hit {
            tool_id: "skill:golang-patterns".into(),
            score: 0.85,
        }];
        let mut langs = HashSet::new();
        langs.insert(Language::Rust);
        let rer = LanguageReranker::new(langs, 0.0);
        rer.apply(&mut hits, &conn).unwrap();
        assert!((hits[0].score - 0.85).abs() < 1e-6);
    }

    #[test]
    fn penalty_from_env_clamps() {
        let key = "QUIVER_LANG_PENALTY";
        let prev = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, "-5.0");
        }
        assert_eq!(LanguageReranker::penalty_from_env(), 0.0);
        unsafe {
            std::env::set_var(key, "10.0");
        }
        assert_eq!(LanguageReranker::penalty_from_env(), 1.0);
        unsafe {
            std::env::set_var(key, "0.5");
        }
        assert!((LanguageReranker::penalty_from_env() - 0.5).abs() < 1e-6);
        unsafe {
            match prev {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[cfg(test)]
mod demerit_tests {
    use super::*;
    use chrono::Utc;
    use quiver_core::tool::{ToolMeta, ToolType};
    use quiver_storage::open;
    use rusqlite::params;

    fn open_with_seed(tool_ids: &[&str]) -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.sqlite");
        let conn = open(&path).unwrap();
        std::mem::forget(dir);
        let now = Utc::now();
        for id in tool_ids {
            let meta = ToolMeta {
                id: id.to_string(),
                r#type: ToolType::Skill,
                name: id.to_string(),
                source_repo: None,
                install_path: None,
                description: None,
                long_description: None,
                category: None,
                triggers: vec![],
                examples: vec![],
                invocation: None,
                requires: vec![],
                enabled: true,
                added_at: now,
                last_seen_at: now,
                last_used_at: None,
            };
            quiver_storage::tools::upsert(&conn, &meta).unwrap();
        }
        conn
    }

    fn seed_demerit(conn: &Connection, tool_id: &str, count: f64, sigs: &[(&str, f64)]) {
        let json = if sigs.is_empty() {
            None
        } else {
            let payload: Vec<_> = sigs
                .iter()
                .map(|(s, w)| serde_json::json!({"sig": s, "weight": w}))
                .collect();
            Some(serde_json::to_string(&payload).unwrap())
        };
        conn.execute(
            "INSERT OR REPLACE INTO tool_scores
                (tool_id, success_rate, sample_size, avg_cost_usd,
                 median_duration_ms, score_updated_at,
                 demerit_count, demerit_updated_at, demerit_signatures_json)
             VALUES (?, NULL, NULL, NULL, NULL, '2026-05-07T00:00:00Z',
                     ?, '2026-05-07T00:00:00Z', ?)",
            params![tool_id, count, json],
        )
        .unwrap();
    }

    fn clear_demerit_env() {
        for k in [
            "QUIVER_DEMERIT_DISABLED",
            "QUIVER_DEMERIT_ALPHA",
            "QUIVER_DEMERIT_TASK_ALPHA",
            "QUIVER_DEMERIT_K",
            "QUIVER_DEMERIT_JACCARD_MIN",
        ] {
            unsafe {
                std::env::remove_var(k);
            }
        }
    }

    #[test]
    fn tokenize_signature_strips_tool_prefix() {
        let toks = tokenize_signature("Bash:cargo build --release");
        assert!(toks.contains("cargo"));
        assert!(toks.contains("build"));
        assert!(toks.contains("release"));
        assert!(!toks.contains("bash"));
        // Single-char and empty tokens dropped.
        assert!(toks.iter().all(|t| t.chars().count() >= 2));
    }

    #[test]
    fn tokenize_signature_handles_no_prefix() {
        let toks = tokenize_signature("plain query without colon");
        assert!(toks.contains("plain"));
        assert!(toks.contains("query"));
        assert!(toks.contains("colon"));
    }

    #[test]
    fn demerit_reranker_no_haircut_when_zero() {
        clear_demerit_env();
        let conn = open_with_seed(&["skill:clean"]);
        let mut hits = vec![Hit {
            tool_id: "skill:clean".into(),
            score: 0.7,
        }];
        DemeritReranker::new("anything")
            .apply(&mut hits, &conn)
            .unwrap();
        assert!((hits[0].score - 0.7).abs() < 1e-6);
    }

    #[test]
    fn demerit_reranker_global_haircut_saturates() {
        clear_demerit_env();
        let conn = open_with_seed(&["skill:badly"]);
        // count=100 → fraction 100/103 ≈ 0.97 → haircut ≈ 0.20*0.97 ≈ 0.194
        seed_demerit(&conn, "skill:badly", 100.0, &[]);
        let mut hits = vec![Hit {
            tool_id: "skill:badly".into(),
            score: 1.0,
        }];
        DemeritReranker::new("unrelated task")
            .apply(&mut hits, &conn)
            .unwrap();
        // Score multiplied by ~0.806 — clearly less than 1.0 but more than 0.79
        assert!(hits[0].score < 1.0 - 0.18);
        assert!(hits[0].score > 1.0 - 0.21);
    }

    #[test]
    fn demerit_reranker_per_task_match_lowers_score() {
        clear_demerit_env();
        let conn = open_with_seed(&["skill:fp"]);
        seed_demerit(&conn, "skill:fp", 1.0, &[("Bash:cargo build release", 1.0)]);
        let mut hits = vec![Hit {
            tool_id: "skill:fp".into(),
            score: 1.0,
        }];
        // Task signature identical → Jaccard = 1.0
        DemeritReranker::new("Bash:cargo build release")
            .apply(&mut hits, &conn)
            .unwrap();
        // Global haircut: 0.20 * 1/(1+3) = 0.05  → factor 0.95
        // Per-task: 0.40 * 1.0                  → factor 0.60
        // Combined: 1.0 * 0.95 * 0.60 = 0.57
        assert!(hits[0].score < 0.60, "got {}", hits[0].score);
        assert!(hits[0].score > 0.55, "got {}", hits[0].score);
    }

    #[test]
    fn demerit_reranker_per_task_no_match_passes_through() {
        clear_demerit_env();
        let conn = open_with_seed(&["skill:fp"]);
        seed_demerit(&conn, "skill:fp", 1.0, &[("Bash:cargo build", 1.0)]);
        let mut hits = vec![Hit {
            tool_id: "skill:fp".into(),
            score: 1.0,
        }];
        DemeritReranker::new("write a poem about butterflies")
            .apply(&mut hits, &conn)
            .unwrap();
        // Only global haircut applies (no Jaccard match).
        // 0.20 * 1/(1+3) = 0.05 → factor 0.95
        assert!((hits[0].score - 0.95).abs() < 0.01, "got {}", hits[0].score);
    }

    #[test]
    fn demerit_reranker_disabled_passes_through() {
        // Constructed with disabled=true → no haircut even if demerits exist.
        // Avoids env-mutation races between parallel tests.
        let conn = open_with_seed(&["skill:fp"]);
        seed_demerit(&conn, "skill:fp", 50.0, &[("Bash:cargo build", 1.0)]);
        let mut hits = vec![Hit {
            tool_id: "skill:fp".into(),
            score: 0.9,
        }];
        DemeritReranker::new("cargo build")
            .with_disabled(true)
            .apply(&mut hits, &conn)
            .unwrap();
        assert!((hits[0].score - 0.9).abs() < 1e-6);
    }

    #[test]
    fn demerit_reranker_changes_ordering() {
        clear_demerit_env();
        let conn = open_with_seed(&["skill:clean", "skill:dirty"]);
        seed_demerit(&conn, "skill:dirty", 5.0, &[("Bash:cargo test", 1.0)]);
        let mut hits = vec![
            Hit {
                tool_id: "skill:dirty".into(),
                score: 0.80,
            },
            Hit {
                tool_id: "skill:clean".into(),
                score: 0.78,
            },
        ];
        DemeritReranker::new("cargo test workspace")
            .apply(&mut hits, &conn)
            .unwrap();
        // Clean tool should now lead.
        assert_eq!(hits[0].tool_id, "skill:clean");
    }

    #[test]
    fn pipeline_order_demerit_after_success_boost() {
        // Even with a perfect success rate, demerit haircut must still bite.
        clear_demerit_env();
        let conn = open_with_seed(&["skill:bad"]);
        // Seed both success boost and demerits in one row.
        conn.execute(
            "INSERT OR REPLACE INTO tool_scores
                (tool_id, success_rate, sample_size, avg_cost_usd,
                 median_duration_ms, score_updated_at,
                 demerit_count, demerit_updated_at, demerit_signatures_json)
             VALUES ('skill:bad', 1.0, 50, NULL, NULL, '2026-05-07T00:00:00Z',
                     20.0, '2026-05-07T00:00:00Z',
                     '[{\"sig\":\"Bash:lint\",\"weight\":1.0}]')",
            [],
        )
        .unwrap();
        let mut hits = vec![Hit {
            tool_id: "skill:bad".into(),
            score: 0.5,
        }];
        SuccessReranker::default().apply(&mut hits, &conn).unwrap();
        let after_boost = hits[0].score;
        // Boost: 0.5 * 1.3 = 0.65
        assert!((after_boost - 0.65).abs() < 1e-3);
        DemeritReranker::new("Bash:lint")
            .apply(&mut hits, &conn)
            .unwrap();
        // After haircut score must be strictly less than the boosted score.
        assert!(
            hits[0].score < after_boost - 0.05,
            "boost={after_boost}, after_demerit={}",
            hits[0].score
        );
    }
}
