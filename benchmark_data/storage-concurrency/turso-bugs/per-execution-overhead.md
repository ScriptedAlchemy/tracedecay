# Prepared single-row writes pay per-execution setup and generic key compares

Turso rev `2829ee1662bd01d79f60c2c170af684a2b50af86`.

A bulk load that runs one prepared `INSERT` per row inside a write
transaction pays three costs per execution that SQLite does not:

1. `op_transaction` re-reads page 1 for the schema cookie on every
   execution (`get_schema_cookie`, `core/vdbe/execute.rs:19784`), although
   under the write lock only this connection changes the header and every
   header write already refreshes the pager's cached cookie
   (`Pager::with_header_mut`).
2. `op_open_write` builds the index cursor's `IndexInfo` twice per execution
   (`BTreeCursor::new_index_boxed` and again for the MVCC promotion it drops
   when MVCC is off), and nothing keeps it across executions.
3. Index seeks compare each key field through `read_value`, `ValueRef::cmp`
   and `cmp_with_sort` (`compare_payload_generic`, `core/types.rs:3098`),
   also for an integer against an integer and binary text or a blob against
   its own type, which need no value materialization.

Repro: any table with a composite `UNIQUE` index, loaded by one prepared
single-row `INSERT` per row inside `BEGIN ... COMMIT`.

Fixes carried, in `MODULE.bazel` order:

- `turso_core-write-tx-schema-cookie.patch`: while the pager holds the write
  lock and the cookie cache is set, `CheckSchemaCookie` uses the cached
  value. A savepoint rollback clears the cache, since page 1 may come back
  from the subjournal.
- `turso_core-shared-index-info.patch`: `ProgramState` keeps one
  `Arc<IndexInfo>` per index cursor slot across resets, keyed by the
  `Arc<Index>` it describes, so a reprepared program rebuilds it.
- `turso_core-same-type-key-compare.patch`: `compare_payload_generic`
  compares same-type integer, binary text and blob fields in place.

Measured on TraceDecay's lexical artifact build (265 files), writer thread
instructions:u, tag `perf4` in `native-perf.jsonl` (five interleaved runs;
every run of a binary gave the same count to 0.1 G):

| step | writer instructions:u |
|---|---|
| base (PERF3 patches) | 88.8 G |
| + driver: no second reset at admit (TraceDecay) | 88.2 G |
| + write-tx schema cookie | 87.8 G |
| + shared IndexInfo | 87.2 G |
| + same-type key compare | 86.2 G |

The artifact digest is unchanged by every step.
