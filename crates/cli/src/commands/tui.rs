use std::io::{self, Stdout};
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use toolhub_storage::{open, tools};

use crate::db_path::default_db_path;
use crate::tui::app::{App, SideEffect};
use crate::tui::{event as tui_event, view};

pub async fn run() -> anyhow::Result<()> {
    tokio::task::spawn_blocking(run_blocking)
        .await
        .context("tui spawn_blocking join")??;
    Ok(())
}

fn run_blocking() -> anyhow::Result<()> {
    let conn = open(&default_db_path()?)?;
    let metas = tools::list_all(&conn)?;
    drop(conn);

    let mut terminal = setup_terminal()?;
    let result = run_app(&mut terminal, App::new(metas));
    restore_terminal(&mut terminal).ok();
    result
}

type Term = Terminal<CrosstermBackend<Stdout>>;

fn setup_terminal() -> anyhow::Result<Term> {
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("enter alternate screen")?;
    Terminal::new(CrosstermBackend::new(stdout)).context("create ratatui terminal")
}

fn restore_terminal(terminal: &mut Term) -> anyhow::Result<()> {
    disable_raw_mode().context("disable raw mode")?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .context("leave alternate screen")?;
    terminal.show_cursor().ok();
    Ok(())
}

fn run_app(terminal: &mut Term, mut app: App) -> anyhow::Result<()> {
    loop {
        terminal.draw(|f| view::draw(f, &app))?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(k) = event::read()? else {
            continue;
        };
        let Some(action) = tui_event::map(app.mode, k) else {
            continue;
        };
        if let Some(SideEffect::OpenEditor(path)) = app.apply(action)
            && let Err(e) = run_editor(terminal, &path)
        {
            app.status = format!("editor failed: {e}");
        }
        if app.should_quit {
            return Ok(());
        }
    }
}

fn run_editor(terminal: &mut Term, path: &Path) -> anyhow::Result<()> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());

    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .ok();

    let status = std::process::Command::new(&editor).arg(path).status();

    enable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )
    .ok();
    terminal.clear().ok();

    let exit = status.with_context(|| format!("spawn {editor:?}"))?;
    if !exit.success() {
        anyhow::bail!("{editor:?} exited with {exit}");
    }
    Ok(())
}
