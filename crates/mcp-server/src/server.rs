//! Quiver MCP server (stdio transport, rmcp `tool_router` macro).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, anyhow};
use chrono::Utc;
use rmcp::ErrorData;
use rmcp::ServiceExt;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::transport::stdio;
use rmcp::{tool, tool_router};
use rusqlite::Connection;

use quiver_ingestion::github_repo;
use quiver_ingestion::llm_extract;
use quiver_recommender::embed::Embedder;
use quiver_recommender::params::{
    COS_WEIGHT, FTS_CANDIDATES, FTS_WEIGHT, VEC_CANDIDATES, build_fts_query,
};
use quiver_recommender::rerank::{DemeritReranker, Reranker};
use quiver_recommender::search;
use quiver_storage::{embeddings, fts, open, scores, sources, tools, usage};

use crate::schema::{
    AddSourceParams, AddSourceResult, InfoParams, RecommendHit, RecommendParams, SearchHit,
    SearchParams, ShouldInvokeParams, ShouldInvokeResult, ToolInfo, UsageEventBrief,
    UsageStatsParams, UsageStatsResult, UsageStatsRow,
};

/// 2 KB upper bound on free-text task input fed to the embedder.
const TASK_INPUT_LIMIT: usize = 2048;

/// Resolve the default Quiver SQLite path: `$XDG_DATA_HOME/quiver/quiver.sqlite`,
/// falling back to `$HOME/.local/share/quiver/quiver.sqlite`.
pub fn default_db_path() -> anyhow::Result<PathBuf> {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".local/share"))
        })
        .ok_or_else(|| anyhow!("cannot resolve XDG_DATA_HOME or HOME"))?;
    Ok(base.join("quiver").join("quiver.sqlite"))
}

/// Shared server state. Wrapped in `Arc<…>` inside `QuiverServer` so each
/// rmcp dispatch only clones the wrapper. The embedder is lazy: tools that
/// don't need it (search/info/add_source/usage_stats) never pay the
/// fastembed cold-init cost.
pub struct ServerState {
    pub conn: Mutex<Connection>,
    pub embedder: Mutex<Option<Embedder>>,
}

#[derive(Clone)]
pub struct QuiverServer {
    state: std::sync::Arc<ServerState>,
}

impl QuiverServer {
    /// Build a server backed by the user's default DB. Embedder loads lazily
    /// on the first `recommend` call.
    pub fn new() -> anyhow::Result<Self> {
        let path = default_db_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = open(&path).with_context(|| format!("open db at {}", path.display()))?;
        Ok(Self::from_conn(conn))
    }

    /// Test/integration entry: build from an already-open connection. Embedder
    /// is created lazily on first `recommend` call.
    pub fn from_conn(conn: Connection) -> Self {
        Self {
            state: std::sync::Arc::new(ServerState {
                conn: Mutex::new(conn),
                embedder: Mutex::new(None),
            }),
        }
    }

    /// Test/integration entry: build with a pre-initialised embedder (avoids
    /// triggering fastembed model download in tests that DO want recommend).
    pub fn from_parts(conn: Connection, embedder: Embedder) -> Self {
        Self {
            state: std::sync::Arc::new(ServerState {
                conn: Mutex::new(conn),
                embedder: Mutex::new(Some(embedder)),
            }),
        }
    }
}

fn err(e: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(format!("{e:#}"), None)
}

