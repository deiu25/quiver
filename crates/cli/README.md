# toolhub

Local tool registry, recommender, and background agent for Claude Code.

This crate ships the `toolhub` binary. It's the user-facing entry point of the [Quiver](https://github.com/deiu25/quiver) workspace — eight crates that catalog every Claude Code skill, plugin, and MCP server on your machine, embed them into a local SQLite database with `sqlite-vec` + FTS5, and recommend the best three for any task in <50 ms.

See the [workspace README](https://github.com/deiu25/quiver#readme) for install instructions, the full command reference, MCP integration, and the architecture overview.

## Install

```bash
cargo install --path crates/cli
```

## Subcommands

`sync`, `list`, `recommend`, `tui`, `serve`, `score`, `stats`, `dead-weight`, `add`, `update`, `remove`, `agent`, `digest`, `mcp`. See the [workspace README](https://github.com/deiu25/quiver#commands) for full docs.

## License

Dual-licensed under MIT or Apache-2.0.
