//! Phase 6 — daily-task agent + learning loop.
//!
//! `run(cfg)` tails Claude Code session JSONL files, calls the recommender
//! whenever a new user message arrives, writes a hint markdown for the
//! current session, records the top-1 as a pending `agent_suggestion`, and
//! marks suggestions accepted when the user actually invokes the suggested
//! tool. `digest(cfg)` produces a markdown report for a sliding window.

pub mod digest;
pub mod hint;
pub mod recommend;
pub mod tail;

mod engine;

use std::path::PathBuf;

pub use digest::digest;
pub use engine::run;

/// Runtime config for the agent loop.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// SQLite path. Default: `default_db_path()` (caller-provided).
    pub db_path: PathBuf,
    /// Root containing `<dir>/<session>.jsonl` files. Default: `~/.claude/projects`.
    pub sessions_dir: PathBuf,
    /// Where to write `<session>.md` hint files. Default: `~/.claude/hints`.
    pub hints_dir: PathBuf,
    /// How long after a suggestion a matching `tool_use` still counts as
    /// "accepted". Default: 60 minutes.
    pub acceptance_window_minutes: i64,
    /// How often the engine recomputes `tool_scores` (seconds).
    pub score_recompute_interval_secs: u64,
    /// Number of recommendations to write into the hint file.
    pub top_k: usize,
}

impl AgentConfig {
    pub fn new(db_path: PathBuf, sessions_dir: PathBuf, hints_dir: PathBuf) -> Self {
        Self {
            db_path,
            sessions_dir,
            hints_dir,
            acceptance_window_minutes: 60,
            score_recompute_interval_secs: 60,
            top_k: 3,
        }
    }
}
