//! Quiver MCP server — exposes the catalog over stdio so Claude Code can
//! call `recommend / search / info / add_source / usage_stats` mid-session.

pub mod schema;
pub mod server;

pub use server::{QuiverServer, default_db_path, serve_stdio};
