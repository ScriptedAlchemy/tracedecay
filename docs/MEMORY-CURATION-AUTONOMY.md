# Final-V2 Agent-Managed Memory Curation

## Purpose

This guide describes the single final-V2 memory-curation authority. It applies
to project and profile memory, the automation runner, its dashboard
observation, its transport adapters, and its durable terminal receipts.

The curator mines bounded canonical facts, asks the configured agent backend
for supported operations, validates them, and automatically commits only
policy-valid results. Durable evidence, receipts, telemetry, and typed partial
effects are the public boundary. Human surfaces may launch a run and inspect
its terminal state; they do not select its operations or settle its effects.

## Current Curation Surfaces

Memory curation is automation-owned and policy-governed. Subagents may inspect
and classify evidence within their assigned scope, but the curator alone may
select, validate, and commit supported policy-valid operations through the
configured curation contract. Those operations include destructive merge and
remove effects only when their reviewed-event CAS and policy checks succeed;
callers cannot request or approve them individually. Each run produces a
ledger record, activity events, post-action telemetry, and, when present,
advertised artifacts.

This is a breaking public-surface cutover. The duplicate launchers
`POST /api/automation/run/memory-curator`, `tracedecay_memory_automation_run`,
and `tracedecay automation run memory-curation` were removed. They are not
aliases: every caller must use `fact_store_curate`, while the existing
automation run list, view, and artifact reads remain unchanged.

`fact_store_curate` is one public semantic launcher. MCP, generic CLI, and HTTP
are adapters for that same retained application operation, not three launchers.
Its request accepts only `fact_review_limit` and
`min_confidence_millionths`; callers cannot supply task, run identity,
operations, validation, policy, or effect authority.

Use existing TraceDecay surfaces before inventing a new plan format:

- Canonical fact tools: direct reads through `tracedecay_fact_store_get`,
  `tracedecay_fact_store_search`, `tracedecay_fact_store_list`,
  `tracedecay_fact_store_probe`, `tracedecay_fact_store_related`,
  `tracedecay_fact_store_reason`, and `tracedecay_fact_store_contradict`;
  separate exact administration through `tracedecay_fact_store_add`,
  `tracedecay_fact_store_update`, and `tracedecay_fact_store_remove`.
- `tracedecay_memory_status`: read-only memory authority and coverage health;
  use only when health/counts are part of the task. Similarity and dedupe
  evidence comes from the bounded verified Grafeo projection, not an alternate
  derived structure.
- HTTP adapter:
  `POST /api/application/retained/fact_store_curate` runs the autonomous app-server
  memory curator. Accepted operations are committed according to automation
  policy and every phase is logged to the run ledger and curation activity
  stream.
- Generic CLI adapter: `tracedecay tool fact_store_curate --args
  '{"fact_review_limit":24,"min_confidence_millionths":720000}'` invokes the
  same retained application operation as HTTP and MCP.
- MCP adapter: `tracedecay_fact_store_curate` accepts only
  `fact_review_limit` and `min_confidence_millionths` bounds. The daemon owns run identity,
  task selection, operations, validation, and effect settlement.
- Read-only inspection: `tracedecay_automation_run_list`,
  `tracedecay_automation_run_view`, and
  `tracedecay_automation_run_artifact_view` expose the ledger and only the
  artifact kinds advertised by a run. CLI equivalents are `tracedecay
  automation runs list --json`, `tracedecay automation runs view <run_id>
  --json`, and `tracedecay automation runs artifact <run_id> <kind> --json`.
