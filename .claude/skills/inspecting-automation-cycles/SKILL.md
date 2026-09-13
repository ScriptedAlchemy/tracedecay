---
name: inspecting-automation-cycles
description: "Inspect TraceDecay automation run outcomes, suspicious skips, and curator or skill-writer validation receipts."
---

# TraceDecay Dev: Inspecting Automation Cycles

TraceDecay automation is a loop, not a single artifact: config schedules jobs,
runs produce artifacts, dashboards expose outcomes/telemetry, and usage
analytics prove whether generated output was adopted.

## Choose the relevant evidence

- For a known run, start with `tracedecay automation runs view <run_id>` and
  read only an artifact kind it advertises through `tracedecay automation runs
  artifact <run_id> <kind> --json` or
  `tracedecay_automation_run_artifact_view`.
- For scheduler or aggregate health, inspect `tracedecay automation config get`
  and a bounded `tracedecay automation runs list`; group by task, status, and
  skip reason before opening suspicious runs.
- For memory or skill settlement, use `tracedecay automation facts list`,
  dashboard telemetry, and `tracedecay_skill_list --state active` as relevant.
  Add analytics and session evidence only when adoption is part of the question.

When the operator explicitly requests an immediate memory-curation cycle, use
the sole semantic launcher, `fact_store_curate`, through
`tracedecay_fact_store_curate`, `tracedecay tool fact_store_curate`, or `POST
/api/application/retained/fact_store_curate`. It accepts only
`fact_review_limit` and `min_confidence_millionths`; the daemon owns run
identity, validation, policy, and settlement.

## Reading Results

| Signal | Meaning | Next step |
|---|---|---|
| `scheduler_interval_not_elapsed` | Healthy throttling | Count only, do not fix. |
| `scheduler_lock_active` | Another run owns the loop | Check age before calling stale. |
| `no_new_session_activity` | Nothing new to process | Verify transcript ingest if surprising. |
| `validation_gate` artifact | Mutation passed validation | Inspect terminal automatic application/deployment receipts. |
| Many automatic fact receipts | Inspect applied/quarantined outcomes and telemetry | Use `tracedecay automation facts list`. |
| Active managed skills with zero use | Possible lack of opportunity or telemetry coverage | Confirm a missed relevant task before diagnosing adoption. |

## Guardrails

- After any requested run, keep inspection read-only. Do not submit curator
  operations or approve, reject, or apply its output.
- Validated memory-curator and skill-writer output settles automatically;
  inspect receipts rather than waiting for a manual gate.
- Do not treat skipped runs as failures until grouped by skip reason and age.
- Avoid parallel `tracedecay_skill_view` calls against one profile while
  automation may write usage ledgers. If a usage read reports a truncated JSON
  or EOF parse error, retry once after `tracedecay_skill_list` succeeds.
- Do not read `.tracedecay` databases directly; use CLI, dashboard APIs, or
  MCP tools.

## Deliverable

Report the inspected run/artifact ids, relevant terminal outcomes, evidence,
and limitations. Include launcher details, aggregate counts, or adoption findings
only when those were part of the task; a read-only review needs no new run.
