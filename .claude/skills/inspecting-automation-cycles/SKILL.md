---
name: inspecting-automation-cycles
description: 'TraceDecay Dev: Use when auditing TraceDecay automation loops, skipped runs, memory-curator/session-reflector/skill-writer output, terminal receipts, or run artifacts.'
---

# TraceDecay Dev: Inspecting Automation Cycles

TraceDecay automation is a loop, not a single artifact: config schedules jobs,
runs produce artifacts, dashboards expose outcomes/telemetry, and usage
analytics prove whether generated output was adopted.

## Workflow

1. Start with `tracedecay automation config get` to identify enabled tasks,
   schedules, locks, and profile paths.
2. When the operator requests an immediate memory-curation cycle, use the sole
   public semantic launcher, `fact_store_curate`, through its MCP adapter
   `tracedecay_fact_store_curate`, generic CLI adapter `tracedecay tool
   fact_store_curate`, or HTTP adapter `POST
   /api/application/retained/fact_store_curate`. Its request accepts only the
   optional `fact_review_limit` and `min_confidence_millionths` bounds. Each
   adapter invokes the same daemon-owned operation; the daemon derives run
   identity, task selection, operations, validation, policy, and effect
   settlement.
3. List recent runs with `tracedecay automation runs list --limit 100`; group
   by task and status before opening individual artifacts.
4. Inspect one exact run with `tracedecay automation runs view <run_id>`.
   Read its verified artifacts with
   `tracedecay automation runs artifact <run_id> <kind> --json`,
   `GET /api/automation/runs/<run_id>/artifacts/<kind>`, or the read-only MCP
   tool `tracedecay_automation_run_artifact_view`. MCP automation analytics
   summarizes run history; exact MCP run list/view is not currently exposed.
5. Inspect terminal memory outcomes plus active managed-skill adoption:
   `tracedecay automation facts list`, dashboard telemetry, and
   `tracedecay_skill_list --state active`.
6. Check adoption evidence: `tracedecay analytics diagnostics --all --no-sync`,
   `tracedecay sessions search "mcp__tracedecay" --provider all`, and managed
   skill usage counts.

## Reading Results

| Signal | Meaning | Next step |
|---|---|---|
| `scheduler_interval_not_elapsed` | Healthy throttling | Count only, do not fix. |
| `scheduler_lock_active` | Another run owns the loop | Check age before calling stale. |
| `no_new_session_activity` | Nothing new to process | Verify transcript ingest if surprising. |
| `validation_gate` artifact | Mutation passed validation | Inspect terminal automatic application/deployment receipts. |
| Many automatic fact receipts | Inspect applied/quarantined outcomes and telemetry | Use `tracedecay automation facts list`. |
| Active managed skills with zero use | Adoption telemetry gap | Use `tracedecay:diagnosing-analytics`. |

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

Report the launcher adapter used, task/status counts, exact run or artifact ids,
terminal automatic application/deployment receipts, read-only verification,
and adoption gaps.
