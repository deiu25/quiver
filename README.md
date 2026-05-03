# ToolHub

Claude Code tool registry, recommender, MCP server, and daily-task agent.

Catalogs locally-installed skills, plugins, and MCP servers. Recommends the right tool for each task. Tracks usage outcomes. Writes session hints in the background.

**Status:** Phase 6 complete — ~293 tools catalogued, hybrid recommender + reranker, foreground agent with learning loop, markdown digest.

---

## Install

```bash
git clone git@github.com:deiu25/quiver.git
cd quiver
cargo build --release
cp target/release/toolhub ~/.local/bin/
```

Requires: Rust stable (2024 edition), ~30 MB disk for the embedding model (downloaded on first sync).

---

## Quick start

```bash
# Scan local Claude Code install and populate DB
toolhub sync

# List all catalogued tools
toolhub list

# Get tool recommendations for a task
toolhub recommend "compress markdown using cave-speak style"

# Run the foreground agent (Ctrl-C to stop)
toolhub agent

# Weekly markdown digest to stdout
toolhub digest --days 7
```

---

## Commands

### `toolhub sync`

Re-scans the filesystem, upserts tools into SQLite, re-embeds every tool with BAAI/bge-small-en-v1.5 (384-dim, CPU-only).

Sources scanned:
- `~/.claude/skills/` — standalone SKILL.md files
- `~/.agents/skills/` — agents directory
- `~/.claude/plugins/cache/` — marketplace plugin bundles (max depth 8, symlink-deduped)
- `~/.claude/installed_plugins.json` — plugin manifests
- `~/.config/claude/claude_desktop_config.json` / `mcp_servers.json` — MCP server entries

```bash
toolhub sync
# synced 293 tool(s), embedded 293 tool(s)
```

### `toolhub list`

Prints all catalogued tools with their id, kind, and description.

### `toolhub recommend <task>`

Hybrid search: 0.6 × cosine (vec0) + 0.4 × BM25 (FTS5), min-max normalised, then reranked by historical success rate. Returns top 3.

```bash
toolhub recommend "write a shell one-liner to rename files"
# 1. skill:shell-wizard  (score 0.84)  One-line shell helper for batch file ops
# 2. skill:caveman       (score 0.71)  Terse caveman-style edits and rewrites
# 3. mcp:filesystem      (score 0.63)  Read/write local files via MCP
```

### `toolhub score [--sessions-dir <path>]`

Replays Claude Code session JSONL files from `~/.claude/projects` into `usage_events` and rebuilds `tool_scores`.

### `toolhub stats [--tool <id>] [--top N] [--json]`

Shows usage statistics from `tool_scores`. List mode shows the top N tools by usage. Detail mode (`--tool`) shows full stats for one tool.

### `toolhub dead-weight [--days N]`

Lists tools with no usage events in the last N days (default 30). Useful for pruning stale installs.

### `toolhub add <url>`

Onboards a tool source from a GitHub URL. Clones the repo, walks it for SKILL.md files, ingests tools, embeds, saves source row.

```bash
toolhub add https://github.com/owner/my-skills-repo
```

### `toolhub update [<source-id>]`

Re-pulls one or all registered GitHub sources and refreshes their tools.

```bash
toolhub update gh:owner/my-skills-repo   # one source
toolhub update                            # all github sources
```

### `toolhub remove <source-id>`

Drops every tool ingested from a source and deletes the source row.

### `toolhub mcp`

Runs the stdio MCP server so Claude Code can call ToolHub mid-session (tool search, recommendations, score queries).

Add to `~/.claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "toolhub": {
      "command": "toolhub",
      "args": ["mcp"]
    }
  }
}
```

### `toolhub agent [--sessions-dir <path>] [--hints-dir <path>]`

Foreground daemon. Watches `~/.claude/projects/**/*.jsonl` with notify-rs. On each new user message:

1. Runs `recommend` against the message text.
2. Writes top-3 to `~/.claude/hints/<session>.md` (atomic temp+rename).
3. Records top-1 in `agent_suggestions` table.
4. Marks suggestions `accepted=1` when the user invokes the suggested tool within 60 min.
5. Recomputes `tool_scores` every 60 s (or every 50 events).

```bash
toolhub agent
# 2026-05-03T12:00:00Z  INFO  watching /home/user/.claude/projects
```

### `toolhub digest [--days N] [--out <path>]`

Generates a markdown report for a sliding window (default 7 days):

- Top tools by usage
- Suggestion acceptance rate
- Dead-weight tools (no usage in window)
- New arrivals (added within window)

```bash
toolhub digest --days 7 --out ~/weekly-toolhub.md
```

---

## Stack

| Component | Library | Notes |
|---|---|---|
| Language | Rust, edition 2024, stable | Single static binary |
| Storage | SQLite + refinery | Migrations 001–006 |
| Vector search | sqlite-vec (`vec0`, cosine) | 384-dim, ANN |
| Full-text search | FTS5 | BM25, porter tokeniser |
| Embeddings | fastembed-rs (BAAI/bge-small-en-v1.5) | CPU-only, ~30 MB |
| FS watch | notify-rs | inotify on Linux |
| MCP server | rmcp (official Rust SDK) | stdio transport |
| TUI | ratatui + crossterm | `toolhub tui` |
| CLI | clap (derive) + tokio | Async, multi-thread |

---

## Crate layout

```
crates/
  core/          domain types, traits, errors
  storage/       SQLite + migrations + sqlite-vec wrapper
  ingestion/     parsers: skill_md, plugin_json, mcp_json, walker
  recommender/   embed, hybrid search, rerank, params
  mcp-server/    rmcp MCP server impl
  agent/         daily-task agent: tail, engine, hint, digest
  cli/           binary entry point (name: toolhub)
tests/fixtures/
```

---

## DB schema (summary)

| Table | Purpose |
|---|---|
| `tools` | id, name, kind, description, source_repo, location |
| `tools_fts` | FTS5 virtual table over tools |
| `tools_vec` | vec0 virtual table (384-dim embeddings) |
| `embeddings` | raw float vectors per tool id |
| `usage_events` | session_id, tool_id, timestamp, uuid |
| `tool_scores` | tool_id, use_count, success_rate, last_used |
| `sources` | id, kind (github/local), location, last_synced |
| `agent_suggestions` | session_id, tool_id, suggested_at, accepted |

DB location: `$XDG_DATA_HOME/toolhub/toolhub.sqlite` (default `~/.local/share/toolhub/`).
Model cache: `$XDG_CACHE_HOME/toolhub/models/` (default `~/.cache/toolhub/`).

---

## Roadmap

| Phase | Status | Description |
|---|---|---|
| 1 | Done | Workspace scaffold, migrations, DB open |
| 2 | Done | Ingestion: SKILL.md, plugin JSON, MCP JSON |
| 3 | Done | MCP server (rmcp stdio) |
| 4 | Done | Hybrid recommender: vec0 cosine + FTS5 BM25 |
| 5 | Done | Usage tracking, score replay, success reranker |
| 6 | Done | Daily-task agent, learning loop, digest command |
| 7 | Optional | Tauri 2 + SvelteKit desktop GUI |

---

## Development

```bash
cargo test --workspace
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

Log level controlled by `RUST_LOG` env var (default: `info`, refinery suppressed to `warn`).
