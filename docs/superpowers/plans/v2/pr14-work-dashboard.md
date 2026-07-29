# PR14 Work, Doctor, and Dashboard Plan

**Goal:** Ship the core Plan 24 graph, minimal Plan 32 runtime, Work workspace,
Doctor remediation, and all thirteen truthful dashboard workspaces.

**Cut:** PR14 owns canonical work events/CAS/readiness/evidence, proposal review
and separate admission/accept-reject-replan commands, one real provider path,
leases/attempts/progress/cancel-resume/restart fencing/terminal evidence, and
core Work projections. PR17 owns advanced workflow capability.

## Files and interfaces

- Domain/application/store: `crates/tracedecay-domain`,
  `crates/tracedecay-application`, `crates/tracedecay-store`, owner-shard root
  adapters.
- API/SSE: `crates/tracedecay-api`, `src/dashboard`, generated contracts.
- UI: `dashboard/src/workspaces/work/**`, shell/navigation, Doctor and existing
  workspace seams.
- Tests: domain/store/application/API integration, `dashboard_api_test`,
  dashboard DOM/Vitest, Playwright/a11y/visual/performance journeys.

Core interfaces: `TaskId`, immutable `WorkEvent`, expected-version
`WorkCommand`, `WorkProjection`, `WorkProposal`, `ExecutionAdmission`,
`RunId`, `AttemptId`, `RunControl`, `TerminalEvidence`, and
`task_activity` SSE. Runtime completion never auto-accepts work.

## Ordered commit slices

1. Doctor authority and HTTP/remediation.
2. Work domain identity/events/CAS/readiness.
3. Owner-shard persistence and deterministic rebuild.
4. Application commands, evidence, proposal review, and projections.
5. Minimal Plan 32 provider runtime and fences.
6. Generated Rust/TypeScript contracts.
7. HTTP commands/queries and `task_activity` SSE.
8. Kanban, DAG, timeline, causal, workload, topology-read, deep links.
9. Remaining Code/Agents/Sessions/Knowledge seams.
10. Remaining Observatory/Automations/Costs/Loom seams.
11. Doctor UI and authorized remediation.
12. Desktop-first visual/performance/a11y/usability acceptance.
13. End-to-end migration, rollback, deletion, and aggregate journey.

## Direct and negative acceptance

Direct: create work, inspect the same TaskId in every projection, expand exact
evidence, review/accept a proposal, separately admit one supported native
provider, watch/cancel/resume/restart, inspect sealed terminal evidence, and
separately accept/reject/replan. Exercise Doctor diagnosis/remediation and all
thirteen workspaces without fabricated state.

Negative: cycle/stale CAS, wrong project/user, partial evidence, unavailable
provider, budget/deadline/lease loss, duplicate effect, crash recovery, SSE
overflow, denied remediation, unsupported data, no-Git work, and completion
without acceptance remain truthful and fail closed.

Desktop visual baselines are 1280×720 and 1440×900. Functionality remains
axe-clean and keyboard-complete at 320/768/1024, 400% zoom, reduced motion,
forced colors, and responsive layouts. Manual AT and the twelve-participant
Plan 11 study remain acceptance evidence.

## Migration, rollback, measurement, deletion

Migrate existing task/runtime-compatible records through versioned replay;
verify deterministic projections before route activation. Rollback disables
routes/capabilities and reverts code while immutable events remain readable;
never dual-write or auto-accept.

Measure Work projection payload/render budgets, SSE sustain, provider
event-to-ready, root/API edit classes, and dashboard bundle limits. Delete
dashboard-local truth, duplicate task stores/schedulers/readiness, private
Doctor policy, and compatibility routes only after direct journey, migration,
rollback, contracts, normal CI, and all workspace acceptance pass.
