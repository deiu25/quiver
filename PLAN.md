# ToolHub — Claude Code Tool Registry, Recommender & Daily-Task Agent

> **Status:** Planning (supersedes prior install plan in this file — caveman/codeburn/designlang already installed and verified).
>
> **Extraction note:** This plan is written so it can be copy-pasted into `new-app/PLAN.md` in a fresh workspace. Filesystem layout below assumes a brand-new repo, not VantagePrompt. Nothing in VantagePrompt repo is touched.

---

## 1. Context (Why)

User has installed 13+ Claude-Code-related tools across 4 categories:

| Category | Examples | Install location |
|---|---|---|
| **Plugin marketplaces** | `everything-claude-code`, `oh-my-claudecode`, `superpowers`, `karpathy-skills`, `ui-ux-pro-max-skill`, `caveman`, `stitch-skills` | `~/.claude/plugins/`, `~/.claude/skills/` |
| **MCP servers** | `ruflo` (~200 tools), `context7`, `playwright`, etc. | `~/.claude/mcp.json` |
| **Standalone CLIs** | `codeburn`, `designlang`, `caveman` (statusline) | `~/.nvm/.../bin/`, `~/.local/bin/` |
| **Docs / awesome lists** | `claude-howto`, `awesome-design-md`, `karpathy guidelines` | clone-once references |

**Verified live counts:** 43 skills under `~/.claude/skills/`, 8 under `~/.agents/skills/`, 16 enabled plugins, 1 mega MCP server (`ruflo`).

**Pain points user stated:**
1. Hard to remember which tool fits which task.
2. New skills/docs land regularly — onboarding is manual.
3. No measurement of which tool actually helps vs which is dead weight.
4. No bridge between "I have a task" and "tool X is the right answer".

**Outcome wanted:** an app/agent that catalogues, recommends, tracks, and self-extends — usable by the human and by Claude Code itself.

---

## 2. Vision (What)

**ToolHub** = single registry + recommender + usage tracker + auto-onboarder, exposed three ways:

1. **CLI** — `toolhub recommend "task description"` returns top-3 tools with usage examples.
2. **MCP server** — Claude Code itself queries ToolHub mid-session: "what skill fits current task?"
3. **TUI dashboard** (Phase 2) — codeburn-like terminal UI for browsing tools, usage stats, success rate.
4. **Daily-task agent** (Phase 4) — long-running agent that observes session activity, learns which tools succeed, retrains recommender weekly.

Two human personas: **owner** (you, manual queries) + **Claude Code** (programmatic queries via MCP). Same backend.

---

## 3. Stack Pick

User constraint: "**runtime over my knowledge**".

### Decision: Rust + SQLite + Tauri 2 (optional Phase 6)

| Layer | Pick | Why |
|---|---|---|
| Core / CLI | **Rust** (clap, tokio, serde) | Single static binary, ~5–20 ms startup, zero runtime deps, perfect for `~/.local/bin/`. |
| Storage | **SQLite + sqlite-vec + FTS5** | Embedded, zero-config, hybrid keyword + vector search in one DB. No server. |
| Embeddings | **fastembed-rs** (BAAI/bge-small-en-v1.5, 384d) | Native Rust, no Python, no Ollama dep. ~30 MB model, runs CPU-only. |
| MCP server | **rmcp** (official Rust MCP SDK) | Same binary, no extra runtime. |
| Filesystem watcher | **notify-rs** | Detects new skills/plugins instantly. |
| TUI (Phase 2) | **ratatui + crossterm** | Same binary, terminal-native, zero-deps for user. |
| Desktop GUI (Phase 6, optional) | **Tauri 2** | Rust shell + web frontend (SvelteKit or React). |
| Agent loop (Phase 4) | **Rust async + Anthropic SDK** | Or call out to local `claude` CLI for non-blocking sessions. |

