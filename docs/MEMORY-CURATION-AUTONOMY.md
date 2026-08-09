# Autonomous Memory Curation Operator Guide

## Purpose

This guide turns the autonomous memory curation design into an operator runbook.
It applies to TraceDecay project memory, the standalone dashboard curation UI,
the `tracedecay memory curate` CLI, and automation backends that submit
validated operations through TraceDecay curation contracts.

The operating principle is conservative and autonomous: agents may mine,
explain, cluster, score, validate, and apply accepted curation operations.
The curator/orchestrator automatically applies policy-valid results with
durable evidence, receipts, and telemetry; validation and risk policy remain
the mutation boundary, not a dashboard preview/review gate. Human CLI and UI
review observes outcomes and can explicitly pause, disable, quarantine, or roll
back a run, but never gates curation. Memory deletion is permanent by design.
There is no archive, soft-delete state, restore flow, undo flow, or recycle-bin
behavior.

## Current Curation Surfaces

Destructive memory curation is automation-owned. Subagents may inspect,
classify, and propose actions within their assigned scope, but the
curator/orchestrator automatically applies only policy-valid operations through
the configured curation contract so runs produce ledger records, activity
events, artifacts, and post-action telemetry.


Use existing TraceDecay surfaces before inventing a new plan format:

- Exact `tracedecay_fact_store_*` tools: direct fact get/search/list/probe/related/reason,
  add, update, remove, and contradiction checks.
- `tracedecay_memory_status`: read-only memory authority and coverage health;
  use only when health/counts are part of the task. Similarity and dedupe
  evidence comes from the bounded verified Grafeo projection, not an alternate
  derived structure.
- Dashboard automation:
  `POST /api/automation/run/memory-curator` queues an autonomous app-server
  memory-curator run. Accepted operations are applied according to automation
  policy and every phase is logged to the run ledger and curation activity
  stream.
- Generic apply:
  `POST /api/plugins/holographic/curate/apply` with `{"ops": [...]}` applies
  explicit `delete` and `merge` operations and reports per-op results.
- CLI dry-run:
  `tracedecay memory curate` prints the same deterministic dry-run plan without
  requiring the dashboard server.
- CLI LLM review request:
  `tracedecay memory curate --llm` emits bounded clusters, hygiene candidates,
  allowed fact ids, confidence floor, and chat messages for an external LLM
  owner.
- CLI validated LLM ops:
  `tracedecay memory curate --llm-ops <file>` validates external
  `{"ops": [...]}` against freshly recomputed evidence and remains dry-run
  unless `--apply` is also passed.

## Native Dry-Run Report Shape

The deterministic dry-run report is the baseline input for any autonomous
curation workflow:

```json
{
  "ran": true,
  "dry_run": true,
  "actions": [
    {
      "op": "delete",
      "fact_id": 102,
      "duplicate_of": 101
    }
  ],
  "hygiene_candidates": {
    "secret_like": [],
    "transient": [],
    "supersession": []
  },
  "counts": {
    "delete": 1
  },
  "applied_counts": null,
  "llm_calls": 0,
  "coverage": {
    "scanned": 3,
    "active_total": 3,
    "due_remaining": 0
  },
  "provider": "tracedecay",
  "mode": "similarity_dedup"
}
```

Operator notes:

- `actions` are executable deterministic similarity-dedup operations. Today
  they are delete plans for likely duplicate loser facts.
- `hygiene_candidates` are review evidence only. They may flag `secret_like`,
  `transient`, and `supersession` facts, but they are not deterministic apply
  operations.
- `counts` describes the proposed plan. `applied_counts` is `null` in dry-run
  mode and populated only after apply.
- `coverage.scanned` and `coverage.active_total` give the active fact count
  considered by the plan.
- `provider` stays `tracedecay` for standalone deterministic curation.
- `mode` is currently `similarity_dedup`.

For `tracedecay memory curate --llm`, the report may additionally include
`llm_review`:

```json
{
  "llm_review": {
    "status": "needs_llm_review",
    "clusters_reviewed": 2,
    "clusters": [],
    "hygiene_candidates": {},
    "allowed_fact_ids": [101, 102],
    "min_confidence": 0.5,
    "messages": [
      { "role": "system", "content": "..." },
      { "role": "user", "content": "..." }
    ],
    "next_step": "run the messages through an LLM and pass its {\"ops\": [...]} JSON back via: tracedecay memory curate --llm-ops <file> [--apply]"
  }
}
```

