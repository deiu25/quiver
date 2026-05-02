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
}
