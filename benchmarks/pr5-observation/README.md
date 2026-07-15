# PR5 observation-pipeline baseline

The versioned [workload](workload-v1.json) runs the production Claude
scan/parse, sanitizer, authoritative commit, projection/V1 fold, and bounded
replay path. The [raw result](result-2026-07-15-b05b4cd5.json) was captured from
clean commit `b05b4cd570ab8e3385604c0fef31902fdc3f1e8b` with:

> **Historical/stale evidence:** this result predates the benchmark integrity
> fixes on the current PR5 integration branch. Retain it only for provenance;
> it is not acceptance evidence for the current HEAD. A final clean-HEAD run
> must replace the summary and raw result before PR5 is complete.

```console
cargo test --quiet --release --lib sessions::claude_observation_benchmark::production_observation_pipeline_baseline -- --ignored --exact --nocapture --test-threads=1
```

The retained run used 3 warmups and 30 independent measured repetitions of 64
records (1,920 records). The raw artifact records the Linux kernel, CPU, memory,
Rust/Cargo toolchains, every repetition, and the nearest-rank/sample-standard-
deviation method.

- Pipeline batch latency: p50 231,468,087 ns; p95 3,370,435,094 ns; p99
  4,614,566,543 ns; sample standard deviation 1,159,889,643 ns.
- Pipeline throughput: 86.06239150940468 records/s.
- Timed pipeline CPU: 4,570 ms; peak RSS: 22,672 KiB.
- Timed process write I/O: 182,652,928 bytes; SQLite database/WAL/SHM growth:
  109,871,520 bytes across the 30 independent databases.
- Exact no-op retry plus bounded replay: p50 1,951,425 ns; p95 2,135,867 ns;
  p99 2,577,411 ns; 50 ms CPU total; zero process write bytes, database growth,
  observation-count change, and coordinator work counters.

The historical run's high pipeline tail variance is retained rather than
filtered. It is not an optimization claim.
