//! Usage tracking domain types — Phase 4.
//!
//! `UsageEvent` rows are produced by replaying Claude Code session JSONL
//! (`crates/ingestion/src/session_jsonl.rs`) and persisted by
//! `crates/storage/src/usage.rs`. `Outcome` is the heuristic verdict assigned
//! per tool invocation; it serialises to lowercase to match the `outcome`
//! TEXT column on `usage_events`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Success,
    Failure,
    Abandoned,
    Unknown,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Success => "success",
            Outcome::Failure => "failure",
            Outcome::Abandoned => "abandoned",
            Outcome::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "success" => Outcome::Success,
            "failure" => Outcome::Failure,
            "abandoned" => Outcome::Abandoned,
            "unknown" => Outcome::Unknown,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEvent {
    /// Mirrors assistant `tool_use.id` from JSONL. UNIQUE — drives idempotent replay.
    pub uuid: Option<String>,
    pub tool_id: String,
    pub session_id: Option<String>,
    pub project: Option<String>,
    pub task_text: Option<String>,
    pub outcome: Outcome,
    pub duration_ms: Option<i64>,
    pub cost_usd: Option<f64>,
    pub occurred_at: DateTime<Utc>,
}
