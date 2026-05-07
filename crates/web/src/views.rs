//! Display-friendly view models. Askama templates can't easily reach
//! `r#type`, JSON values, or chrono `DateTime` formatting in-line, so we
//! pre-flatten everything we render into plain strings here. Pure functions,
//! no DB.

use chrono::{DateTime, Utc};
use quiver_core::tool::{ToolMeta, ToolType};
use quiver_storage::scores::ScoreRow;

pub struct ToolView {
    pub id: String,
    pub name: String,
    pub type_name: &'static str,
    pub description: Option<String>,
    pub long_description: Option<String>,
    pub source_repo: Option<String>,
    pub install_path: Option<String>,
    pub category: Option<String>,
    pub invocation: Option<String>,
    pub triggers_csv: String,
    pub requires_csv: String,
    pub last_seen_at: String,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl ToolView {
    pub fn description_or_dash(&self) -> &str {
        self.description.as_deref().unwrap_or("—")
    }
    pub fn source_or_dash(&self) -> &str {
        self.source_repo.as_deref().unwrap_or("—")
    }
    pub fn install_path_or_dash(&self) -> &str {
        self.install_path.as_deref().unwrap_or("—")
    }
    pub fn category_or_dash(&self) -> &str {
        self.category.as_deref().unwrap_or("—")
    }
    pub fn invocation_or_dash(&self) -> &str {
        self.invocation.as_deref().unwrap_or("—")
    }
    pub fn last_used_or_dash(&self) -> String {
        self.last_used_at
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "—".to_string())
    }
}

impl From<ToolMeta> for ToolView {
    fn from(m: ToolMeta) -> Self {
        let triggers_csv = if m.triggers.is_empty() {
            "—".to_string()
        } else {
            m.triggers.join(", ")
        };
        let requires_csv = if m.requires.is_empty() {
            "—".to_string()
        } else {
            m.requires.join(", ")
        };
        ToolView {
            id: m.id,
            name: m.name,
            type_name: type_label(m.r#type),
            description: m.description,
            long_description: m.long_description,
            source_repo: m.source_repo,
            install_path: m.install_path,
            category: m.category,
            invocation: m.invocation,
            triggers_csv,
            requires_csv,
            last_seen_at: m.last_seen_at.format("%Y-%m-%d %H:%M").to_string(),
            last_used_at: m.last_used_at,
        }
    }
}

pub fn type_label(t: ToolType) -> &'static str {
    match t {
        ToolType::Skill => "skill",
        ToolType::Plugin => "plugin",
        ToolType::Mcp => "mcp",
        ToolType::Cli => "cli",
        ToolType::Doc => "doc",
    }
}

/// Parse a `type=` query param into the canonical lowercase name. Returns
/// `None` on empty or unrecognised values (caller treats that as "no filter").
pub fn parse_type_filter(s: &str) -> Option<ToolType> {
    match s.trim().to_ascii_lowercase().as_str() {
        "skill" => Some(ToolType::Skill),
        "plugin" => Some(ToolType::Plugin),
        "mcp" => Some(ToolType::Mcp),
        "cli" => Some(ToolType::Cli),
        "doc" => Some(ToolType::Doc),
        _ => None,
    }
}

/// Read `QUIVER_ENFORCE` once per request and return one of the canonical
/// labels (`"strict"`, `"advisory"`, `"off"`). Default `strict` matches the
/// hook handler's `EnforceMode::from_env`. The base template uses the value
/// to colour the global enforcement banner.
pub fn enforce_label() -> &'static str {
    if std::env::var("QUIVER_HOOK_DISABLED").as_deref() == Ok("1") {
        return "off";
    }
    match std::env::var("QUIVER_ENFORCE")
        .ok()
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "advisory" | "soft" | "hint" => "advisory",
        "off" | "disabled" | "no" | "0" => "off",
        _ => "strict",
    }
}

pub struct ScoreView {
    pub success_rate_pct: String,
    pub sample_size: i64,
    pub avg_cost_str: String,
    pub median_duration_str: String,
}

impl From<ScoreRow> for ScoreView {
    fn from(s: ScoreRow) -> Self {
        ScoreView {
            success_rate_pct: format!("{:.0}", s.success_rate.unwrap_or(0.0) * 100.0),
            sample_size: s.sample_size.unwrap_or(0),
            avg_cost_str: s
                .avg_cost_usd
                .map(|c| format!("{c:.4}"))
                .unwrap_or_else(|| "—".to_string()),
            median_duration_str: s
                .median_duration_ms
                .map(|d| d.to_string())
                .unwrap_or_else(|| "—".to_string()),
        }
    }
}
