# PR17 Residual Advanced Workflow Plan

> **Archived provenance — not current requirements.** This document records
> historical planning and execution evidence. Current scope and acceptance come
> only from [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate its task checklists, file inventories,
> branch/worktree/SHA or commit protocol, Gate A/B, timing/JUnit receipts, exact
> test names/counts, generated-byte/source-shape checks, PR closure gates, or
> platform gate lattice.
> Historical version/compatibility/migration language cannot resurrect
> branch-only transient scaffolding. Potentially persisted workflow
> definitions, runs, attempts, effects, journals, checkpoints, and receipts
> keep backward-read/recovery until a separately authorized
> registered-store/profile census proves absence.

**Goal:** Extend PR14's single graph/runtime authorities with advanced workflow
definition, fan-out/synthesis/recovery, placement, expertise, automation, and
host handoff behavior.

**Historical dependency assumption:** PR14 supplied Work and a minimal runtime.
The recorded PR17 cut did not create
a second task store, scheduler, provider dispatcher, clock, receipt, or Kanban
authority.

## Historical file and interface inventory

- Extend Plan 24/32 domain/application/store/runtime authorities.
- Add workflow definition lifecycle and advanced placement/topology adapters.
- Add automation execution controls, expertise/calibration projections,
  host-task observations, LSP task handoff, and advanced Work controls.

Interfaces: versioned `WorkflowDefinition`, `WorkflowRunPlan`,
`AuxiliaryAttemptRequest`, `SynthesisPolicy`, `RecoveryDirective`,
`PlacementProposal`, `ExpertiseGrant`, `CalibrationSnapshot`,
`TaskHandoffToken`, and receipt-backed automation controls.

## Historical ordered slices

1. Acceptance/replan policy over PR14 immutable events.
2. Definition validation/activation and multi-step runtime.
3. Advanced topology/placement and explicit Git effect handoff.
4. Automation execution UI and controls.
5. Purpose-authorized ephemeral expertise and calibration/drift.
6. Bounded fan-out, isolated review, minority-preserving synthesis, recovery.
7. Host task observations and LSP handoff.
8. Aggregate residual workflow journey.

## Product outcome contributed

The work contributed advanced workflow definition, bounded fan-out and
synthesis, recovery, placement, consent-bound expertise, automation controls,
and host/LSP handoff without introducing duplicate authorities. Current direct
behavior and acceptance live in the applicable numbered V2 plan.

## Historical migration, rollback, measurement, and deletion notes

All records reference PR14 `TaskId`/`RunId`; no data fork. Rollback disables
advanced routes/capabilities while PR14 graph events and minimal runs remain
readable. Measure queue/fan-out/synthesis/recovery, placement, handoff, UI, and
event-to-ready. Delete scaffolds and duplicate advanced adapters only after
aggregate direct journey, consent/revocation, recovery, host parity, contracts,
and normal CI pass.
