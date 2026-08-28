# Vendor patches

Patches against the pinned `grafeo` fork that are **not yet on the fork
branch**. Each one is applied by hand to
`github.com/ScriptedAlchemy/grafeo` branch `tracedecay/0.5.42-arena-multichunk`,
then consumed here by bumping the `rev` in the root `Cargo.toml`
`[patch.crates-io]` block.

Apply from the fork checkout root:

```sh
git apply /path/to/tracedecay/docs/vendor-patches/<name>.patch
```

## `grafeo-close-checkpoint-dirty-only.patch`

**Defect.** `GrafeoDB::close()` flushes with `FlushReason::Explicit`
(`crates/grafeo-engine/src/database/mod.rs:2269`). In
`crates/grafeo-engine/src/database/flush.rs:68` that reason means
*serialize every section unconditionally*:

```rust
if reason == FlushReason::Explicit || section.is_dirty() {
    targets.push((section.section_type(), section.serialize()?));
}
```

So closing a database re-serializes the whole store even when nothing is
dirty and every generation is already durably published. For a large graph
this faults the entire working set back in and writes gigabytes that are
already on disk — the dominant cost of daemon shutdown's `store_close`
phase, measured in the tiered-storage investigation at close-time
serialization peaking around 14x the on-disk size.

`FlushReason::Checkpoint` is the correct reason and is already documented as
*"Periodic checkpoint (timer-driven) **or database close**"*. It writes only
dirty sections and returns `sections_written: 0` when there is nothing to do.

**Why this is safe.** `close()` already carries the escalation path. If the
dirty-only pass writes zero sections while the WAL still holds records, it
retries with `FlushReason::Explicit` (a genuine full flush), and if *that*
still writes nothing it keeps the sidecar WAL so the next open recovers.
Switching the first attempt to `Checkpoint` only skips work that the dirty
tracking says is unnecessary; the safety valve behind it is unchanged.

The patch also drops the now-wrong `#[allow(dead_code)]` on
`FlushReason::Checkpoint`, which was only there because nothing in the
default feature set constructed the variant.

**TODO (tracedecay-side consumption).** This patch is *not* on the fork
branch yet — this working session was not permitted to push to the fork.
Nothing in tracedecay consumes it. Once it is pushed to
`tracedecay/0.5.42-arena-multichunk`, bump every `grafeo-*` `rev` in the root
`Cargo.toml` `[patch.crates-io]` block (they must move together — a mixed
graph builds two type-incompatible copies of the unpatched crates) and drop
this section. `crates/tracedecay-graph-db/src/runtime.rs:622`
(`GraphDb::close`, span `graph_db.runtime.close`) is the tracedecay-side
caller whose cost this removes; the `daemon.shutdown.store_close` span is the
outer view.
