# Native Turso cutover: synthesized design (#3402)

Inputs are the arena candidates (`/fast/tmp/turso-arena/c{1,2,3}`), the cross-judge verdict (`judge.md`), the scope map (`scope-map.md`), and Zack's rulings of 2026-10-10. The rulings are: delete all TraceDecay databases and start fresh on Turso, never keep backups for any reason, make native FTS ranking a permitted change, and use logical deletion without page scrubbing.

## Shape

- **In-place driver swap. Base is candidate 3.** The writer actor, admission, `FairQueue`, watermark, checkpoint scheduling, ledger, the payload executors, exact SQL and the work/workflow/remote/graph-publication stores stay where they are. Only the lines that call the driver change. The prototype's duplicate native writer, ledger, Diagnostics executor, MVCC opt-in and `engine/native.rs` separate-connection facade are deleted, not finished.
- **Driver crate `tracedecay-sql`.** It is `crates/tracedecay-turso-runtime` renamed, which keeps the history of `policy.rs` and `pinned_io.rs`, and it is the only database driver for owned stores. Its API mirrors the rusqlite subset the runtime already uses: `Connection`, `Transaction`, `Savepoint`, `Statement`, `Row(s)`, `params!`, `params_from_iter`, `OptionalExtension`, `ToSql`/`FromSql` for the types in use, and `Error` with `ErrorKind`. It adds a small control surface: guard scopes, checkpoint, page stats, interrupt and file identity.
- **Engine chosen at compile time.** A Bazel `string_flag` `//crates/tracedecay-sql:engine` has the values `sqlite` (default until the flip) and `native`. A `select` maps it to `--cfg td_engine="native"` on `tracedecay-sql` only. `.bazelrc` gets `build:native --//crates/tracedecay-sql:engine=native`.
  - Under `sqlite` the crate mostly re-exports rusqlite, with extension methods for the control surface. A half-migrated tree therefore compiles, and every unit lands green on the default engine.
  - Under `native` the same names are implemented over `turso_core`, and the `--config=native` build acts as the API gate.
  - Every binary contains exactly one owned-store engine.
- **End-state cleanup (judge fix).** At the flip, the extension traits become inherent methods and the engine flag and cfg are deleted. Nothing with a single implementation remains.

## Driver contract (native arm)

- Synchronous and thread-owned. A connection is created on, and used by, its owner thread: the writer actor or one reader worker. `drive(stmt)` handles each step result as follows:
  - `Row` and `Done` return.
  - `IO` and `Yield` call `io.step()`.
  - `Sleep{d}` really sleeps, for `min(d, deadline-now)` in 5 ms slices with a guard recheck between slices. `UnixIO::step` is a no-op, so pumping would spin.
  - `Interrupt` maps to the guard's recorded reason.
  - `Busy` maps to `ErrorKind::Busy`.
- **Open path.** `turso_core::Database::do_open` is used, which skips the registry (`database.rs:220-262`). A process-wide `OPEN_IDENTITIES` set (device, inode) refuses a second in-process open, because POSIX fcntl locks do not exclude the same process. Turso's exclusive file lock stops a second process from opening the store. Multiprocess WAL stays off.
- **WAL mode, one writer per shard, no MVCC.** `TransactionBehavior` has no `Concurrent` variant. Automatic checkpoints are disabled with `wal_auto_actions_disable`, so the actor's checkpoint lane is the only scheduler. Checkpoint modes map 1:1.
- **Commit.** COMMIT runs without the authority or cancellation guard. Any error after COMMIT has been stepped is reported as commit-unknown, which the existing `settle_prepared` handles by receipt lookup. Rollback ignores authority and cancellation.
- **Guard parity with the SQLite authorizer.**
  1. A pure AST verdict `authorize(cmd, Capability)` over turso's parser. The capabilities mirror the existing authorizers:
     - `Reader`: query-only.
     - `Writer`: DML, plus CREATE TABLE and CREATE INDEX for the ledger bootstrap. CREATE TRIGGER and CREATE VIEW are denied, which fixes a live bug in the prototype.
     - `ExactSql{savepoints}`: temp TABLE and INDEX are allowed. Temp TRIGGER and VIEW are denied, as are ATTACH and DETACH, pragmas outside the allowlist, and transaction control outside its lease.
     - `SchemaInstall` / `Maintenance`: durable DDL, including the canonical triggers and views.
  2. `GuardedDialect` denies `load_extension` and functions outside the allowlist. It also runs for trigger bodies and on automatic reprepare. `register_catalog` registers only an allowlist: `json_each`, `json_tree`, and the allowlisted `pragma_*`. A test asserts that `sqlite_dbpage`, `btree_dump` and `turso_types` cannot be resolved.
  3. Readers set `query_only`. A progress handler checks deadline, cancellation, shutdown, authority, the VM-op budget and file identity every 1000 ops.
  4. The statement cache is keyed by `(Capability, sql)` and cleared after DDL. Authority is never baked into a compiled program.
  5. The final-shape inventory pins the exact text of every trigger and view.
