---
design_status: current
evidence_class: concept_synthetic
---

# Definition lifecycle and run ledger

## User job

Inspect exactly what a workflow version will do and which policy,
configuration, and catalog revisions it pins; validate whether it is eligible
for use; make an authorized lifecycle decision; and read the exact history and
outcomes of runs without mistaking a rendered button for a completed action.

## Product behavior

- The definition registry exposes stable workflow identity, immutable version,
  lifecycle status, and authoritative update time. Selection changes only the
  detail view.
- The center workspace decodes the selected version's steps and names every
  pinned policy/config/catalog identity and digest. A missing or mismatched pin
  remains a blocking typed state.
- Version history distinguishes created, validated, active, retired, and
  rejected transitions, including the source authority and immutable receipt.
- Activate, Retire, and Reject are production commands only when validation,
  permission, and expected-revision checks are served. Conflict, refusal,
  denial, stale input, and unavailable authority fail closed. The UI never
  changes lifecycle status before the daemon confirms the compare-and-swap.
- Exact run lookup and history remain independent from definition readiness.
  A run shows its own version, timing, state, decoded steps, pinned references,
  artifacts, and outcome evidence.
- Selecting a step or run can open Work, Agents, Sessions, Code, checks, or
  Delivery at the matching stable identity while preserving a return path.

## Interaction and evidence

Arrow keys move through definitions and versions; Enter selects; lifecycle
commands open an explicit review/confirmation surface that states permission,
validation, expected revision, and the operation that will occur. The result
region is populated only by the production receipt.

Every definition, digest, transition, command, and run uses `EXACT`,
`EXPLICIT`, `INFERRED`, `AMBIGUOUS`, `STALE`, or `UNAVAILABLE`. Registry facts
and lifecycle receipts are direct authority facts; human or agent rationale is
an attributed persisted claim, not automatic truth.

## Acceptance gates

- Keyboard traversal, selection, confirmation, cancellation, and run lookup
  match pointer behavior and expose a visible focus state.
- Reduced motion preserves lifecycle and run meaning without animated tracks,
  bloom, or auto-scrolling.
- At 200% browser zoom, registry, definition detail, and run history reflow or
  become focus modes; long identifiers and structured step content wrap,
  scroll, or expand without silent truncation.
- Dense real registries and run histories use search, filters, pagination,
  stable selection, and virtualized exact tables.
- Exact definition/version/step/run tables and structured-text views are the
  complete accessible fallback, including source identity, timestamps,
  permissions, validation, expected revisions, and result receipts.

## Truth boundary

The plate is `CONCEPT / SYNTHETIC DATA`. Its sample workflow names, digests,
versions, timestamps, step results, and successful lifecycle state are not
runtime receipts. If validation, authorization, compare-and-swap, or run
authority is unavailable, the shipping control fails closed and says why.

## Production authorities

The definition registry owns immutable definitions and pins; validation,
permission, and lifecycle services own command eligibility and result receipts;
the run projection owns run history and outcomes. The concrete composition
targets and browser architecture are listed in [`README.md`](README.md) and
[`IMPLEMENTATION.md`](../../IMPLEMENTATION.md).