- Read-only HTTP inspection: `GET /api/automation/runs?limit=<limit>` lists
  terminal rows, `GET /api/automation/runs/{run_id}/artifacts` lists and
  verifies that run's published artifact chain, and `GET
  /api/automation/runs/{run_id}/artifacts/{kind}` reads one verified advertised
  artifact. HTTP does not expose a separate exact-run record route.
- Terminal evidence: the exact run record, its advertised artifacts, and
  committed curation receipts are the settled outcomes. A typed partial effect
  carries its committed receipt and requires reconciliation; it is never safe
  to replay blindly.

## Runner Contract

The backend sees a bounded canonical-fact context and returns strict JSON:

```json
{
  "ops": [
    {
      "op": "normalize_tags",
      "target": {
        "fact_id": "fact...",
        "expected_last_event_id": "event..."
      },
      "tags": ["memory"],
      "evidence_facts": [{
        "fact_id": "fact...",
        "expected_last_event_id": "event..."
      }],
      "confidence": 0.92
    }
  ]
}
```

Operator notes:

- Supported automatic operations are canonical add, update, merge, remove,
  normalize-tags, and link-facts effects. Every operation is bound to the
  reviewed fact event snapshots and passes policy/privacy validation before
  the atomic curation batch is committed; unsupported or stale shapes are
  rejected without exposing an approval/apply lane.
- Every fact id must come from the bounded canonical context, every confidence
  must meet the configured floor, and timestamps are not truth evidence.
- The runner owns validation repair and commits accepted operations within the
  same terminal run.

## Terminal Contract

Successful operations carry canonical commit receipts. A failure before any
commit is an ordinary typed application failure. A failure after a commit is a
typed `partial_effect` with the committed `EffectReceipt`, retry `never`, and
legal action `reconcile`. That receipt must survive HTTP, MCP, CLI, SDK, and
daemon restart boundaries unchanged.

## Operator Workflow

1. **Resolve scope.** Confirm the active project root and memory store. Project
   profiles use user-level TraceDecay storage scoped to the project by default.
2. **Start read-mostly.** Prefer TraceDecay MCP graph/context tools, then
   fact-store `get`, `contradict`, `search`, `list`, `probe`, `related`, or
   `reason`. Note that some recall-style tools may update access metadata.
3. **Run the canonical curator.** Use `tracedecay_fact_store_curate`,
   `tracedecay tool fact_store_curate`, or the retained application HTTP
   endpoint. The runner validates and commits supported backend output in one
   bounded operation.
4. **Use subagents for evidence only.** Assign disjoint read-only research
   scopes such as session mining, duplicate review, or skeptic review.
   Subagents must not call add/update/remove/feedback tools.
5. **Run the skeptic pass.** Reject unsupported, secret-like, local-only,
   transient, stale-but-uncertain, and ambiguous same-topic findings. Do not
   lower trust solely because a fact is old.
6. **Inspect the terminal result read-only.** Use
   `tracedecay_automation_run_list` to find the run,
   `tracedecay_automation_run_view` for its exact ledger record, and
   `tracedecay_automation_run_artifact_view` only for an artifact kind named by
   that record. Report every committed receipt, rejection, and reconciliation
   requirement. A `partial_effect` is terminal evidence of a committed mutation
   and must not be retried as if nothing happened.
7. **Use direct fact operations only for exact administration.** Fact-store
   add, update, and remove remain independent retained operations and never
   continue a curator run. Removal requires an exact user instruction because
   it is permanent; if the target is ambiguous, resolve and confirm only that
   exact target.
8. **Verify read-only.** Re-run targeted get/search/list/contradict checks and
   inspect the oplog or receipt projection. Report changed, skipped, rejected,
   and still ambiguous facts.

## Required Operator Report

For each completed run, produce this compact report:

- `scope`: project/profile identity and the launcher adapter used.
- `evidence`: bounded coverage, unavailable facts, and counterevidence.
- `terminal`: committed receipts, rejections, or typed partial
  effect with reconciliation action.
- `verification`: exact read-only checks and the resulting canonical fact
  state.

## Risk Tiers

| Tier | Operations | Default |
| --- | --- | --- |
| Read-mostly | MCP context/search and canonical fact reads | allowed |
| Automatic curation | add, update, merge, remove, normalize-tags, and link-facts selected from reviewed evidence | the runner validates exact reviewed-event CAS and policy, commits atomically, and records a durable receipt |
| Exact administration | direct fact add, update, or remove | requires an exact caller instruction and its own retained-operation receipt |

Deletion remains high risk because it permanently removes canonical memory.
The automatic curator may select remove or merge only inside its closed,
policy-validated batch; a caller cannot force that choice. A direct removal is
a separate exact-administration request and requires an exact user instruction.

## Subagent Roles

When subagents participate, give each one an explicit project selector and a non-overlapping ownership boundary such as a path set, memory namespace, report section, or review category. Subagents should return evidence-backed recommendations and exact target identifiers, leaving cross-scope reconciliation and destructive curation to the parent agent.


- **Session Scout**: mines bounded recent sessions and summaries for durable
  facts, explicit "remember" language, superseded facts, repeated pain points,
  and source spans.
- **Memory Curator**: inspects bounded canonical facts, trust/access signals,
  and recall evidence.
- **Skeptic Reviewer**: tries to disprove each candidate by checking scope,
  contradictions, secret exposure, transient state, and same-topic false
  positives.
- **Telemetry Analyst**: measures hint uptake, accepted/rejected candidates,
  false positives, and audited net token deltas from real transcript data.
- **Run Observer**: inspects terminal receipts and verifies resulting canonical
  state without effect authority.

For multi-agent runs, each role owns separate notes or database rows. Writers
do not share editable artifacts. The automation runner alone validates and
commits automatic curation output.

## Standalone And Wrapper Boundaries

- Standalone TraceDecay retains one automation contract regardless of which
  configured backend performs the model call.
- Backends receive only bounded canonical facts, never an unfiltered session
  corpus or a stored holographic-vector bank.
- Strict JSON is required. Unknown operations, low-confidence operations, and
  ids outside the evidence guard are rejected.
- Host wrappers may launch or observe the run but cannot supply operations or
  settle effects.

## Permanent-Delete Guardrails

- Never promise archive, restore, undo, recycle-bin, or soft-delete behavior.
- Automatic curation never hard-deletes a fact.
- Direct deletion requires an exact user instruction and a canonical retained
  operation receipt; avoid copying secret content into reports.
- Record partial effects and do not retry in a way that hides uncertainty.
- After any mutation, verify the resulting fact set and report failed or
  skipped operations.

## Telemetry To Capture

Telemetry should measure usefulness without overstating savings:

- Hint emitted/followed/ignored, category match, latency, and dedupe status.
- Candidate lifecycle: mined, rejected, validated, committed, failed, or
  reconciled.
- Operation kind: normalize tags, link facts, or independent direct
  administration.
- Outcome quality: later recall helpful/unhelpful feedback, duplicate
  recurrence, manual corrections, and rejected-candidate reasons.
- Token accounting: audited net token delta using real transcript and usage
  data, not gross avoided-read estimates.

## Verification Targets

When curation logic changes, run the focused agent-host automation and public
journey tests that cover validation, application, partial effects, restart
projection, cancellation, and exact project/profile isolation.

For docs-only changes, at minimum inspect the scoped diff and run a Markdown or
spell/style check if the project has one. Do not skip flaky tests to make CI
green; fix them or report the failure honestly.
