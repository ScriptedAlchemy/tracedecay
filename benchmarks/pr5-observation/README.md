# PR5 observation-pipeline baseline

The versioned [workload](workload-v1.json) runs the production Claude
scan/parse, sanitizer, authoritative commit, projection/V1 fold, and bounded
replay path. A normal test deserializes the complete manifest with unknown
fields denied and checks its input, phases, excluded setup, no-op invariants,
metrics, platform, repetitions, identity, and command against the executable
harness.

An acceptance result uses schema 2. It embeds SHA-256 identities for the
manifest, all compiled harness sources, and executing test binary; build-time and
runtime Git commit/tree identities must match. Matching clean Git snapshots are
taken before and after the workload. The runner builds in a target directory
keyed by the clean commit, and the harness rejects debug assertions, direct
unattested invocations, a changing HEAD/tree, or a worktree that becomes dirty.

Linux `/proc` is the explicit measurement platform contract. Preflight requires
all measured interfaces, a successful write of `5` to
`/proc/self/clear_refs`, and a nonzero `getconf CLK_TCK`, before warmup begins.
The module still compiles on non-Linux targets; an attempted evidence run there
with the manifest's exact Cargo command executes the ignored test and rejects
the unsupported platform at preflight. Direct Cargo invocations on Linux also
reject because they lack the clean-build attestation supplied by the runner.
CPU identity accepts common x86, ARM, POWER, and other Linux
`/proc/cpuinfo` labels.

Every replayed authoritative payload is checked for canary removal and a
redaction marker, and every folded V1 message is checked for exact identity,
role, text, and canary absence. These assertions and V1 point reads run after
each phase snapshot, so correctness verification is not charged to latency,
CPU, I/O, or storage-growth measurements. The run also requires zero legacy
transcript writes: the observation projector is the only V1 message writer,
and compatibility transcript counters report those projector outputs.
The timed no-op retry replays after the durable end cursor and must return zero
new observations; a full replay verifies unchanged cardinality afterward.

Evidence uses a two-commit workflow:

1. Commit all code and manifest changes, then start from that clean commit.
2. Run the command below. It creates one schema-2 result, updates
   [evidence-index.json](evidence-index.json), and runs the strict directory
   validator. Commit only that result, the index, and this README's measured
   summary as the evidence-only follow-up. Do not change product, harness, or
   workload files in the evidence commit.

The runner removes partial result/index changes on failure. The index permits
the checked historical artifact but the finalization gate requires exactly one
fully typed current acceptance artifact and rejects unindexed, duplicate, or
unknown-field results.

The [historical result](result-2026-07-15-b05b4cd5.json) was captured from clean
commit `b05b4cd570ab8e3385604c0fef31902fdc3f1e8b` with:

> **Historical/stale evidence:** this result predates schema 2 provenance and
> complete workload validation. Its JSON carries
> `"evidence_status": "historical_stale"` and is rejected as acceptance
> evidence by normal tests. Retain it only for provenance. A final clean-HEAD
> run must add a schema 2 result and replace the summary before PR5 is complete.

```console
scripts/run-pr5-observation-benchmark.sh
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
