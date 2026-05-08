//! Re-export of the shared `quiver-llm` task classifier so the agent loop
//! and existing test call sites keep working without `quiver_llm::` prefixes.
//!
//! The implementation lives in [`quiver_llm::task`]; this module exists only
//! to preserve the historical `quiver_agent::classify::*` paths used by
//! `crates/agent/tests/classify.rs` and downstream embedders.

pub use quiver_llm::backend::ClaudeBackend;
pub use quiver_llm::task::{
    ClassifiedTask, NoopClassifier, SonnetClassifier, TaskClassifier, parse_classify_json,
};