#[tool_router(server_handler)]
impl QuiverServer {
    #[tool(
        description = "Authoritative tool router for the user's installed catalog. \
                       Call before any non-trivial subtask (refactor, scaffold, debug, \
                       search, doc lookup) to discover the right local skill / plugin / \
                       MCP server. Returns top-k hits ordered by hybrid vec+FTS score \
                       (0.0 to ~1.3 with success-rate boost). Score interpretation: \
                       >= 0.75 = mandatory (project policy requires invoking it unless \
                       the user explicitly chose another tool), 0.60-0.74 = strong \
                       preference, 0.40-0.59 = hint, < 0.40 = noise. Default k=3. \
                       Use `info` to fetch the full skill body when an excerpt is \
                       truncated."
    )]
    fn recommend(&self, Parameters(p): Parameters<RecommendParams>) -> Result<String, ErrorData> {
        let k = p.k.unwrap_or(3).clamp(1, 50);
        let task: String = p.task.chars().take(TASK_INPUT_LIMIT).collect();

        let q_emb = {
            let mut emb = self
                .state
                .embedder
                .lock()
                .map_err(|e| err(anyhow!("{e}")))?;
            if emb.is_none() {
                tracing::info!("loading fastembed model (one-time)");
                *emb = Some(Embedder::new().map_err(err)?);
            }
            emb.as_ref()
                .expect("embedder just initialized")
                .embed_one(&task)
                .map_err(err)?
        };

        let conn = self.state.conn.lock().map_err(|e| err(anyhow!("{e}")))?;

        let vec_sims: HashMap<String, f32> = embeddings::vec_search(&conn, &q_emb, VEC_CANDIDATES)
            .map_err(err)?
            .into_iter()
            .map(|(id, dist)| (id, 1.0 - dist))
            .collect();

        let fts_query = build_fts_query(&task);
        let fts_hits: HashMap<String, f32> = if fts_query.is_empty() {
            HashMap::new()
        } else {
            match fts::search(&conn, &fts_query, FTS_CANDIDATES) {
                Ok(rows) => rows.into_iter().collect(),
                Err(e) => {
                    tracing::warn!("fts search failed, vec-only: {e:#}");
                    HashMap::new()
                },
            }
        };

        let mut hits =
            search::hybrid_from_score_maps(&vec_sims, &fts_hits, k, COS_WEIGHT, FTS_WEIGHT);
        DemeritReranker::new(&task)
            .apply(&mut hits, &conn)
            .map_err(err)?;
        let metas: HashMap<String, _> = tools::list_all(&conn)
            .map_err(err)?
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect();

        let out: Vec<RecommendHit> = hits
            .into_iter()
            .map(|h| {
                let m = metas.get(&h.tool_id);
                RecommendHit {
                    tool_id: h.tool_id.clone(),
                    score: h.score,
                    name: m.map(|m| m.name.clone()).unwrap_or_default(),
                    description: m.and_then(|m| m.description.clone()),
                    invocation: m.and_then(|m| m.invocation.clone()),
                    install_path: m.and_then(|m| m.install_path.clone()),
                }
            })
            .collect();
        serde_json::to_string(&out).map_err(|e| err(anyhow!(e)))
    }

    #[tool(
        description = "Decide whether to invoke a candidate tool or defer to Quiver's \
                       top-1 recommendation. Use this as a cheap pre-flight check \
                       before a Bash/Read/WebFetch detour to confirm Quiver does not \
                       have a higher-confidence installed tool. Returns a decision \
                       (`use_candidate` | `use_recommended` | `no_strong_signal`), the \
                       top-1 hit, the score delta, and a one-line rationale."
    )]
    fn should_invoke(
        &self,
        Parameters(p): Parameters<ShouldInvokeParams>,
    ) -> Result<String, ErrorData> {
        let task: String = p.task.chars().take(TASK_INPUT_LIMIT).collect();
        let candidate = p.candidate_tool.trim().to_string();

        let q_emb = {
            let mut emb = self
                .state
                .embedder
                .lock()
                .map_err(|e| err(anyhow!("{e}")))?;
            if emb.is_none() {
                tracing::info!("loading fastembed model (one-time)");
                *emb = Some(Embedder::new().map_err(err)?);
            }
            emb.as_ref()
                .expect("embedder just initialized")
                .embed_one(&task)
                .map_err(err)?
        };

        let conn = self.state.conn.lock().map_err(|e| err(anyhow!("{e}")))?;

        let vec_sims: HashMap<String, f32> = embeddings::vec_search(&conn, &q_emb, VEC_CANDIDATES)
            .map_err(err)?
            .into_iter()
            .map(|(id, dist)| (id, 1.0 - dist))
            .collect();
        let fts_query = build_fts_query(&task);
        let fts_hits: HashMap<String, f32> = if fts_query.is_empty() {
            HashMap::new()
        } else {
            fts::search(&conn, &fts_query, FTS_CANDIDATES)
                .map(|rows| rows.into_iter().collect())
                .unwrap_or_default()
        };
        let mut hits =
            search::hybrid_from_score_maps(&vec_sims, &fts_hits, 5, COS_WEIGHT, FTS_WEIGHT);
        DemeritReranker::new(&task)
            .apply(&mut hits, &conn)
            .map_err(err)?;
        if hits.is_empty() {
            let res = ShouldInvokeResult {
                decision: "no_strong_signal".into(),
                recommended: None,
                delta: 0.0,
                rationale: "Quiver catalog returned no hits for this task.".into(),
                level: "silent".into(),
            };
            return serde_json::to_string(&res).map_err(|e| err(anyhow!(e)));
        }

        let metas: HashMap<String, _> = tools::list_all(&conn)
            .map_err(err)?
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect();
        let top = &hits[0];
        let top_meta = metas.get(&top.tool_id);
        let recommended = RecommendHit {
            tool_id: top.tool_id.clone(),
            score: top.score,
            name: top_meta.map(|m| m.name.clone()).unwrap_or_default(),
            description: top_meta.and_then(|m| m.description.clone()),
            invocation: top_meta.and_then(|m| m.invocation.clone()),
            install_path: top_meta.and_then(|m| m.install_path.clone()),
        };
        let candidate_score = hits
            .iter()
            .find(|h| {
                h.tool_id == candidate
                    || metas
                        .get(&h.tool_id)
                        .and_then(|m| m.invocation.as_deref())
                        .map(|inv| inv.eq_ignore_ascii_case(&candidate))
                        .unwrap_or(false)
            })
            .map(|h| h.score)
            .unwrap_or(0.0);
        let delta = top.score - candidate_score;

        let thresholds = quiver_recommender::policy::Thresholds::from_env();
        let policy = thresholds.classify(top.score);
        let same = top.tool_id == candidate
            || top_meta
                .and_then(|m| m.invocation.as_deref())
                .map(|inv| inv.eq_ignore_ascii_case(&candidate))
                .unwrap_or(false);

        let decision = if same {
            "use_candidate"
        } else if matches!(
            policy,
            quiver_recommender::policy::Policy::Strong
                | quiver_recommender::policy::Policy::Mandatory,
        ) && delta >= thresholds.tau_delta
        {
            "use_recommended"
        } else if policy == quiver_recommender::policy::Policy::Silent {
            "no_strong_signal"
        } else {
            "use_candidate"
        };
        let rationale = format!(
            "top={} score={:.3} band={} candidate_score={:.3} Δ={:.3} τ_delta={:.2}",
            top.tool_id,
            top.score,
            policy.as_str(),
            candidate_score,
            delta,
            thresholds.tau_delta,
        );

        let res = ShouldInvokeResult {
            decision: decision.into(),
            recommended: Some(recommended),
            delta,
            rationale,
            level: policy.as_str().into(),
        };
        serde_json::to_string(&res).map_err(|e| err(anyhow!(e)))
    }

    #[tool(description = "Keyword search over the catalog using FTS5 BM25.")]
    fn search(&self, Parameters(p): Parameters<SearchParams>) -> Result<String, ErrorData> {
        let k = p.k.unwrap_or(10).clamp(1, 100);
        let conn = self.state.conn.lock().map_err(|e| err(anyhow!("{e}")))?;

        let fts_query = build_fts_query(&p.query);
        let rows = if fts_query.is_empty() {
            Vec::new()
        } else {
            fts::search(&conn, &fts_query, k).map_err(err)?
        };

        let metas: HashMap<String, _> = tools::list_all(&conn)
            .map_err(err)?
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect();

        let out: Vec<SearchHit> = rows
            .into_iter()
            .map(|(id, bm25)| {
                let m = metas.get(&id);
                SearchHit {
                    tool_id: id.clone(),
                    // FTS bm25 is more-negative = better; flip for caller-friendly score.
                    score: -bm25,
                    name: m.map(|m| m.name.clone()).unwrap_or_default(),
                    description: m.and_then(|m| m.description.clone()),
                }
            })
            .collect();
        serde_json::to_string(&out).map_err(|e| err(anyhow!(e)))
    }

    #[tool(description = "Return full metadata for a single tool by id.")]
    fn info(&self, Parameters(p): Parameters<InfoParams>) -> Result<String, ErrorData> {
        let conn = self.state.conn.lock().map_err(|e| err(anyhow!("{e}")))?;
        let meta = tools::get(&conn, &p.tool_id).map_err(err)?;
        match meta {
            None => Ok("null".to_string()),
            Some(m) => {
                let info: ToolInfo = m.into();
                serde_json::to_string(&info).map_err(|e| err(anyhow!(e)))
            },
        }
    }

    #[tool(
        description = "Onboard a GitHub repo: shallow-clone, classify (skill/plugin/mcp/cli/doc), \
                       parse, persist tools + embeddings, and record the source for later sync."
    )]
    async fn add_source(
        &self,
        Parameters(p): Parameters<AddSourceParams>,
    ) -> Result<String, ErrorData> {
        // Only github type is wired up in Phase 5; other hints fall through.
        let type_ = p.r#type.as_deref().unwrap_or("github");
        if type_ != "github" {
            return Err(err(anyhow!(
                "only type=\"github\" is supported in Phase 5 (got {type_:?})"
            )));
        }

        let force_regex = std::env::var("QUIVER_LLM_EXTRACT")
            .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
            .unwrap_or(false);
        let (extractor, _label) = llm_extract::build_default(force_regex);
        let result = github_repo::onboard(&p.url, extractor.as_ref())
            .await
            .map_err(err)?;
        let n = result.tools.len();

        if n > 0 {
            let q_emb = {
                let mut emb = self
                    .state
                    .embedder
                    .lock()
                    .map_err(|e| err(anyhow!("{e}")))?;
                if emb.is_none() {
                    tracing::info!("loading fastembed model (one-time)");
                    *emb = Some(Embedder::new().map_err(err)?);
                }
                let texts: Vec<String> = result.tools.iter().map(embed_text_for).collect();
                emb.as_ref()
                    .expect("embedder just initialized")
                    .embed_batch(texts)
                    .map_err(err)?
            };

            let conn = self.state.conn.lock().map_err(|e| err(anyhow!("{e}")))?;
            for meta in &result.tools {
                tools::upsert(&conn, meta).map_err(err)?;
            }
            fts::rebuild(&conn).map_err(err)?;
            for (m, v) in result.tools.iter().zip(&q_emb) {
                embeddings::upsert(&conn, &m.id, v).map_err(err)?;
            }
            sources::upsert_full(
                &conn,
                &result.source_id,
                "github",
                &result.web_url,
                Utc::now(),
                result.commit_sha.as_deref(),
            )
            .map_err(err)?;
        } else {
            let conn = self.state.conn.lock().map_err(|e| err(anyhow!("{e}")))?;
            sources::upsert_full(
                &conn,
                &result.source_id,
                "github",
                &result.web_url,
                Utc::now(),
                result.commit_sha.as_deref(),
            )
            .map_err(err)?;
        }

        let out = AddSourceResult {
            source_id: result.source_id,
            web_url: result.web_url,
            repo_type: format!("{:?}", result.repo_type),
            tools_count: n,
            commit_sha: result.commit_sha,
            status: "registered",
        };
        serde_json::to_string(&out).map_err(|e| err(anyhow!(e)))
    }

    #[tool(description = "Aggregated success/cost/duration scores per tool. \
                       Pass `tool_id` for the detail view (includes the 5 most-recent events).")]
    fn usage_stats(
        &self,
        Parameters(p): Parameters<UsageStatsParams>,
    ) -> Result<String, ErrorData> {
        let conn = self.state.conn.lock().map_err(|e| err(anyhow!("{e}")))?;
        let raw = scores::list(&conn, p.tool_id.as_deref()).map_err(err)?;
        let rows: Vec<UsageStatsRow> = raw
            .into_iter()
            .map(|r| UsageStatsRow {
                tool_id: r.tool_id,
                success_rate: r.success_rate,
                sample_size: r.sample_size,
                avg_cost_usd: r.avg_cost_usd,
                median_duration_ms: r.median_duration_ms,
                score_updated_at: r.score_updated_at,
            })
            .collect();

        let recent_events = match &p.tool_id {
            Some(id) => usage::list_events(&conn, Some(id), 5)
                .map_err(err)?
                .into_iter()
                .map(|e| UsageEventBrief {
                    occurred_at: e.occurred_at,
                    outcome: e
                        .outcome
                        .map(|o| o.as_str().to_string())
                        .unwrap_or_else(|| "unknown".into()),
                    session_id: e.session_id,
                    project: e.project,
                })
                .collect(),
            None => Vec::new(),
        };

        let result = UsageStatsResult {
            rows,
            recent_events,
            note: "Run `quiver score` to populate from session JSONL.",
        };
        serde_json::to_string(&result).map_err(|e| err(anyhow!(e)))
    }
}

