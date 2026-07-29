# PR15 Authorized Multi-Root Plan

**Goal:** Add explicit scope-set federation across query, LSP, Git, feedback,
Work, and dashboard without CWD/path fallback or identity aliasing.

## Files and interfaces

- Domain/application scope authorities and generated contracts.
- Query federation and cursors in `crates/tracedecay-query`.
- Daemon project/worktree registry, LSP workspace folders, native Git handoff,
  feedback fanout, and dashboard scope pivots.

Interfaces: `ScopeSetId`, immutable `AuthorizedScopeSet`,
`ScopeSetRevision`, per-root `ScopeOutcome<T>` (`Exact`, `Partial`, `Denied`,
`Unavailable`), frozen `CollectionRevision`, and frozen `StackRevision`.
Every cursor binds scope-set digest, root generations, query, order, and page.

## Ordered slices

1. Scope-set identity, canonical digest, and CAS.
2. Centralized authorization/resolution authority.
3. Query federation, stable ordering, and cursors.
4. Worktree inventory and stack registry.
5. LSP workspace-folder routing.
6. Git preview/apply handoff.
7. Feedback fanout, bounds, and circuit breaker.
8. Dashboard scope pivots and per-root truth.
9. Joint CLI/MCP/HTTP/LSP/dashboard acceptance journey.

## Tests

Direct: select an authorized mixed root/worktree set, query it, page with a
frozen cursor, inspect per-root Work/Git/feedback state, edit through LSP, and
render dashboard pivots with exact provenance.

Negative: duplicate aliases, moved roots, unauthorized sibling, stale
collection/stack revision, root deletion, one unavailable store, partial
feedback, cursor tampering, CWD/path fallback, and mixed project/profile/store
identity cannot widen scope or become success.

## Migration, rollback, measurement, deletion

Single-root requests lower to a one-element scope set; no dual authority.
Rollback disables multi-root capability/routes while retaining single-root
behavior and immutable revisions. Measure per-root and aggregate query/LSP/Git/
feedback latency and payload bounds. Delete scattered root resolution and CWD
fallback only after Plan 36 CAS, all surfaces, direct/negative journeys,
contracts, and normal CI pass.
