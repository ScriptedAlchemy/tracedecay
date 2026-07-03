---
name: project-memory
description: 'Use when recalling prior decisions, durable facts, user/project preferences, or past project context before answering or planning; or when reviewing, updating, merging, deleting, pruning, or repairing tracedecay memory facts and dashboard curation.'
---

# Project memory

One skill for both halves of project memory. **Recall** is read-only and where
you start; **Curate** mutates stored facts and requires explicit approval before
any destructive action. Prefer TraceDecay-native registered-project selectors
whenever a recall or curation spans or targets a project other than the active
checkout.

## Recall (read-only, start here)

Recall memory **before** reaching for external or web search — prior sessions
often already answered the question, and a memory hit is cheaper and
project-specific.

1. **Durable facts → `tracedecay_fact_store`** with `action: "search"` (or
   `"probe"` / `"reason"`), plus `query` and `min_trust`.
2. **Past conversations → `tracedecay_message_search`** (`query`, optional
   `provider`, `limit`) over ingested Cursor/Codex/agent transcripts (active
   project FTS index). This skill owns the **FTS → fact** lane: use
   `message_search` to surface durable project facts. For raw conversation
   recall — scoped/role/time-filtered grep, lossless replay, or summary-DAG
   drill-down — hand off to `tracedecay:recalling-session-context`, which owns
   the **FTS → LCM** lane.
3. **If the user rates a recalled fact → `tracedecay_fact_feedback`**
   (`helpful` / `unhelpful`) to tune its trust score.
4. **Persist a new durable decision → `tracedecay_fact_store`** `action: "add"`
   (`content`, `category`, `tags`, `trust`) proactively whenever a durable
   decision, user preference, correction, or pitfall surfaces — do not wait for
   the user to ask. The add path already rejects secrets and reports
   near-duplicates/conflicts.

Do NOT capture: secrets/credentials, transient errors, environment-specific
failures, one-off narratives, task progress, or soon-stale session outcomes —
recover those from transcripts via `tracedecay:recalling-session-context`.

## Curate (mutation, requires approval)

Destructive curation is a parent-agent responsibility. Use subagents only for
scoped inspection or recommendation work, with explicit project selectors and
non-overlapping ownership; do not delegate delete/apply/merge/retention actions
to subagents. Begin read-only, gather evidence, propose a mutation plan, then
write only narrow durable changes.

1. **Resolve scope:** confirm the active project root/store before touching
   memory. Project-bound profiles use the user-level TraceDecay store scoped to
   the current project by default.
2. **Start read-mostly:** `tracedecay_fact_store` with `action: "get"`,
   `"contradict"`, `"search"`, `"list"`, `"probe"`, `"related"`, or `"reason"`;
   note that search/list/probe/related/reason may update retrieval/access
   metadata. Use `tracedecay_memory_status` only when the user asks for memory
   counts/health because it may repair vectors/banks. Use `tracedecay_dashboard`
   (`action: "start"`) only when they want visual curation.
3. **Run native dry-run:** prefer `tracedecay memory curate` or
   `POST /api/plugins/holographic/curate` with `{"dry_run": true}`. Dry-run is
   the default and returns `actions`, `hygiene_candidates`, `counts`,
   `coverage`, `provider`, and `mode`.
4. **Inventory candidates:** group facts into add, update, merge/dedupe, stale,
   contradiction, secret-like, transient, supersession, and possible
   hard-delete buckets. Keep fact ids, source/provenance, trust, tags,
   entities, evidence links, and counterevidence with each candidate.
5. **Research gaps:** use TraceDecay graph/search plus LCM/session/message tools
   to mine past sessions, raw messages, summary DAGs, branch/PR context, docs,
   and tests. Scoped subagents may research bounded read-only questions only;
   the parent agent is the sole memory writer and must review raw findings
   before trusting them.
