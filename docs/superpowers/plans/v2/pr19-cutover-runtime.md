# PR19 Runtime Fresh-Store Reset

> Historical planning evidence only. Current scope and acceptance come from
> [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate branch/worktree/SHA protocols, gate lattices,
> generated-byte/source-shape checks, or transition-era inventories.

**Goal:** Admit one exact final V2 persisted shape, return `ResetRequired` for
every other shape, and retain only independently released public API protocol
compatibility.

Every TraceDecay database, store, spool, file, journal, checkpoint, receipt,
and projection accepts only its exact final shape. A non-final shape is refused
before interpretation and requires an explicit reset or recreation. There is
no stored-data reader, conversion, backfill, dual write, shadow read, census,
or recovery path, including for data written by an older installed binary.

## Scope

- Validate final-shape admission at each persisted-state open boundary.
- Preserve one fenced writer and canonical daemon route for a valid final
  store.
- Provide an explicit reset/recreation action scoped to a refused target.
- Delete storage-transition code and source-only aliases after internal callers
  move.
- Retain a public protocol façade only with evidence of an actual independent
  package or API release; it delegates to the canonical operation and owns no
  storage or lifecycle behavior.

## Direct acceptance

- Exact-final fixtures admit through the canonical daemon route.
- Every older, partial, unknown, unversioned, or foreign persisted fixture
  returns `ResetRequired` before read, write, replay, or projection.
- Explicit reset/recreation creates a clean final store without consuming old
  bytes.
- Tests prove no stored-data reader, converter, backfill, dual write, shadow
  read, census, or recovery route remains.
- Retained public protocol compatibility is independently release-evidenced
  and preserves canonical authorization, errors, redaction, effects,
  pagination, streaming, cancellation, and retry behavior.

## Not in PR19

- Persisted-data conversion, rollback, retention, or recovery workflows.
- Memory special handling.
- Transition dashboards, execution ledgers, schema-only conformance suites, or
  placeholder acceptance baselines.
