---
name: quiver-pilot
description: Authoritative tool router. Quiver injects directive blocks into every prompt and vetoes wrong tool calls. The model MUST obey level=mandatory directives unless the user explicitly named a different tool in this prompt.
---

# Quiver pilot — directive policy

Quiver runs hooks on `UserPromptSubmit`, `PreToolUse`, and `Stop`. Two markup
forms appear in your context:

- `<quiver-directive level="…" invoke_now="…" tool_id="…" invoke="…" score="…">`
  — a system-reminder. Treat as load-bearing project policy, not a
  suggestion.
- `<quiver-recommendation>` — the recommended tool's body excerpt (the
  canonical instructions the user installed).

## Levels — what you MUST do

- **`level="mandatory"`** (`invoke_now="true"`, score ≥ 0.75): your **next**
  action MUST be the named tool. Bypassing it without an explicit user
  override is a violation of user instructions.
- **`level="strong"`** (0.60 ≤ score < 0.75): use the named tool unless you
  can articulate a concrete reason it does not fit; state that reason in
  your reply if you bypass.
- **(no directive, recommendation only, 0.40 ≤ score < 0.60)**: prefer the
  suggested tool but exercise judgement.

## PreToolUse vetoes

If a tool call you propose is denied with `Quiver: a higher-confidence
installed tool fits this task. Use \`<invocation>\` …`, treat the deny as the
user's policy. Switch to the named tool and continue.

Re-invoking the same denied tool is allowed (Quiver vetoes once per
session/tool/task and records the bypass as a false-positive candidate).
Only do so when you have a concrete reason — Quiver's auto-tuner reads the
bypass rate to calibrate thresholds.

## Stop circuit-breaker

If you finish a session and Quiver emits `decision: block` naming an unused
mandatory recommendation, you must either invoke that tool or write one
sentence explaining why it was wrong for the task. Both signals feed the
auto-tuner.

## Explicit user overrides

When the user's prompt names a specific tool ("use Read", "with bash",
"/some-skill"), the user wins. Acknowledge briefly and proceed with their
choice. Quiver's PreToolUse hook also recognises invocation patterns and
suppresses vetoes for tools the user named.

## Disabling

- `QUIVER_HOOK_DISABLED=1` — hooks short-circuit; no enrichment, no veto,
  no Stop block.
- `QUIVER_ENFORCE=advisory` — keep system-reminders, drop PreToolUse vetoes
  and Stop blocks (good for noisy sessions or unfamiliar codebases).
- `QUIVER_ENFORCE=off` — equivalent to `QUIVER_HOOK_DISABLED=1`.

## What NOT to do

- Don't echo the directive or recommendation back to the user — they
  already see it in the system reminder. Just act on it.
- Don't treat low-score (< 0.40) recommendations as authoritative. The
  Silent band suppresses emit; if one slips through, weight it low.
- Don't re-query `mcp__quiver__recommend` for every micro-step inside the
  same subtask — one call per subtask boundary is enough.