- **Proof of parity.** `testdata/policy.tsv` records the SQLite authorizer's verdict for each capability, generated while SQLite still runs. `testdata/census.tsv` lists every statement the full suite executes, captured with `trace_v2`. The native gate has zero tolerance: every census statement must prepare and be allowed, and no SQLite denial may become an allow.
- **IO confinement.** `ConfinedIo` replaces the pass-through in `PinnedIo` (`pinned_io.rs:115-131`). It admits only the pinned main file and `<main>-wal`, opened with O_NOFOLLOW, and every other open or remove fails. Windows keeps parity with SQLite: path open plus (volume serial, file index) identity checks before open, after open and on each connect. There is no Windows descriptor adoption.

## Schema

- **ROWID conversion, first and on SQLite.** The 11 WITHOUT ROWID tables (4 ledger, 4 external_source, 2 git_correlation, `observation_projection/schema.rs:300`) drop the layout token and get explicit `NOT NULL` on every primary-key column. All UNIQUE constraints and indexes are kept, as are the explicit payload and occurrence rowids. NULL, duplicate, UPSERT, DELETE and prune differential tests are added for each table. The same source runs on native later.
- **Schema lint.** A test installs the registered stage sequence in memory. It fails on WITHOUT ROWID, a nullable PK column, `secure_delete`, or `fts5` outside the four allowlisted FTS5 tables. The allowlist empties at the flip.
- **FTS.** The native `USING fts` indexes (`native_search.rs`) become the only search DDL after the flip. The FTS5 DDL, its sync triggers and the `BackendKind` query arms are deleted.
- **Final shape.** The checked-in golden inventory is generated from a fresh native install by `bazel run` of a regen target, with a stale-golden test. The native refusal at `final_shape.rs:271` is deleted.
- **Format stamp.** Every native store is stamped `PRAGMA application_id = 0x54444E31` ("TDN1"). The driver reads the 100-byte header with `std::fs` before turso opens a file. A file that is not `SQLite format 3\0` with TDN1 is refused with `ForeignFormat`.

## On-disk layout and the fresh start (Zack ruling)

- **`StoreFile`.** A table in `tracedecay-store` is the single owner of store file names: native `<stem>.tdb` with sidecar `<stem>.tdb-wal`, and legacy `<stem>.db` with `-wal`, `-shm` and `-journal`. The duplicate constants are deleted. `Database::open` accepts only the `OwnedStorePath` newtype, which only `StoreFile` constructs.
- **Fresh start.** When the native runtime first opens a profile, it deletes the legacy owned SQLite families (exactly the `StoreFile` legacy names under the canonical roots, nothing else). It does this under the profile lifecycle authority, after daemon ownership is established. No conversion or backup takes place. The memory artifacts that the existing complete-wipe CLI misses are also deleted.
- **Foreign SQLite.** The foreign rusqlite adapter (`tracedecay-sessions/runtime/{shared.rs,hosts/**}`) refuses any path under a TraceDecay data root.

## No backups, ever (Zack ruling)

The following are deleted, not ported: the backup API usage, the `profile_storage` backup and export CLI, `sqlite_read_snapshot.rs`, `sqlite_snapshot_materialize.rs`, `sqlite_snapshot_connection.rs`, `VACUUM INTO` export, export-and-swap compaction, and the incremental-vacuum lane, which turso cannot run anyway. The storage report and orphan scan use file stats plus page stats from the owning daemon. Compaction reports typed `Unavailable(EngineReuseOnly)`.

