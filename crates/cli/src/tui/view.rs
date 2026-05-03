use quiver_core::tool::{ToolMeta, ToolType};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::tui::app::{App, Mode};

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);

    match app.mode {
        Mode::List | Mode::Search => draw_list(f, app, chunks[0]),
        Mode::Detail => draw_detail(f, app, chunks[0]),
    }
    draw_status_bar(f, app, chunks[1]);

    if app.mode == Mode::Search {
        draw_search_modal(f, app, area);
    }
}

fn draw_list(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .filtered
        .iter()
        .map(|idx| list_row(&app.tools[*idx]))
        .collect();

    let title = format!(
        " Quiver — Tools {}{} ",
        type_filter_label(app.type_filter),
        search_label(&app.search_buf),
    );

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");

    let mut state = ListState::default();
    if !app.filtered.is_empty() {
        state.select(Some(app.selected));
    }
    f.render_stateful_widget(list, area, &mut state);
}

fn list_row(m: &ToolMeta) -> ListItem<'static> {
    let ttype = type_label(m.r#type);
    let desc = m
        .description
        .clone()
        .unwrap_or_default()
        .chars()
        .take(80)
        .collect::<String>();
    let line = Line::from(vec![
        Span::styled(format!("{ttype:<6} "), Style::default().dim()),
        Span::styled(format!("{:<32} ", m.name), Style::default().bold()),
        Span::raw(desc),
    ]);
    ListItem::new(line)
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
    let Some(tool) = app.selected_tool() else {
        let p = Paragraph::new("(no tool selected)")
            .block(Block::default().borders(Borders::ALL).title(" Detail "));
        f.render_widget(p, area);
        return;
    };

    let mut lines: Vec<Line> = Vec::new();
    push_kv(&mut lines, "id", &tool.id);
    push_kv(&mut lines, "type", type_label(tool.r#type));
    push_kv(&mut lines, "name", &tool.name);
    if let Some(c) = &tool.category {
        push_kv(&mut lines, "category", c);
    }
    if let Some(p) = &tool.install_path {
        push_kv(&mut lines, "install_path", p);
    }
    if let Some(inv) = &tool.invocation {
        push_kv(&mut lines, "invocation", inv);
    }
    if let Some(repo) = &tool.source_repo {
        push_kv(&mut lines, "source_repo", repo);
    }
    if !tool.requires.is_empty() {
        push_kv(&mut lines, "requires", &tool.requires.join(", "));
    }
    if !tool.triggers.is_empty() {
        push_kv(&mut lines, "triggers", &tool.triggers.join(", "));
    }

    if let Some(desc) = &tool.description {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "description",
            Style::default().bold().underlined(),
        )));
        for l in desc.lines() {
            lines.push(Line::from(l.to_string()));
        }
    }
    if let Some(body) = &tool.long_description {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "body",
            Style::default().bold().underlined(),
        )));
        for l in body.lines() {
            lines.push(Line::from(l.to_string()));
        }
    }
    if !tool.examples.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "examples",
            Style::default().bold().underlined(),
        )));
        for ex in &tool.examples {
            lines.push(Line::from(
                serde_json::to_string_pretty(ex)
                    .unwrap_or_else(|_| "(unrenderable example)".into()),
            ));
        }
    }

    let title = format!(" {} ", tool.name);
    let para = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false })
        .scroll((app.detail_scroll, 0));
    f.render_widget(para, area);
}

fn push_kv(lines: &mut Vec<Line<'static>>, k: &str, v: &str) {
    lines.push(Line::from(vec![
        Span::styled(format!("{k:<14}"), Style::default().dim()),
        Span::raw(v.to_string()),
    ]));
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let hint = match app.mode {
        Mode::List => format!(
            "↑↓ nav · Enter detail · / search · Tab type · Esc clear · q quit · {}/{}",
            app.filtered.len(),
            app.tools.len()
        ),
        Mode::Detail => "e edit · ↑↓ scroll · Esc back · q quit".into(),
        Mode::Search => "type to filter · Enter commit · Esc cancel".into(),
    };
    let text = if app.status.is_empty() {
        hint
    } else {
        format!("{} · {}", app.status, hint)
    };
    let para = Paragraph::new(text).style(Style::default().dim());
    f.render_widget(para, area);
}

fn draw_search_modal(f: &mut Frame, app: &App, area: Rect) {
    let modal = centered_rect(area, 60, 3);
    f.render_widget(Clear, modal);
    let para = Paragraph::new(format!("/{}", app.search_buf))
        .block(Block::default().borders(Borders::ALL).title(" Search "));
    f.render_widget(para, modal);
}

fn centered_rect(area: Rect, width_pct: u16, height: u16) -> Rect {
    let inner = area.inner(Margin {
        horizontal: 0,
        vertical: 0,
    });
    let w = inner.width.saturating_mul(width_pct) / 100;
    let h = height.min(inner.height);
    let x = inner.x + (inner.width.saturating_sub(w)) / 2;
    let y = inner.y + (inner.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

fn type_label(t: ToolType) -> &'static str {
    match t {
        ToolType::Skill => "skill",
        ToolType::Plugin => "plugin",
        ToolType::Mcp => "mcp",
        ToolType::Cli => "cli",
        ToolType::Doc => "doc",
    }
}

fn type_filter_label(t: Option<ToolType>) -> String {
    match t {
        Some(t) => format!("· filter: {}", type_label(t)),
        None => String::new(),
    }
}

fn search_label(buf: &str) -> String {
    if buf.is_empty() {
        String::new()
    } else {
        format!(" · search: {buf:?}")
    }
}
