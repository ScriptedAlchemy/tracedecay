# An at-rest form for the graph: measurement and verdict

Bench-lane investigation. Nothing here is wired into production; the two
harnesses are `#[ignore]`d and the one new crate method is gated on
`test-helpers` so it cannot compile into a shipped binary.

## The question

Activating a sealed generation replays every node and edge into grafeo's in-RAM
`LpgStore` MVCC arenas. Publishing one serializes that whole store back out.
Both costs scale with the size of the generation rather than with what changed,
because `LpgStore` has no byte-addressable at-rest representation.

grafeo already ships the thing that would fix this: `CompactStore`, a columnar
read-optimised store whose deserialize is a pointer re-base over `Bytes`, and
`GrafeoDB::compact()`, which freezes the live store into a `CompactStore` base
plus a mutable overlay. Nothing in TraceDecay calls it.

This investigation measured what calling it would buy.

> **Status:** the schema change this document asks for has since landed, and
> the numbers were re-measured on the real schema. Phase 1 is done; phase 2 is
> blocked on two things this investigation did not reach. See
> [Follow-up: the schema change, measured](#follow-up-the-schema-change-measured)
> at the end — its table supersedes the `compact` rows below.
>
> **Superseded for phase 2:** the per-generation sealed-store lane
> (`src/sealed_store.rs`) shipped the "different design" the follow-up asked
> for — at seal time each verified generation's rows stream into their own
> single-generation database, which is compacted, digest-proven after reopen,
> and served read-only. That dissolves blocker 2 (nothing ever writes to a
> sealed artifact), the pinned fork's compact-store property hash index
> dissolves blocker 1 (sealed reopens register `INDEXED_PROPERTIES` exactly
> like staging opens), and vector search deliberately never routes to sealed
> stores. Bytes round-trip the pinned dictionary codec losslessly now, but
> `COMPACT_ROUND_TRIPS_BYTES` stays **off**: a real 431-file code generation
> (~45k entities carrying serialized-record Bytes payloads) fails its
> post-reopen recovered-digest proof in compact form with "relation scalar
> endpoints do not match native topology" and stays permanently unseated
> behind activation retries, while the same corpus seals and serves in replay
> form; the toy-scale sealed-store contract compacts and reads exactly, so
> the defect only shows at generation scale. This document remains the
> measurement record for the whole-database compaction that was measured and
> rejected.

## Verdict

**`compact()` cannot be applied to TraceDecay's graph as it is currently
shaped.** Not "does not pay off" — it returns an error.

The blocker is TraceDecay's own schema, not grafeo. And under a schema that
does not trip it, the columnar form is a large, real win: **10.9x faster
activation at 1M rows, 2.4x lower resident memory, 185x faster adjacency.**

So the at-rest work is worth doing, but the first ticket is a schema change,
not a storage change.

## Numbers

All figures from a single box, `--test-threads=1`, one scenario per process
(`VmHWM` is process-wide and monotonic, so two scenarios in one process would
report each other's peaks). Harnesses:
`crates/tracedecay-graph-db/tests/at_rest_snapshot.rs` and
`crates/tracedecay-graph-db/tests/at_rest_grafeo_probe.rs`.

### TraceDecay's real schema — `at_rest_snapshot.rs`

Entities plus relations through the public `apply_unverified` path, staged in
production-sized 65,536-mutation pages.

| rows | mode | open wall | VmRSS after open | publish peak | on-disk | point reads | traversal |
|---|---|---|---|---|---|---|---|
| 20k + 5k | replay | 0.539s | 140.1 MiB | 156.0 MiB (15.0x disk) | 10.4 MiB | 64/64 in 3.89ms | 9 visits, 0.37ms |
| 20k + 5k | compact | 0.308s | 290.0 MiB | 341.5 MiB | 40.4 MiB | **0/64** | **error** |
| 40k + 10k | compact | — | — | — | — | — | **`compact()` fails** |
| 500k + 125k | replay | 12.065s | 2589.6 MiB | **3804.1 MiB (14.4x disk)** | 264.9 MiB | 64/64 in 1.69ms | 9 visits, 7.44ms |
| 1M + 250k | replay | 21.265s | 6067.6 MiB | **7581.7 MiB (14.3x disk)** | 531.4 MiB | 64/64 in 1.15ms | 9 visits, 4.00ms |

Two things to read off this table.

The **14.3–14.4x publish peak reproduces exactly** as reported, which is the
harness validating itself against a known figure before it is trusted on
anything new.

And compaction **fails on TraceDecay's graph**. At 20k entities it "succeeds"
and produces a store that answers nothing — zero of 64 point reads, traversal
errors with `traversal start entity does not exist` — while being *larger* on
disk (40.4 MiB vs 10.4 MiB) and costing *more* RAM (290 MiB vs 140 MiB) than
the thing it was supposed to improve. At 40k it stops pretending:

```
compact result : FAILED -- grafeo compact failed: GRAFEO-X001: Internal error:
                 table count 50002 exceeds compact ID limit of 32767 (node tables)
```

50,002 = 40,000 entities + 10,000 relations + 2 fixed labels. Exactly one
columnar node table per entity and per relation. The real repo graph
(~430k chunks) would ask for well over 430,000.

### Schema-neutral — `at_rest_grafeo_probe.rs`

The same close/reopen cycle straight against grafeo, with the same node and
edge counts spread over **4 shared labels** instead of one label per entity.
This prices the schema change: it is what TraceDecay would measure if entity
identity were an indexed property rather than a label.

| rows | mode | open wall | VmRSS after open | publish peak | on-disk | adjacency walk |
|---|---|---|---|---|---|---|
| 50k | replay | 0.144s | 69.2 MiB | 78.6 MiB | 4.4 MiB | 0.013ms |
| 50k | compact | **0.009s** | 31.4 MiB | 45.5 MiB | 2.7 MiB | 0.006ms |
| 500k | replay | 2.210s | 790.1 MiB | 890.6 MiB | 43.8 MiB | 3.194ms |
| 500k | compact | **0.164s** | 315.0 MiB | 572.5 MiB | 27.2 MiB | 0.014ms |
| 1M | replay | 3.703s | 1417.3 MiB | 1763.9 MiB | 87.7 MiB | 2.399ms |
| 1M | compact | **0.339s** | 592.2 MiB | 1157.5 MiB | 54.5 MiB | 0.013ms |

Compact vs replay, same scale:

| scale | open | VmRSS | publish peak | on-disk | adjacency |
|---|---|---|---|---|---|
| 500k | **13.5x faster** | 2.51x lower | 1.56x lower | 1.61x smaller | 228x faster |
| 1M | **10.9x faster** | 2.39x lower | 1.52x lower | 1.61x smaller | 185x faster |

Point reads and the adjacency walk both return correct results in every
compact run here — 64/64 hits, 8 hops — so this is a real win and not a store
that opened quickly by not loading anything.

Note what compaction does **not** fix: the publish peak improves by ~1.5x, but
the transient multiple over file size remains. `Section::serialize()` returns
`Vec<u8>`, so the entire section is still materialised in the heap before it
reaches the file. Compaction shrinks that buffer; it does not stream it.

## Sealed generations are immutable — confirmed

This was the question that decides how big the production design has to be, and
the answer collapses it.

**A sealed generation's node/edge data is never mutated.** Enforced at four
independent points:

- `src/generation_runtime.rs:337-340` — once `generation_dependency_digest` is
  set (the seal marker written by `finalize_staged_generation`,
  `generation_runtime.rs:412-492`), any further stage-page write returns
  `GraphDbError::Conflict` unless it is a byte-identical replay.
- `src/registry/staging.rs:252-254` — the semantic-vector path refuses any
  batch once the stage leaves `SemanticVectorStageState::Pending`.
- `src/generation.rs:651-665` — `physical_namespace()` is
  `sha256(namespace, projection, generation_id)`, so a different generation id
  resolves to a different physical namespace. Cross-generation overwrite is
  structurally impossible, not merely forbidden.
- `src/registry/publication.rs:764-813` — publication is a CAS that installs a
  *new* `GraphVerifiedHeadV1`. Head history is append-only.

The only post-seal writes are exact idempotent replays of identical content and
whole-generation deletion during retirement
(`generation_runtime.rs:743-819`) — never a partial patch of live data.
Registry *metadata* about a generation (retiring/collected/quarantined
bookkeeping) does change; node and edge rows do not.

**Consequence: there is no delta or replay-tail problem.** The design is
`seal => compact => serve reads from the columnar base`, with the RAM
`LpgStore` needed only during staging. No overlay merge policy, no
recompaction schedule, no tombstone reconciliation for sealed generations. That
is a much smaller system than the one this investigation set out to scope.

It also means **the schema change needs no migration**. Generations are
content-addressed and immutable; new generations adopt the new index and old
ones age out through the retirement path that already exists.

## Gaps: what CompactStore cannot answer

TraceDecay's entire read surface against grafeo is seven methods, all reached
through `GrafeoDB::graph_store() -> Arc<dyn GraphStore>`:

| method | call sites | CompactStore |
|---|---|---|
| `nodes_by_label` | 7 | implemented, but see gap 2 |
| `get_node` | 5 | implemented |
| `nodes_by_label_count` | 3 | implemented |
| `get_edge` | 3 | implemented |
| `edges_from` | 2 | implemented (CSR) |
| `node_count` | 1 | implemented |
| `has_vector_index` | 1 | **gap 3** |

Because everything already flows through `Arc<dyn GraphStore>` and
`impl GraphStore for CompactStore` exists
(`grafeo-core/src/graph/compact/graph_store_impl.rs:21`), a compacted base is
type-compatible with every TraceDecay read path as written. The gaps are
semantic, not structural.

1. **Label cardinality — blocking.** `CompactStore` allocates one node table
   per distinct label key and addresses tables with a `u16`: hard cap 32,767
   (`grafeo-core/src/graph/compact/builder.rs:725`). TraceDecay mints a unique
   key label per entity (`schema.rs:92`) and per relation (`schema.rs:99`).
   Measured failure at 50,002 tables.

2. **Multi-label collapse — blocking.** A node with several labels is filed
   under a *composite* key, the sorted labels joined with `|`
   (`builder.rs:1129-1136`). TraceDecay nodes carry both a type label
   (`ENTITY_LABEL`) and a key label, so they land in a table named
   `"tde_k_<hash>|tracedecay_entity"`. `nodes_by_label("tde_k_<hash>")` looks
   up that exact string in `label_to_table_id`, misses, and returns empty —
   which is why every point read returned `None` and traversal reported
   `traversal start entity does not exist`. `get_node` compounds it: it
   restores the composite string as a *single* label
   (`graph_store_impl.rs:31`), so the original label set is not recoverable.

3. **`has_vector_index` — non-blocking, silent.** `CompactStore` does not
   override it, so it inherits the trait default `false`
   (`grafeo-core/src/graph/traits.rs:443`). The three call sites in
   `src/runtime.rs` (578, 602, 802) would silently take the non-HNSW plan
   rather than error. Vector data lives in its own section, so this is a
   planning regression, not data loss — but it is silent, which is worse.

4. **No MVCC — non-blocking given immutability.** `get_node_versioned` and
   friends ignore epoch and transaction
   (`graph_store_impl.rs:58-82`), `current_epoch()` is hardcoded to 1, and
   history queries return empty. Correct for a sealed generation; means the
   compact base can never serve the staging path, which keeps writing to
   `LpgStore`.

5. **Publish peak is reduced, not removed.** `Section::serialize()` returns an
   owned `Vec<u8>`. A streaming section writer is the fix; it is a fork change
   and was not needed to answer this investigation's question.

6. **Reopen is not zero-copy today.** The zero-copy path exists —
   `CompactStoreSection::deserialize_from_bytes(Bytes::from_owner(mmap))`
   (`compact/section.rs:88`), used by `CompactStoreTiered::open_mmap`
   (`grafeo-engine/src/database/compact_tiered.rs:170`). But the container open
   path, `GrafeoDB::extract_compact_base`
   (`grafeo-engine/src/database/mod.rs:1511`), calls
   `Section::deserialize(&data)` over a heap `Vec<u8>` from
   `fm.read_section_data`. So the 10.9x measured above is what the *heap-copy*
   path already delivers; the mmap path is additional headroom that was not
   measured here.

**No fork change was required.** The compact/persist/reopen round trip is
already complete in grafeo at the pinned rev: `compact()` builds the layered
store, `build_sections()` writes `SectionType::CompactStore` plus the overlay
(`mod.rs:2645`), and `extract_compact_base` + `wire_layered_after_load`
reconstruct it on open (`mod.rs:1511`, `mod.rs:1448`). This investigation
therefore commits no `.patch` — there is nothing to patch. Gaps 5 and 6 are the
only candidates for a future fork change, and both are optimisations on top of
a working path.

## Projection to the real repo graph

Stated as ratios, because the measured harness graph is not the repo graph.

Activation of the ~430k-chunk graph currently costs **619s**. The measured
replay-to-compact open ratio under a sane label count is **10.9x at 1M rows and
13.5x at 500k**. If activation is dominated by grafeo's open — which the
harness supports but does not prove for the production path, since production
activation also runs digest verification — then compaction lands it at roughly
**45–60s**.

That assumption is the one number in this document that is not directly
measured, and it should be confirmed with a profile of the real activation path
before anyone commits to it.

Publish peak at 1M rows measures 7581.7 MiB against a 531.4 MiB store (14.3x).
Compaction reduces the absolute peak by ~1.52x, so the 5.2GB transient becomes
roughly **3.4GB** — better, but still a multiple. Removing the multiple needs
the streaming writer from gap 5.

## Production wiring plan

Ordered by dependency. Phase 1 is the bulk of the work and everything else is
blocked on it.

**Phase 1 — replace the per-entity label index with a property index.**
The prerequisite. Store the stable entity/relation key as a node *property* and
resolve through `GraphStore::find_nodes_by_property`, which `CompactStore`
implements with zone-map pruning (`graph_store_impl.rs:272`). This removes both
blocking gaps at once: label cardinality drops to the fixed schema set, and
nodes stop needing a second label.

The lookup is already funnelled through two functions, so the change is
narrower than the entity count suggests:
- `src/schema.rs:92,99` — `entity_key_label` / `relation_key_label` become
  property writes.
- `src/state.rs:207` — `load_indexed_entity_node`, and the `unique_labeled_node`
  helper it calls.
- `src/traversal.rs:805` — `optional_node_for_entity`.
- `src/mutation.rs` — the write side that mints the labels.

No migration: generations are immutable and content-addressed, so new
generations adopt the property index and old ones retire normally. Needs a
generation format marker so a mixed store is never ambiguous.

**Phase 2 — seal implies compact.** Call `compact()` at the end of
`finalize_staged_generation` (`src/generation_runtime.rs:412-492`), before the
close/reopen verification. Small: the crate already holds
`RwLock<Option<GrafeoDB>>` (`src/runtime.rs:59`), so a write guard yields the
`&mut GrafeoDB` that `compact()` needs — exactly what
`compact_snapshot_for_bench` does today. Everything downstream (section write,
section reload, layered rewiring) is already implemented in grafeo.

**Phase 3 — restore vector-index planning.** Either override
`has_vector_index` on the layered store so it consults the overlay and the
vector section, or hoist the check in `src/runtime.rs` off `graph_store()`.
Small, but do not skip it: gap 3 fails silently.

**Phase 4 — optional fork work.** Streaming section serialize (gap 5) and
mmap-backed container open (gap 6). Both are pure wins on top of a working
path, and neither is needed for the activation number.

## Reproducing

```text
# TraceDecay's real schema
TRACEDECAY_ATREST_MODE=replay TRACEDECAY_ATREST_ROWS=500000 \
  cargo test -p tracedecay-graph-db --features test-helpers,graph-disk-tier \
  --test at_rest_snapshot -- --ignored --nocapture --exact at_rest_reopen_probe

# schema-neutral
TRACEDECAY_ATREST_MODE=compact TRACEDECAY_ATREST_ROWS=1000000 \
  cargo test -p tracedecay-graph-db --features test-helpers,graph-disk-tier \
  --test at_rest_grafeo_probe -- --ignored --nocapture --exact grafeo_at_rest_probe
```

One scenario per process. `TRACEDECAY_ATREST_MODE` is `replay` or `compact`;
`TRACEDECAY_ATREST_ROWS` sets the scale.

## Follow-up: the schema change, measured

Phase 1 landed. The synthetic key labels are gone — entity, relation, relation
edge, publication, and projection identity all resolve through indexed
properties — and TraceDecay's label reads now flatten the composite key a
`CompactStore` files a multi-label node under. `compact()` runs on the real
schema at every scale, and the reopened columnar store answers every point read
and walks adjacency.

Two things the original investigation did not reach turned up in the process.

### The label change was not the whole schema blocker

The plan said removing the key label would leave nodes "not needing a second
label". It does not. An entity also carries a record label, an owner label, and
two labels per domain label, so it is still multi-label, and gap 2 still
applies: `get_node` on a compacted base returns the fused composite as the
node's only label, `nodes_by_label` answers only for that exact composite, and
`ProjectionSpec` matches labels by exact name. Point reads survive the property
index on their own, but the record-label filters, the projection scans, the two
GQL projection queries, and the traversal spec all had to learn to read through
the composite before a compacted generation answered anything.

### Numbers

Same box, `--test-threads=1`, one scenario per process, and — unlike the tables
above — `--profile perf` throughout, so replay is re-measured here rather than
carried over.

| rows | mode | open wall | VmRSS after open | publish peak | on-disk | point reads | traversal |
|---|---|---|---|---|---|---|---|
| 500k + 125k | replay | 7.552s | 2048.2 MiB | 2816.5 MiB | 166.1 MiB | 64/64 in 0.64ms | 9 visits, 0.24ms |
| 500k + 125k | compact | **0.431s** | 1217.8 MiB | 1744.3 MiB | 95.4 MiB | 64/64 in **156.4ms** | 9 visits, 2.09ms |
| 1M + 250k | replay | 16.584s | 4684.2 MiB | 5614.5 MiB | 332.8 MiB | 64/64 in 0.78ms | 9 visits, 0.23ms |
| 1M + 250k | compact | **0.903s** | 2410.3 MiB | 3365.4 MiB | 192.3 MiB | 64/64 in **328.9ms** | 9 visits, 3.92ms |

Compact against replay, same scale:

| scale | open | VmRSS | publish peak | on-disk | point reads | traversal |
|---|---|---|---|---|---|---|
| 500k | **17.5x faster** | 1.68x lower | 1.61x lower | 1.74x smaller | **243x slower** | 8.7x slower |
| 1M | **18.4x faster** | 1.94x lower | 1.67x lower | 1.73x smaller | **421x slower** | 17x slower |

The activation win is real and larger than the schema-neutral probe predicted.
The point-read cost is new, and it is the number that decides phase 2.

### Blocker 1 — the columnar base has no point-read index

`CompactStore::find_nodes_by_property` walks every node table, prunes on a zone
map, and scans the column (`compact/graph_store_impl.rs:272`). There is no hash
index, so a point read is a scan whose cost grows with the store: 2.4ms per
read at 500k, 5.1ms at 1M, against 10µs on the live store's property index.

This did not show up in the original schema-neutral probe because that probe
timed adjacency, not identity lookups, and because under the old schema
identity resolved through a label index that `CompactStore` *does* answer in
O(1). Moving identity onto a property is what made the columnar form's missing
property index visible.

Trading 18x on activation for 400x on point reads is not obviously a win for a
graph whose interactive queries are dominated by identity lookups. Either the
base needs a real property index — a fork change — or reads have to be routed
by kind, which is a much larger design than "serve reads from the compact base".

### Blocker 2 — writing to a compacted store loses base rows

Deleting a node created in the overlay after `compact()` also drops an
unrelated node from the columnar base, and the loss survives the next open.
`tests/at_rest_compact_mutation_probe.rs` reproduces it in nine lines of pure
grafeo, with no TraceDecay schema involved:

```text
life1 post-compact:  node_count=3  Alpha=1 Beta=1 Gamma=1
life2 after create:  node_count=4  Alpha=1 Beta=1 Gamma=1 Delta=1
life2 after delete:  node_count=3  Alpha=0 Beta=1 Gamma=1 Delta=0   <-- Alpha
life3 open:          node_count=2  Alpha=0 Beta=1 Gamma=1
```

`LayeredStore` masks base rows by `NodeId`, and the overlay allocates ids that
collide with base ids, so an overlay delete masks a base node that shares the
id. This is data loss, and the generation lifecycle triggers it directly:
retirement deletes whole generations after a seal, and the recovery path
creates and clears quarantine markers.

Wiring `compact()` into `finalize_staged_generation` was implemented and then
reverted for this reason. It first surfaced as four generation-runtime
contract failures — a sealed generation reopened twice came back with its
format marker gone — which is the same defect reached through TraceDecay's own
lifecycle rather than the minimal probe.

### The vector index is durable now — fork branch ready, pin not moved

The claim above that the HNSW index "was never durable across a reopen
anyway" was true, and it turned out to be a wiring gap rather than a
missing format.

**Where durability was lost.** grafeo has always *written* the index.
`build_sections` emits a `SectionType::VectorStore` carrying each index's
full HNSW topology — entry point, max level, per-node per-level neighbour
lists — and `CatalogSection::collect_indexes` records every index's
label, property, dimensions, metric, M and ef_construction beside it. The
restore side dropped both:

- `CatalogSection::deserialize` handed the loader its `property_indexes`
  and nothing else. The `vector_indexes` it had just decoded were
  discarded (`catalog_section.rs`).
- `load_from_sections` guarded the topology restore on
  `store.vector_index_entries()` being non-empty
  (`database/mod.rs`, the `SectionType::VectorStore` block). A cold open
  builds its `LpgStore` from nothing, so that map is empty at exactly the
  moment the guard runs. The section bytes were read off disk and
  dropped on the floor.

So the index was durable on disk and absent in memory, and every reopen
re-indexed the whole corpus.

**Fork, not sidecar.** The section format round-trips HNSW faithfully
already: topology is all it needs to carry, because the vectors live in
LPG node properties (persisted anyway) and are read through a
`VectorAccessor` at query time. Nothing was missing from the format, only
the restore wiring — a tracedecay-side sidecar would have duplicated a
working serializer to re-derive an artifact grafeo already had on disk,
and left every other consumer broken.

**Branch:** `tracedecay/0.5.42-vector-index-durable`, stacked on
`tracedecay/0.5.42-close-and-overlay`, at
`f38218653dfc69fda67f9c669371036fde1ed5fe`. Three commits:

1. re-register catalog definitions as empty indexes and let the section
   fill their topology. Two incomplete-restore cases are deliberately
   left absent for rebuild rather than half-restored — a definition with
   no section, and a definition the section carried no topology for —
   because an index that exists and covers nothing reports
   `has_vector_index() == true` and answers every search with nothing.
   Quantized indexes are left to a rebuild too: their codebook lives
   inside the index and the section carries only topology, so restoring
   one would silently downgrade it to full precision. The catalog
   snapshot is v2 to record the mode; v1 files still load.
2. mark index creation, removal, and binding changes as unlogged state.
   `close` may skip its flush when the WAL has not moved, and nothing
   about an index reaches the WAL — so an index built after open never
   reached the file, and the next open rebuilt it, and the next. Harmless
   while indexes were always rebuilt; load-bearing the moment they are
   restored.
3. price it: 5,000 x 64-dim vectors, release profile — **8.7ms** to
   restore the index as part of opening the whole database against
   **359.6ms** to rebuild the index alone on a database already open.
   ~41x, and identical neighbour sets across 24 probes.

The branch also adds an opaque per-index **binding token** that rides the
catalog section, so a caller can stamp the generation digest its index
was built over and compare after a reopen instead of trusting that a
restored topology still describes the rows.

**The pin has moved.** The vector-durability commits were reintegrated
onto the fork's `fix/catalog-version-guard` branch (ported over the
out-of-place checkpoint and close-skip storage rewrite that landed in
the meantime) and the five `grafeo-*` revs in `[patch.crates-io]` now
point at that lineage. The reopen tests in `tests/runtime_contract.rs`
assert `Available` — restored, within the same admission bound that
proves no rebuild ran — and the fork gates the semantics with
`vector_index_reopen`, `vector_index_restore_cost`, and
`torn_vector_checkpoint` (a checkpoint killed at every injection point
over a populated HNSW index must reopen and serve search).

### `has_vector_index` — resolved, and worse than gap 3 described

Gap 3 said `CompactStore` inherits the trait default `false`. The layered store
is more specific than that: `LayeredStore::has_vector_index` delegates to the
*overlay* (`compact/layered.rs:1081`), which `compact()` leaves empty, so it
answers `false` for indexes the store had a moment earlier. That part is at
least truthful — the HNSW index really is gone, and it was never durable across
a reopen anyway.

The silent failure is one layer along. `create_vector_index` builds by scanning
`nodes_by_label(label)`, and TraceDecay's vector label is
`entity_projection_label`, one of several labels on an entity node. On a
compacted base that scan matches nothing, so the build succeeds over zero
vectors, `has_vector_index` then answers `true`, and every vector search
returns empty while reporting an index exists.

The resolution, when phase 2 becomes unblocked, is the routed one: refuse to
build an HNSW index over a store serving from a columnar base, with a typed
error rather than an empty index, and keep a generation that carries vector
indexes in its replay form. Both halves were implemented against the seal-time
compaction and reverted with it; neither is reachable while nothing calls
`compact()` outside the bench lane.

### Why "never write after compact" is not a guarantee TraceDecay can make

The obvious containment for blocker 2 is to promise the store is never mutated
after it is compacted, and enforce it with a typed refusal. That promise cannot
be kept, because the two scopes do not line up: **immutability is per
generation, `compact()` is per database.**

A `GraphDb` holds one `GrafeoDB` (`runtime.rs`, `Inner::database`), and
generations live inside it separated by physical namespace, not by file. Sealing
generation N and compacting freezes the same store that generation N+1 stages
into — and staging the next generation is the daemon's whole job. The recovery
path also writes: `set_projection_quarantine` creates and clears markers on a
store that may already be compacted.

That is not a deduction, it is what the wiring did. With `compact()` called at
the end of `finalize_staged_generation`, five contract tests failed:
`later_generation_page_creates_its_first_native_vector_index`, which writes a
later page after an earlier seal, and four recovered-generation tests where a
sealed generation reopened a second time came back with its format marker gone.

A refusal that actually held would make the graph read-only after its first
seal. Per-generation compaction would need per-generation stores, which is a
different design from the one this document scoped.

### The fork's mmap open needs no wiring here

`extract_compact_base` is called from `GrafeoDB::with_config`'s own open path
(`grafeo-engine/src/database/mod.rs:419,477`), and that is the constructor both
`runtime.rs` and `recovery.rs` already call. Nothing in TraceDecay names the
section, the mmap, or the base. So when the pin moves to the fork branch whose
`extract_compact_base` prefers `fm.mmap_section`, the mmap-backed open arrives
without a line of wiring in this crate — the open numbers in the table above are
the heap-copy path and are a floor, not a ceiling.

### Revised wiring plan

- **Phase 1 — property index. Done.** Also fixed on the way: the property
  indexes were only ever registered on the store that initialized them, so
  every reopened store ran without them. That was invisible while identity
  resolved through the intrinsic label index, and cost 23.7s for 64 point reads
  at 500k once it did not.
- **Phase 2 — seal implies compact. Landed as per-generation stores; compact
  form still gated.** The whole-database `compact()` this plan scoped stays
  rejected; the sealed-store lane (`src/sealed_store.rs`) seals each
  generation into its own single-generation database — exactly the
  "per-generation stores" design blocker 2 demanded — and compacts it when
  the rows carry no Bytes or Vector properties. The fork's compact property
  index settles blocker 1's economics and the pinned dictionary codec
  round-trips Bytes losslessly, but Bytes-carrying generations stay in replay
  form behind `COMPACT_ROUND_TRIPS_BYTES` until a generation-scale compact
  seal passes its post-reopen proof (see the constant's doc for the measured
  scale failure).
- **Phase 3 — vector planning. Not applicable to sealed stores:** vector
  search never routes to a sealed artifact; HNSW indexes are rebuilt against
  the staging database (`apply_sealed_copy_batch` doc).
- **Phase 4 — streaming section writer, mmap container open.** Unchanged, and
  now clearly behind a property index for the columnar base in priority.
