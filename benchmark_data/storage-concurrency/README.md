# SQLite storage concurrency workload

[Recorded measurements](MEASUREMENTS.md) include the machine, build settings,
three repetitions, raw results, and latency limitations.

The runtime retains one persistent SQLite writer actor per shard file and
independent WAL reader connections. Concurrent submissions to one shard are
serialized and batched; distinct shard files can execute write transactions
concurrently. Adding connections that write the same SQLite file does not
provide overlapping write transactions.

The reader pool reserves two general workers for foreground requests. Pending
connection opens now consume their prospective lease capacity before startup
completes. This prevents parallel background pool growth from taking the
foreground reservation. The deterministic unit regression parks four concurrent
opens in a pool with two existing leases and a maximum of eight workers, rejects
a fifth background acquisition, and serves both reserved foreground readers.

Run the correctness suites through the repository's Bazel authority:

```sh
bazel test //crates/tracedecay-rusqlite-runtime:unit_test --test_output=errors
bazel test //crates/tracedecay-rusqlite-runtime:rusqlite_suite --test_output=errors
```

The integration suite's `storage_contention` module covers production diagnostic
publication SQL with retained WAL snapshots, simultaneous submitters, contiguous
durable receipts and checkpoint sequences, idempotency conflicts, rollback and
resubmission, contention with an external SQLite writer, graceful reopen, and
abrupt process termination during an uncommitted publication. Fixtures use
temporary files and never read an operator profile. Existing actor tests prove
that a shard can commit while another shard holds an open write transaction.

Run the explicit measurement workload:

```sh
bazel test //crates/tracedecay-rusqlite-runtime:rusqlite_suite \
  --test_arg=storage_contention::real_file_contention_benchmark \
  --test_arg=--ignored --test_arg=--nocapture --test_output=all \
  --nocache_test_results
```

The default workload compares 1,024 publications at `(shards, submitters)`
`(1, 1)`, `(1, 8)`, and `(4, 8)`, with four concurrent reader threads and eight
diagnostics per publication. Each run creates fresh real files, initializes
schema, and performs one unmeasured durable seed per shard. Request construction
is outside receipt latency; it remains inside total throughput time. Reader
latency includes acquisition, snapshot creation, and the production current
diagnostic generation query. JSON output includes p50/p95/p99 latency, throughput,
failure counts, SQLite busy events, and committed transaction counts. Every
measurement verifies receipt/checkpoint continuity and SQLite integrity.

To select one shape, pass `--test_env=TRACEDECAY_CONTENTION_SHARDS=4` and
`--test_env=TRACEDECAY_CONTENTION_PRODUCERS=8`. Other settings are
`TRACEDECAY_CONTENTION_OPS` (per submitter), `TRACEDECAY_CONTENTION_READERS`, and
`TRACEDECAY_CONTENTION_RECORDS`. Use fresh processes and repeat runs; do not use
a throughput threshold as a correctness assertion.

These measurements cover diagnostic publication and a scalar repository read.
They do not characterize all FTS/search, graph, index, or session workloads. The
runtime uses WAL with SQLite `synchronous=NORMAL`; a process termination test is
not a power-loss durability test. Shard throughput comparisons measure existing
shard parallelism, not a speedup attributable to the admission accounting fix.
Partitioning also reduces per-file history and working sets, so the comparison
does not isolate thread parallelism from all other benefits of sharding.

## Turso compatibility boundary

The [pinned compatibility evidence](TURSO-COMPATIBILITY.md) covers official Turso
v0.8.0 at commit `2829ee1662bd01d79f60c2c170af684a2b50af86`. Its experimental
`WITHOUT ROWID` support does not cover the update/delete/upsert operations this
runtime needs. Required authorizer and update hooks and SQLite FTS5 are absent;
WAL hooks are an additional gap, not a current production dependency. Ordinary
triggers and JSON extraction are supported.

The separate [opt-in native engine prototype](turso-prototype/README.md) pins the
official `pyturso==0.8.0` wheel by SHA-256. It uses real files and ordinary ROWID
tables for a reduced checkpoint/receipt protocol. It tests two independently
mutated transactions open before either commits, snapshot isolation, shared
checkpoint conflict, full rollback/retry, replay, digest conflict, and reopen.
The exact production ledger DDL is a separate negative compatibility probe.
This is an engine experiment, not a production adapter or migration.
Its added differential lane exercises the actual four-ledger schema adapted
to ROWID tables against SQLite's original schema: 34 constraint rejections,
rollback/CAS/retry, representative mutations, replay and fresh-process reopen.
That lane is sequential; its success does not establish overlapping execution
of TraceDecay's full ledger protocol or hook/search parity.

The [caller and data migration plan](MIGRATION-PLAN.md) identifies the exact
rejected DDL, all search/guard dependencies, the complete cutover sequence,
and the smallest search compatibility decision. It includes direct database
opens and foreign SQLite sources, rather than replacing only registered
connection constructors.

Every committed TraceDecay operation updates one checkpoint/commit-sequence row
per shard. That singleton remains a conflict with otherwise independent
mutations. A production engine switch needs full security, search, migration,
and recovery evidence and a justified protocol design. The default durable
store, root workspace dependencies, and single-writer-per-shard architecture
remain unchanged.

## Verification

The final patch against `5fe50528b317bb6fa5f380994be1adc127e6c61d` passed:

- Default production library build without `test-transport`.
- All 336 runtime unit tests.
- All 143 runtime integration tests; two opt-in entry points remain ignored in
  the normal suite. The crash helper executes through the process recovery test.
- Three explicit benchmark process runs, nine measured workload shapes.
- Strict Clippy over the production library, unit tests, and integration suite.
- The affected crate's Bazel rustfmt gate and `git diff --check`.

The new admission regression was also run with the original `leased`-only
condition. It failed at the fifth background acquisition assertion. After
restoring the fix, all unit tests passed. [Baseline failure](baseline-regression.txt),
[final unit summary](final-unit-summary.txt), and
[integration summary](final-integration-summary.txt) preserve that behavior.

Additional successful build/check commands:

```sh
bazel build //crates/tracedecay-rusqlite-runtime:tracedecay-rusqlite-runtime
bazel build --config=clippy \
  //crates/tracedecay-rusqlite-runtime:tracedecay-rusqlite-runtime \
  //crates/tracedecay-rusqlite-runtime:unit_test \
  //crates/tracedecay-rusqlite-runtime:rusqlite_suite
bazel build //crates/tracedecay-rusqlite-runtime:rustfmt_check_2024
```

The full workspace build passed (386 targets). All 148 workspace test targets
were exercised across the initial run and continuations of only unfinished
targets: 135 passed, 12 failed, and one timed out. Workspace rustfmt, strict
Clippy, and generated Bazel build checks passed.
[Workspace verification](workspace-verification.txt)
records the exact scope, failures, controlled baseline evidence, and host-tool
reruns. The full workspace test suite is not green.

[Focused failure triage](TRIAGE.md) records exact isolated controls and
original-condition comparisons without weakening assertions or increasing
budgets.
