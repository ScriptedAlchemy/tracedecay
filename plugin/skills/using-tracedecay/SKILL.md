---
name: using-tracedecay
description: 'Use when starting any code or repo-context task in a TraceDecay-indexed project, before Grep/Glob/cat/Read/gh pr diff/tests/memory writes, including cross-project or sibling workspace context via project registry. Skip only for scoped subagents handed exact files.'
---

# Using TraceDecay

```
IF A TRACEDECAY TOOL OR SKILL APPLIES, USING IT IS NOT OPTIONAL.
GRAPH BEFORE GREP. FACTS BEFORE FILES. CONTEXT BEFORE CODE.
```

This project has a live TraceDecay code graph, memory store, and session
archive. If there is even a **1% chance** a tracedecay tool or skill applies
to what you are doing, you MUST use it — before any response or action,
including clarifying questions and "quick looks" at files. This is not
negotiable. You cannot rationalize your way out of it. Violating the letter
of this rule is violating its spirit.

## Scope and priority

- **SUBAGENT-STOP:** a scoped subagent handed exact files, symbols, or
  excerpts acts on what it was given — no re-discovery. This mandate governs
  open-ended work, not narrow handoffs.
- Priority: explicit user instructions and project rules (CLAUDE.md /
  AGENTS.md) > this skill > the host's default grep habit. Never fight a
  direct instruction to satisfy the mandate.
- **Announce** every skill you follow: "Using tracedecay:<skill> to <purpose>"
  — then follow it exactly. If it has a checklist, make a todo per item.

## Bootstrap (once per session)

tracedecay tools may be **deferred** — listed by name only, uncallable until
their schemas load. First need → ONE batched ToolSearch call:
`select:tracedecay_context,tracedecay_search,tracedecay_grep,tracedecay_outline,tracedecay_body`
(add project/session tools such as `tracedecay_project_list`,
`tracedecay_project_search`, `tracedecay_project_context`, or
`tracedecay_message_search` when the moment needs them). If any MCP call
errors or times out, the same tool runs as `tracedecay tool <name> --args '<json>'` — see
`tracedecay:using-the-cli`. Transport failure never justifies grep.

## Moment to mandatory action

| The moment you are in | Do this instead |
|---|---|
| About to grep/rg a literal string, regex, or config key | `tracedecay_grep` — skill: `tracedecay:exploring-code` |
| About to search for a symbol or concept, or open/Read a source file | `tracedecay_search` / `tracedecay_context`; read via outline→body→read slices — `tracedecay:exploring-code` |
| User names another repo/project/workspace, sibling checkout, or cross-repo/cross-project context | `tracedecay_project_list` / `tracedecay_project_search` → `tracedecay_project_context`; pass `project_id`, `project_path`, or `project_selector` to `tracedecay_context`, `tracedecay_search`, and `tracedecay_message_search` |
| "Who calls X" / "what does X call" / "trace this" | `tracedecay:tracing-functions` |
| Wondering what breaks or which tests to run | `tracedecay:assessing-impact` |
| Trait/interface/class type-hierarchy question (implementors, extenders, inheritance) | `tracedecay_type_hierarchy` — skill: `tracedecay:exploring-code` |
| About to run `gh pr diff` / read a raw diff to review | `tracedecay_pr_context` / `tracedecay_diff_context` (offline, no gh needed) — `tracedecay:reviewing-changes` |
| Need a saved document's combined post-edit diagnostics | `tracedecay_feedback_advisory_cycle`; consume its canonical diagnostics, impact, affected-test result, and daemon-minted read handle |
| Need Context Scout status or recent bounded suggestions | `tracedecay_context_scout_status` / `tracedecay_context_scout_capability` / `tracedecay_context_scout_explain` / `tracedecay_context_scout_budget` / `tracedecay_context_scout_recent` |
| Need to control an authenticated Context Scout generation | `tracedecay_context_scout_pause` / `tracedecay_context_scout_resume` require the exact configuration revision; `tracedecay_context_scout_cancel` / `tracedecay_context_scout_claim` / `tracedecay_context_scout_delivery` / `tracedecay_context_scout_feedback` require the daemon-returned exact address and typed work, claim, or receipt |
| Need effective configuration or provenance | `tracedecay_configuration_list` / `tracedecay_configuration_get` / `tracedecay_configuration_explain` / `tracedecay_configuration_audit` / `tracedecay_configuration_observed_state` |
| Need to mutate configuration | `tracedecay_configuration_batch`, `tracedecay_configuration_set`, or `tracedecay_configuration_unset`; protected and rollback changes require their exact `*_preview` then `*_apply` pair |
| About to write a new helper, rename, or mass-edit | `tracedecay:editing-safely` |
| Looking for duplicate code, similar function bodies, repeated helper logic, or consolidation targets | `tracedecay_redundancy` first for body/functionality matches; `tracedecay_similar` only for near-duplicate names — `tracedecay:code-health` / `tracedecay:editing-safely` |
| Build/type errors present, or about to run cargo check/tsc | `tracedecay:fixing-build-and-type-errors` |
| About to write MEMORY.md/CLAUDE.md notes, or asked about a past decision | `tracedecay:project-memory` (`fact_store`) |
| Need raw past-session transcripts or compaction recovery | `tracedecay:managing-session-context` |
| Creating or running a Work task/attempt/placement | `tracedecay:managing-work` |
| Registering or running a Workflow definition | `tracedecay:managing-workflows` |
| Querying several registered roots as one frozen scope | `tracedecay_multi_root_scope_set_read` / `tracedecay_multi_root_scope_set_compare_and_swap` then `tracedecay_multi_root_execute` |
| Architecture, tech debt, index/project status | `tracedecay:code-health` |
| An MCP call just failed | `tracedecay:using-the-cli` — never abandon over transport |

