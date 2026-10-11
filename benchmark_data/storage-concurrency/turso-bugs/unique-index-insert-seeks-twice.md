# A unique index insert seeks the index twice

Turso rev `2829ee1662bd01d79f60c2c170af684a2b50af86`.

For an `INSERT` without REPLACE semantics, `translate_insert` probes every
unique index with `NoConflict` during preflight
(`core/translate/insert.rs:3126`), then writes the index entries in
`emit_commit_phase` (`:1382`) with `IdxInsert` flags of
`IdxInsertFlags::new().nchange(true)`, without `USE_SEEK`. `op_idx_insert`
(`core/vdbe/execute.rs:13156`) therefore runs a second eq-only GE seek from
the root for a key the probe already located, and for a unique index it also
re-reads the cell for `UniqueConstraintCheck`. The probe left the cursor at
the insertion point: no REPLACE deletion ran in between, and nothing else
moves that index cursor before its `IdxInsert`. SQLite passes
`OPFLAG_USESEEKRESULT` in the same position
(`sqlite3CompleteInsertion`, `useSeekResult`).

Repro: any table with a `UNIQUE` index or a non-rowid `PRIMARY KEY`.

```sql
CREATE TABLE t(k TEXT PRIMARY KEY, v BLOB);
INSERT INTO t VALUES ('a', x'00');  -- EXPLAIN: NoConflict on the autoindex,
                                    -- then IdxInsert with flags lacking USE_SEEK
```

Measured on TraceDecay's lexical artifact build (265 files), writer thread:
`seek_internal` under `op_idx_insert` was 7.5% of the writer's
instructions:u, about as much as the `NoConflict` probes themselves (6.7%).

Fix carried in `scripts/bazel/patches/turso_core-insert-use-seek-result.patch`:
`emit_commit_phase` sets `USE_SEEK` on a unique index's `IdxInsert` when the
statement has no REPLACE (statement level, index level or rowid alias).
`op_idx_insert` already falls back to a seek when the key holds a NULL,
because `NoConflict` skips its probe then. The built artifact is
byte-identical (same digest and size) with and without the patch.
