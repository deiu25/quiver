//! Stats / digest dashboard.

use std::collections::HashMap;

use askama::Template;
use axum::Router;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use chrono::{Duration, Utc};
use quiver_core::tool::ToolMeta;
use quiver_storage::{scores, suggestions, tools};
use rusqlite::Connection;

use crate::error::{WebError, WebResult};
use crate::state::AppState;
use crate::views::{enforce_label, type_label};

pub fn routes() -> Router<AppState> {
    Router::new().route("/stats", get(stats_page))
}

#[derive(Template)]
#[template(path = "stats.html")]
struct StatsPage {
    active: &'static str,
    enforce: &'static str,
    acceptance: AcceptanceView,
    veto_stats: VetoStatsView,
    level_breakdown: Vec<LevelCountView>,
    top_tools: Vec<TopToolView>,
    demerits: Vec<DemeritToolView>,
    dead_weight: Vec<DeadToolView>,
    sources: Vec<SourceCountView>,
}

pub struct DemeritToolView {
    pub tool_id: String,
    pub demerit_str: String,
    pub bar_width_px: i64,
    pub top_sig: String,
    pub sig_count: usize,
}

pub struct VetoStatsView {
    pub vetoed: i64,
    pub bypassed: i64,
    pub nudged: i64,
    pub false_positives: i64,
    pub bypass_rate_pct_str: String,
    pub mandatory_total: i64,
    pub mandatory_accepted: i64,
    pub mandatory_rate_pct_str: String,
}

pub struct LevelCountView {
    pub level: &'static str,
    pub count: i64,
    pub bar_width_px: i64,
}

pub struct AcceptanceView {
    pub suggested: i64,
    pub accepted: i64,
    pub rate_pct_str: String,
}

pub struct TopToolView {
    pub tool_id: String,
    pub success_rate_pct: String,
    pub sample_size: i64,
    pub avg_cost_str: String,
    pub total_cost_str: String,
    pub combined_str: String,
    pub bar_width_px: i64,
}

pub struct DeadToolView {
    pub id: String,
    pub type_name: &'static str,
    pub last_seen_str: String,
}

pub struct SourceCountView {
    pub name: String,
    pub count: usize,
}

async fn stats_page(State(state): State<AppState>) -> WebResult<Response> {
    let stats: StatsData = tokio::task::spawn_blocking(move || -> WebResult<StatsData> {
        let conn = state.pool.get()?;
        Ok(load(&conn)?)
    })
    .await??;

    render(StatsPage {
        active: "stats",
        enforce: enforce_label(),
        acceptance: stats.acceptance,
        veto_stats: stats.veto_stats,
        level_breakdown: stats.level_breakdown,
        top_tools: stats.top_tools,
        demerits: stats.demerits,
        dead_weight: stats.dead_weight,
        sources: stats.sources,
    })
}

struct StatsData {
    acceptance: AcceptanceView,
    veto_stats: VetoStatsView,
    level_breakdown: Vec<LevelCountView>,
    top_tools: Vec<TopToolView>,
    demerits: Vec<DemeritToolView>,
    dead_weight: Vec<DeadToolView>,
    sources: Vec<SourceCountView>,
}

