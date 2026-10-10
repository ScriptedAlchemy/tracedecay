# Measurements on 2026-10-09

Baseline: `5fe50528b3` with the storage concurrency patch applied. Three fresh
process repetitions per shape, each using fresh temporary real files. The host
was an Apple M1 Ultra (20 CPU cores, 64 GiB RAM), macOS 27.0 (26A428), with local
storage. Host background load was not controlled. Bazel 9.3.0, Rust 1.99.0,
rusqlite 0.40.2, bundled SQLite 3.53.2. Bazel `fastbuild` with the repository's
per-crate optimization settings; debug assertions remained enabled. This is not
a release-build capacity estimate.

Each shape performed 1,024 production diagnostic publications with eight records
per publication and four concurrent foreground reader threads. The seed's
admission payload was 6,903 bytes; each request accounts for its actual serialized
bytes. SQLite WAL, `synchronous=NORMAL`, manual checkpoint policy. Requests
specified `Full` durability, reported separately from the connection policy.

Numbers below are medians of the three per-run statistics, not percentiles pooled
across repetitions. Receipt latency includes actor queueing and SQL execution,
but excludes constructing the request. Throughput includes request construction.

| Shards | Submitters | Committed publications/s | Receipt p50 ms | p95 ms | p99 ms | Per-run p99 range ms |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 1 | 142.8 | 6.649 | 8.871 | 13.941 | 10.729–14.722 |
| 1 | 8 | 503.1 | 15.326 | 21.068 | 28.188 | 24.587–29.375 |
| 4 | 8 | 690.2 | 9.498 | 14.245 | 54.178 | 38.685–76.289 |

| Shards | Submitters | Reads/s | Read p50 ms | p95 ms | p99 ms |
|---:|---:|---:|---:|---:|---:|
| 1 | 1 | 49004 | 0.078 | 0.113 | 0.149 |
| 1 | 8 | 47158 | 0.081 | 0.117 | 0.152 |
| 4 | 8 | 43640 | 0.087 | 0.128 | 0.165 |

All runs completed without failed submissions, failed reads, or SQLite busy
events. Every run verified contiguous durable receipt and checkpoint sequences
and `PRAGMA integrity_check`. [Raw JSON](results.jsonl) preserves all observations.

At eight submitters, four shard files provided about 1.37x the median throughput
and lower p50/p95 receipt latency than one file. Its p99 was higher and variable
(38.7–76.3 ms versus 24.6–29.4 ms); sharding did not improve every tail metric.
One-shard/eight-submitter batching used 129 transactions including the seed,
versus 1,025 for one submitter. Four-shard/eight-submitter runs used 516,
so these comparisons include batching and smaller per-file histories as well as
parallel writer threads. They do not measure a throughput gain from the admission
fix, which protects foreground reader capacity during concurrent background opens.

The reads are scalar current-generation queries, not full search or FTS workloads.
Process exit recovery is verified separately; power-loss durability and broader
application throughput remain outside these measurements.

Reproduction commands are in [README](README.md). The initial benchmark was one
explicit run; two further fresh processes used the same command with
`--runs_per_test=2`. Each run reported one benchmark test passed.
