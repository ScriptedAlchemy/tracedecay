# Storage contention benchmark

The `storage_contention` bench in `crates/tracedecay-rusqlite-runtime` measures
the publication and search protocols of one owned store engine on fresh real
files. It compares the SQLite engine against the native Turso engine. The
engine is chosen at build time by `//crates/tracedecay-sql:engine`.

Each benchmark process runs four shapes, in order. Every shape opens fresh
store files and installs the diagnostics schema and a full-text index over
diagnostic messages. It then commits one unmeasured seed publication per shard.

| Shape | Shards | Submitters | Publications per submitter | Keys |
|---|---:|---:|---:|---|
| `queue-1x1` | 1 | 1 | 1,024 | unique |
| `queue-1x8` | 1 | 8 | 128 | unique |
| `queue-4x8` | 4 | 8 | 128 | unique |
| `conflict-1x8` | 1 | 8 | 128 | contended |

- **Publication.** Submitters send production diagnostic publications of eight
  records through the shard's persistent writer actor. Submitters on one shard
  queue behind that actor, which batches them into write transactions.
- **Contended keys.** In `conflict-1x8`, every submitter publishes every key.
  Odd submitters send a different command digest. Each key commits once. An
  identical attempt either joins the in-flight commit as a follower or replays
  the commit. An attempt with the other digest settles as an idempotency
  conflict.
- **Retries.** An attempt rejected as a duplicate operation in flight is
  retried after 1 ms. An attempt answered `Saturated` is retried after its
  `retry_after_ms`. Receipt latency runs from the first attempt to the settled
  outcome, so it includes queueing and every retry.
- **Reads.** While submitters run, two readers repeat the current-generation
  diagnostic read and two readers repeat a full-text search for `mismatched`.
  Both go through production reader pools on WAL snapshots. A search that
  returns no hits counts as a failed read.

A shape fails unless every attempt settles, no read fails, and each shard's
durable receipt and checkpoint sequences are contiguous, with exactly one
commit per key. It also fails unless `PRAGMA integrity_check` passes, every
unique key commits, and the contended shape reports idempotency conflicts.

## Running

Record five fresh processes on the current commit:

```sh
scripts/run-storage-contention-benchmark.sh            # SQLite engine
scripts/run-storage-contention-benchmark.sh --native   # native Turso engine
```

The runner refuses a dirty tree outside this directory. Each run calls
`bazel test //crates/tracedecay-rusqlite-runtime:storage_contention
--test_arg=--bench` uncached, with the commit SHA and host name in the test
environment. A passing run appends its JSON lines to
[results.jsonl](results.jsonl); a failed run appends nothing and stops the
runner. The runner then regenerates [MEASUREMENTS.md](MEASUREMENTS.md) from
every recorded row.

Each row records the source SHA, the host, the engine compiled into the
binary, OS, architecture, CPU count, and debug assertions. It also records
the one-minute load average at the start and end of the shape. A row is
`representative` only when both load averages are at or below 200.

Bazel's default `fastbuild` keeps debug assertions on, so the numbers compare
engines on one host and build. They do not estimate release capacity.

## Scope

The full-text index belongs to this benchmark and is not a production search
index. On SQLite it is an FTS5 external-content table with an insert trigger.
On native Turso it is a `USING fts` index. Both are maintained inside the
publishing transaction. Search ranking differs between the engines. The
benchmark checks only that a search returns hits.

Process termination and power-loss durability are covered by tests, not by
this benchmark. The cutover contract is [CUTOVER-DESIGN.md](CUTOVER-DESIGN.md).
