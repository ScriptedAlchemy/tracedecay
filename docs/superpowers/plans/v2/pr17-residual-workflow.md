# PR17 Residual Advanced Workflow Plan

**Goal:** Extend PR14's single graph/runtime authorities with advanced workflow
definition, fan-out/synthesis/recovery, placement, expertise, automation, and
host handoff behavior.

**Dependency:** PR14 Work and minimal runtime are accepted. PR17 may not create
a second task store, scheduler, provider dispatcher, clock, receipt, or Kanban
authority.

## Files and interfaces

- Extend Plan 24/32 domain/application/store/runtime authorities.
- Add workflow definition lifecycle and advanced placement/topology adapters.
- Add automation execution controls, expertise/calibration projections,
  host-task observations, LSP task handoff, and advanced Work controls.

Interfaces: versioned `WorkflowDefinition`, `WorkflowRunPlan`,
`AuxiliaryAttemptRequest`, `SynthesisPolicy`, `RecoveryDirective`,
`PlacementProposal`, `ExpertiseGrant`, `CalibrationSnapshot`,
`TaskHandoffToken`, and receipt-backed automation controls.

## Ordered slices

1. Acceptance/replan policy over PR14 immutable events.
2. Definition validation/activation and multi-step runtime.
3. Advanced topology/placement and explicit Git effect handoff.
4. Automation execution UI and controls.
5. Purpose-authorized ephemeral expertise and calibration/drift.
6. Bounded fan-out, isolated review, minority-preserving synthesis, recovery.
7. Host task observations and LSP handoff.
8. Aggregate residual workflow journey.

## Tests

Direct: define/activate a workflow, admit multi-step work, fan out bounded
independent attempts, synthesize without erasing minority evidence, recover a
fenced attempt, inspect placement, use ephemeral expertise under consent,
control it from Automations/Work, and hand off to a supported host/LSP client.

Negative: recursive dispatch, unbounded fan-out, hidden model choice, expertise
leak/durable ranking change, auto-replan/acceptance, stale definition/lease,
ambiguous Git effect, minority loss, provider failure, revocation, and host
capability mismatch fail closed.

## Migration, rollback, measurement, deletion

All records reference PR14 `TaskId`/`RunId`; no data fork. Rollback disables
advanced routes/capabilities while PR14 graph events and minimal runs remain
readable. Measure queue/fan-out/synthesis/recovery, placement, handoff, UI, and
event-to-ready. Delete scaffolds and duplicate advanced adapters only after
aggregate direct journey, consent/revocation, recovery, host parity, contracts,
and normal CI pass.
