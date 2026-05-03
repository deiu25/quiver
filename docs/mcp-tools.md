# ToolHub MCP Tools

ToolHub exposes its catalog over the Model Context Protocol so Claude Code (or any MCP client) can call it mid-session.

## Wire-up

Append to `~/.claude/mcp_servers.json`:

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

Then restart Claude Code. `tools/list` will report 5 tools under the `toolhub` namespace.

The server uses **stdio transport**: it speaks JSON-RPC on stdin/stdout. Logs go to stderr.

## Tools

| Tool | Description |
|---|---|
| `recommend` | Hybrid vec+FTS search for free-text task descriptions. Returns top-k tools with scores. |
| `search` | Pure FTS5 keyword search. Faster than `recommend` for known terms. |
| `info` | Full metadata for a single tool by id. |
| `add_source` | Register a GitHub repo / URL as a tool source. **Phase 3 stub** — only records the row; fetch lands in Phase 5. |
| `usage_stats` | Aggregated success rate / cost / duration per tool. Pass `tool_id` for the detail view (includes the 5 most-recent events). Run `toolhub score` first to populate. |

### `recommend`

Inputs:
```json
{ "task": "extract design tokens from a marketing page", "k": 3 }
```

Output (JSON-encoded text content):
```json
[
  {
    "tool_id": "skill:design-md",
    "score": 0.83,
    "name": "design-md",
    "description": "Generate design docs from markdown",
    "invocation": "/design-md",
    "install_path": "/home/deiu/.claude/skills/design-md"
  }
]
```

`k` defaults to 3 (clamped to `[1, 50]`). Task input is truncated to 2 KB before embedding.

### `search`

Inputs:
```json
{ "query": "design tokens", "k": 10 }
```

Output: array of `{ tool_id, score, name, description }`. `score` is `-bm25` so larger = better match. `k` defaults to 10 (clamped to `[1, 100]`).

### `info`

Inputs:
```json
{ "tool_id": "skill:design-md" }
```

Output: full `ToolMeta` JSON. Returns the literal string `null` if the id is unknown.

### `add_source`

Inputs:
```json
{ "url": "https://github.com/google-labs-code/stitch-skills", "type": "github" }
```

Output:
```json
{
  "source_id": "gh:google-labs-code/stitch-skills",
  "status": "registered",
  "note": "Phase 3 stub — fetch + parse lands in Phase 5."
}
```

`type` defaults to `github`. Source id is derived deterministically:
- `https://github.com/<owner>/<repo>(/|.git)?` → `gh:<owner>/<repo>`
- `git@github.com:<owner>/<repo>(.git)?` → `gh:<owner>/<repo>`
- Otherwise → `<type>:<url>`

### `usage_stats`

Inputs:
```json
{ "tool_id": "skill:caveman" }
```

`tool_id` is optional; omit to list all rows.

Output (with `tool_id` set):
```json
{
  "rows": [
    {
      "tool_id": "skill:caveman",
      "success_rate": 0.85,
      "sample_size": 42,
      "avg_cost_usd": null,
      "median_duration_ms": 240,
      "score_updated_at": "2026-05-03T12:00:00+00:00"
    }
  ],
  "recent_events": [
    {
      "occurred_at": "2026-05-03T12:00:00+00:00",
      "outcome": "success",
      "session_id": "sess-1",
      "project": "quiver"
    }
  ],
  "note": "Run `toolhub score` to populate from session JSONL."
}
```

`recent_events` is omitted from the response when `tool_id` is not set
(or when no events match). Outcome heuristic: `success` (tool_result without
`is_error`), `failure` (`is_error == true`), `abandoned` (no tool_result
before EOF), `unknown`. See PLAN.md §7 Phase 4.

## Smoke test (no Claude Code restart needed)

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
  | toolhub mcp \
  | head -1
```

Expect a `result` object describing the server. Then:

```bash
echo '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' | toolhub mcp | head -1
```

Should list 5 tools.

## Storage location

The server reads/writes the same SQLite DB as the CLI: `$XDG_DATA_HOME/toolhub/toolhub.sqlite`, falling back to `~/.local/share/toolhub/toolhub.sqlite`.

Run `toolhub sync` first (from the CLI) to populate the catalog before the MCP server is useful.

## First-call latency

`recommend` lazy-loads the fastembed BAAI/bge-small-en-v1.5 model on its first invocation. Cold load is a few seconds (model already cached at `~/.cache/toolhub/models/` after the first `toolhub recommend`/`toolhub sync`). `search`, `info`, `add_source`, `usage_stats` never touch the embedder.
