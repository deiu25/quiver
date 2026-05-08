use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ToolType {
    Skill,
    Plugin,
    Mcp,
    Cli,
    Doc,
}

/// Scope of a catalogued tool. `User` (default) covers globally installed
/// skills/plugins/MCP servers under `$HOME`. `Project` covers per-project
/// skills discovered under `<cwd>/.claude/skills/` — those rows carry a
/// `scope_root` pointing at the canonicalised project directory and earn a
/// boost from `ProjectScopeReranker` when the active recommend originates
/// from that same root.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToolScope {
    #[default]
    User,
    Project,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMeta {
    pub id: String,
    pub r#type: ToolType,
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
    pub added_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    /// Default `User` for globally installed catalog entries; `Project` for
    /// rows ingested from `<cwd>/.claude/skills/` on-the-fly.
    #[serde(default)]
    pub scope: ToolScope,
    /// Canonicalised project root for `scope=Project` rows. Always `None`
    /// for `scope=User`.
    #[serde(default)]
    pub scope_root: Option<String>,
}
