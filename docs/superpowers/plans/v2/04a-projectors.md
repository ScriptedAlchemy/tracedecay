# Pure Projector Extraction Plan

> **Archived provenance — not current requirements.** This document records
> historical planning and execution evidence. Current scope and acceptance come
> only from [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate its task checklists, file inventories,
> branch/worktree/SHA or commit protocol, Gate A/B, timing/JUnit receipts, exact
> test names/counts, generated-byte/source-shape checks, PR closure gates, or
> platform gate lattice.
> Historical schema/migration language applies only to evidenced released/live
> persistence; branch-local projector shapes change in place.

**Goal:** Extract deterministic session and observation reducers so projection
edits do not compile database/runtime adapters.

## Historical file and interface inventory

- Source: `src/sessions/claude/canonical_projection.rs` and
  `src/global_db/observation_projection/**`.
- Create `crates/tracedecay-projectors/{Cargo.toml,src/lib.rs}` with focused
  session and observation modules.
- Modify root projection persistence adapters and architecture tests.

Public handoff:

```rust
pub trait Projector<E, S> {
    type Error;
    fn apply(&self, state: &mut S, event: &E) -> Result<(), Self::Error>;
}

pub struct ProjectionBatch<S> {
    pub state: S,
    pub source_watermark: SourceWatermark,
}
```

The crate accepts owned domain events/state and returns deterministic state plus
watermark. It imports no rusqlite, filesystem, daemon, transport, clock,
network, or mutable registry authority.

## Historical task checklist

- [ ] Add architecture failures for DB/runtime imports and nondeterministic
      time/random/filesystem access.
- [ ] Move pure reducers unchanged and pin byte-exact snapshot fixtures.
- [ ] Replace root calls with crate invocations; keep persistence and
      transaction boundaries in adapters.
- [ ] Add replay, reorder rejection, idempotency, redaction, and watermark tests.

## Product outcome contributed

Deterministic session and observation reducers became separable from
database/runtime adapters while replay, identity, redaction, ordering, and
watermark behavior remained equivalent. Current direct behavior and acceptance
live in the applicable numbered V2 plan.

## Historical migration, rollback, measurement, and deletion notes

No schema migration. Commit session reducer, observation reducer, adapter
wiring, and root cleanup separately. `git revert` restores each boundary.
Measure warm projector-private edits and root projection adapter edits with
rebuilt units. Delete root reducer copies only after deterministic rebuild,
repair, and production persistence callers use the crate.
