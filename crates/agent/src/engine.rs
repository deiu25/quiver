//! Agent loop wiring: notify-rs watcher → tail readers → handlers.
//!
//! Foreground only — runs until Ctrl-C / SIGTERM. The Anthropic-Haiku
//! task-classification backend mentioned in PLAN §7 Phase 6 is deferred;
//! `UserText.text` is sent verbatim to the recommender.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration as StdDuration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::Connection;
use toolhub_core::usage::{Outcome, UsageEvent};
use toolhub_recommender::embed::Embedder;
use toolhub_storage::{open, suggestions, tools as tools_store, usage};

use crate::AgentConfig;
use crate::hint;
use crate::recommend::top_k;
use crate::tail::{TailEvent, TailReader, walk_jsonl};

/// Pending `tool_use` waiting for its `tool_result`. Mirrors the bookkeeping
/// in `session_jsonl::replay` but indexed by tool_use uuid.
#[derive(Debug, Clone)]
struct PendingUse {
    tool_id: String,
    session_id: String,
    task_text: Option<String>,
    occurred_at: DateTime<Utc>,
}

/// Most-recent user text per session — assigned to the next tool_use as
/// `task_text`, matching the replay heuristic.
type LastTexts = HashMap<String, String>;

pub async fn run(cfg: AgentConfig) -> Result<()> {
    if let Some(parent) = cfg.db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !cfg.sessions_dir.exists() {
        anyhow::bail!(
            "sessions_dir does not exist: {}",
            cfg.sessions_dir.display()
        );
    }
    std::fs::create_dir_all(&cfg.hints_dir)
        .with_context(|| format!("create hints_dir {}", cfg.hints_dir.display()))?;

    if let Err(e) = hint::cleanup_stale(&cfg.hints_dir, 7) {
        tracing::warn!("hints cleanup failed: {e}");
    }

    let mut conn = open(&cfg.db_path)?;
    let catalogue = load_catalogue(&conn)?;
    tracing::info!(
        catalogued = catalogue.len(),
        sessions_dir = %cfg.sessions_dir.display(),
        hints_dir = %cfg.hints_dir.display(),
        "agent: indexing existing sessions"
    );

    let mut readers: HashMap<PathBuf, TailReader> = HashMap::new();
    for p in walk_jsonl(&cfg.sessions_dir) {
        match TailReader::at_eof(&p) {
            Ok(r) => {
                readers.insert(p, r);
            },
            Err(e) => tracing::warn!("tail seed {} failed: {e}", p.display()),
        }
    }

    let embedder = Embedder::new().context("init fastembed")?;

    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(tx)?;
    watcher
        .watch(&cfg.sessions_dir, RecursiveMode::Recursive)
        .with_context(|| format!("watch {}", cfg.sessions_dir.display()))?;
    tracing::info!("agent: watching for new prompts. Ctrl-C to exit.");

    let mut last_texts: LastTexts = HashMap::new();
    let mut pending: HashMap<String, PendingUse> = HashMap::new();
    let mut events_since_recompute = 0usize;
    let mut last_recompute = Instant::now();

    loop {
        let next = rx.recv_timeout(StdDuration::from_millis(500));
        match next {
            Ok(Ok(ev)) => {
                handle_fs_event(&ev, &cfg, &mut readers);
                let mut all_events = Vec::new();
                for r in readers.values_mut() {
                    match r.poll() {
                        Ok(mut evs) => all_events.append(&mut evs),
                        Err(e) => tracing::warn!("poll {} failed: {e}", r.path.display()),
                    }
                }
                for tev in all_events {
                    if let Err(e) = dispatch_event(
                        tev,
                        &cfg,
                        &conn,
                        &embedder,
                        &mut last_texts,
                        &mut pending,
                        &catalogue,
                    ) {
                        tracing::warn!("dispatch failed: {e}");
                    } else {
                        events_since_recompute += 1;
                    }
                }
            },
            Ok(Err(e)) => tracing::warn!("notify error: {e}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {},
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                tracing::error!("notify channel disconnected, exiting");
                break;
            },
        }

        let elapsed = last_recompute.elapsed();
        let interval = StdDuration::from_secs(cfg.score_recompute_interval_secs);
        if events_since_recompute >= 50 || (elapsed >= interval && events_since_recompute > 0) {
            match usage::recompute_scores(&mut conn) {
                Ok(n) => tracing::info!("recomputed scores for {n} tool(s)"),
                Err(e) => tracing::warn!("recompute_scores failed: {e}"),
            }
            events_since_recompute = 0;
            last_recompute = Instant::now();
        }
    }
    Ok(())
}

