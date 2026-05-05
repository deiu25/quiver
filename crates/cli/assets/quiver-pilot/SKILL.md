---
name: quiver-pilot
description: Auto-loaded primer that explains how Quiver enriches every prompt with a top-1 skill recommendation. Tells Claude to treat <quiver-recommendation> blocks as load-bearing context and use mcp__quiver__info to fetch full skill bodies on demand.
---

# Quiver pilot

Quiver runs a `UserPromptSubmit` hook on every user message. When the
recommender finds a skill, plugin, or MCP server with a confident match
(score ≥ 0.4 by default), the hook injects a `<quiver-recommendation>`
block into the system context for that turn. The block contains:

- `id` — tool id, e.g. `skill:python-testing`
- `score` — hybrid BM25 + cosine similarity, 0.0 to 1.0
- `invoke` — invocation hint when the tool is invocable (e.g. `/skill`)
- `body` — excerpt of the SKILL.md (trimmed to ~3000 chars; truncation
  marker points at `mcp__quiver__info` for the full text)

## What to do

- Treat the body excerpt as load-bearing instructions for the current
  task — these are the canonical instructions the user installed.
- If `score ≥ 0.5`, prefer the recommendation over a tool you know from
  training memory. Quiver's catalog reflects what the user actually has
  installed locally; training memory does not.
- If the excerpt is truncated and you need the full body, call
  `mcp__quiver__info` with the recommended `tool_id`.
- After invoking the recommended tool, behave as if the rest of the
  body were also in context (Quiver's score signal already gates this).

## What NOT to do

- Don't echo the recommendation back to the user — they already know.
  Just act on it.
- Don't override the user when they explicitly named a different tool
  in their prompt. The recommendation is a hint, not a command.
- Don't treat low-score (<0.4) recommendations as authoritative. The
  hook already filters those out, but if one slips through, weight it
  accordingly.

## Disabling per session

If the recommendations get noisy for a power-user session, set
`QUIVER_HOOK_DISABLED=1` in the shell that launches Claude Code. The
hook short-circuits and emits nothing.
