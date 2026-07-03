---
name: retrieving-cached-context
description: 'Use when a tracedecay response was truncated with a handle and the missing detail is needed — dereference the cached original with tracedecay_retrieve instead of re-running, or expand one LCM node.'
---

# Retrieving cached context

TraceDecay truncates large tool responses and emits a **handle** envelope
instead of the full body. The original text is cached in the active-project
store; you dereference it with `tracedecay_retrieve` rather than re-running the
source tool. This skill covers that handle/caching mechanic and the related
single-node expansion via `tracedecay_lcm_expand`.

## When to retrieve vs re-run

- A prior response ended with a `handle` (e.g. `rh_…`) and the missing details
  are actually needed to answer the user → **retrieve the handle**. Do not
  re-run the broad query, guess, or read a file again.
- You do NOT need the truncated tail → leave it; retrieval costs tokens.
- The result was truncated because the query was too broad → also consider
  narrowing the original query next time (see `tracedecay:using-tracedecay`).

## Tools

1. **Dereference a handle → `tracedecay_retrieve`** (`handle` required, copied
   exactly from the truncated envelope). It returns the **exact cached original
   text** — it does not re-run the source tool or re-read a file/session/node.
   Handles are scoped to the active project store, expire automatically, and
   never reference remote storage. If the truncated response used a
   `project-id`/`project-path` selector, pass the same selector to `retrieve`.
2. **Expand one LCM node → `tracedecay_lcm_expand`** (`provider`, `session-id`,
   `target` with `kind`: `raw_message`|`summary_node`|`external_payload`):
   opens a single session node through the bounded LCM query API. Page a summary
   node's sources with `source-offset`/`source-limit`, and page long content
   with `content-offset`/`content-limit`. If a returned source has
   `content_truncated: true`, continue via `target.kind: "raw_message"` for that
   source's `store_id` and `content_offset`.

## Guardrails

- Both tools are **read-only**; they surface already-cached content and never
  mutate state.
- Retrieve only what you need — handles and node expansion are bounded on
  purpose; do not dump the full cached body when a slice answers the question.
- Handles expire; if `retrieve` reports an expired/unknown handle, re-run the
  original tool with a narrower query rather than retrying the stale handle.
- Handles are local and project-scoped — never treat them as durable
  references to store or reuse across sessions.

## Handoff

- Finding which session node to expand (grep, replay, summary-DAG shape) → `tracedecay:recalling-session-context`.
- Driving compression that produces those summary nodes → `tracedecay:managing-session-context`.
- General "narrow the query instead of re-running" guidance → `tracedecay:using-tracedecay`.

## Output

- The retrieved cached text or expanded node content, and a note that it came
  from a handle/cache rather than a fresh query.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
