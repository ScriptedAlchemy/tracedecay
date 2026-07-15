# PR5 observation-pipeline baseline

The versioned [workload](workload-v1.json) runs the production Claude
scan/parse, sanitizer, authoritative commit, projection/V1 fold, and bounded
replay path. It also carries one provider-neutral, versioned baseline schema
for claude, codex, cursor, hermes, kiro, cline, roo-code, and kilo. Each entry
uses a deterministic redacted synthetic fixture and bounds parse, normalize,
sanitize, commit, replay, duplicate-noop, projection, backlog, fairness, and
peak-resource checks supported by this harness. A normal test deserializes the
complete manifest with unknown fields denied and checks its input, provider
catalog, phases, excluded setup, no-op invariants, metrics, platform,
repetitions, identity, and command against the executable harness.

An acceptance result uses result schema 2 against workload schema 3. It embeds
SHA-256 identities for the manifest, all compiled harness sources, and executing
test binary; build-time and runtime Git commit/tree identities must match. Matching clean Git snapshots are
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

The former [acceptance result](result-2026-07-15-0c289212.json) was captured
from clean commit `0c289212de5429e5d5abf309f6bb27e49f66a64e` with 3 warmups and
30 independent measured repetitions of 64 records (1,920 records). It is now
historical because it predates workload schema 3; capture a new clean acceptance
artifact before using this catalog for regression comparison. The raw artifact
records the Linux kernel, CPU, memory, Rust/Cargo toolchains, every repetition,
and the nearest-rank/sample-standard-deviation method.

- Pipeline batch latency: p50 323,175,923 ns; p95 336,411,433 ns; p99
  341,652,217 ns; sample standard deviation 9,804,394 ns.
- Pipeline throughput: 198.60320836792673 records/s.
- Timed pipeline CPU: 6,480 ms; peak RSS: 24,704 KiB.
- Timed process write I/O: 250,552,320 bytes; SQLite database/WAL/SHM growth:
  112,230,320 bytes across the 30 independent databases.
- Exact no-op retry plus bounded replay: p50 194,962 ns; p95 217,902 ns; p99
  224,762 ns; 10 ms CPU total; zero process write bytes, database growth,
  observation-count change, and coordinator work counters.

The [historical result](result-2026-07-15-b05b4cd5.json) was captured from clean
commit `b05b4cd570ab8e3385604c0fef31902fdc3f1e8b`.

> **Historical/stale evidence:** this result predates schema 2 provenance and
> complete workload validation. Its JSON carries
> `"evidence_status": "historical_stale"` and is rejected as acceptance
> evidence by normal tests. Retain it only for provenance.

```console
scripts/run-pr5-observation-benchmark.sh
```
