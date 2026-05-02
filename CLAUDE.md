# ToolHub — Claude Code project memory

> Single-line pitch: Claude Code tool registry, recommender, MCP server, and daily-task agent. Catalogs locally-installed skills/plugins/MCP servers, recommends the right one per task, tracks usage outcomes, self-extends.

**Status:** Phase 1 (MVP). Skeleton only — `Cargo.toml` declares binary `toolhub`, `src/main.rs` is hello-world. Workspace conversion pending.

**Authoritative design:** [PLAN.md](./PLAN.md) (~23 KB, 7-phase roadmap, full schema, risks, verification gates).

---

## Stack

- **Lang:** Rust, edition 2024, MSRV = stable.
- **Storage:** SQLite + `sqlite-vec` (384d vec0) + FTS5. Migrations via `refinery`.
- **Embeddings:** `fastembed-rs` with BAAI/bge-small-en-v1.5 (384 dim, ~30 MB, CPU-only).
- **MCP:** `rmcp` (official Rust SDK), stdio transport.
- **TUI (Phase 2):** `ratatui` + `crossterm`.
- **FS watch:** `notify-rs`.
- **Desktop (Phase 6, optional):** Tauri 2 + SvelteKit/React.
- **CLI:** `clap` derive + `tokio` + `serde`.

No Python, no Node, no Ollama runtime deps. Single static binary target.

---

## Layout (target — see PLAN §5)

```
crates/
  core/         # domain types, traits, errors
  storage/      # SQLite + migrations + sqlite-vec wrapper
  ingestion/    # parsers (skill_md, plugin_json, mcp_json, github_readme, session_jsonl)
  recommender/  # embed, hybrid FTS+vec search, rerank
  mcp-server/   # Phase 3 — rmcp impl
  agent/        # Phase 4 — daily learning loop
  cli/          # binary entry, name = `toolhub`
tests/fixtures/
docs/
```

Currently: single-crate skeleton. Convert to workspace per PLAN §13 week-1 tasks.

---

## Build / Test / Lint

```bash
cargo build                              # debug
cargo build --release                    # release
cargo check                              # fast type-check
cargo test                               # workspace tests
cargo test -p <crate>                    # per-crate
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

CI gate (planned): all four — `test`, `clippy -D warnings`, `fmt --check`.

---

## Conventions (project-specific — extends global rules under `~/.claude/rules/common/`)

- **Workspace crate names match dirs.** Binary lives in `cli/` crate, binary name `toolhub`.
- **Migrations:** `crates/storage/migrations/NNN_name.sql`. Sequential, never edit applied migrations.
- **Embedding dimensionality is 384.** Don't change without a `tools_vec` migration.
- **Ingestion parsers** implement the `Source` trait (PLAN §8.1) — one file per source type.
- **No daemon required for v1.** Cheap rescan on every command. `toolhub watch` opt-in for live updates.
- **Single binary.** Subcommands cover everything (`toolhub recommend|list|sync|tui|mcp-server|agent`).

---

## Performance budgets (PLAN §9)

| Concern | Target |
|---|---|
| Cold-start CLI | <30 ms |
| `toolhub recommend` | <50 ms (60 tools) |
| Resident memory | <50 MB |
| DB at 200 tools | <10 MB |

Treat regressions on these as blocking.

---

## Out of scope (PLAN §11)

- Not a marketplace, not a sandbox, no team/multi-user features in v1.
- No Windows-native build until Phase 6 (WSL fine in interim).
- No execution of catalogued tools — ToolHub only catalogs.

---

## Git workflow (project rule)

- **Remote:** `git@github.com:deiu25/quiver.git` (origin), default branch `main`.
- **After every implementation step**, commit + push to `origin main`. No batching multiple unrelated changes into one commit.
- Commit messages follow conventional format (`<type>: <description>` — see global `~/.claude/rules/common/git-workflow.md`).
- Never `--force` push to `main`. Never `git add -A` blindly — review with `git status` first.
- Attribution disabled globally; no Co-Authored-By footer.

## Pointers

- [PLAN.md](./PLAN.md) — design, schema, roadmap, risks, verification gates
- [README.md](./README.md) — short pitch
- `.omc/project-memory.json` — oh-my-claudecode session state (gitignored)

When in doubt, read PLAN.md before changing architecture.
