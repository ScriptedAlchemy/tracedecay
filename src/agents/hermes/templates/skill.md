---
name: tracedecay
description: Prefer tracedecay tools for codebase exploration, graph queries, and memory recall.
---

# Use tracedecay

Use tracedecay tools before broad file reads for codebase exploration, symbol lookup,
call graph traversal, impact analysis, affected files, and architectural navigation.

## If a tool call fails

If a tracedecay tool invocation fails, times out, or the plugin is unavailable,
every tool is also available directly as a shell command:
`tracedecay tool <name> --key value` (`tracedecay tool` lists all tools,
`tracedecay tool <name> --help` shows parameters). Hermes tool calls already run
through this CLI under the hood, so a direct shell invocation follows the same
execution path without the plugin wrapper. Fall back to it instead of querying
`.tracedecay` databases directly or abandoning tracedecay.

## Memory

- **Recall before external search.** Run `fact_search` (and `lcm_grep` for past
  conversations) before reaching for web or external search — prior sessions
  often already answered the question.
- **Calibrate trust; don't default everything high.** Aim for a spread across
  stored facts rather than uniform high trust:
  - `>= 0.85` — verified, durable facts (confirmed decisions, observed behavior,
    user-stated preferences).
  - `~ 0.7` — ordinary well-sourced observations.
  - `~ 0.5` — plausible but unverified; prefer not storing over storing noise.
- **Read the add result's diff report.** `fact_add` returns
  `diff` / `closest_fact_id` / `similarity` / `reason`:
  - `near_duplicate` — a very similar fact exists; prefer `fact_update` on the
    existing fact over piling on duplicates.
  - `possible_conflict` — a negation/state-change cue suggests supersession;
    confirm which fact is current and update or remove the stale one.
  - `rejected_secret_like` — the content looked like a credential and was NOT
    stored; never try to re-store secrets.
- **Never store secrets, transient run output (ports, PIDs, temp paths, run
  logs), or facts you have not verified.**
