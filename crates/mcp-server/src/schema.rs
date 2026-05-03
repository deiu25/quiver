//! Param + result structs exposed by the MCP server.
//!
//! Every param struct derives `serde::Deserialize + schemars::JsonSchema`
//! so rmcp can build the JSON-Schema returned in `tools/list`.
//! Every result struct derives `serde::Serialize` so handlers can
//! `serde_json::to_string` the value before returning a text content block.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use toolhub_core::tool::ToolMeta;

// ─── recommend ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecommendParams {
    /// Free-text task description ("extract design tokens from a marketing page").
    pub task: String,
    /// Number of results to return; defaults to 3.
    #[serde(default)]
    pub k: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RecommendHit {
    pub tool_id: String,
    pub score: f32,
    pub name: String,
    pub description: Option<String>,
    pub invocation: Option<String>,
    pub install_path: Option<String>,
}

// ─── search ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// FTS5 keyword query (whitespace-tokenised, OR-joined).
    pub query: String,
    /// Number of results to return; defaults to 10.
    #[serde(default)]
    pub k: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchHit {
    pub tool_id: String,
    pub score: f32,
    pub name: String,
    pub description: Option<String>,
}

// ─── info ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InfoParams {
    /// Tool id, e.g. `skill:design-md`, `plugin:caveman@caveman`, `mcp:ruflo`.
    pub tool_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ToolInfo {
    pub id: String,
    pub r#type: String,
    pub name: String,
    pub source_repo: Option<String>,
    pub install_path: Option<String>,
    pub description: Option<String>,
    pub long_description: Option<String>,
    pub category: Option<String>,
    pub triggers: Vec<String>,
    pub examples: Vec<serde_json::Value>,
    pub invocation: Option<String>,
    pub requires: Vec<String>,
    pub enabled: bool,
    pub added_at: String,
    pub last_seen_at: String,
    pub last_used_at: Option<String>,
}

impl From<ToolMeta> for ToolInfo {
    fn from(m: ToolMeta) -> Self {
        ToolInfo {
            id: m.id,
            r#type: format!("{:?}", m.r#type).to_lowercase(),
            name: m.name,
            source_repo: m.source_repo,
            install_path: m.install_path,
            description: m.description,
            long_description: m.long_description,
            category: m.category,
            triggers: m.triggers,
            examples: m.examples,
            invocation: m.invocation,
            requires: m.requires,
            enabled: m.enabled,
            added_at: m.added_at.to_rfc3339(),
            last_seen_at: m.last_seen_at.to_rfc3339(),
            last_used_at: m.last_used_at.map(|d| d.to_rfc3339()),
        }
    }
}

// ─── add_source ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddSourceParams {
    /// Source URL (GitHub repo, raw URL, etc.).
    pub url: String,
    /// Source type hint: `github` | `local-dir` | `url`. Defaults to `github`.
    #[serde(default)]
    pub r#type: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AddSourceResult {
    pub source_id: String,
    pub web_url: String,
    pub repo_type: String,
    pub tools_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    pub status: &'static str,
}

// ─── usage_stats ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UsageStatsParams {
    /// If set, restrict to a single tool id; otherwise return all rows.
    #[serde(default)]
    pub tool_id: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UsageStatsRow {
    pub tool_id: String,
    pub success_rate: Option<f64>,
    pub sample_size: Option<i64>,
    pub avg_cost_usd: Option<f64>,
    pub median_duration_ms: Option<i64>,
    pub score_updated_at: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UsageEventBrief {
    pub occurred_at: String,
    pub outcome: String,
    pub session_id: Option<String>,
    pub project: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UsageStatsResult {
    pub rows: Vec<UsageStatsRow>,
    /// Most-recent events for the requested tool when `tool_id` is set; empty
    /// otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_events: Vec<UsageEventBrief>,
    pub note: &'static str,
}
