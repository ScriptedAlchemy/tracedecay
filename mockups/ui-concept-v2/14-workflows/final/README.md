---
design_status: current
evidence_class: concept_synthetic
---

# Workflows final state set

This folder is the authoritative implementation reference for Workflows.
Workflows lets an authorized user inspect immutable definitions, validate their
pinned dependencies, understand lifecycle history, activate or retire a
specific version through compare-and-swap, and inspect exact run history.

All identities and values pictured here are concept/synthetic. Production must
bind every definition, digest, version, step, permission, command, conflict,
and run result to the evidence ladder in
[`DESIGN-SYSTEM.md`](../../DESIGN-SYSTEM.md). A visible control never implies
that activation, retirement, rejection, or execution has occurred.

## State manifest

| State | Image | Product brief | Status |
|---|---|---|---|
| Definition lifecycle and run ledger | [01-definition-lifecycle-ledger.png](01-definition-lifecycle-ledger.png) | [01-definition-lifecycle-ledger.md](01-definition-lifecycle-ledger.md) | approved |

## Shared interaction contract

- The registry lists stable workflow identities and immutable versions.
  Selecting a definition is read-only and does not alter lifecycle state.
- Definition detail shows decoded steps plus exact pinned policy, configuration,
  and catalog identities and digests. Missing, stale, mismatched, or denied
  references block validation and remain visible.
- Validation is a production daemon result, not a client-side promise.
  Activation, retirement, and rejection require authorization, validation,
  expected revision, and the daemon's compare-and-swap command/result path.
- Lifecycle history records who or what requested the transition, source time,
  expected and observed revision, permission decision, validation result, and
  immutable outcome receipt. No optimistic green state is permitted.
- Run history is an independent exact lookup/projection. A loaded definition
  does not imply a run exists; concealed, denied, unavailable, stale, running,
  failed, cancelled, and successful runs remain distinct.
- A selected run links to exact steps, attempts, agents, sessions, code inputs,
  artifacts, checks, and Delivery evidence without rewriting immutable source
  records.

## Browser and accessibility contract

- React/DOM owns the registry, definition and run tables, lifecycle controls,
  permission/refusal copy, exact digests, keyboard order, and accessible names.
  A shared scene layer may visualize step or run topology but cannot own
  lifecycle state or command completion.
- Every definition, version, run, and lifecycle command is reachable by
  keyboard with visible focus and an explicit confirmation/result boundary.
- At 200% browser zoom, the three-column layout reflows into independently
  addressable definition, detail, and run regions or a focus mode; identifiers,
  YAML/JSON, and result messages remain readable rather than clipped.
- Reduced motion removes transition animation and activity bloom. Static state
  text, icons, line patterns, timestamps, and receipts preserve meaning.
- Dense registries and run histories use virtualized tables, stable selection,
  search, filters, pagination, and exact-text export. No definition, version,
  step, or run becomes accessible only through a canvas.

## Production authorities

- The workflow definition registry owns stable workflow identity, immutable
  versions, decoded steps, and pinned policy/config/catalog references.
- Workflow validation and lifecycle authorities own schema/policy validation,
  permission checks, expected revision, compare-and-swap transitions,
  conflicts, refusals, and immutable result receipts.
- The workflow run projection owns run identity, state, timing, decoded steps,
  artifacts, and links to Work, Agents, Sessions, Code, checks, and Delivery.
- `dashboard/src/workspaces/workflows/WorkflowsPage.tsx` and
  `workflowQueries.ts` compose the canonical `/application/workflow` routes;
  generated contracts decode every rendered value.
- [`IMPLEMENTATION.md`](../../IMPLEMENTATION.md) owns the hybrid DOM/scene
  boundary, density strategy, and renderer proof-of-capability decision.
