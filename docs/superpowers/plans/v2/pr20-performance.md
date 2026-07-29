# PR20 Measured Performance and Cleanup Plan

**Goal:** Close V2 with same-host measured product speed, bounded storage and
tests, accepted crate-boundary gains, and deletion of superseded paths.

## Files and interfaces

- Plan 33 benchmark/evidence assets, nextest JUnit and slow-test budgets.
- Production event-to-ready telemetry, Plan 38 retention/storage evidence.
- Query/runtime/index optimizations, build/test/package scripts, and final
  boundary receipts.

Interfaces: versioned `MeasurementEnvironment`, `EditClassReceipt`,
`JourneyTiming`, `EventToReadyReceipt`, `SlowTestBudget`,
`StorageBudget`, and optimization `Disposition` (`Accept`, `Pending`,
`Reject`). Each receipt records host, toolchain, features, warmth, samples,
rebuilt units, semantic digest, and baseline/treatment commits.

## Ordered slices

S0. Freeze measurement contract and A/A noise.
S1. Publish JUnit totals and enforce slow-test budgets.
S2. Measure production capture/edit/event-to-ready journeys.
S3. Close Plan 38 storage/index retention and Doctor evidence.
S4. Optimize only measured runtime/query/index bottlenecks.
S5. Reduce build/test/package critical paths without contract loss.
S6. Accept/reject every extraction boundary using Gate A/B evidence.
S7. Delete dead paths and publish final V2 summary.

## Tests

Direct: run the same representative user journeys, edit classes, focused tests,
packages, and storage workloads at baseline/treatment and compare semantic
digests plus resource use.

Negative: cross-host comparison, cold/warm mismatch, changed features, missing
sample, A/A-sized delta, semantic drift, masked test, raised timeout, omitted
JUnit, unbounded storage, and unavailable metric cannot produce `Accept`.

## Migration, rollback, measurement, deletion

Every optimization is isolated and reverted if it misses the Linux same-host
threshold or semantic equivalence. `Pending` names dominant units and remains
visible; it is never reported as a win. Storage changes retain verified backup
and restoration evidence.

Delete superseded code, benchmarks, aliases, feature exceptions, and temporary
façades only after default/all/no-default/lite/package/platform gates, JUnit,
event-to-ready, retention, accepted boundaries, direct journeys, and designated
safe dogfood pass.