External LLM output must be strict JSON with an `ops` array. Each op includes
`op`, `confidence`, `reason`, and the required ids for that operation. The
TraceDecay validator rejects ops below the confidence floor or referencing fact
ids outside the recomputed clusters and hygiene candidates.

## Apply Contract

Use the generic apply contract after validation. Scheduled curation submits
policy-valid operations automatically; CLI and dashboard review is an
observability surface or explicit override, not an approval prerequisite:

```json
{
  "ops": [
    {
      "op": "delete",
      "fact_id": 102,
      "reason": "near-exact duplicate of fact 101"
    },
    {
      "op": "merge",
      "winner_id": 101,
      "loser_ids": [102],
      "merged_content": "Optional consolidated durable fact."
    }
  ]
}
```

Apply responses return per-op results plus aggregate counts. Per-op failures do
not make the whole request fail; malformed bodies are the whole-request failure
case. Merge may rewrite the winner content and then hard-delete losers. Delete
hard-deletes the target fact. Both paths write oplog entries, and delete oplog
details carry hashes rather than deleted content.

## Operator Workflow

1. **Resolve scope.** Confirm the active project root and memory store. Project
   profiles use user-level TraceDecay storage scoped to the project by default.
2. **Start read-mostly.** Prefer TraceDecay MCP graph/context tools, then
   fact-store `get`, `contradict`, `search`, `list`, `probe`, `related`, or
   `reason`. Note that some recall-style tools may update access metadata.
3. **Run a native dry-run.** Use dashboard preview or `tracedecay memory
   curate`. Save the report, preview timestamp, command/API shape, and project
   scope in your notes.
4. **Inventory candidates.** Split findings into `adds`, `updates`, `merges`,
   `deletes`, and `skipped`. Preserve fact ids, source/provenance, trust, tags,
   entities, similarity evidence, source spans, and counterevidence.
5. **Use subagents for evidence only.** Assign disjoint read-only research
   scopes such as session mining, duplicate review, or skeptic review.
   Subagents must not call add/update/remove/feedback tools or dashboard apply.
6. **Run the skeptic pass.** Reject unsupported, secret-like, local-only,
   transient, stale-but-uncertain, and ambiguous same-topic findings. Do not
   lower trust solely because a fact is old.
7. **Produce a dry-run curation report.** Include every proposed operation,
   every skipped candidate, and the apply-policy state for any destructive action.
8. **Apply policy-valid operations.** The curator/orchestrator submits
   add/update/merge/delete operations only after fresh evidence and risk
   validation. Record fact id, content/source summary, reason, permanent-delete
   warning, and the resulting receipt; review may observe or override but does
   not gate the run.
9. **Apply narrowly.** Use fact-store add/update only for directly supported
   facts. Use `--llm-ops <file> --apply` or `/curate/apply` only for validated
   ops. Avoid `POST /curate {"dry_run": false}` unless the operator explicitly
   requested the deterministic duplicate-deletion plan; scheduled automation
   uses the same validated apply contract without a review prerequisite.
10. **Verify read-only.** Re-run targeted get/search/list/contradict checks and
    inspect apply results/oplog. Report changed, skipped, rejected, and still
    ambiguous facts.

## Required Operator Report

Before any mutation, produce this compact report:

- `scope`: project root, store/scope, tool/API used, dry-run timestamp, and
  whether a memory health status read or dashboard start/stop was invoked.
- `native_plan`: `mode`, `provider`, `coverage`, `counts`, `actions` count, and
  hygiene-candidate counts.
- `adds`: durable candidate facts with source spans, category, entities, trust,
  duplicate-search result, and why they should be stored.
- `updates`: fact ids, old summary, new summary, evidence, confidence, and why
  update beats add.
- `merges`: winner id, loser ids, similarity evidence, retained provenance,
  optional `merged_content`, and why separate facts are redundant.
- `deletes`: fact ids, content/source summary, permanent-delete reason, risk,
  surviving fact if any, and apply-policy status.
- `skipped`: rejected transient, secret-like, unsupported, stale-but-uncertain,
  same-topic-not-duplicate, out-of-scope, or duplicate candidates.
- `verification_plan`: exact read-only checks to run after apply.

## Risk Tiers

