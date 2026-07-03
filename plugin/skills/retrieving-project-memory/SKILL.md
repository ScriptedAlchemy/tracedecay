---
name: retrieving-project-memory
description: 'Use when querying or reasoning over stored tracedecay memory facts — searching, probing by entity, multi-fact reasoning, or fetching a fact with trust history. For recall framing see recalling-project-memory.'
---

# Retrieving project memory

This skill owns the **read/reason mechanics** of the holographic fact store:
the exact `tracedecay_fact_store` retrieval actions plus
`tracedecay_memory_status`. It is the mechanical counterpart to
`tracedecay:recalling-project-memory` (which frames memory recall around a task
or decision and starts from transcripts). When the question is "what does the
fact store know about X and how do the facts relate," start here.

## Retrieval actions (`tracedecay_fact_store`)

All read-mostly; they may update access/retrieval metadata but do not add or
delete facts. Read-only project selectors (`project-id` / `project-path`) are
supported for these actions.

1. **search** (`query`, optional `category`, `limit` default 20 / max 200,
   `min_trust`) — phase-vector similarity search; the default entry point for
   "find facts about X."
2. **probe** (`entity` / `query`) — probe memory around a single named entity.
3. **related** (`entity`) — facts connected to an entity via stored relations.
4. **reason** (`query`, `entities`) — assemble and reason over multiple facts
   for a query, following entity relations rather than returning a flat list.
5. **get** (`fact-id`) — the full fact plus its `trust_history`, so you can
   answer *why* a trust score is what it is.
6. **list** (optional `category`, `min_trust`, `limit`) — enumerate stored
   facts for review.
7. **contradict** (`threshold`) — scan for contradictory facts; non-destructive.

## Memory health

- **`tracedecay_memory_status`** — fact/entity counts, trust distribution,
  below-threshold and missing-vector signals, capacity-per-bank, and repair
  stats. Note it **repairs** derived vectors/banks as a side effect, so call it
  when the user asks for memory counts/health, not on every recall.

## How to query and reason

- Prefer `search`/`probe` to locate candidates, then `reason` (or `related`)
  when the answer spans several linked facts.
- Use `min_trust` to filter out low-confidence facts; use `get` on a specific
  `fact-id` when the user challenges a fact or asks why its trust changed.
- Keep retrieval bounded and token-aware: set `limit` deliberately and narrow
  `query`/`category` rather than pulling the whole store with a broad `list`.

## Guardrails

- search/probe/related/reason/get/list/contradict are read-only recall (they
  may touch access counters); they never mutate fact content.
- `tracedecay_memory_status` mutates derived state (vector/bank repair) — treat
  it as a health action, not a passive read.
- Recall memory before external or web search — a prior session likely already
  answered the question, cheaper and project-specific.

## Handoff

- Task/decision recall that should start from transcripts → `tracedecay:recalling-project-memory`.
- Persisting a new durable fact → `tracedecay:storing-project-memory`.
- Fixing stale/contradictory/duplicate facts → `tracedecay:curating-project-memory`.

## Output

- The facts found/reasoned over with their ids, trust, and source, plus which
  action answered the question.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
