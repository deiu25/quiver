<div align="center">

# ToolHub

**Local tool registry, recommender, and background agent for Claude Code.**

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust 2024](https://img.shields.io/badge/rust-2024%20edition-orange.svg)](https://www.rust-lang.org)
[![Status](https://img.shields.io/badge/status-active-brightgreen.svg)](#roadmap)

</div>

---

## Why ToolHub?

Claude Code lets you install hundreds of skills, plugins, and MCP servers. After a few months you have so many that you can't remember what you have, which one fits the task at hand, and which ones you never actually use. The CLI gives you no way to search, rank, or prune them.

ToolHub is a single static binary that catalogs every tool on your machine, embeds them into a local SQLite database with `sqlite-vec` + FTS5, and recommends the best three for any task in <50 ms. It plugs into Claude Code as an MCP server (so you can ask for recommendations mid-session), and a background agent watches your sessions to learn which tools you actually accept — feeding that signal back into the ranker.

No telemetry. No cloud. No API keys. The embedding model runs on CPU and weighs ~30 MB.

---

## Features

- **Hybrid search** — 0.6 × cosine (sqlite-vec) + 0.4 × BM25 (FTS5), reranked by per-tool success rate from your real usage.
- **Catalogs everything** — standalone `SKILL.md` files, plugin marketplaces, MCP servers, and CLIs cloned from GitHub.
- **Mid-session integration** — stdio MCP server (`rmcp` 1.6) exposes 5 tools to Claude Code while you work.
- **Self-improving** — replays session JSONL into `usage_events`, scores tools by outcome, boosts hits that worked.
- **Background agent** — tails `~/.claude/projects/*.jsonl`, drops a hint markdown per session, tracks which suggestions you actually invoke.
- **GitHub onboarding** — `toolhub add <url>` clones any tool repo, auto-detects its kind, and ingests its tools.
- **Interactive TUI** — `ratatui` dashboard with search, type filter, and `$EDITOR` jump.

---

## Demo

```console
$ toolhub recommend "extract design tokens from a marketing page"
  score   id                                 description
  0.842   skill:design-md                    Generate semantic design system
  0.721   skill:enhance-prompt               Transform vague UI ideas
  0.633   cli:designlang                     Grade designs from URL

$ toolhub stats --top 3
 tool_id                       success_rate   sample_size
 skill:caveman                  92%            38
 skill:design-md                88%            17
 mcp:context7                   84%            25
```

---

## Install

```bash
git clone https://github.com/deiu25/quiver.git
cd quiver
cargo build --release
cp target/release/toolhub ~/.local/bin/
```

Or install directly with cargo:

```bash
cargo install --path crates/cli
```

**Requirements:** Rust stable (2024 edition). On first `sync`, ToolHub downloads the BAAI/bge-small-en-v1.5 model (~30 MB) into `$XDG_CACHE_HOME/fastembed`.

---

## Quick start

```bash
toolhub sync                                              # scan + index every tool
toolhub recommend "extract design tokens"                 # top-3 hybrid search
toolhub tui                                               # browse interactively
toolhub add https://github.com/google-labs-code/stitch    # onboard a new source
toolhub agent                                             # background hint writer
```

---

## Claude Code integration

Append to `~/.claude/mcp_servers.json`:

```json
{ "mcpServers": { "toolhub": { "command": "toolhub", "args": ["mcp"] } } }
```

Restart Claude Code. Five MCP tools become available mid-session:

| Tool | Behaviour |
|---|---|
| `recommend(task, k?)` | Hybrid vec+FTS top-k. Lazy fastembed init on first call. |
| `search(query, k?)` | Pure FTS5 BM25. Faster than `recommend` for known terms. |
| `info(tool_id)` | Full `ToolMeta` JSON. Returns `null` if unknown. |
| `add_source(url, type?)` | Clone a repo, ingest its tools, persist a `sources` row. |
| `usage_stats(tool_id?)` | Read `tool_scores`. Detail mode includes the 5 most-recent events. |

---

## Commands

### Catalog & search

| Command | Purpose |
|---|---|
| `toolhub sync` | Re-scan `~/.claude/skills`, `~/.claude/plugins`, `~/.claude/mcp_servers.json`, etc. Re-embeds every tool. |
| `toolhub list` | Print every catalogued tool with id, kind, description. |
| `toolhub recommend <task>` | Hybrid search + success-rate rerank. Returns top 3. |
| `toolhub tui` | Interactive dashboard (`/` = search, `Tab` = type filter, `e` = open in `$EDITOR`, `q` = quit). |
| `toolhub info <id>` | Print full metadata for one tool. _(stub — coming soon)_ |

### Usage tracking

| Command | Purpose |
|---|---|
| `toolhub score [--sessions-dir <path>]` | Replay session JSONL into `usage_events`, rebuild `tool_scores`. Idempotent on `tool_use.uuid`. |
| `toolhub stats [--tool <id>] [--top N] [--json]` | List by success rate, or detail one tool's recent events. |
| `toolhub dead-weight [--days N]` | Tools with zero usage in the last N days (default 30). |

Outcome heuristic per `tool_use` event: `success` (clean `tool_result`), `failure` (`is_error: true`), `abandoned` (no result before EOF), or `unknown`.

### Source onboarding

| Command | Purpose |
|---|---|
| `toolhub add <url>` | Clone, auto-detect kind, ingest tools, register source with `last_commit_sha`. Accepts `https://`, `gh:`, or `git@` URLs. |
| `toolhub update [<source>]` | Re-pull one or every registered GitHub source. Skips no-op updates by SHA. |
| `toolhub remove <source>` | Drop every tool from `source`, then delete the row. FK cascades the embedder index. |

### Background agent

| Command | Purpose |
|---|---|
| `toolhub agent [--sessions-dir <path>] [--hints-dir <path>]` | Foreground watcher. On every new user message: run the recommender, atomically write top-3 to `<hints-dir>/<session>.md`, log the top-1 to `agent_suggestions`. Acceptance flips when you invoke the suggested tool within 60 min. `recompute_scores` runs every 60 s / 50 events. Wrap with tmux/systemd for long runs. |
| `toolhub digest --days N [--out <path>]` | Markdown report: top tools, suggestion acceptance rate, dead weight, new arrivals. |

### MCP server

| Command | Purpose |
|---|---|
| `toolhub mcp` | Run the stdio MCP server. JSON-RPC on stdin/stdout, logs on stderr. Built on `rmcp` 1.6 with `tool_router` macros. |

---

## Configuration

| Variable / path | Default | Purpose |
|---|---|---|
| `$XDG_DATA_HOME/toolhub/toolhub.sqlite` | `~/.local/share/toolhub/` | Main SQLite DB. |
| `$XDG_CACHE_HOME/fastembed/` | `~/.cache/fastembed/` | Embedding model cache. |
| `~/.claude/projects/` | (Claude Code default) | Sessions root watched by `toolhub agent`. Override with `--sessions-dir`. |
| `~/.claude/hints/` | _agent default_ | Per-session hint markdown output. Override with `--hints-dir`. |
| `RUST_LOG` | `info,refinery_core=warn` | Log level. |

---

## Stack

| Component | Library |
|---|---|
| Language | Rust, edition 2024, stable |
| Storage | SQLite (`rusqlite` bundled) + `refinery` migrations |
| Vector search | `sqlite-vec` (`vec0`, cosine, 384-dim) |
| Full-text search | FTS5 / BM25 |
| Embeddings | `fastembed-rs` (BAAI/bge-small-en-v1.5, CPU-only, ~30 MB) |
| MCP server | `rmcp` 1.6 (`server`, `transport-io`, `macros`, `schemars`) |
| FS watcher | `notify-rs` 6.1 |
| TUI | `ratatui` 0.29 + `crossterm` 0.28 |
| CLI | `clap` (derive) + `tokio` |

---

## Architecture

Workspace with seven crates: `core` (domain types), `storage` (SQLite + migrations), `ingestion` (parsers + onboarding), `recommender` (embed + hybrid search + rerank), `mcp-server`, `agent` (background loop), and `cli` (binary entry point named `toolhub`).

Eight tables: `tools`, `usage_events`, `tool_scores`, `sources`, `tools_fts` (FTS5), `tools_vec` (vec0), `embeddings`, `agent_suggestions`. Six migrations.

Performance budgets: cold-start CLI <30 ms, `recommend` <50 ms over 60 tools, resident memory <50 MB, DB <10 MB at 200 tools.

<!--
crates/
  core/          domain types, traits, errors
  storage/       SQLite + migrations + sqlite-vec wrapper
  ingestion/     parsers + onboard pipeline
  recommender/   embed, hybrid search, reranker
  mcp-server/    rmcp 1.6 stdio server
  agent/         daily-task agent loop
  cli/           binary entry point (name: toolhub)
-->

---

## Roadmap

What's next: a Tauri 2 desktop GUI with usage timelines, category heatmaps, and dependency graphs.

Three deferred polish items (orthogonal, will land any time): cost extraction from JSONL `usage` field, optional Anthropic-SDK README distillation in `add`, and a Haiku 4.5 task classifier in front of the embedder.

---

## Development

```bash
cargo build                                 # debug
cargo build --release                       # release, ~30 s cold
cargo test --workspace                      # all tests (68+)
cargo test -p toolhub-mcp-server            # MCP handler tests
cargo test -p toolhub-cli --bins            # TUI logic tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

---

## Limitations

- `toolhub info <id>` is currently a stub.
- `toolhub agent` runs in the foreground — no daemon mode yet, wrap with `tmux` or `systemd`.
- Linux + macOS only for now; Windows is untested (notify-rs supports it but no CI gate).

---

## Contributing

Issues and PRs welcome. Before opening a PR, please run:

```bash
cargo test --workspace
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

---

## License

Dual-licensed under either of:

- MIT License
- Apache License, Version 2.0

at your option. See [Cargo.toml](Cargo.toml) (`workspace.package.license`) for the canonical declaration.
