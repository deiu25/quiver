//! Shared Sonnet-backed classifiers used by the daily-task agent and the
//! Claude Code hook layer.
//!
//! Two classifiers ship here:
//!
//!   * [`SonnetClassifier`] / [`task::ClassifiedTask`] — task triage. Drives
//!     the agent loop; filters greetings, rewrites real tasks into focused
//!     queries before embedding.
//!   * [`IntentClassifier`] / [`intent::IntentVerdict`] — read-only vs
//!     mutation. Drives PreToolUse veto suppression: when the user asked for
//!     an explanation/analysis, the hook skips the veto so Claude can read
//!     and grep without being re-routed.
//!
//! Both reuse [`ClaudeBackend`] (Anthropic API or local `claude` CLI),
//! [`backend::call_backend`], and the same fail-open + 15s timeout pattern.

pub mod backend;
pub mod intent;
pub mod task;

pub use backend::{ClaudeBackend, call_backend, detect_backend, label_for};
pub use intent::{IntentClassifier, IntentVerdict, parse_intent_json};
pub use task::{
    ClassifiedTask, NoopClassifier, SonnetClassifier, TaskClassifier, parse_classify_json,
};