**Why not alternatives:**
- **Bun + TypeScript**: 70% of Rust speed, faster to write, but bigger binary, requires Bun runtime present, weaker for FS watchers.
- **Go**: simpler, fast, but vec-search libs less mature; GC pauses irrelevant here but Rust's `serde` + `clap` stronger.
- **Python (FastAPI)**: slowest startup, packaging nightmare for end-users (PyInstaller bloat).
- **PHP/Laravel**: user familiar, but heavyweight server runtime for what's essentially a local agent tool.

**Cross-platform:** Rust + Tauri = Linux/macOS/Windows from one codebase. Pop_OS (user's host) supported natively.

---

## 4. Architecture

```
                    ┌─────────────────────────────────────────────┐
                    │             User filesystem                 │
                    │  ~/.claude/skills/    ~/.agents/skills/     │
                    │  ~/.claude/plugins/   ~/.claude/mcp.json    │
                    │  ~/.claude/projects/*.jsonl  (sessions)     │
                    └────────────────┬────────────────────────────┘
                                     │ notify-rs watch + parse
                                     ▼
   ┌──────────────────┐    ┌──────────────────┐    ┌──────────────────┐
   │   Ingestor       │───▶│   Indexer        │───▶│   SQLite         │
   │ (parse YAML/MD,  │    │ (embed via       │    │ tools, FTS,      │
   │  README, slash)  │    │  fastembed-rs)   │    │ vec0, usage      │
   └──────────────────┘    └──────────────────┘    └────────┬─────────┘
                                                            │
                ┌───────────────────────────────────────────┼────────────────┐
                ▼                ▼                          ▼                ▼
         ┌────────────┐   ┌────────────┐           ┌────────────┐    ┌─────────────┐
         │  CLI       │   │  TUI       │           │ MCP Server │    │ Agent Loop  │
         │ toolhub    │   │ (Phase 2)  │           │ (Phase 3)  │    │ (Phase 4)   │
         │  list      │   │ ratatui    │           │ stdio      │    │ daily       │
         │  recommend │   │ dashboard  │           │ transport  │    │ retrain     │
         │  add       │   └────────────┘           └────────────┘    └─────────────┘
         │  stats     │
         └────────────┘
```

**Key design choices:**
- **Single binary**: `toolhub` CLI subcommands cover everything (`toolhub mcp-server`, `toolhub tui`, `toolhub agent`).
- **Stateless workers, stateful DB**: any subcommand can run independently against shared SQLite.
- **No daemon required for v1**: filesystem rescan on `toolhub sync` (cron) and on every command (cheap diff).
- **Optional daemon mode**: `toolhub watch` runs notify-rs in background for live updates.

---

## 5. Repository Layout

```
new-app/
├── Cargo.toml                      # workspace root
├── README.md
├── PLAN.md                         # this document
├── crates/
│   ├── core/                       # domain types, traits, errors
│   │   ├── src/
│   │   │   ├── tool.rs             # Tool, ToolType, ToolMeta
│   │   │   ├── usage.rs            # UsageEvent, OutcomeScore
│   │   │   ├── source.rs           # GitHubRef, LocalPath, MarketplaceEntry
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   ├── storage/                    # SQLite + migrations + sqlite-vec wrapper
│   │   ├── migrations/
│   │   │   ├── 001_init.sql
│   │   │   ├── 002_fts.sql
│   │   │   └── 003_vec.sql
│   │   └── src/lib.rs
│   ├── ingestion/                  # parsers
│   │   ├── src/
│   │   │   ├── skill_md.rs         # YAML frontmatter + body
│   │   │   ├── plugin_json.rs      # ~/.claude/plugins/installed_plugins.json
│   │   │   ├── mcp_json.rs         # ~/.claude/mcp.json
│   │   │   ├── github_readme.rs    # fetch + parse remote README
│   │   │   ├── session_jsonl.rs    # parse Claude Code session events
│   │   │   └── lib.rs
│   ├── recommender/                # embedding + ranking
│   │   ├── src/
│   │   │   ├── embed.rs            # fastembed-rs wrapper
│   │   │   ├── search.rs           # hybrid FTS + vec search
│   │   │   ├── rerank.rs           # success-rate reweight
│   │   │   └── lib.rs
│   ├── mcp-server/                 # Phase 3 — rmcp impl
│   │   └── src/lib.rs              # tools: search, recommend, add, stats
│   ├── agent/                      # Phase 4 — daily learning loop
│   │   └── src/lib.rs
│   └── cli/                        # binary entry
│       ├── src/
│       │   ├── main.rs
│       │   ├── commands/
│       │   │   ├── list.rs
│       │   │   ├── recommend.rs
│       │   │   ├── add.rs
│       │   │   ├── stats.rs
│       │   │   ├── sync.rs
│       │   │   ├── tui.rs          # Phase 2
│       │   │   ├── mcp.rs          # Phase 3
│       │   │   └── agent.rs        # Phase 4
│       │   └── tui/                # ratatui views
│       └── Cargo.toml
├── tests/
│   ├── fixtures/                   # sample skill.md, plugin.json, mcp.json
│   ├── ingestion_test.rs
│   ├── recommender_test.rs
│   └── e2e_test.rs
├── docs/
│   ├── architecture.md
│   ├── adding-a-source-type.md     # extension point doc
│   └── mcp-tools.md
└── .github/workflows/
    └── ci.yml                      # cargo test + clippy + fmt
```

---

## 6. Data Model (SQLite)

```sql
-- Source of every catalogued item
CREATE TABLE tools (
    id              TEXT PRIMARY KEY,           -- e.g. "skill:design-md", "plugin:caveman@caveman", "mcp:ruflo"
    type            TEXT NOT NULL,              -- skill | plugin | mcp | cli | doc
    name            TEXT NOT NULL,
    source_repo     TEXT,                       -- github URL or null
    install_path    TEXT,                       -- local path
    description     TEXT,                       -- one-liner from frontmatter
    long_description TEXT,                      -- full README/body
    category        TEXT,                       -- design | testing | refactor | docs | observability | ...
    triggers        TEXT,                       -- JSON: ["task patterns", "keywords"]
    examples        TEXT,                       -- JSON: [{input, output}]
    invocation      TEXT,                       -- "/caveman", "skills add X", "designlang grade <url>"
    requires        TEXT,                       -- JSON: ["mcp:stitch", "node20+"]
    enabled         INTEGER DEFAULT 1,
    added_at        TIMESTAMP NOT NULL,
    last_seen_at    TIMESTAMP NOT NULL,
    last_used_at    TIMESTAMP
);

CREATE INDEX idx_tools_type ON tools(type);
CREATE INDEX idx_tools_category ON tools(category);

-- Full-text search
CREATE VIRTUAL TABLE tools_fts USING fts5(
    name, description, long_description, triggers, examples, category,
    content='tools', content_rowid='rowid'
);

-- Vector search (sqlite-vec)
CREATE VIRTUAL TABLE tools_vec USING vec0(
    tool_id TEXT PRIMARY KEY,
    embedding FLOAT[384]
);

-- Usage tracking — populated from ~/.claude/projects/*.jsonl
CREATE TABLE usage_events (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    tool_id         TEXT NOT NULL REFERENCES tools(id),
    session_id      TEXT,
    project         TEXT,                       -- "vantageprompt" etc.
    task_text       TEXT,                       -- inferred from preceding user msg
    outcome         TEXT,                       -- success | failure | abandoned | unknown
    duration_ms     INTEGER,
    cost_usd        REAL,
    occurred_at     TIMESTAMP NOT NULL
);

CREATE INDEX idx_usage_tool ON usage_events(tool_id, occurred_at DESC);

-- Aggregated success scores (recomputed nightly)
CREATE TABLE tool_scores (
    tool_id         TEXT PRIMARY KEY REFERENCES tools(id),
    success_rate    REAL,                       -- 0.0–1.0
    sample_size     INTEGER,
    avg_cost_usd    REAL,
    median_duration_ms INTEGER,
    score_updated_at TIMESTAMP
);

-- Source registrations — lets toolhub re-pull when upstream changes
CREATE TABLE sources (
    id              TEXT PRIMARY KEY,           -- "gh:juliusbrussee/caveman"
    type            TEXT NOT NULL,              -- github | local-dir | url
    location        TEXT NOT NULL,
    last_pulled_at  TIMESTAMP,
    last_commit_sha TEXT
);
```

---

## 7. Phased Roadmap

### Phase 1 — MVP Registry (≈ 2 weeks)
**Deliverable:** `toolhub list` shows every installed skill/plugin/MCP, `toolhub recommend "task"` returns top-3.

- [ ] Workspace + crates skeleton (`cargo new --workspace`).
- [ ] Schema + migrations via `refinery`.
- [ ] Ingestor: parse `~/.claude/skills/*/SKILL.md` (YAML frontmatter), `~/.agents/skills/*`, `installed_plugins.json`, `mcp.json`.
- [ ] Embed each tool's `name + description + triggers` into vec0.
- [ ] CLI: `list`, `sync`, `recommend "<task>"`, `info <id>`.
- [ ] Hybrid search: FTS5 BM25 + cosine similarity, weighted 0.4/0.6.
- [ ] Tests: 10+ sample fixtures covering each tool type.

**Verify:** run `toolhub recommend "extract design tokens from a marketing page"` — top result must be `designlang` (or `design-md` / `enhance-prompt`).

### Phase 2 — TUI Dashboard (≈ 1 week)
**Deliverable:** `toolhub tui` opens ratatui app: tool list, search, detail view, recent usage.

- [ ] Ratatui screens: List → Detail, modal search, status bar.
- [ ] Live filter by category / type.
- [ ] Open install path in `$EDITOR` from detail view.

**Verify:** keyboard-navigate 60 tools in <300 ms render budget.

### Phase 3 — MCP Server (≈ 1 week)
**Deliverable:** Claude Code can call `toolhub.recommend(task)` mid-session.

- [ ] Implement MCP server using `rmcp` over stdio.
- [ ] Expose tools: `recommend(task: string)`, `search(query: string)`, `info(tool_id: string)`, `add_source(url: string)`, `usage_stats(tool_id?)`.
- [ ] Wire into `~/.claude/mcp.json` as a stdio MCP server (one-line config).
- [ ] Document in `docs/mcp-tools.md`.

**Verify:** restart Claude Code → call `toolhub.recommend("write a Tailwind config from a competitor site")` → returns `designlang`.

### Phase 4 — Usage Tracking + Success Scoring (≈ 1.5 weeks)
**Deliverable:** `toolhub stats` shows which tools used most, success rate, dead weight.

- [ ] `session_jsonl.rs` parser — replay Claude Code session JSONL, detect tool invocations (slash-commands, `mcp__*` tool calls, MCP responses).
- [ ] Heuristic outcome scoring: success if no follow-up correction within N turns; failure if user retries or rolls back.
- [ ] Cost extraction from session events (already present in JSONL — same source codeburn uses).
- [ ] Nightly recompute: `toolhub score` (cron-friendly).
- [ ] CLI: `toolhub stats`, `toolhub stats --tool <id>`, `toolhub dead-weight` (tools with 0 usage in 30d).

**Verify:** stats show non-zero usage for `caveman`, `codeburn`, `designlang` after a week of sessions.

### Phase 5 — Auto-Onboard from GitHub URL (≈ 1 week)
**Deliverable:** `toolhub add https://github.com/<owner>/<repo>` → registers automatically.

- [ ] Detect repo type: skill bundle (has `.skills/` or `SKILL.md`), plugin marketplace (has `marketplace.json`), MCP server (has `package.json` w/ `mcp-server`), CLI (has `bin` field), doc collection.
- [ ] Fetch README + recursively any `*.skill.md` / `plugin.json`.
- [ ] LLM-assisted metadata extraction (optional, falls back to regex): use local `claude` CLI or Anthropic API to fill `triggers`, `examples`, `category` from README.
- [ ] Register in `sources` table for future re-pull.
- [ ] CLI: `toolhub add <url>`, `toolhub update [<source>]`, `toolhub remove <source>`.

**Verify:** `toolhub add https://github.com/google-labs-code/stitch-skills` → 8 new skills appear in `toolhub list`.

### Phase 6 — Daily-Task Agent + Learning Loop (≈ 2 weeks)
**Deliverable:** Long-running agent observes work, suggests tools proactively, learns from outcomes.

- [ ] Daemon: `toolhub agent start` — watches `~/.claude/projects/*.jsonl` tail.
- [ ] On new user message in any session: extract task intent, call recommender, optionally inject suggestion into session as MCP "hint" tool result (or write to `~/.claude/hints/<session>.md` for caveman-style hook).
- [ ] Reinforcement: feedback signal from outcome (success/fail) updates per-tool weights.
- [ ] Weekly digest: `toolhub digest` → markdown report (top tools, deprecated suggestions, new arrivals).
- [ ] Optional: Anthropic Claude Haiku 4.5 backend for cheap task-classification calls.

**Verify:** after 2 weeks of use, `toolhub digest` produces a readable weekly report.

### Phase 7 (optional) — Tauri 2 Desktop GUI (≈ 2 weeks)
**Deliverable:** native desktop dashboard for non-terminal users.

- [ ] Tauri 2 shell, SvelteKit frontend.
- [ ] Same Rust core via crate import (no IPC duplication).
- [ ] Visualizations: tool usage timeline, category heatmap, dependency graph (skills that need MCPs).

---

## 8. Extension Points (the "scalable" requirement)

User said: "when something new appears, I want to be able to implement it." Three extension surfaces:

### 8.1. New tool **type** (e.g. browser extensions, LangChain agents)
Add a variant to `core::tool::ToolType` enum + a parser in `ingestion/src/<type>.rs` implementing the `Source` trait:

```rust
pub trait Source {
    fn discover(&self) -> Vec<ToolMeta>;
    fn supports(&self, location: &str) -> bool;
}
```

One file, registered in a `Vec<Box<dyn Source>>` in `cli/src/main.rs`. ~50 LOC per new type.

### 8.2. New **source provider** (e.g. GitLab, sourcehut, npm registry)
Implement `SourceProvider` trait in `ingestion/src/providers/`. Auto-detected by URL pattern.

### 8.3. New **recommender ranking signal** (e.g. team usage, semantic version freshness)
Add a `Reranker` in `recommender/src/rerank.rs`. Composable pipeline.

### 8.4. New **MCP tool exposed**
Add a method to the `rmcp` server in `mcp-server/src/lib.rs`. ~20 LOC + doc.

All extension points documented in `docs/adding-a-source-type.md`.

---

## 9. Non-functional Requirements

| Concern | Target |
|---|---|
| Cold-start CLI | < 30 ms |
| `toolhub recommend` latency | < 50 ms (60 tools, local embedding) |
| Memory footprint | < 50 MB resident |
| DB size at 200 tools | < 10 MB |
| Embedding model download | one-time, ~30 MB, lazy on first run |
| Cross-platform | Linux ✓, macOS ✓, Windows (WSL ok, native later) |
| Offline mode | All Phase 1–4 fully offline. Phase 5 needs GitHub access. Phase 6 LLM calls optional. |

---

## 10. Success Criteria (measurable)

By end of Phase 4 (≈ 6 weeks of work):

1. `toolhub list` returns all 51+ currently installed skills + 16 plugins + ruflo MCP without manual configuration.
2. `toolhub recommend "<random task description>"` returns a relevant tool ≥ 80% of the time on a 50-task benchmark set we curate during Phase 1.
3. `toolhub stats` correctly attributes usage of `/caveman`, `/superpowers:*`, `mcp__ruflo__*` from session JSONL.
4. Adding a new skill bundle from GitHub takes 1 command and < 30 seconds.
5. Recommender accuracy improves measurably after Phase 4 reranking lands (track via the same benchmark set).

---

## 11. Out of Scope (explicit)

- **Not** a replacement for Claude Code's plugin system — ToolHub *catalogs* what Claude Code already manages.
- **Not** a marketplace — no hosting, no distribution. Source-of-truth stays GitHub/upstream.
- **Not** sandboxed execution — ToolHub never runs the tools it catalogs.
- **No** team features in v1 (multi-user sync). Single-user, single-machine.
- **No** Windows-native build until Phase 6 (WSL works fine in the interim).

---

## 12. Risks & Mitigations

| Risk | Likelihood | Mitigation |
|---|---|---|
| `sqlite-vec` extension load fragility on macOS Apple Silicon | Med | Bundle precompiled extension in `vendor/`; fall back to brute-force cosine in pure Rust if extension fails (acceptable at <500 tools). |
| Embedding model accuracy on terse YAML frontmatter | Med | Concatenate `name + description + triggers + examples` before embedding; tune weights in benchmark loop. |
| Session JSONL format drift across Claude Code releases | Med | Version-detect via `meta.claude_code_version` field; keep parser adapters per major version (similar to codeburn's approach). |
| Heuristic outcome scoring noisy | High | Allow manual feedback (`toolhub feedback <session> <tool> success|failure`); weight manual signals 5× heuristic. |
| Scope creep into "yet another agent framework" | High | Strict MVP gate: Phase 1 ships before any Phase 2+ work begins. Each phase has a hard stop demo. |

---

## 13. First-week Concrete Tasks (when execution starts)

1. `mkdir new-app && cd new-app && cargo init --name toolhub`.
2. Create workspace `Cargo.toml` with the 7 crates listed in §5.
3. Wire `clap` derive in `crates/cli/src/main.rs` with stub commands.
4. Write `crates/storage/migrations/001_init.sql` matching §6 schema.
5. Implement `crates/ingestion/src/skill_md.rs` — parse one real skill from `~/.claude/skills/design-md/` as fixture.
6. End-of-week 1 demo: `toolhub list` reads exactly the design-md skill from disk.

---

## 14. Verification Strategy

**Per-phase smoke test:**
- Phase 1: `cargo test -p ingestion -p recommender && toolhub list | wc -l` → ≥ 50.
- Phase 2: manual TUI walkthrough on a recording.
- Phase 3: integration test that spawns the MCP server and issues a JSON-RPC `tools/call` for `recommend`.
- Phase 4: replay a known session JSONL, assert `usage_events` count matches expected slash-command count ± 5%.
- Phase 5: `toolhub add https://github.com/google-labs-code/stitch-skills` → 8 rows added.
- Phase 6: 7-day live-run, check `digest` output is readable and accurate.

**Continuous:** GitHub Actions runs `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt -- --check` on every push.

---

## 15. Files to Modify When Execution Starts

This plan creates a brand-new repo (`new-app/` or wherever the user extracts it). **No existing VantagePrompt file is touched.**

Initial commit creates:
- `new-app/Cargo.toml`
- `new-app/PLAN.md` (copy of this document)
- `new-app/README.md`
- `new-app/crates/{core,storage,ingestion,recommender,cli}/Cargo.toml` + `src/lib.rs` stubs
- `new-app/.github/workflows/ci.yml`
- `new-app/.gitignore`

That's it for week 1 setup.
