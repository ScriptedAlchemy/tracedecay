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