## Red flags — these thoughts mean STOP, you are rationalizing

| Thought | Reality |
|---|---|
| "Grep is faster for this" | `tracedecay_grep` runs the same match over the index and returns the enclosing symbol. Same speed, more answer. |
| "I'll just read the whole file" | `outline`/`body` answer at a fraction of the tokens, cached across sessions. |
| "This repo probably isn't indexed" | Check `tracedecay_status` (one cheap call). Guessing "unindexed" to justify grep is the rationalization itself. |
| "I'll use gh to get the PR diff" | `pr_context` computes changed symbols + dependents + tests from the local graph — offline. gh is for comments/CI only. |
| "I made one context call; now I'll bash around" | One call is discovery, not license. Stay on the skill's ladder; pass `seen_node_ids` forward and narrow — don't switch to grep. |
| "I'll jot this in MEMORY.md" | Durable facts go to `fact_store` (add) — searchable, trust-ranked, cross-session. MEMORY.md is not memory. |
| "The index might be stale — I should sync first" | Hooks auto-sync on every session and edit. Never run manual sync; if results look stale, check `tracedecay_status` and report it. |
| "The MCP call might fail / just failed" | `tracedecay tool <name>` always works. Transport ≠ capability. |
| "This is a simple lookup" | Simple lookups are exactly what the graph is for. |
| "I already know this codebase" | The graph is fresher than your memory. Check it. |
| "I'll explore first, then use the skill" | The skills tell you HOW to explore. Check first. |
| "The skill is overkill here" | Simple things become complex. Use it. |

## Procedure

1. Before the first tool call of ANY task (questions included), check the
   moment table. A row matches → announce and follow it.
2. Never dispatch an Explore agent for codebase research while tracedecay is
   available; if one must be spawned, its prompt must mandate
   `tracedecay_context` as its only exploration tool with `seen_node_ids`
   threading.
3. Truncated response with a `handle` → narrow the query, or
   `tracedecay_retrieve` when the omitted detail is needed. Never re-run broad.
4. A result includes a `tracedecay_metrics:` line → report the savings.
5. A durable decision, preference, correction, or pitfall surfaced → store it
   via `tracedecay:project-memory` without being asked.

Configuration mutation is never inferred from a read request. Use
`tracedecay_configuration_protected_preview` before
`tracedecay_configuration_protected_apply`, and
`tracedecay_configuration_rollback_preview` before
`tracedecay_configuration_rollback_apply`. Credential material crosses only
`tracedecay_configuration_write_credential`; never place it in ordinary
configuration values, logs, or chat output.