fn load(conn: &Connection) -> anyhow::Result<StatsData> {
    let now = Utc::now();
    let (suggested, accepted) = suggestions::acceptance_stats(conn, now - Duration::days(7))?;
    let rate_pct = if suggested == 0 {
        0.0
    } else {
        (accepted as f64) * 100.0 / (suggested as f64)
    };
    let acceptance = AcceptanceView {
        suggested,
        accepted,
        rate_pct_str: format!("{rate_pct:.1}"),
    };
    let veto = suggestions::veto_stats(conn, now - Duration::days(7))?;
    let bypass_rate = if veto.vetoed == 0 {
        0.0
    } else {
        (veto.bypassed as f64) * 100.0 / (veto.vetoed as f64)
    };
    let mandatory_rate = if veto.mandatory_total == 0 {
        0.0
    } else {
        (veto.mandatory_accepted as f64) * 100.0 / (veto.mandatory_total as f64)
    };
    let veto_stats = VetoStatsView {
        vetoed: veto.vetoed,
        bypassed: veto.bypassed,
        nudged: veto.nudged,
        false_positives: veto.false_positives,
        bypass_rate_pct_str: format!("{bypass_rate:.1}"),
        mandatory_total: veto.mandatory_total,
        mandatory_accepted: veto.mandatory_accepted,
        mandatory_rate_pct_str: format!("{mandatory_rate:.1}"),
    };
    let max_level_count = veto
        .by_level
        .iter()
        .map(|(_, c)| *c)
        .max()
        .unwrap_or(1)
        .max(1);
    let level_breakdown: Vec<LevelCountView> = ["mandatory", "strong", "hint"]
        .into_iter()
        .map(|name| {
            let count = veto
                .by_level
                .iter()
                .find(|(l, _)| l == name)
                .map(|(_, c)| *c)
                .unwrap_or(0);
            LevelCountView {
                level: name,
                count,
                bar_width_px: ((count as f64 / max_level_count as f64) * 80.0).round() as i64,
            }
        })
        .collect();

    let score_rows = scores::list(conn, None)?;
    let mut combined: Vec<(String, f64, i64, f64, Option<f64>)> = score_rows
        .into_iter()
        .filter_map(|s| {
            let rate = s.success_rate?;
            let n = s.sample_size?;
            if n <= 0 {
                return None;
            }
            let weighted = rate * ((n as f64) + 1.0).ln();
            Some((s.tool_id, rate, n, weighted, s.avg_cost_usd))
        })
        .collect();
    combined.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    combined.truncate(20);
    let max_combined = combined.first().map(|c| c.3).unwrap_or(1.0).max(1e-9);
    let top_tools: Vec<TopToolView> = combined
        .into_iter()
        .map(|(tool_id, rate, n, w, avg_cost)| {
            let avg_cost_str = avg_cost
                .map(|c| format!("${c:.4}"))
                .unwrap_or_else(|| "—".to_string());
            let total_cost_str = avg_cost
                .map(|c| format!("${:.4}", c * n as f64))
                .unwrap_or_else(|| "—".to_string());
            TopToolView {
                tool_id,
                success_rate_pct: format!("{:.0}", rate * 100.0),
                sample_size: n,
                avg_cost_str,
                total_cost_str,
                combined_str: format!("{w:.2}"),
                bar_width_px: ((w / max_combined) * 80.0).round() as i64,
            }
        })
        .collect();

    // Phase 9 auto-tuner: top demerited tools — surfaces FP/bypass feedback
    // that the recommender is using to demote skills for similar prompts.
    let demerit_rows = scores::list_demerits(conn, 10)?;
    let max_demerit = demerit_rows
        .iter()
        .map(|r| r.demerit_count)
        .fold(0.0f64, f64::max)
        .max(1e-9);
    let demerits: Vec<DemeritToolView> = demerit_rows
        .into_iter()
        .map(|r| {
            let parsed: Vec<quiver_storage::usage::DemeritSignature> = r
                .demerit_signatures_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            let top_sig = parsed
                .first()
                .map(|d| d.sig.clone())
                .unwrap_or_else(|| "—".to_string());
            DemeritToolView {
                tool_id: r.tool_id,
                demerit_str: format!("{:.2}", r.demerit_count),
                bar_width_px: ((r.demerit_count / max_demerit) * 80.0).round() as i64,
                top_sig,
                sig_count: parsed.len(),
            }
        })
        .collect();

    let cutoff = now - Duration::days(30);
    let mut dead_weight: Vec<DeadToolView> = tools::list_all(conn)?
        .into_iter()
        .filter(|t| match t.last_used_at {
            Some(t) => t < cutoff,
            None => true,
        })
        .map(|t: ToolMeta| DeadToolView {
            id: t.id,
            type_name: type_label(t.r#type),
            last_seen_str: t.last_seen_at.format("%Y-%m-%d").to_string(),
        })
        .collect();
    // Newest first by id (cheap, deterministic).
    dead_weight.sort_by(|a, b| a.id.cmp(&b.id));
    dead_weight.truncate(50);

    let mut by_source: HashMap<String, usize> = HashMap::new();
    for t in tools::list_all(conn)? {
        let key = t
            .source_repo
            .clone()
            .or_else(|| {
                t.install_path
                    .as_ref()
                    .and_then(|p| p.split('/').next_back().map(|s| s.to_string()))
            })
            .unwrap_or_else(|| "(local)".to_string());
        *by_source.entry(key).or_default() += 1;
    }
    let mut sources: Vec<SourceCountView> = by_source
        .into_iter()
        .map(|(name, count)| SourceCountView { name, count })
        .collect();
    sources.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    sources.truncate(20);

    Ok(StatsData {
        acceptance,
        veto_stats,
        level_breakdown,
        top_tools,
        demerits,
        dead_weight,
        sources,
    })
}

fn render<T: Template>(t: T) -> WebResult<Response> {
    match t.render() {
        Ok(html) => Ok(Html(html).into_response()),
        Err(err) => Err(WebError::Internal(anyhow::anyhow!("render: {err}"))),
    }
}