6. **Propose changes:** summarize durable additions, stale-fact updates,
   trust/tag/source changes, dedupe merges, and delete candidates. Prefer
   update/merge over removal when useful provenance should survive.
7. **Apply narrowly → `tracedecay_fact_store`** `action: "add"` / `"update"` /
   `"remove"` for reviewed operations (or `POST
   /api/plugins/holographic/curate/apply` / `tracedecay memory curate
   --llm-ops <file> --apply`). Require explicit approval immediately before
   every `remove`, dashboard hard delete, or merge loser removal, showing fact
   id, content/source summary, reason, and permanent-delete warning.
8. **Verify read-only:** re-run search/list/probe/related/contradict/get as
   appropriate, inspect apply results/oplog when used, and report final facts
   changed, skipped, or still needing human judgment.

## Guardrails

- `tracedecay_message_search` and `fact_store` search/get/contradict are
  read-only recall. Search/list/probe/related/reason are read-mostly but can
  update access/retrieval counters. `fact_store` add/update/remove,
  `fact_feedback`, `memory_status` repair, and `dashboard` start/stop mutate
  state or launch a local process; respect host approval/run-mode.
- Deletion is permanent: there is no archive, soft-delete, restore, or undo
  path. Prefer update/merge when useful provenance should survive; delete only
  approved stale, duplicate, wrong, secret-like, or user-requested facts.
- Never store secrets, credentials, API keys, or PII. Do not lower trust merely
  because a fact is old; cite the newer evidence or contradiction.
- Dashboard curation can apply hard deletes. Use preview/dry-run first when
  available and surface high-risk delete/merge operations before applying them.
  `POST /api/plugins/holographic/curate` with `dry_run=false` applies
  deterministic duplicate deletion; `POST /api/plugins/holographic/curate/apply`
  applies explicit delete/merge ops.
- Do not let subagents call add/update/remove/feedback tools, apply curation
  ops, start dashboard mutation flows, or run memory health repair. Ask them for
  cited evidence, candidate facts, suspected duplicates, and stale/conflicting
  claims, then perform parent-agent validation before writing.
- Hygiene candidates (`secret_like`, `transient`, `supersession`) are review
  evidence, not deterministic apply operations. External LLM plans must use
  strict JSON `{"ops": [...]}` and pass the TraceDecay evidence guard; rejected
  low-confidence or out-of-scope ops must stay skipped.

## Memorize a subject

Use only when the user explicitly asks to memorize or remember a subject, code
area, branch, PR, or decision set.

1. **Research read-only:** TraceDecay graph/search, LCM/session/message tools,
   docs, existing fact searches, and relevant branch/PR context.
2. **Filter:** keep durable, scoped facts with citations. Reject secrets,
   credentials, PII, large code blobs, transient branch state, and uncited
   speculation.
3. **Calibrate trust:** `0.85+` for independently verified decisions, about
   `0.7` for ordinary well-sourced facts, about `0.5` for plausible but
   uncertain facts. Do not ask for approval solely because trust is low.
4. **Dedupe before writing:** search `tracedecay_fact_store` with the subject
   plus candidate, matching category, `limit: 10`, `min_trust: 0.5`; skip
   near-duplicates and ask before replacing contradictory facts.
5. **Store accepted facts → `tracedecay_fact_store`** `action: "add"` with
   content, category, source, tags, entities, trust, and metadata containing
   subject/confidence/citations. Act on `near_duplicate`, `possible_conflict`,
   and `rejected_secret_like`; never rephrase a rejected secret to bypass
   filtering.

## Handoff

- Raw session messages, scoped grep, or summary-DAG replay →
  `tracedecay:recalling-session-context`.
- Index/server status without memory mutation → `tracedecay:code-health`.

## Output

- Recall: the relevant prior context/decisions/messages found, with source.
- Curate: facts searched/changed, confirmations requested, final verification
  result, and any skipped high-risk candidates.
- If any result includes a `tracedecay_metrics:` line, report the savings to the
  user.