fn handle_fs_event(ev: &Event, cfg: &AgentConfig, readers: &mut HashMap<PathBuf, TailReader>) {
    match ev.kind {
        EventKind::Create(_) | EventKind::Modify(_) => {
            for p in &ev.paths {
                if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                    continue;
                }
                if !readers.contains_key(p) {
                    readers.insert(p.clone(), TailReader::at_start(p));
                    tracing::debug!("agent: new file {}", p.display());
                }
            }
        },
        EventKind::Remove(_) => {
            for p in &ev.paths {
                readers.remove(p);
            }
        },
        _ => {},
    }
    // Best-effort fill-in for platforms with weak Create semantics.
    for p in walk_jsonl(&cfg.sessions_dir) {
        if !readers.contains_key(&p)
            && let Ok(r) = TailReader::at_eof(&p)
        {
            readers.insert(p, r);
        }
    }
}

fn dispatch_event(
    ev: TailEvent,
    cfg: &AgentConfig,
    conn: &Connection,
    embedder: &Embedder,
    last_texts: &mut LastTexts,
    pending: &mut HashMap<String, PendingUse>,
    catalogue: &HashSet<String>,
) -> Result<()> {
    match ev {
        TailEvent::UserText {
            session_id,
            text,
            ts,
        } => {
            last_texts.insert(session_id.clone(), text.clone());
            let hits = top_k(conn, embedder, &text, cfg.top_k)?;
            if let Err(e) = hint::write_hint(&cfg.hints_dir, &session_id, Some(&text), ts, &hits) {
                tracing::warn!("write_hint {session_id} failed: {e}");
            }
            if let Some(top) = hits.first()
                && catalogue.contains(&top.tool_id)
            {
                suggestions::record(
                    conn,
                    &session_id,
                    &top.tool_id,
                    Some(&text),
                    Some(top.score as f64),
                    ts,
                )?;
                tracing::info!(
                    session = %session_id,
                    tool = %top.tool_id,
                    "suggested top-1"
                );
            }
        },
        TailEvent::ToolUse {
            session_id,
            uuid,
            tool_id,
            ts,
        } => {
            if catalogue.contains(&tool_id) {
                let n = suggestions::mark_accepted(
                    conn,
                    &session_id,
                    &tool_id,
                    ts,
                    cfg.acceptance_window_minutes,
                )?;
                if n > 0 {
                    tracing::info!(
                        session = %session_id,
                        tool = %tool_id,
                        accepted = n,
                        "suggestion accepted"
                    );
                }
            }
            let task_text = last_texts.get(&session_id).cloned();
            pending.insert(
                uuid,
                PendingUse {
                    tool_id,
                    session_id,
                    task_text,
                    occurred_at: ts,
                },
            );
        },
        TailEvent::ToolResult {
            session_id: _,
            uuid,
            is_error,
            ..
        } => {
            if let Some(p) = pending.remove(&uuid)
                && catalogue.contains(&p.tool_id)
            {
                let outcome = match is_error {
                    Some(true) => Outcome::Failure,
                    _ => Outcome::Success,
                };
                let evt = UsageEvent {
                    uuid: Some(uuid),
                    tool_id: p.tool_id,
                    session_id: Some(p.session_id),
                    project: None,
                    task_text: p.task_text,
                    outcome,
                    duration_ms: None,
                    cost_usd: None,
                    occurred_at: p.occurred_at,
                };
                if let Err(e) = usage::insert_event(conn, &evt) {
                    tracing::warn!("insert_event failed: {e}");
                }
            }
        },
    }
    Ok(())
}

fn load_catalogue(conn: &Connection) -> Result<HashSet<String>> {
    Ok(tools_store::list_all(conn)?
        .into_iter()
        .map(|m| m.id)
        .collect())
}
