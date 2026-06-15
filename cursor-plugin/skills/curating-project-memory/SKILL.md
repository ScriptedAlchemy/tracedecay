---
name: curating-project-memory
description: Use when reviewing, updating, merging, deleting, pruning, or repairing tracedecay memory facts; handling stale, contradictory, duplicate, or secret-like facts; inspecting memory health; or opening the dashboard curation UI.
---

# Curating project memory

This skill owns memory lifecycle changes. For read-only recall, start with `tracedecay:recalling-project-memory`; for adding a researched subject from scratch, use `tracedecay:memorizing-subject`.

## Workflow

1. **Start read-only:** `tracedecay_fact_store` with `action: "search"`, `"list"`, `"probe"`, `"related"`, `"reason"`, or `"contradict"`; use `tracedecay_memory_status` when the user asks for memory counts/health; use `tracedecay_dashboard` (`action: "start"`) when they want visual curation.
2. **Classify the change:** update stale content/trust/tags, remove confirmed duplicates or wrong facts, or record `tracedecay_fact_feedback` only when the user rates a fact that was actually used.
3. **Confirm destructive actions:** before `action: "remove"`, show the fact id, content/source, and reason, unless the user already named the exact fact to delete.
4. **Apply narrowly:** `tracedecay_fact_store` `action: "update"` / `"remove"` / `"add"` only for the approved fact set. Re-run a read-only search/list to verify the final state.

## Guardrails

- Search/list/probe/related/reason/contradict are read-only. Add/update/remove, feedback, memory status repair, and dashboard start/stop mutate state or launch a local process; respect Cursor approval/run-mode.
- Deletion is permanent: there is no archive, soft-delete, restore, or undo path. Prefer update/merge when useful provenance should survive; delete only confirmed stale, duplicate, wrong, secret-like, or user-requested facts.
- Never store secrets, credentials, API keys, or PII. Do not lower trust merely because a fact is old; cite the newer evidence or contradiction.
- Dashboard curation can apply hard deletes. Use preview/dry-run first when available and surface high-risk delete/merge operations before applying them.

## Handoff

- Need to remember a new subject with research fan-out → `tracedecay:memorizing-subject`.
- Need raw session messages or summary-DAG replay → `tracedecay:recalling-session-context`.
- Need only index/server status, not memory mutation → `tracedecay:project-status`.

## Output

- Facts searched/changed, confirmations requested, final verification result, and any skipped high-risk candidates.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
