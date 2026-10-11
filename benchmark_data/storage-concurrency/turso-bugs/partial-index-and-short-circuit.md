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

## Workaround

`idx_lcm_raw_lossy_ingest` indexes `WHERE metadata_json LIKE '%"lossy"%'`, a
predicate that cannot raise. The queries that rely on it carry that same term,
as SQLite partial-index substitution needs, and apply the exact
`json_type(... '$.ingest_protection.lossy') = 'true'` test behind a
`json_valid` guard to the indexed rows only (`tracedecay-lcm` `query/status.rs`
and the global-db schema contract test).

## The same gap in a `CASE WHEN` condition

`CASE WHEN json_valid(m) AND json_extract(m, '$.a') IS NOT NULL THEN 1 ELSE 0 END`
also raises `malformed JSON` on Turso for `m = '[x'`. A nested `CASE` defers
the extraction and returns 0 on both engines:

```sql
CASE WHEN CASE WHEN json_valid(m) THEN json_extract(m, '$.a') END IS NOT NULL
     THEN 1 ELSE 0 END
```

`tracedecay-lcm` `summary_convergence.rs` (the raw-row update trigger that
reads `$.ingest_protection.sanitization_receipt`) uses that form; before it,
tombstoning a payload placeholder in a row whose metadata is not JSON failed
the payload delete (`gc::tests::delete_tombstones_only_the_owner_row`).
