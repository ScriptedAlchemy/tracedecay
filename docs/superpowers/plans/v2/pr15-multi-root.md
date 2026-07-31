# PR15 Authorized Multi-Root Plan

> **Archived provenance — not current requirements.** This document records
> historical planning and execution evidence. Current scope and acceptance come
> only from [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate its task checklists, file inventories,
> branch/worktree/SHA or commit protocol, Gate A/B, timing/JUnit receipts, exact
> test names/counts, generated-byte/source-shape checks, PR closure gates, or
> platform gate lattice.

**Goal:** Add explicit scope-set federation across query, LSP, Git, feedback,
Work, and dashboard without CWD/path fallback or identity aliasing.

## Historical file and interface inventory

- Domain/application scope authorities and generated contracts.
- Query federation and cursors in `crates/tracedecay-query`.
- Daemon project/worktree registry, LSP workspace folders, native Git handoff,
  feedback fanout, and dashboard scope pivots.

Interfaces: `ScopeSetId`, immutable `AuthorizedScopeSet`,
`ScopeSetRevision`, per-root `ScopeOutcome<T>` (`Exact`, `Partial`, `Denied`,
`Unavailable`), frozen `CollectionRevision`, and frozen `StackRevision`.
Every cursor binds scope-set digest, root generations, query, order, and page.

## Historical ordered slices

1. Scope-set identity, canonical digest, and CAS.
2. Centralized authorization/resolution authority.
3. Query federation, stable ordering, and cursors.
4. Worktree inventory and stack registry.
5. LSP workspace-folder routing.
6. Git preview/apply handoff.
7. Feedback fanout, bounds, and circuit breaker.
8. Dashboard scope pivots and per-root truth.
9. Joint CLI/MCP/HTTP/LSP/dashboard acceptance journey.

## Product outcome contributed

The work contributed explicit authorized scope-set federation across query,
LSP, Git, feedback, Work, and dashboard while preserving exact root identity,
bounded partial outcomes, and fail-closed scope. Current direct behavior and
acceptance live in the applicable numbered V2 plan.

## Historical migration, rollback, measurement, and deletion notes

Single-root requests lower to a one-element scope set; no dual authority.
Rollback disables multi-root capability/routes while retaining single-root
behavior and immutable revisions. Measure per-root and aggregate query/LSP/Git/
feedback latency and payload bounds. Delete scattered root resolution and CWD
fallback only after Plan 36 CAS, all surfaces, direct/negative journeys,
contracts, and normal CI pass.
