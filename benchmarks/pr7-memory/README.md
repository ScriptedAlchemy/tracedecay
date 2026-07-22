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

## Acceptance status: pending parent run

The checked-in result is **provisional, unattested evidence**, not accepted
benchmark evidence. The PR5 runner requires a clean commit, expands it with
`git archive`, and attests every compiler input. The PR7 harness does not
currently provide that acceptance path, so commit-attested naming
(`result-<date>-<commithash>.json`) and the `acceptance` evidence status cannot
be used here.

- The measurement runs as an `#[ignore]`-gated test,
  `store::memory_benchmark::pr7_memory_baseline` (skipped by the default
  `cargo test` / `cargo nextest` runs so CI test jobs do not pay it; run it
  explicitly by adding `--ignored` as shown above). When run, it rewrites
  [result-provisional.json](result-provisional.json) on Linux
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
- `current_acceptance` must remain null until a parent run completes every gate
  below and produces a clean-commit, compiler-input-attested artifact. Merely
  rerunning the current measurement test does not promote provisional evidence.

### Unmeasured behavioral acceptance coverage

The retained PR6/PR7 behavior gates are intentionally separate from the
performance samples in `result-provisional.json`:

- checked-in native provider fixtures exercise definition/binding separation,
  complete and partial source frontiers, snapshot completion, and partial
  non-deletion through the production host-ingestion path;
- the production database authority exercises generic active, unavailable,
  superseded, and deleted anchor dispositions, append-only history, reverse
  lineage, terminal tombstones, and direct-evidence derivative suppression;
- V3 Git-topology domain fixtures exercise immutable repository/worktree/ref/
  object, PR/check, preview/apply, and integration-receipt targets; authorized
  drilldown preserves exact ordered sources and reports retargeting, stale
  generations, and source dispositions;
- evidence assembly exercises occurrence-set normalization, ordered spans,
  contribution publication/replay, and paged contribution-to-span-to-exact-
  source drilldown through production APIs.

These regressions are acceptance prerequisites, not benchmark measurements.
Their presence does not change the provisional artifact or the null acceptance
index.

### Required parent-run gates

Run these gates from the exact clean product/harness/workload commit that the
artifact will attest:

1. `cargo test --test host_event_fixture_test native_host_event_fixtures_execute_provider_admission_paths -- --exact`
2. `cargo test -p tracedecay-domain --test git_topology_anchor_contract`
3. `cargo test --lib db::retrieval_anchor_authority::tests -- --test-threads=1`
4. `cargo test --lib application::anchor_resolution::tests::topology_drilldown_preserves_sources_and_reports_stale_or_retargeted -- --exact`
5. `cargo test --lib application::evidence_assembly::tests::authorized_drilldown_expands_contribution_span_set_and_exact_members -- --exact`
6. `cargo test --all-features`
7. `cargo test --release --lib store::memory_benchmark::pr7_memory_baseline -- --exact --ignored --nocapture --test-threads=1`
8. `cargo test --lib store::memory_benchmark::workload_manifest_matches_code_contract -- --exact`
9. `cargo test --lib store::memory_benchmark::evidence_directory_matches_index_contract -- --exact`

The parent must additionally use a clean-archive attestation path equivalent to
the PR5 runner, verify the artifact's commit equals the clean source commit,
verify `git.dirty == false` and `platform.debug_assertions == false`, and verify
the embedded workload and harness digests against the compiler inputs. No such
PR7 acceptance artifact has been generated yet. Until all of those conditions
hold, the index remains pending with `current_acceptance: null`.

Any `cargo test --lib store::memory_benchmark -- --ignored` invocation
re-executes the measurement and re-emits the provisional artifact (the
measurement is `#[ignore]`-gated, so a bare `cargo test --lib
store::memory_benchmark` now runs only the un-ignored manifest and evidence
directory validators). Those validators run in the same module and hold an
inter-process file lock plus atomic rename so concurrent test processes never
read a partial artifact.

## Provisional measurement snapshot

[result-provisional.json](result-provisional.json) is the sole authority for
the latest local measurement values. It records its dirty commit snapshot,
build profile, toolchain, raw samples, and derived distributions. Those values
are diagnostic only: they may be regenerated by an ordinary test run and must
not be quoted as accepted PR7 performance evidence.

## Developer build feedback

`scripts/dev/pr7-build-feedback.sh 52dbcac8 <PR7 tip>` (3 runs per phase,
fresh shared clones, AMD EPYC 7742, cargo 1.95.0). First run per phase warms
caches and is excluded from the comparison:

| Phase | Baseline `52dbcac8` | PR7 tip |
|---|---|---|
| Warm no-op `cargo check --all-features` | 0.35–0.36 s | 0.35–0.37 s |
| Touched-unit rebuild (touch store crate) | 2 units | 2 units |
| `session_suite` test build, warm | 2.60–2.64 s, ~556 MB peak RSS | 2.70–2.76 s, ~595 MB peak RSS |
| `session_suite` test build, cold link | 148.2 s, 5.51 GB peak RSS | 155.4 s, 6.04 GB peak RSS |

No incremental rebuild amplification; the cold-link and RSS growth (~5–10%)
tracks the slice's added code. Absolute timings are diagnostic, not portable
gates.

The touched-unit row above reports only the rebuilt-unit count because the
measurement script's `/usr/bin/time` output was piped straight into the
`grep -c` used to count `Checking` lines, silently discarding the wall/maxrss/cpu
line, and a second `/usr/bin/time ... true` line measured a no-op instead of
recovering it. The script now writes `/usr/bin/time`'s output to a file with
`-o` and `cat`s it after counting rebuilt units, so re-running
`scripts/dev/pr7-build-feedback.sh` will surface wall/maxrss/cpu for this
phase alongside the unit count.