/// Concatenate name + description + triggers — same blend `sync` and `add` use,
/// so the embedding produced by `add_source` matches what `recommend` expects.
fn embed_text_for(m: &quiver_core::tool::ToolMeta) -> String {
    let desc = m.description.as_deref().unwrap_or("");
    let triggers = m.triggers.join(", ");
    format!("{}\n{}\n{}", m.name, desc, triggers)
}

/// Run the server on stdio. Blocks until the client disconnects.
pub async fn serve_stdio() -> anyhow::Result<()> {
    let server = QuiverServer::new()?;
    let service = server.serve(stdio()).await.context("serve stdio")?;
    service
        .waiting()
        .await
        .context("waiting on stdio service")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use quiver_core::tool::{ToolMeta, ToolType};

    fn sample_meta(id: &str, name: &str) -> ToolMeta {
        let now = Utc::now();
        ToolMeta {
            id: id.into(),
            r#type: ToolType::Skill,
            name: name.into(),
            source_repo: None,
            install_path: Some("/tmp/x".into()),
            description: Some("desc".into()),
            long_description: Some("body".into()),
            category: None,
            triggers: vec!["a".into(), "b".into()],
            examples: vec![],
            invocation: Some("/x".into()),
            requires: vec![],
            enabled: true,
            added_at: now,
            last_seen_at: now,
            last_used_at: None,
        }
    }

    fn make_server() -> (QuiverServer, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        (QuiverServer::from_conn(conn), dir)
    }

    #[test]
    fn info_returns_null_for_unknown_id() {
        let (server, _dir) = make_server();
        let out = server
            .info(Parameters(InfoParams {
                tool_id: "skill:does-not-exist".into(),
            }))
            .unwrap();
        assert_eq!(out, "null");
    }

    #[test]
    fn info_returns_meta_for_existing_id() {
        let (server, _dir) = make_server();
        {
            let conn = server.state.conn.lock().unwrap();
            tools::upsert(&conn, &sample_meta("skill:x", "X")).unwrap();
        }
        let out = server
            .info(Parameters(InfoParams {
                tool_id: "skill:x".into(),
            }))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["id"], "skill:x");
        assert_eq!(v["type"], "skill");
        assert_eq!(v["name"], "X");
        assert_eq!(v["triggers"][0], "a");
    }

    #[test]
    fn search_returns_empty_when_no_match() {
        let (server, _dir) = make_server();
        let out = server
            .search(Parameters(SearchParams {
                query: "absolutely-no-such-keyword".into(),
                k: Some(3),
            }))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v.as_array().unwrap().is_empty());
    }

    #[test]
    fn search_finds_seeded_tool_after_fts_rebuild() {
        let (server, _dir) = make_server();
        {
            let conn = server.state.conn.lock().unwrap();
            let mut meta = sample_meta("skill:design-md", "design-md");
            meta.description = Some("Generate design docs from markdown".into());
            tools::upsert(&conn, &meta).unwrap();
            fts::rebuild(&conn).unwrap();
        }
        let out = server
            .search(Parameters(SearchParams {
                query: "design".into(),
                k: Some(5),
            }))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(!v.as_array().unwrap().is_empty());
        assert_eq!(v[0]["tool_id"], "skill:design-md");
    }

    #[tokio::test]
    async fn add_source_rejects_non_github_type() {
        let (server, _dir) = make_server();
        let res = server
            .add_source(Parameters(AddSourceParams {
                url: "https://example.com/foo.tar.gz".into(),
                r#type: Some("url".into()),
            }))
            .await;
        assert!(res.is_err(), "expected error for non-github type");
    }

    #[test]
    fn usage_stats_returns_empty_when_no_data() {
        let (server, _dir) = make_server();
        let out = server
            .usage_stats(Parameters(UsageStatsParams { tool_id: None }))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v["rows"].as_array().unwrap().is_empty());
        assert!(v["note"].as_str().unwrap().contains("quiver score"));
        // recent_events is skipped when empty.
        assert!(
            v.get("recent_events").is_none() || v["recent_events"].as_array().unwrap().is_empty()
        );
    }

    #[test]
    fn usage_stats_includes_recent_events_for_tool_filter() {
        use chrono::{TimeZone, Utc};
        use quiver_core::usage::{Outcome, UsageEvent};

        let (server, _dir) = make_server();
        {
            let conn = server.state.conn.lock().unwrap();
            tools::upsert(&conn, &sample_meta("skill:caveman", "caveman")).unwrap();
            usage::insert_event(
                &conn,
                &UsageEvent {
                    uuid: Some("u1".into()),
                    tool_id: "skill:caveman".into(),
                    session_id: Some("sess-1".into()),
                    project: Some("quiver".into()),
                    task_text: Some("write something terse".into()),
                    outcome: Outcome::Success,
                    duration_ms: Some(120),
                    cost_usd: None,
                    occurred_at: Utc.with_ymd_and_hms(2026, 5, 3, 12, 0, 0).unwrap(),
                },
            )
            .unwrap();
        }
        let out = server
            .usage_stats(Parameters(UsageStatsParams {
                tool_id: Some("skill:caveman".into()),
            }))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let events = v["recent_events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["outcome"], "success");
        assert_eq!(events[0]["session_id"], "sess-1");
        assert_eq!(events[0]["project"], "quiver");
    }
}
