# Every FTS statement flush allocates term tables for a 64 MiB budget (Turso 0.8.0, 2829ee1662)

A native FTS index buffers the documents a statement writes and builds one
immutable Tantivy segment when the statement ends
(`core/index_method/fts/mod.rs` `build_segment`). It creates that segment's
writer with `SegmentWriter::for_segment(DEFAULT_MEMORY_BUDGET_BYTES, ..)`,
64 MiB. Tantivy sizes the writer's term hash tables from the budget
(`compute_initial_table_size`: a third of it, capped at 2^19 entries), so
every flush allocates, zeroes and, at `finalize`, iterates tables of 2^19
entries, whether the statement wrote one document or a thousand.

## Measured

`tracedecay-session-memory`
`fact_store::crud::tests::add_succeeds_past_ten_thousand_eligible_facts`
commits one fact per batch, and each batch inserts one row into the
FTS-indexed `memory_v2_assertion_payloads`:

| statement | per call |
|---|---|
| `INSERT INTO memory_v2_assertion_payloads ...` | 11.3 ms, 106 VM steps |
| the other 19 statements of the batch, together | about 4.3 ms |

`perf` over the same test puts the time in `kernel_init_pages` and page
faults under `ArenaHashMap::with_capacity` and `memset`, and in
`shared_arena_hashmap::Iter::next` walking the empty table. The same cost
lands on every session message insert into `lcm_raw_messages` (11.2 ms per
call in `tracedecay-session-runtime` `session_backlog_read_cost`).

## Patch

`scripts/bazel/patches/turso-core-fts-segment-budget.patch` (applied by the
`turso_core` `crate.annotation` in `MODULE.bazel`) sizes the budget by the
documents being flushed, 64 KiB each, between 1 MiB and the 64 MiB default.
Tantivy grows the tables when a batch needs more, so the segment contents are
unchanged.
