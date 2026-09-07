# PR20 Measured Performance and Cleanup Plan

> **Archived provenance — not current requirements.** This document records
> historical planning and execution evidence. Current scope and acceptance come
> only from [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate its task checklists, file inventories,
> branch/worktree/SHA or commit protocol, Gate A/B, timing/JUnit receipts, exact
> test names/counts, generated-byte/source-shape checks, PR closure gates, or
> platform gate lattice.

**Outcome contributed:** Measure and improve production event-to-ready time and
other user-visible bottlenecks while preserving equivalent product behavior,
truthful unavailable metrics, and bounded storage.

## Retired measurement framework

`EditClassReceipt`, boundary receipts, Gate A/B dispositions, JUnit/timing
receipt schemas, exact test-count or slow-test gates, generated-byte/source-shape
checks, and the default/all/no-default/lite/package/platform gate lattice are
retired. Historical receipts may preserve those fields as provenance, but they
are not prerequisites and must not be recreated.

## Historical work areas

- Measure representative production capture/edit/event-to-ready journeys under
  comparable conditions.
- Optimize measured runtime/query/index bottlenecks without semantic drift.
- Keep retention/storage behavior bounded and surface missing measurements
  truthfully.
- Remove superseded paths when the applicable numbered V2 plan permits it.

## Product outcome contributed

The enduring outcome is measured event-to-ready improvement with equivalent
behavior. Current representative journeys, equivalence criteria, storage
bounds, and acceptance are defined by the applicable numbered V2 plans.

## Historical measurement notes

Historical experiments isolated optimizations, compared like-for-like samples,
and rejected semantic drift or false claims from unavailable data. Their
thresholds, receipt types, boundary dispositions, platform matrix, and deletion
choreography do not define current closure.