| Tier | Operations | Default |
| --- | --- | --- |
| Read-mostly | MCP context/search, fact get/contradict/search/list/probe/related/reason, dry-run preview | allowed |
| Draft | propose add, update, merge, delete, retag-like notes, LLM review request | allowed |
| Low-risk apply | add clearly durable facts with source links | automatically apply after evidence and policy validation; record a durable receipt |
| Medium-risk apply | update or merge facts with retained source evidence | automatically apply when policy-valid; retain provenance and rollback/quarantine controls |
| High-risk apply | hard-delete facts or merge losers | automatically apply only when policy-valid; record the permanent effect and rollback/forward-recovery identity |

Deletion and merge loser removal remain high risk because they remove rows from
`memory_facts`; entity links cascade and FTS rows drop. There is no recovery
path other than independently reconstructing a fact from outside evidence.

## Subagent Roles

When subagents participate, give each one an explicit project selector and a non-overlapping ownership boundary such as a path set, memory namespace, report section, or review category. Subagents should return evidence-backed recommendations and exact target identifiers, leaving cross-scope reconciliation and destructive curation to the parent agent.


- **Session Scout**: mines bounded recent sessions and summaries for durable
  facts, explicit "remember" language, superseded facts, repeated pain points,
  and source spans.
- **Memory Curator**: clusters candidates against existing facts, similarity
  pairs, hygiene candidates, trust/access signals, and recall evidence.
- **Skeptic Reviewer**: tries to disprove each candidate by checking scope,
  contradictions, secret exposure, transient state, and same-topic false
  positives.
- **Telemetry Analyst**: measures hint uptake, accepted/rejected candidates,
  false positives, and audited net token deltas from real transcript data.
- **Apply Operator**: the parent/operator role only. It invokes mutating APIs
  after policy checks pass and records the resulting receipt; review remains an
  observability or explicit override surface.

For multi-agent runs, each role owns separate notes or database rows. Writers
do not share editable artifacts. The apply role consumes finalized plans only.

## Standalone And Wrapper Boundaries

- Standalone TraceDecay must remain deterministic and useful without Hermes or
  any LLM dependency.
- Hermes or another wrapper may own the LLM call, but it must build against the
  TraceDecay plan/apply contract and pass reviewed ids only.
- Wrapper planners operate on bounded clusters and hygiene candidates, never on
  the full unfiltered session corpus.
- Strict JSON is required for external ops. Unknown operations, low-confidence
  operations, and ids outside the evidence guard are rejected.
- Stock TraceDecay contracts must stay usable by the Hermes plugin; wrapper-only
  extras cannot become required server behavior.

## Permanent-Delete Guardrails

- Never promise archive, restore, undo, recycle-bin, or soft-delete behavior.
- Prefer update or merge-with-retained-provenance over delete when the old fact
  still carries useful history.
- Treat hygiene candidates as evidence for review, not as automatic deletes.
- Show content/source summaries before applying or reporting a delete; avoid
  copying secrets verbatim into reports.
- The curation owner automatically applies policy-valid operations after fresh
  evidence validation. CLI and dashboard review records outcomes and may issue
  explicit pause, disable, quarantine, or rollback overrides, but never blocks
  a valid apply. Every applied result must be recorded.
- Record partial failures and do not retry in a way that hides uncertainty.
- After apply, verify the resulting fact set and report any failed or skipped
  operation.

## Telemetry To Capture

Telemetry should measure usefulness without overstating savings:

- Hint emitted/followed/ignored, category match, latency, and dedupe status.
- Candidate lifecycle: mined, clustered, rejected, reviewed, accepted, applied,
  failed, or manually overridden.
- Operation risk: add, update, merge, delete, hygiene-only, or
  needs-human-review.
- Outcome quality: later recall helpful/unhelpful feedback, duplicate
  recurrence, manual corrections, and rejected-candidate reasons.
- Token accounting: audited net token delta using real transcript and usage
  data, not gross avoided-read estimates.

## Verification Targets

Existing code already covers core dry-run/apply behavior. When curation logic
changes, run the focused tests that match the touched surface:

- `cargo nextest run curation_delete_lifecycle`
- `cargo nextest run curate_apply_merge_with_missing_loser_is_atomic`
- `cargo nextest run validate_llm_ops_allows_delete_and_merge_with_candidate_evidence`

For docs-only changes, at minimum inspect the scoped diff and run a Markdown or
spell/style check if the project has one. Do not skip flaky tests to make CI
green; fix them or report the failure honestly.
