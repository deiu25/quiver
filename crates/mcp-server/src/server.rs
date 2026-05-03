//! ToolHub MCP server (stdio transport, rmcp `tool_router` macro).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, anyhow};
use rmcp::ErrorData;
use rmcp::ServiceExt;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::transport::stdio;
use rmcp::{tool, tool_router};
use rusqlite::Connection;

use toolhub_recommender::embed::Embedder;
use toolhub_recommender::params::{
    COS_WEIGHT, FTS_CANDIDATES, FTS_WEIGHT, VEC_CANDIDATES, build_fts_query,
};
use toolhub_recommender::search;
use toolhub_storage::{embeddings, fts, open, scores, sources, tools, usage};

use crate::schema::{
    AddSourceParams, AddSourceResult, InfoParams, RecommendHit, RecommendParams, SearchHit,
    SearchParams, ToolInfo, UsageEventBrief, UsageStatsParams, UsageStatsResult, UsageStatsRow,
};

/// 2 KB upper bound on free-text task input fed to the embedder.
const TASK_INPUT_LIMIT: usize = 2048;

/// Resolve the default ToolHub SQLite path: `$XDG_DATA_HOME/toolhub/toolhub.sqlite`,
/// falling back to `$HOME/.local/share/toolhub/toolhub.sqlite`.
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
    Ok(base.join("toolhub").join("toolhub.sqlite"))
}

/// Shared server state. Wrapped in `Arc<…>` inside `ToolHubServer` so each
/// rmcp dispatch only clones the wrapper. The embedder is lazy: tools that
/// don't need it (search/info/add_source/usage_stats) never pay the
/// fastembed cold-init cost.
pub struct ServerState {
    pub conn: Mutex<Connection>,
    pub embedder: Mutex<Option<Embedder>>,
}

#[derive(Clone)]
pub struct ToolHubServer {
    state: std::sync::Arc<ServerState>,
}

impl ToolHubServer {
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
impl ToolHubServer {
    #[tool(
        description = "Recommend tools for a free-text task using hybrid vec+FTS search. \
                       Returns top-k tools (default 3) ordered by combined score."
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

        let hits = search::hybrid_from_score_maps(&vec_sims, &fts_hits, k, COS_WEIGHT, FTS_WEIGHT);
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
        description = "Register a tool source (GitHub repo / URL) for later sync. \
                       Phase 3 only records the row; actual fetch lands in Phase 5."
    )]
    fn add_source(&self, Parameters(p): Parameters<AddSourceParams>) -> Result<String, ErrorData> {
        let conn = self.state.conn.lock().map_err(|e| err(anyhow!("{e}")))?;
        let type_ = p.r#type.as_deref().unwrap_or("github");
        let id = derive_source_id(type_, &p.url);
        sources::upsert(&conn, &id, type_, &p.url).map_err(err)?;
        let result = AddSourceResult {
            source_id: id,
            status: "registered",
            note: "Phase 3 stub — fetch + parse lands in Phase 5.",
        };
        serde_json::to_string(&result).map_err(|e| err(anyhow!(e)))
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
            note: "Run `toolhub score` to populate from session JSONL.",
        };
        serde_json::to_string(&result).map_err(|e| err(anyhow!(e)))
    }
}

/// Derive a deterministic source id from URL + type.
fn derive_source_id(type_: &str, url: &str) -> String {
    if type_ == "github" {
        // gh:owner/repo
        if let Some(rest) = url
            .trim_end_matches('/')
            .trim_end_matches(".git")
            .strip_prefix("https://github.com/")
            .or_else(|| {
                url.trim_end_matches('/')
                    .trim_end_matches(".git")
                    .strip_prefix("git@github.com:")
            })
        {
            return format!("gh:{rest}");
        }
    }
    format!("{type_}:{url}")
}

/// Run the server on stdio. Blocks until the client disconnects.
pub async fn serve_stdio() -> anyhow::Result<()> {
    let server = ToolHubServer::new()?;
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
    use toolhub_core::tool::{ToolMeta, ToolType};

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

    fn make_server() -> (ToolHubServer, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        (ToolHubServer::from_conn(conn), dir)
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

    #[test]
    fn add_source_writes_row_with_derived_id() {
        let (server, _dir) = make_server();
        let out = server
            .add_source(Parameters(AddSourceParams {
                url: "https://github.com/foo/bar".into(),
                r#type: None,
            }))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["source_id"], "gh:foo/bar");
        assert_eq!(v["status"], "registered");

        let conn = server.state.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn usage_stats_returns_empty_when_no_data() {
        let (server, _dir) = make_server();
        let out = server
            .usage_stats(Parameters(UsageStatsParams { tool_id: None }))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v["rows"].as_array().unwrap().is_empty());
        assert!(v["note"].as_str().unwrap().contains("toolhub score"));
        // recent_events is skipped when empty.
        assert!(
            v.get("recent_events").is_none() || v["recent_events"].as_array().unwrap().is_empty()
        );
    }

    #[test]
    fn usage_stats_includes_recent_events_for_tool_filter() {
        use chrono::{TimeZone, Utc};
        use toolhub_core::usage::{Outcome, UsageEvent};

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

    #[test]
    fn derive_source_id_github_https() {
        assert_eq!(
            derive_source_id("github", "https://github.com/foo/bar"),
            "gh:foo/bar"
        );
        assert_eq!(
            derive_source_id("github", "https://github.com/foo/bar.git"),
            "gh:foo/bar"
        );
        assert_eq!(
            derive_source_id("github", "https://github.com/foo/bar/"),
            "gh:foo/bar"
        );
    }

    #[test]
    fn derive_source_id_github_ssh() {
        assert_eq!(
            derive_source_id("github", "git@github.com:foo/bar.git"),
            "gh:foo/bar"
        );
    }

    #[test]
    fn derive_source_id_falls_back_for_unknown_type() {
        assert_eq!(
            derive_source_id("url", "https://example.com/skill.tar.gz"),
            "url:https://example.com/skill.tar.gz"
        );
    }
}
