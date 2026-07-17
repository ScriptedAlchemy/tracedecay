# PR7 memory, fact, anchor, and migration benchmark

This directory follows the PR5/PR6 `benchmarks/pr5-observation` evidence
contract for the PR7 slice required by `docs/plans/tracedecay-v2/NEXT.md`:
bounded fact-write, anchor-create, anchor resolution, replay, and migration
baselines recorded for later PR20 comparison.

The versioned [workload](workload-v1.json) is machine-asserted by a normal
test in `src/store/memory_benchmark.rs` with unknown fields denied. Each
measured repetition commits a bounded batch of records through the production
path and samples latency, CPU, process write I/O, database storage growth, and
peak RSS under the same Linux `/proc` contract as the PR5 baseline. Record and
batch construction, database open and schema initialization, daemon authority
acquisition, and all correctness point-reads are excluded from the measured
samples. Every phase reports nearest-rank p50/p95/p99 and sample standard
deviation over 30 measured repetitions after 3 warmups.

Measured phases:

- `fact_write` (`memory_application_commit_fact_v1`): one owner-bound fact per
  record committed through `MemoryApplication::commit_fact` over
  `DatabaseFactStore` — derived fact identity, one sanitized assertion with one
  evidence reference, one new retrieval anchor materialized in the fact shard,
  and one assertion-recorded lineage event in a single daemon authority
  transaction.
- `anchor_create` (`observation_store_persist_anchored_observation_v1`): one
  sanitized observation and its stable V2 retrieval anchor committed through
  `GlobalDbObservationStore::persist_observation` in one authoritative
  transaction.
- `anchor_resolution` (`global_db_resolve_observation_evidence_anchor_v1`):
  one owner-bound evidence anchor resolution through
  `GlobalDb::resolve_observation_evidence_anchor`, the same query the
  daemon-authorized `EvidenceAnchorResolver` boundary uses.
- `anchor_replay` (`observation_store_repeat_persist_exact_duplicate_v1`): one
  exact repeat persist of an already-committed anchored observation. Every
  sample must return `ExactDuplicate` with the originally created anchor, and
  durable observation cardinality must not advance.
- `migration_v19_to_v22` (`db_migrate_user_version_18_to_latest`): one
  production `db::migrations::migrate` run over a fixture database pinned at
  `user_version = 18`. The PR7 migrations are additive over memory-v2 and
  retrieval-anchor tables only, so the pinned empty fixture exercises exactly
  the same migration steps a pre-PR7 database runs; legacy projection backfill
  and cutover are daemon-authorized runtime actions and are not part of schema
  migration. Each sample's record count is the number of applied migration
  steps (4 at workload authoring, when the chain ended at v22; 5 at capture
  time, after the additive v23 PR7 migration landed in the same chain).

## Provisional, unattested evidence

Unlike the PR5 baseline, this evidence is **provisional**. The PR5 runner
requires a clean commit, expands it with `git archive`, and attests every
compiler input; that workflow is impossible while the PR7 tree is dirty, so
commit-attested naming (`result-<date>-<commithash>.json`) and the
`acceptance` evidence status cannot be used here. Instead:

- The measurement runs as a normal test,
  `store::memory_benchmark::pr7_memory_baseline`, and rewrites
  [result-provisional.json](result-provisional.json) on every run on Linux
  (unsupported platforms skip without emitting). The artifact carries
  `"evidence_status": "provisional"`,
  `"provisional_reason": "dirty_worktree_no_commit_attestation"`, an honest
  Git snapshot (including the dirty flag), and `debug_assertions` so the
  capture profile is explicit. Numbers in the checked-in copy come from the
  last local run and are regenerated, not curated.
- [evidence-index.json](evidence-index.json) names the provisional artifact
  and keeps `current_acceptance` null; the directory validator rejects any
  acceptance artifact from a dirty tree, unindexed or duplicate artifacts, and
  artifacts whose embedded workload or harness identity differs from the
  compiled sources.
- When PR7 lands on a clean commit, regenerate this baseline through an
  attested run and promote the index to a commit-attested acceptance artifact
  before PR20 compares against it.

The checked-in artifact was captured with:

```console
cargo test --release --lib store::memory_benchmark::pr7_memory_baseline -- --exact --nocapture --test-threads=1
```

Any `cargo test --lib store::memory_benchmark` invocation re-executes the
measurement and re-emits the provisional artifact; the manifest and evidence
directory validators run in the same module and hold an inter-process file
lock plus atomic rename so concurrent test processes never read a partial
artifact.

## Measured summary

Captured from the dirty PR7 worktree at commit
`0c7c2c4ae3a4dce53c6ebddb97fdb0b3cd20528a` (dirty, so no commit attestation),
release test profile (`debug_assertions: false`), on an AMD EPYC 7742 under
Linux 6.8.0-134-generic, rustc/cargo 1.95.0, 3 warmups and 30 measured
repetitions of 8 records per record phase (30 x 8 = 240 records) and 30
migration runs of 5 applied steps each (v19 through v23; the chain gained the
additive v23 PR7 migration while this baseline was recorded). The raw artifact
records the Linux kernel, CPU, memory, Rust/Cargo toolchains, every
repetition, and the nearest-rank/sample-standard-deviation method.

- Fact write (8 facts/repetition): p50 16,786,352 ns; p95 40,404,376 ns; p99
  41,284,518 ns; 339.2 facts/s. Timed CPU: 520 ms; process write I/O:
  85,409,792 bytes; database growth: 2,703,960 bytes; peak RSS: 25,844 KiB.
- Anchor create (8 anchored observations/repetition): p50 25,695,910 ns; p95
  32,589,403 ns; p99 35,251,936 ns; 299.5 records/s. Timed CPU: 440 ms;
  process write I/O: 25,460,736 bytes; database growth: 3,337,400 bytes; peak
  RSS: 30,080 KiB.
- Anchor resolution (8 resolutions/repetition): p50 11,493,323 ns; p95
  11,603,194 ns; p99 12,320,053 ns; 693.6 resolutions/s. Timed CPU: 330 ms;
  zero process write bytes and database growth; peak RSS: 30,100 KiB.
- Anchor replay (8 exact repeat persists/repetition): p50 11,325,881 ns; p95
  11,446,143 ns; p99 11,563,782 ns; 706.0 replays/s; every sample returned the
  originally created anchor as an exact duplicate and durable observation
  cardinality stayed at 264. Timed CPU: 360 ms; zero process write bytes and
  database growth; peak RSS: 30,100 KiB.
- Migration v19-to-v23 (one production `migrate()` run per repetition, 5
  applied steps): p50 24,465,974 ns; p95 25,545,768 ns; p99 95,928,022 ns;
  185.7 migration steps/s. Timed CPU: 680 ms; process write I/O: 12,922,880
  bytes; database growth: 12,854,400 bytes across the 30 independent fixture
  databases; peak RSS: 29,672 KiB.
