# ToolHub

Claude Code tool registry, recommender, MCP server, and daily-task agent.

Catalogs locally-installed skills, plugins, and MCP servers. Recommends the right tool for each task. Tracks usage outcomes. Writes session hints in the background.

**Status:** Phase 2 complete — `toolhub list / sync / recommend / tui` operational. 7-crate workspace, SQLite + FTS5 + sqlite-vec, fastembed embeddings, ratatui dashboard. Phases 3–7 pending.

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

# Browse catalogued tools in interactive TUI
toolhub tui
```

---

## Commands (Phase 2)

### `toolhub sync`

Re-scans the filesystem, upserts tools into SQLite, re-embeds every tool with BAAI/bge-small-en-v1.5 (384-dim, CPU-only).

Sources scanned:
- `~/.claude/skills/` — standalone SKILL.md files
- `~/.agents/skills/` — agents directory
- `~/.claude/plugins/cache/` — marketplace plugin bundles
- `~/.claude/plugins/installed_plugins.json` — plugin manifests
- `~/.claude/mcp_servers.json` — MCP server entries

```bash
toolhub sync
# synced N tool(s) → /home/user/.local/share/toolhub/toolhub.sqlite
# embedded N tool(s)
```

### `toolhub list`

Prints all catalogued tools with their id, kind, and description.

### `toolhub recommend <task>`

Hybrid search: 0.6 × cosine (vec0) + 0.4 × BM25 (FTS5). Returns top 3.

```bash
toolhub recommend "extract design tokens from a marketing page"
#  score  id                                        description
# ------------------------------------------------------------
#  0.842  skill:design-md                           Generate semantic design system
#  0.721  skill:enhance-prompt                      Transform vague UI ideas
#  0.633  cli:designlang                            Grade designs from URL
```

### `toolhub tui`

Interactive ratatui dashboard. Same SQLite DB as the CLI — read-only view.

| Mode | Key | Action |
|---|---|---|
| List | ↑↓ / PgUp / PgDn / Home / End | navigate |
| List | Enter | open Detail |
| List | `/` | open Search modal |
| List | Tab | cycle type filter (None → Skill → Plugin → Mcp → Cli → Doc → None) |
| List | Esc | clear all filters |
| Detail | ↑↓ / PgUp / PgDn | scroll body |
| Detail | `e` | open `install_path` in `$EDITOR` |
| Detail | Esc / Backspace | back to List |
| Search | type | live filter |
| Search | Enter | commit |
| Search | Esc | cancel, restore prior filter |
| any | `q` / Ctrl+C | quit |

### `toolhub info <id>`

Stub — Phase 1 follow-up. Will print full ToolMeta dump for one id.

---

## Stack

| Component | Library | Notes |
|---|---|---|
| Language | Rust, edition 2024, stable | Single static binary |
| Storage | SQLite + refinery | Migrations 001–004 |
| Vector search | sqlite-vec (`vec0`, cosine) | 384-dim |
| Full-text search | FTS5 | BM25 |
| Embeddings | fastembed-rs (BAAI/bge-small-en-v1.5) | CPU-only, ~30 MB |
| TUI | ratatui 0.29 + crossterm 0.28 | `toolhub tui` |
| CLI | clap (derive) + tokio | Async runtime |

Planned (Phase 3+): rmcp (MCP server), notify-rs (FS watch), Anthropic SDK (agent classifier).

---

## Crate layout

```
crates/
  core/          domain types, traits, errors
  storage/       SQLite + migrations + sqlite-vec wrapper
  ingestion/     parsers: skill_md, plugin_json, mcp_json, walker
  recommender/   embed, hybrid search
  mcp-server/    stub (Phase 3)
  agent/         stub (Phase 6)
  cli/           binary entry point (name: toolhub)
                 ├── commands/  list, sync, recommend, tui
                 └── tui/       app, event, filter, view
