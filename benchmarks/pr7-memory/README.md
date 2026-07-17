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

Captured from the dirty PR7 worktree at the recorded Git HEAD (dirty, so no
commit attestation), release test profile (`debug_assertions: false`), 3
warmups and 30 measured repetitions of 8 records per record phase (30 x 8 =
240 records) and 30 migration runs. The raw artifact records the Linux kernel,
CPU, memory, Rust/Cargo toolchains, every repetition, and the
nearest-rank/sample-standard-deviation method.

<!-- MEASURED_SUMMARY -->
