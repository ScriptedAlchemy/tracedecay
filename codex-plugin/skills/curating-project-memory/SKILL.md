---
name: curating-project-memory
description: 'Use when reviewing, updating, merging, deleting, pruning, or repairing tracedecay memory facts; handling stale, contradictory, duplicate, or secret-like facts; inspecting memory health; or opening the dashboard curation UI.'
---

# Curating project memory

Destructive curation is a parent-agent responsibility. Use subagents only for scoped inspection or recommendation work, with explicit project selectors and non-overlapping ownership; do not delegate delete/apply/merge/retention actions to subagents. TraceDecay should progressively expose registered-project selectors in its own MCP and CLI surfaces, so this skill documents the workflow rather than being the sole routing mechanism.

This skill owns memory lifecycle changes. For read-only recall, start with `tracedecay:recalling-project-memory`. For autonomous curation, begin read-only, gather evidence, propose a mutation plan, then write only narrow durable changes. The installed Codex plugin ships this skill as the required operator runbook, so follow the workflow below without depending on external `docs/` files.

## Workflow

1. **Resolve scope:** confirm the active project root/store before touching memory. Project-bound profiles use the user-level TraceDecay store scoped to the current project by default.
2. **Start read-mostly:** use TraceDecay MCP context/search first for code/session orientation, then `tracedecay_fact_store` with `action: "get"`, `"contradict"`, `"search"`, `"list"`, `"probe"`, `"related"`, or `"reason"`; note that search/list/probe/related/reason may update retrieval/access metadata. Use `tracedecay_memory_status` only when the user asks for memory counts/health because it may repair vectors/banks. Use `tracedecay_dashboard` (`action: "start"`) only when they want visual curation.
3. **Run native dry-run:** prefer `tracedecay memory curate` or `POST /api/plugins/holographic/curate` with `{"dry_run": true}`. Dry-run is the default and returns `actions`, `hygiene_candidates`, `counts`, `coverage`, `provider`, and `mode`.
4. **Inventory candidates:** group facts into add, update, merge/dedupe, stale, contradiction, secret-like, transient, supersession, and possible hard-delete buckets. Keep fact ids, source/provenance, trust, tags, entities, evidence links, and counterevidence with each candidate.
5. **Research gaps:** use TraceDecay graph/search plus LCM/session/message tools to mine past sessions, raw messages, summary DAGs, branch/PR context, docs, and tests. For multi-step evidence gathering, scoped subagents may research bounded read-only questions only; the parent agent is the sole memory writer and must review raw findings before trusting them.
6. **Propose changes:** summarize durable additions, stale-fact updates, trust/tag/source changes, dedupe merges, and delete candidates. Prefer update/merge over removal when useful provenance should survive.
7. **Apply narrowly:** add/update only facts supported by evidence. Use `/curate/apply` or `tracedecay memory curate --llm-ops <file> --apply` only for reviewed operations. Require explicit approval immediately before every `action: "remove"`, dashboard hard delete, or merge loser removal, showing fact id, content/source summary, reason, and permanent-delete warning.
8. **Verify read-only:** re-run search/list/probe/related/contradict/get as appropriate, inspect apply results/oplog when used, and report final facts changed, skipped, or still needing human judgment.

## Guardrails