```

---

## DB schema (current)

| Table | Purpose | Migration |
|---|---|---|
| `tools` | id, name, kind, description, install_path, triggers, examples | 001 |
| `usage_events` | tool_id, session_id, project, task_text, outcome, occurred_at | 001 |
| `tool_scores` | tool_id, success_rate, sample_size, score_updated_at | 001 |
| `sources` | id, kind (github/local-dir/url), location, last_pulled_at | 001 |
| `tools_fts` | FTS5 virtual table over tools | 002 |
| `tools_vec` | vec0 virtual table (384-dim) | 003 |
| `embeddings` | raw f32 vectors per tool id | 004 |

DB location: `$XDG_DATA_HOME/toolhub/toolhub.sqlite` (default `~/.local/share/toolhub/`).
Model cache: `$XDG_CACHE_HOME/fastembed/` (default `~/.cache/fastembed/`).

---

## Roadmap

| Phase | Status | Description |
|---|---|---|
| 1 | ✅ Done | Workspace, migrations, ingestion (SKILL.md + plugin JSON + MCP JSON), hybrid recommender, `list / sync / recommend` |
| 2 | ✅ Done | TUI dashboard (`toolhub tui`): list, detail, search modal, type filter, `$EDITOR` launch |
| 3 | ⬜ Next | MCP server (rmcp stdio): expose `recommend / search / info / add_source / usage_stats` to Claude Code mid-session |
| 4 | ⬜ Pending | Usage tracking + success scoring: `session_jsonl.rs` parser, heuristic outcomes, `toolhub stats / dead-weight` |
| 5 | ⬜ Pending | Auto-onboard from GitHub URL: `toolhub add / update / remove`, repo-type detection, optional LLM metadata extraction |
| 6 | ⬜ Pending | Daily-task agent + learning loop: `notify-rs` tail of session JSONL, hint files, weekly `digest` |
| 7 | ⬜ Optional | Tauri 2 + SvelteKit/React desktop GUI |

See [PLAN.md](./PLAN.md) (gitignored, local) for full per-phase deliverables, schema, risks, verification gates.

---

## What's needed after Phase 2

To unlock Phase 3 (MCP server), the next concrete tasks are:

1. **`crates/mcp-server/` — implement `rmcp` server** over stdio. Expose four tools wired to existing core APIs:
   - `recommend(task: string) → top-3` — call `recommender::search::hybrid_from_score_maps` (already in `crates/recommender/src/search.rs`)
   - `search(query: string) → matches` — call `storage::fts::search`
   - `info(tool_id: string) → ToolMeta` — fix the `Cmd::Info` stub in [`crates/cli/src/main.rs:50`](crates/cli/src/main.rs#L50) and reuse here
   - `usage_stats(tool_id?) → scores` — Phase 4 dependency, return empty until `tool_scores` is populated
2. **CLI subcommand** `toolhub mcp-server` to launch the rmcp loop (mirror the `tui` slicing pattern in [`crates/cli/src/commands/`](crates/cli/src/commands/))
3. **MCP registration**: document `~/.claude/mcp_servers.json` snippet so Claude Code picks up the server on restart
4. **Integration test**: spawn `toolhub mcp-server`, send JSON-RPC `tools/call` for `recommend("write a Tailwind config from a competitor site")`, assert top result is `designlang` / `design-md`

Phase 4 (usage tracking) prerequisites already in DB (migrations 001 created `usage_events` + `tool_scores`); just needs the JSONL parser + `score / stats / dead-weight` subcommands.

---

## Development

```bash
cargo build                              # debug
cargo build --release                    # release, ~30s cold
cargo test --workspace                   # all tests
cargo test -p toolhub-cli --bins         # TUI logic tests (30 in tui::*)
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

Log level via `RUST_LOG` env var (default: `info`, `refinery_core=warn`).

---

## Known gaps (post-Phase 2)

- `toolhub info <id>` — stub, prints "not yet implemented"
- No MCP server yet (Phase 3)
- No usage tracking yet (Phase 4) — `tool_scores` table empty, recommender skips reranking step
- No GitHub `add` flow (Phase 5)
- No background agent (Phase 6)
- 2 pre-existing clippy errors in [`commands/list.rs:10`](crates/cli/src/commands/list.rs#L10) (empty format string) and [`commands/sync.rs:103`](crates/cli/src/commands/sync.rs#L103) (`&PathBuf` → `&Path`) — Phase 1 debt
