# Code-index restore performance evidence (2026-07-31)

> **Dated runtime evidence — not acceptance authority.** Preserve these raw
> samples and their provenance, but do not recreate their exact counts,
> snapshots, receipts, attestations, binary/worktree choreography, or gates as
> build prerequisites. Current requirements come only from the
> `docs/plans/tracedecay-v2/` hierarchy; validate current runtime behavior directly.

## Question under test

A historical defect from the delivery snapshot (not recorded in-repo) claims
code-index **restore** — loading an already-built index at project open, not
initial indexing — costs about **488.8 s wall / 7.8 GiB peak RSS**. The
release-readiness investigation requires this to be disproven or fixed.

## Verdict: DISPROVEN at repo-scale workload

Seven cold-restore samples over a pre-indexed isolated profile of the
repository's own code tree measured:

- restore-path total (daemon cold start + project open to first successful
  query): **2.13 – 3.56 s** (median 2.42 s)
- daemon peak RSS (`/proc/<pid>/status` VmHWM): **100 – 160 MiB**
- whole-sample process-tree peak (GNU time, includes driver + daemon + CLI):
  **318 – 338 MiB**

That is ~140–230x faster and ~50–80x less peak memory than the historical
defect figures. Caveat: the historical baseline's workload identity is not
recorded in-repo, so an exact same-workload replay is impossible; this
evidence uses the TraceDecay repository code tree itself. At that scale no
restore pathology exists. If the historical
figure came from a radically larger profile, that workload would need to be
reconstructed to compare; nothing in-repo identifies one.

## Workload identity

Fixture project = checked-in deterministic runtime fixture plus a copy of the
repository's `src/` and `crates/` trees (no `.git`, no `target`):

- 1,861 files on disk; indexed: 1,808 files (1,769 Rust, per
  `workload.status.json`)
- 122,076 graph nodes, 255,898 edges
- project graph DB 438,317,056 bytes; total isolated profile data dir after
  init: 888,508,091 bytes
- one-time `tracedecay init` (full index): 83.2 s wall, CLI peak RSS
  1,265,908 KiB (recorded in `report.samples.jsonl` as `index-build-once`;
  this is initial indexing, NOT restore)

## Method

Isolation used only the repo's own harness primitives, exactly as
`benchmarks/runtime/run.py` composes them (`restore_driver.py` in this
directory is the generation script):

1. `benchmarks/runtime/fixtures.py::prepare_fixture_snapshot(fixture_root=...)`
   prepared an isolated snapshot (isolated `HOME`, `TRACEDECAY_DATA_DIR`,
   `TRACEDECAY_DAEMON_SOCKET` all derived by the harness from the snapshot
   root — never exported manually) whose fixture project embeds the repo code
   tree. Snapshot root `/tmp/td-restore-ev/snap-base` (short path: the daemon
   socket must fit `SUN_LEN`).
2. `tracedecay init <project>` ran once, daemon-less, building the full index
   into the isolated profile.
3. Each sample: `benchmarks/runtime/lifecycle.py::OwnedDaemon` cold-started a
   fresh daemon process over the same pre-indexed profile
   (`daemon run --socket <snapshot>/run/tracedecay.sock`), readiness by
   socket probe (`admission_ns`), then the deterministic query
   `tool tracedecay_find_exact_symbol --args {"name":"active_data_dir_name","limit":20,"format":"json"}`
   retried while the daemon reported the project as "warming in the
   background" until first success (`query_wall_ns`, `query_attempts`).
   Anti-vacuity: every sample verified the response contains the expected
   symbol from the embedded repo tree (`query_symbol_found: true`, response
   about 10 KiB). The daemon admits pre-indexed projects asynchronously, so
   time-to-first-successful-query is the user-observable restore latency.
   Daemon VmHWM/PSS read before teardown; process tree verified reaped.
   Every sample invocation wrapped in `/usr/bin/time -v` (parsed into
   `gnu_time` per JSONL line).
4. Controls: three daemon cold starts over freshly prepared, never-indexed
   snapshots (admission 0.04 / 2.91 / 4.83 s — first-open global-DB creation
   variance; peak RSS 25–31 MiB). Restore-sample admission was uniformly
   about 0.04 s (one 0.17 s outlier); nearly all restore-path time is the
   post-admission warming window.

