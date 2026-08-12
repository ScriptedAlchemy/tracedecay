---
name: inspecting-automation-cycles
description: 'TraceDecay Dev: Use when auditing TraceDecay automation loops, skipped runs, memory-curator/session-reflector/skill-writer output, automatic application/deployment receipts, or run artifacts.'
---

# TraceDecay Dev: Inspecting Automation Cycles

TraceDecay automation is a loop, not a single artifact: config schedules jobs,
runs produce artifacts, dashboards expose outcomes/telemetry, and usage
analytics prove whether generated output was adopted.

## Workflow

1. Start with `tracedecay automation config get` to identify enabled tasks,
   schedules, locks, profile paths, backend, and host mode.
2. List recent runs with `tracedecay automation runs list --limit 100`; group
   by task and status before opening individual artifacts.
3. For failures or suspicious skips, open the relevant artifact with
   `tracedecay_automation_run_artifact_view` or
   `tracedecay tool automation_run_artifact_view --args ...`.
4. Inspect model-managed memory outcomes plus active managed-skill adoption:
   `tracedecay automation facts list`, dashboard telemetry, and
   `tracedecay_skill_list --state active`.
5. Check adoption evidence: `tracedecay analytics diagnostics --all --no-sync`,
   `tracedecay sessions search "mcp__tracedecay" --provider all`, and managed
   skill usage counts.

## Reading Results

| Signal | Meaning | Next step |
|---|---|---|
| `scheduler_interval_not_elapsed` | Healthy throttling | Count only, do not fix. |
| `scheduler_lock_active` | Another run owns the loop | Check age before calling stale. |
| `no_new_session_activity` | Nothing new to process | Verify transcript ingest if surprising. |
| `validation_gate` artifact | Mutation passed validation | Inspect automatic application/deployment receipts. |
| Many automatic fact outcome records | Inspect validation/application receipts | Use `tracedecay:project-memory`. |
| Active managed skills with zero use | Adoption telemetry gap | Use `tracedecay:diagnosing-analytics`. |

## Guardrails

- Prefer read-only inspection. Do not mutate fact records.
- Validated skill-writer output activates and deploys automatically; inspect
  receipts rather than waiting for a manual gate.
- Do not treat skipped runs as failures until grouped by skip reason and age.
- Avoid parallel `tracedecay_skill_view` calls against one profile while
  automation may write usage ledgers. If a usage read reports a truncated JSON
  or EOF parse error, retry once after `tracedecay_skill_list` succeeds.
- Do not read `.tracedecay` databases directly; use CLI, dashboard APIs, or
  MCP tools.

## Deliverable

Report task/status counts, the exact run or artifact ids inspected, automatic
application/deployment state, adoption gaps, and the next concrete command for
any direct operator override the user should choose.
