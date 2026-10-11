# Partial-index predicate does not short-circuit `AND` (Turso 0.8.0, 2829ee1662)

SQLite evaluates a partial index's `WHERE` with jump-based code, so a later
`AND` term never runs once an earlier one is false. Turso computes the whole
predicate as one value (`emit_partial_index_check` calls
`emit_dml_expr_index_value` in `core/translate/insert.rs`), so every term runs.
A term that raises on some input then fails the INSERT.

## Repro

```sql
CREATE TABLE t (metadata_json TEXT);
CREATE INDEX t_lossy ON t (metadata_json)
  WHERE json_valid(metadata_json)
    AND json_type(metadata_json, '$.lossy') = 'true';
INSERT INTO t VALUES ('[not json');
```

- SQLite: the INSERT succeeds; the row is outside the index.
- Turso: `Parse error: malformed JSON`.

Seen through `tracedecay-lcm` `gc::tests::*` under `--config=native`: every raw
message whose `metadata_json` holds a placeholder that is not JSON failed to
insert because of `idx_lcm_raw_lossy_ingest`.

A `CASE WHEN json_valid(...) THEN json_type(...) END` guard fixed the INSERT
but not the UPDATE that rewrites a placeholder row
(`gc::tests::delete_tombstones_only_the_owner_row`), so the guard is not a
complete workaround.

## Workaround

`idx_lcm_raw_lossy_ingest` indexes `WHERE metadata_json LIKE '%"lossy"%'`, a
predicate that cannot raise. The queries that rely on it carry that same term,
as SQLite partial-index substitution needs, and apply the exact
`json_type(... '$.ingest_protection.lossy') = 'true'` test behind a
`json_valid` guard to the indexed rows only (`tracedecay-lcm` `query/status.rs`
and the global-db schema contract test).