Sanity: warming completed in about 2–3 s while a full re-index takes about
83 s, so the samples measure sealed-index restore, not a silent rebuild.

Exact commands (repository root; scratchpad path abbreviated):

```
python3 restore_driver.py index   --binary target/release/tracedecay \
  --snapshot /tmp/td-restore-ev/snap-base --fixture-root <scratch>/fixture-root-large \
  --out /tmp/td-restore-ev/index.json
# N=7:
/usr/bin/time -v python3 restore_driver.py sample --binary target/release/tracedecay \
  --snapshot /tmp/td-restore-ev/snap-base --index <i> --out /tmp/td-restore-ev/sample-<i>.json
# controls, N=3:
/usr/bin/time -v python3 restore_driver.py control --binary target/release/tracedecay \
  --snapshot /tmp/td-restore-ev/ctrl-<i> --out /tmp/td-restore-ev/control-<i>.json
```

## Per-sample results (cold restore over pre-indexed profile)

| sample | admission_s | to_first_success_s | attempts | total_s | daemon VmHWM MiB | GNU time tree-peak MiB |
|--------|------------:|-------------------:|---------:|--------:|-----------------:|-----------------------:|
| 1 | 0.039 | 2.184 | 3 | 2.223 | 135 | 324 |
| 2 | 0.172 | 3.383 | 4 | 3.556 | 160 | 338 |
| 3 | 0.039 | 3.022 | 4 | 3.061 | 159 | 338 |
| 4 | 0.038 | 2.092 | 3 | 2.130 | 100 | 319 |
| 5 | 0.045 | 2.083 | 3 | 2.128 | 128 | 323 |
| 6 | 0.039 | 2.383 | 3 | 2.422 | 125 | 318 |
| 7 | 0.041 | 2.877 | 4 | 2.918 | 158 | 338 |

Raw per-sample records: `report.samples.jsonl` (custom explicit line schema —
`kind` in {`index-build-once`, `cold-restore-sample`,
`control-empty-profile-admission`} — not the `run.py` sample schema, whose
fields do not describe this composed measurement).

## Provenance and environment

- Repository HEAD at capture: `65c6d1ff0f2846c3b784e2ed97d826f3ce3fd2d5`
  (branch `codex/tracedecay-total-redesign-plan`), working tree dirty
  (39 entries) from a concurrent peer refactor.
- Measured binary: `target/release/tracedecay`, version
  `0.0.66+22b2c3d31fa4.dirty`, sha256
  `46fbe184cca265a34dbf491d7d50058e69e1682b050f88699d0655cd1a5336f8`.
  Commit `22b2c3d31` (2026-07-27) is an ancestor of HEAD, 1,263 commits
  behind.
- Host: Linux 6.8.0-136-generic, 96 CPUs, 160 GiB RAM. Cold means a cold
  daemon process; the OS page cache was warm (no privilege to drop caches).

## Deviations from the requested method

1. **Binary is not current HEAD.** `cargo build --release --bin tracedecay`
   fails at HEAD's working tree (peer refactor in flight; E0599/E0560 in
   `src/db/connection.rs`, `src/dashboard/memory_curate.rs`, error set
   changing between attempts). A clean-HEAD build was not possible without a
   side worktree or a target-dir override, both disallowed for this session.
   The newest available release build on this branch lineage was measured
   instead, as documented above.
2. **Indexing did not run under `scripts/with-isolated-tracedecay-daemon.sh`.**
   That wrapper deletes its profile on exit and owns a single daemon
   lifecycle, so a pre-indexed profile cannot survive into a separate
   restore measurement. The same isolation contract was composed from
   `benchmarks/runtime/run.py`'s own primitives instead (the other sanctioned
   isolation vehicle).
3. **No "tracedecay-n" workload exists in the tree** (contrary to the task
   brief); the repo's own code tree was used as the largest natural workload.
4. **Placeholder socket file workaround.** This binary build exits fatally
   when the socket path does not exist ("failed to remove stale daemon
   socket ... No such file or directory"), so the driver pre-creates a plain
   placeholder file at the socket path before daemon start. Worth re-checking
   at HEAD; `run.py` captures on 2026-07-30 did not need this.
