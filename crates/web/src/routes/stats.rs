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
use crate::views::type_label;

pub fn routes() -> Router<AppState> {
    Router::new().route("/stats", get(stats_page))
}

#[derive(Template)]
#[template(path = "stats.html")]
struct StatsPage {
    active: &'static str,
    acceptance: AcceptanceView,
    top_tools: Vec<TopToolView>,
    dead_weight: Vec<DeadToolView>,
    sources: Vec<SourceCountView>,
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
        acceptance: stats.acceptance,
        top_tools: stats.top_tools,
        dead_weight: stats.dead_weight,
        sources: stats.sources,
    })
}

struct StatsData {
    acceptance: AcceptanceView,
    top_tools: Vec<TopToolView>,
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
        top_tools,
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