- `get` and `contradict` are non-destructive recall. Search/list/probe/related/reason are read-mostly but can update access/retrieval counters. Add/update/remove, feedback, memory status repair, and dashboard start/stop mutate state or launch a local process; respect host approval/run-mode.
- Deletion is permanent: there is no archive, soft-delete, restore, or undo path. Prefer update/merge when useful provenance should survive; delete only approved stale, duplicate, wrong, secret-like, or user-requested facts.
- Never store secrets, credentials, API keys, or PII. Do not lower trust merely because a fact is old; cite the newer evidence or contradiction.
- Dashboard curation can apply hard deletes. Use preview/dry-run first when available and surface high-risk delete/merge operations before applying them. `POST /api/plugins/holographic/curate` with `dry_run=false` applies deterministic duplicate deletion; `/curate/apply` applies explicit delete/merge ops.
- Do not let subagents call add/update/remove/feedback tools, apply curation ops, start dashboard mutation flows, or run memory health repair. Ask them for cited evidence, candidate facts, suspected duplicates, and stale/conflicting claims, then perform parent-agent validation before writing.
- Default autonomous grooming output is report-only. If a tool or dashboard action mutates unexpectedly, disclose it and verify state before continuing.
- Hygiene candidates (`secret_like`, `transient`, `supersession`) are review evidence, not deterministic apply operations.
- External LLM plans must use strict JSON `{"ops": [...]}` and pass through the TraceDecay evidence guard; rejected low-confidence or out-of-scope ops must stay skipped.

## Dry-run report

Before any mutation, produce a compact report with these sections:

- `scope`: project root/store, tool/API used, dry-run timestamp, and whether memory health repair or dashboard start/stop was invoked.
- `native_plan`: `mode`, `provider`, `coverage`, `counts`, action count, and hygiene-candidate counts from `tracedecay memory curate` or `/curate`.
- `adds`: candidate durable facts with source spans, category, entities, trust, and duplicate-search result.
- `updates`: fact ids, old/new summary, evidence, confidence, and why update beats add.
- `merges`: winner/loser ids, similarity evidence, retained provenance, optional `merged_content`, and why separate facts are redundant.
- `deletes`: fact ids, content/source summary, permanent-delete reason, risk, surviving fact if any, and explicit approval status.
- `skipped`: rejected transient, secret-like, unsupported, stale-but-uncertain, or duplicate candidates.
- `verification_plan`: exact read-only checks to run after apply.

Map native curation fields into those sections as follows:

- `actions`: deterministic similarity-dedup delete proposals; list them under `deletes` unless operator review converts them into a safer `merge`.
- `hygiene_candidates`: review-only evidence; list confirmed candidates under `deletes`, `updates`, or `merges`, and unconfirmed candidates under `skipped`.
- `llm_review`: bounded external-review request; use `clusters`, `hygiene_candidates`, `allowed_fact_ids`, and `min_confidence` as evidence constraints.
- `llm_apply`: validated external ops and rejected ops; list valid dry-run ops under `merges`/`deletes`, and rejected ops under `skipped`.

## Memorize a subject

Use only when the user explicitly asks to memorize or remember a subject, code area, branch, PR, or decision set.

1. **Research read-only:** use TraceDecay graph/search, LCM/session/message tools, docs, existing fact searches, and relevant branch/PR context. Scoped research agents may gather evidence but the parent agent is the only memory writer.
2. **Filter:** keep durable, scoped facts with citations. Reject secrets, credentials, PII, large code blobs, transient branch state, unsupported claims, and uncited speculation.
3. **Calibrate trust:** use `0.85+` for independently verified decisions/observations, about `0.7` for ordinary well-sourced facts, and about `0.5` for plausible but uncertain facts. Do not ask for approval solely because trust is low.
4. **Dedupe before writing:** search `tracedecay_fact_store` with the subject plus candidate, matching category, `limit: 10`, and `min_trust: 0.5`; skip near-duplicates and ask before replacing contradictory facts.
5. **Store accepted facts:** propose the candidate set, then call `tracedecay_fact_store` `action: "add"` with content, category, source, tags, entities, trust, and metadata containing subject/confidence/citations.
6. **Read add diffs:** act on `near_duplicate`, `possible_conflict`, and `rejected_secret_like`; never rephrase a rejected secret to bypass filtering.

## Handoff

- Need raw session messages or summary-DAG replay -> `tracedecay:recalling-session-context`.
- Need only index/server status, not memory mutation -> `tracedecay:project-status`.

## Output

- Facts searched/changed, confirmations requested, final verification result, and any skipped high-risk candidates.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
