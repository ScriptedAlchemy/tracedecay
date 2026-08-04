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
`tracedecay tool <name> --args '<json>'` — the same JSON arguments object as the
MCP tool; pipe it via `--args -` (a quoted heredoc) when it contains quotes or
newlines (`tracedecay tool` lists all tools, `tracedecay tool <name> --help`
shows parameters). Hermes tool calls already run through this CLI under the hood
(passing `--args <json>`), so a direct shell invocation follows the same
execution path without the plugin wrapper. Fall back to it instead of querying
`.tracedecay` databases directly or abandoning tracedecay.

Do not invent per-key CLI flags or enum values from memory. Preserve the native
tool's exact JSON schema through `--args`; for example:

```sh
tracedecay tool context --args '{"task":"trace project routing","mode":"explore","max_nodes":20,"include_code":true}'
```

For `context`, the supported modes are `explore` and `plan`, and its result
budget is expressed with `max_nodes` / `max_code_blocks`, not guessed flags such
as `--max-tokens` or `--paths`. When uncertain, run
`tracedecay tool <name> --help` before invoking the fallback.

## Storage and project identity

Hermes may keep its own host files under its Hermes home, but that path never
selects a TraceDecay installation, store, or project. TraceDecay always uses
the normal user-profile installation and the same profile-sharded project
store used by every other host.

Project facts remain sharded: each registered project's `tracedecay.db` owns
its `memory_facts` and derived banks. Durable preferences and projectless chat
facts use the profile-level `~/.tracedecay/user-memory.db` store.
`~/.tracedecay/global.db` remains the cross-project registry/usage database,
not a shared project-fact table with a project tag.

Resolve the code project from an explicit runtime project root when the host
provides one; otherwise use stock Hermes' logical session workspace
(`agent.runtime_cwd`, including its per-session gateway context) and its Git
root. Fall back to `TERMINAL_CWD` and only then the process working directory.
Project routing belongs to the CLI transport (`--project`), not to MCP tool
arguments. Do not pass `storage_scope`, `hermes_home`, a Hermes profile name,
or a configured project pin: those legacy routing inputs are not part of the
current schemas and are rejected rather than translated.

Use `memory_scope=user` for durable user preferences and untethered chat.
Use `memory_scope=project` for codebase decisions and project knowledge. In a
project, Hermes recall combines user facts with the active project's facts and
labels their provenance. Without an initialized project, Hermes uses only user
memory and does not write project LCM data into an arbitrary working directory.

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
