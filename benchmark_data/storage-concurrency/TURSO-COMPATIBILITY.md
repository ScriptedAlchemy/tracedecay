# Turso compatibility evidence

Reviewed 2026-10-09 against official Turso **v0.8.0**, source commit
`2829ee1662bd01d79f60c2c170af684a2b50af86`. The separate prototype pins the
official `pyturso==0.8.0` native wheel by SHA-256. Its reported engine version is
checked at runtime; the wheel's source commit is not independently attested.

| Boundary | Pinned evidence | TraceDecay implication |
| --- | --- | --- |
| Concurrent local transactions | [Python binding](https://github.com/tursodatabase/turso/blob/2829ee1662bd01d79f60c2c170af684a2b50af86/bindings/python/turso/lib.py), [Rust transactions](https://github.com/tursodatabase/turso/blob/2829ee1662bd01d79f60c2c170af684a2b50af86/bindings/rust/src/transaction.rs) | Explicit `BEGIN CONCURRENT` with `PRAGMA journal_mode='mvcc'`; Python uses `isolation_level=None`. MVCC is not a Python experimental-feature token. |
| Same-file state | [Database registry](https://github.com/tursodatabase/turso/blob/2829ee1662bd01d79f60c2c170af684a2b50af86/core/database.rs) | Connections share database state by OS file identity. Two connections alone do not prove overlapping mutation; the probe must hold both mutated transactions before either commits. |
| Write conflicts | [MVCC implementation](https://github.com/tursodatabase/turso/blob/2829ee1662bd01d79f60c2c170af684a2b50af86/core/mvcc/database/mod.rs) | Conflicts can arise during mutation as well as commit. Roll back the whole transaction and retry from a fresh snapshot. TraceDecay updates the same checkpoint row on every recorded operation. |
| `WITHOUT ROWID` | [Compatibility matrix](https://github.com/tursodatabase/turso/blob/2829ee1662bd01d79f60c2c170af684a2b50af86/COMPAT.md) | Experimental INSERT/SELECT support does not cover UPDATE/DELETE/UPSERT or secondary indexes. The runtime checkpoint, idempotency, outbox, and inbox tables use this layout. |
| Search | [Compatibility matrix](https://github.com/tursodatabase/turso/blob/2829ee1662bd01d79f60c2c170af684a2b50af86/COMPAT.md) | SQLite FTS5 and `snippet()` are absent; the alternative search API does not preserve the existing store contract. |
| Hooks | [Compatibility matrix](https://github.com/tursodatabase/turso/blob/2829ee1662bd01d79f60c2c170af684a2b50af86/COMPAT.md), [C binding](https://github.com/tursodatabase/turso/blob/2829ee1662bd01d79f60c2c170af684a2b50af86/bindings/c/src/lib.rs) | `sqlite3_set_authorizer` explicitly returns `SQLITE_ERROR`. Required authorizer and update hooks prevent a drop-in runtime replacement. WAL hooks are an additional gap, not a current production dependency. |
| JSON and triggers | [Compatibility matrix](https://github.com/tursodatabase/turso/blob/2829ee1662bd01d79f60c2c170af684a2b50af86/COMPAT.md) | JSON extraction and ordinary triggers are supported; these do not resolve the ledger, search, or hook incompatibilities. |

The runtime dependencies are concrete: `src/ledger/schema.rs` defines the table
layouts, `src/ledger/checkpoint.rs` persists the ordered checkpoint, and
`src/connection/mod.rs` plus `src/exact_sql/guard.rs` install authorizer/update
hooks. Paths are relative to `crates/tracedecay-rusqlite-runtime`.

The experiment deliberately uses ordinary ROWID tables for its reduced
checkpoint/receipt protocol. This is a compatibility adaptation within an
isolated experiment, not a migration, production integration, or claim that
TraceDecay's complete schema works on Turso. The experiment's own README and
results describe its executed cases and remaining limitations.
