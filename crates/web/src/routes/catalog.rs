//! Catalog browser: full page, htmx fragment, tool detail.

use askama::Template;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use quiver_core::tool::{ToolMeta, ToolType};
use quiver_storage::{scores, tools};
use serde::Deserialize;

use crate::error::{WebError, WebResult};
use crate::state::AppState;
use crate::views::{ScoreView, ToolView, enforce_label, parse_type_filter};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/catalog", get(catalog_page))
        .route("/catalog/list", get(catalog_list_fragment))
        .route("/tool/:id", get(tool_detail))
}

#[derive(Debug, Deserialize, Default)]
pub struct CatalogQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default, rename = "type")]
    pub type_filter: String,
}

#[derive(Template)]
#[template(path = "catalog.html")]
struct CatalogPage {
    active: &'static str,
    enforce: &'static str,
    q: String,
    type_filter: String,
    tools: Vec<ToolView>,
    total: usize,
    chips: Vec<ChipView>,
}

#[derive(Template)]
#[template(path = "catalog_list.html")]
struct CatalogListFragment {
    tools: Vec<ToolView>,
    type_filter: String,
}

pub struct ChipView {
    pub value: &'static str,
    pub label: &'static str,
    pub count: usize,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct TypeCounts {
    pub total: usize,
    pub skill: usize,
    pub plugin: usize,
    pub mcp: usize,
    pub cli: usize,
    pub doc: usize,
}

pub fn count_by_type(metas: &[ToolMeta]) -> TypeCounts {
    let mut c = TypeCounts {
        total: metas.len(),
        ..TypeCounts::default()
    };
    for m in metas {
        match m.r#type {
            ToolType::Skill => c.skill += 1,
            ToolType::Plugin => c.plugin += 1,
            ToolType::Mcp => c.mcp += 1,
            ToolType::Cli => c.cli += 1,
            ToolType::Doc => c.doc += 1,
        }
    }
    c
}

fn build_chips(c: &TypeCounts) -> Vec<ChipView> {
    vec![
        ChipView {
            value: "",
            label: "All",
            count: c.total,
        },
        ChipView {
            value: "skill",
            label: "Skills",
            count: c.skill,
        },
        ChipView {
            value: "plugin",
            label: "Plugins",
            count: c.plugin,
        },
        ChipView {
            value: "mcp",
            label: "MCP",
            count: c.mcp,
        },
        ChipView {
            value: "cli",
            label: "CLI",
            count: c.cli,
        },
        ChipView {
            value: "doc",
            label: "Doc",
            count: c.doc,
        },
    ]
}

#[derive(Template)]
#[template(path = "tool_detail.html")]
struct ToolDetailPage {
    active: &'static str,
    enforce: &'static str,
    tool: ToolView,
    score: Option<ScoreView>,
}

async fn catalog_page(
    State(state): State<AppState>,
    Query(q): Query<CatalogQuery>,
) -> WebResult<Response> {
    let filter = parse_type_filter(&q.type_filter);
    let needle = q.q.trim().to_string();
    let needle_filter = needle.clone();
    let (tools, counts) =
        tokio::task::spawn_blocking(move || -> WebResult<(Vec<ToolView>, TypeCounts)> {
            let conn = state.pool.get()?;
            Ok(load_tools(&conn, filter, &needle_filter)?)
        })
        .await??;

    let chips = build_chips(&counts);
    let page = CatalogPage {
        active: "catalog",
        enforce: enforce_label(),
        total: tools.len(),
        q: needle,
        type_filter: q.type_filter,
        tools,
        chips,
    };
    render(page)
}

async fn catalog_list_fragment(
    State(state): State<AppState>,
    Query(q): Query<CatalogQuery>,
) -> WebResult<Response> {
    let filter = parse_type_filter(&q.type_filter);
    let needle = q.q.trim().to_string();
    let type_filter = q.type_filter.clone();
    let (tools, _counts) =
        tokio::task::spawn_blocking(move || -> WebResult<(Vec<ToolView>, TypeCounts)> {
            let conn = state.pool.get()?;
            Ok(load_tools(&conn, filter, &needle)?)
        })
        .await??;

    render(CatalogListFragment { tools, type_filter })
}

async fn tool_detail(State(state): State<AppState>, Path(id): Path<String>) -> WebResult<Response> {
    let id_for_query = id.clone();
    let (tool, score) = tokio::task::spawn_blocking(
        move || -> WebResult<(Option<ToolView>, Option<ScoreView>)> {
            let conn = state.pool.get()?;
            let tool = tools::get(&conn, &id_for_query)?.map(ToolView::from);
            let score = scores::list(&conn, Some(&id_for_query))?
                .into_iter()
                .next()
                .map(ScoreView::from);
            Ok((tool, score))
        },
    )
    .await??;

    let Some(tool) = tool else {
        return Ok((StatusCode::NOT_FOUND, format!("no such tool: {id}")).into_response());
    };
    render(ToolDetailPage {
        active: "catalog",
        enforce: enforce_label(),
        tool,
        score,
    })
}

fn load_tools(
    conn: &rusqlite::Connection,
    type_filter: Option<ToolType>,
    needle: &str,
) -> anyhow::Result<(Vec<ToolView>, TypeCounts)> {
    let all = tools::list_all(conn)?;
    // Counts span the *whole* catalog regardless of the active type filter so
    // chip badges stay stable as the user clicks through them.
    let counts = count_by_type(&all);
    let needle_lower = needle.to_ascii_lowercase();
    let mut out: Vec<ToolView> = all
        .into_iter()
        .filter(|t| match type_filter {
            Some(want) => t.r#type == want,
            None => true,
        })
        .filter(|t| {
            if needle_lower.is_empty() {
                return true;
            }
            text_haystack(t)
                .to_ascii_lowercase()
                .contains(&needle_lower)
        })
        .map(ToolView::from)
        .collect();
    // Stable sort: name, then id, so the htmx fragment stays predictable.
    out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    Ok((out, counts))
}

fn text_haystack(t: &quiver_core::tool::ToolMeta) -> String {
    let mut s = String::with_capacity(256);
    s.push_str(&t.name);
    s.push(' ');
    s.push_str(&t.id);
    s.push(' ');
    if let Some(d) = &t.description {
        s.push_str(d);
        s.push(' ');
    }
    if let Some(c) = &t.category {
        s.push_str(c);
        s.push(' ');
    }
    for trigger in &t.triggers {
        s.push_str(trigger);
        s.push(' ');
    }
    s
}

fn render<T: Template>(t: T) -> WebResult<Response> {
    match t.render() {
        Ok(html) => Ok(Html(html).into_response()),
        Err(err) => Err(WebError::Internal(anyhow::anyhow!("render: {err}"))),
    }
}
