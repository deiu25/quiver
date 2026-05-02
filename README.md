# ToolHub

Claude Code tool registry, recommender, and daily-task agent.

> **Status:** planning. See [PLAN.md](./PLAN.md) for the full architecture, stack rationale, phased roadmap, data model, and verification strategy.

## Quick links

- [PLAN.md](./PLAN.md) — single source of truth for the design
- Stack: Rust + SQLite (sqlite-vec + FTS5) + fastembed-rs + rmcp (MCP server) + ratatui (TUI) + Tauri 2 (optional desktop)
- Roadmap: 6 phases, MVP in ~2 weeks, full system ~8–10 weeks

## Why

13+ Claude Code skills, plugin marketplaces, and MCP servers installed locally.
Hard to remember which fits which task. ToolHub catalogs them, recommends the right one, tracks usage, and self-extends when new tools land.

## Next step

```bash
cargo init --name toolhub
# then follow PLAN.md §13 "First-week Concrete Tasks"
```
