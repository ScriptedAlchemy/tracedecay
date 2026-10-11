# A rollback that undid no DDL recompiles every prepared statement (Turso 0.8.0)

A connection's prepared programs carry the connection's prepare-context
generation and are recompiled on their next step when it changed. Two
rollback paths bump that generation even when no schema changed:

- `ROLLBACK` calls `rollback_attached_wal_txns`, which counts the temp
  database as attached. Once a connection has created a temp table, every
  `ROLLBACK` bumps the generation, including the one that ends a read-only
  transaction.
- `ROLLBACK TO` restores the savepoint's schema snapshots and bumps the
  generation whether or not any DDL ran after the savepoint.

## Repro

```sql
CREATE TABLE t(a);
CREATE TEMP TABLE scratch(x);
-- prepare and run: INSERT INTO t VALUES (1)
BEGIN; SELECT count(*) FROM t; ROLLBACK;
-- run the prepared INSERT again: its Reprepare counter is now 1
BEGIN; SAVEPOINT s; INSERT INTO t VALUES (2); ROLLBACK TO s; RELEASE s; COMMIT;
-- run it again: Reprepare is 2
```

## Measured

`tracedecay-session-runtime` `session_drain_read_cost`: the session writer
keeps exact-SQL temp tables, so every rollback on it recompiled each cached
ledger, external-source and projection statement of the next request: a
probe counted each of them reprepared once per request. With only the
driver's own cache fix the test took 254 s natively, and `Statement::reprepare`
was in 15 of the 87 busy-thread stack samples.

## Patch

`scripts/bazel/patches/turso-core-rollback-keeps-prepared-statements.patch`
bumps the generation in `rollback_attached_wal_txns` only when a staged
schema was discarded, and in `ROLLBACK TO` only when a schema differs from the
frame's snapshot. Schemas are copy-on-write and the frame holds a reference,
so an untouched schema is still the snapshot itself (`Arc::ptr_eq`). Temp DDL
already bumps the generation when it runs. `tracedecay-sql`
`native::tests::driver::programs_survive_unchanged_schema_and_rollbacks`
asserts both that rollbacks keep programs and that a rollback which undid DDL
still leaves no stale program.