## Telemetry

Page size, page count, freelist count and WAL state come from the engine. SQLite VM and `db_status` counters and `telemetry/sqlite_vm.rs` are deleted together with the fields that only they fed. A field that a budget decision reads becomes typed `Unavailable`.

## Verification harness

1. `baseline-outcomes.tsv` records `bazel test //... --keep_going` on the pre-change base, once per target.
2. Conformance suite: `tracedecay-sql` tests run under both configs with identical assertions.
3. The policy corpus and census gate described above.
4. Burn-down: `bazel test --config=native` per crate partition.
5. Golden receipt trace: fixture request streams (the ledger, all payload variants, and the work/workflow/remote stores) produce a receipt trace and a logical dump that are checked in from SQLite and asserted on native.
6. Crash harness: a re-exec'd child runs the registered native writer. CrashIo covers two modes: aborting at the Nth write or sync (kill), and discarding writes since the last fsync (power loss). At least 200 seeded points run. The checks are contiguous receipts, exact replay, commit-unknown resolved through receipt lookup, interrupted checkpoint recovery, and a watermark that never exceeds the durable sequence.
7. Outcome-diff gate at the flip: every test that passed at baseline passes, or appears in `approved-deltas.tsv` under one of these categories: fts-ranking, secure-delete, attach-removed, backup-removed, vacuum-removed, sqlite-introspection-removed, fts-shape.
8. Benchmarks: publication and search with conflicts, retries, queueing and receipt p50/p95/p99, with at least five fresh runs per shape, native against the SQLite baseline.

## Units

| Unit | Scope | Deps | Check |
|---|---|---|---|
| H0 | Baseline outcomes on the base commit | none | `baseline-outcomes.tsv` |
| U1 | Subtract ATTACH end to end | U0 | default build and affected suites green |
| U1b | Subtract backups, export, snapshot copies, the vacuum lane and compaction | U0 | default build and affected suites green; no backup or export surface left |
| U2 | `tracedecay-sql` crate (rename turso-runtime and delete its writer, ledger, Diagnostics and MVCC), engine flag, SQLite arm, native skeleton, conformance suite, codemod plus postcondition script | U0 | conformance green on default; `--config=native` builds `//crates/tracedecay-sql/...` |
| U3 | ROWID conversion of the 11 tables plus the schema lint | U0 | default suites green; differential tests per table |
| U4 | Codemod the data plane onto `tracedecay_sql` | U2 | default green; postcondition clean on its paths |
| U5 | Control plane onto the driver (connection, guard, writer settlement, reader, checkpoint, telemetry, doctor) | U2 | default green; no rusqlite on its paths |
| U6 | Native driver implementation (step loop, guard, capability cache, ConfinedIo, header preflight, open identities, commit classification) | U2 | conformance green under `--config=native` |
| U7 | Owned-store users outside the crate onto the driver | U2 | default green; postcondition limits rusqlite to the allowed paths |
| U8 | Native schema and search admission, golden inventory, FTS doctor and bound | U2, U3 | native install, admission and doctor tests green |
| U9 | `StoreFile` layout, deletion of legacy families on first native boot, foreign-path guard, OwnedElsewhere | U2 | legacy files deleted and fresh `.tdb` created; second process gets Unavailable; foreign adapter rejects owned paths |
| U10 | Storage report and orphan scan via stats; telemetry | U1b, U5, U6 | native maintenance tests green |
| U11 | Policy corpus and census capture (on SQLite) plus native gate | U2, U6 | gate green |
| U12a-d | Native burn-down by crate partition | U3-U9 | `--config=native` partition green and default still green |
| U13 | Crash harness, kill and power-loss modes | U5, U6 | 200+ seeded points pass |
| U14 | Golden receipt trace | U4-U8 | native equals golden |
| U15 | Benchmarks | U12 | report |
| U16 | Flip, delete and rename: native default; delete SQLite arm, flag, FTS5 arms, `BackendKind`, `engine::native`; traits become inherent methods; rename the runtime crate to `tracedecay-storage-runtime`; rusqlite-dependents allowlist test | all | full suite green; outcome-diff gate; rusqlite only in sessions hosts and the query artifact |

The lexical projection artifact in `tracedecay-query` stays on SQLite as a named exception tracked in its own issue.
