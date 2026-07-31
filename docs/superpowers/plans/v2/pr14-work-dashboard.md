# PR14 Work, Doctor, and Dashboard Plan

> **Archived provenance — not current requirements.** This document records
> historical planning and execution evidence. Current scope and acceptance come
> only from [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate its task checklists, file inventories,
> branch/worktree/SHA or commit protocol, Gate A/B, timing/JUnit receipts, exact
> test names/counts, generated-byte/source-shape checks, PR closure gates, or
> platform gate lattice.
> Historical version/compatibility/migration language cannot resurrect
> branch-only transient scaffolding. Potentially persisted Work events, graph
> records, leases, journals, checkpoints, projections, and receipts keep
> backward-read/recovery until a separately authorized registered-store/profile
> census proves absence.

**Goal:** Ship the core Plan 24 graph, minimal Plan 32 runtime, Work workspace,
Doctor remediation, and all thirteen truthful dashboard workspaces.

**Historical cut:** PR14 grouped Work events/CAS/readiness/evidence, proposal review
and separate admission/accept-reject-replan commands, one real provider path,
leases/attempts/progress/cancel-resume/restart fencing/terminal evidence, and
core Work projections. PR17 owns advanced workflow capability.

## Historical file and interface inventory

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

## Historical ordered commit slices

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

## Product outcome contributed

The work contributed one truthful Work event/runtime authority, explicit
proposal/admission/acceptance transitions, Doctor remediation, and dashboard
views that did not fabricate unavailable or partial state. Current direct
behavior, accessibility, visual, and acceptance requirements live in the
applicable numbered V2 plans.

## Historical migration, rollback, measurement, and deletion notes

Pure transient PR14 DTOs change in place. Potentially persisted task/runtime
records retain versioned replay until the separately authorized
registered-store/profile census proves absence. Verify deterministic projections
before route activation. Rollback disables
routes/capabilities and reverts code while immutable events remain readable;
never dual-write or auto-accept.

Measure Work projection payload/render budgets, SSE sustain, provider
event-to-ready, root/API edit classes, and dashboard bundle limits. Delete
dashboard-local truth, duplicate task stores/schedulers/readiness, private
Doctor policy, and compatibility routes only after direct journey, migration,
rollback, contracts, normal CI, and all workspace acceptance pass.
