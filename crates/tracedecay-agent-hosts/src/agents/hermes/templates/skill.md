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

## Session context retrieval

When prior transcript evidence is relevant, climb this read-only ladder and
stop when it answers the question. A recall tool never ingests, refreshes, or
compresses a session.

1. Start with `tracedecay_message_search`. Its defaults are `provider=all`,
   `include_subagents=true`, `scope=all`, `message_type=all`, `limit=10`, and
   `catch_up=false`. It finds stored message evidence and session ids; an
   explicit freshness request can return `refresh_required`, but never catches
   data up itself.
2. Hermes exposes native aliases `lcm_grep`, `lcm_load_session`,
   `lcm_describe`, `lcm_expand`, `lcm_expand_query`, `lcm_status`, and
   `lcm_doctor`. They dispatch to their matching `tracedecay_lcm_*` commands;
   use the Hermes alias and its schema when the context engine offers it, or the
   canonical command and its schema elsewhere. Do not invent fields by mixing
   the two surfaces.
3. Narrow temporal evidence with `lcm_grep` / `tracedecay_lcm_grep` (default
   `temporal_mode=current`; Hermes starts its native alias at the current
   session), replay one session with `lcm_load_session` /
   `tracedecay_lcm_load_session` (default `temporal_mode=forensic`), then use
   `lcm_describe` / `tracedecay_lcm_describe` and `lcm_expand` /
   `tracedecay_lcm_expand` to open only the needed DAG node or payload.
   Summary node IDs are opaque strings, not integers. `source_offset`,
   `source_limit`, and the opaque continuation cursor apply only to summary
   source pages; raw and external payload expansion use content pagination.
   Continue a summary page only by returning its opaque `next_cursor`
   unchanged with the same target, source limit, and content slice; changing a
   bound continuation input is denied.
4. `lcm_expand_query` / `tracedecay_lcm_expand_query` can assemble bounded
   context. When it returns `needs_synthesis=true`, the host must synthesize
   from that context; only use the direct answer when synthesis is not needed.
5. Treat `coverage`, `anchors`, watermarks, and explanations as evidence
   bounds. Partial or redacted coverage is not proof that a message never
   existed; preserve anchors when citing or drilling into the result.
6. For git-scoped recall, use `tracedecay_sessions_for` with its default
   `relation=produced` and `limit=20`; feed its session ids back into the
   temporal rungs. Use `tracedecay_workflows` (also default `limit=20`) to list
   a parent thread or git-scoped run, inspect one `wf_*` run, or select one
   agent before searching its messages.

Freshness and lifecycle are explicit host decisions. When a read says refresh
is required, invoke `tracedecay_session_refresh` only with clear host or user
intent: its actions are `begin`, `status`, and `cancel`; `begin` returns the
opaque handle used by `status` and `cancel`. Leave host context-window
preflight, compression, and boundaries to the Hermes context engine rather
than triggering them during recall.

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
