# Pure Projector Extraction Plan

**Goal:** Extract deterministic session and observation reducers so projection
edits do not compile database/runtime adapters.

## Files and interfaces

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

## Tasks and tests

- [ ] Add architecture failures for DB/runtime imports and nondeterministic
      time/random/filesystem access.
- [ ] Move pure reducers unchanged and pin byte-exact snapshot fixtures.
- [ ] Replace root calls with crate invocations; keep persistence and
      transaction boundaries in adapters.
- [ ] Add replay, reorder rejection, idempotency, redaction, and watermark tests.

Direct tests replay canonical session/observation fixtures into identical rows
and digests. Negative tests reject out-of-order, wrong-owner, malformed,
unredacted, stale-watermark, and unsupported-version input before persistence.

Run crate checks/nextest, root projection suites, observation/session temporal
journeys, and architecture boundaries.

## Migration, rollback, measurement, deletion

No schema migration. Commit session reducer, observation reducer, adapter
wiring, and root cleanup separately. `git revert` restores each boundary.
Measure warm projector-private edits and root projection adapter edits with
rebuilt units. Delete root reducer copies only after deterministic rebuild,
repair, and production persistence callers use the crate.
