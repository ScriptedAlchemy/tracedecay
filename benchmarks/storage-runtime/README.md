# SQLite storage runtime — Phase S0 frozen evidence / baseline harness

This directory is the Phase S0 delivery barrier for the SQLite storage runtime
plan (`sqlite_storage_runtime_a482f404.plan.md`). It freezes the last released
schema/binary identity and defines the current, 10x, open-loop overload, crash,
recovery, FTS, backup/restore, and A/A noise-floor workloads across the
supported store families (`graph`, `profile`, `project`, `session`), with a
reproducible, non-destructive, portable runner.

**Status: framework and contract only.** `workload-s0.json` is the frozen S0
baseline *definition*; its product command templates are intentionally `null`
(pending) and the runner fails closed on them. The checked-in dry-run workload
and self-tests exercise the full runner machinery without a daemon, a store, or
a tracedecay binary. Dry-run, pending, failed, partial, and `--only` artifacts
are explicitly `not_evidence`, never `completed`, even if a synthetic workload
is frozen and identity-bound. Per-platform S0 evidence
(Linux, Windows, macOS) is still pending execution; see "Evidence status"
below.

## Layout

- `run_storage_baseline.py` — the runner. Python 3.10+, standard library only.
- `workload-s0.json` — workload schema v1: the frozen S0 baseline definition
  (families, phases, counts invariants, A/A policy, platform matrix). All
  product steps are pending; this file is the contract, not evidence.
- `workload-dry-run.json` — workload schema v1: fully executable dry-run using
  stdlib python one-liners against the synthetic fixture below.
- `fixtures/dry-run-input/` — synthetic, runner-owned fixture. It contains no
  TraceDecay store files, no product schema, and no protocol fields.
- `tests/test_run_storage_baseline.py` — stdlib `unittest` suite covering
  recursive filesystem safety, identity binding, result lifecycle, output
  atomicity, process trees, platform/network guards, and redaction.

## Safety contract (fail closed)

The runner never discovers or touches the live TraceDecay profile implicitly:

- `--input` and `--output` are required; there are no defaults.
- Both are refused when they resolve to, are inside, or contain a known
  live/default profile location — `$TRACEDECAY_DATA_DIR`, the parent of
  `$TRACEDECAY_GLOBAL_DB`, or `~/.tracedecay` (matching `src/config.rs`
  `user_data_dir`) — including through aliases.
- Every external input tree and every runner-owned output tree is recursively
  inspected with `lstat`. Symlinks, Windows reparse points, devices, FIFOs,
  sockets, and regular-file hardlinks are rejected. The runner does not use a
  symlink-following preflight as a safety decision.
- Input and output must be disjoint. The output leaf must **not exist**:
  it is created with a fresh `mkdir`, and result/identity files use
  create-new/no-follow temporary writes plus no-replace atomic publication.
- Every phase/family/repetition receives a fresh runner-owned copy of the
  supplied corpus under its run directory. Recovery deliberately reuses only
  the matching crash copy.
- Child commands run from a runner-owned CWD with runner-owned `HOME`,
  config/cache/temp/output roots, and `TRACEDECAY_DATA_DIR`/
  `TRACEDECAY_GLOBAL_DB` roots. `TMPDIR`, `TEMP`, `TMP`, and `SQLITE_TMPDIR`
  are pinned to the per-run temp root.
  Existing `TRACEDECAY_*` and `NEXTEST_TEST_NAME` variables are scrubbed with
  Windows case-insensitive normalization; workload declarations cannot override
  those protected roots. Version probes use the same isolated environment.
- Paths derived from placeholders are containment checked; phase/family/evidence
  identifiers are constrained to safe identifiers. A template cannot escape a
  runner-owned root with `..`.
- Detected NFS/SMB/SSHFS-like input/output filesystems are refused. Linux uses
  `/proc/self/mountinfo`, Windows uses `GetDriveTypeW` when available; platforms
  without a stdlib detector report `not_detectable`, and such a run cannot be
  evidence rather than claiming filesystem locality.
- Workload execution is refused where stdlib process-tree cleanup is
  unsupported. In particular, Windows version probes are not run and workload
  execution is blocked until safe descendant cleanup is available.
- Steps with a `null` argv (pending product commands) refuse to execute
  unless `--allow-pending` records them as `pending` with no measurements and
  a top-level `not_evidence` lifecycle state.
- Result artifacts contain no machine-specific absolute paths: store paths are
  recorded relative to the output root, binaries by basename + SHA-256, and
  the input is identified by a path-independent content fingerprint. Child
  stdout/stderr and FTS probe text are represented only by redacted size/line
  metadata and hashes. The hostname is redacted unless `--record-hostname` is
  passed.

The JSON result is redacted, but the mode-0700 output tree intentionally holds
private working copies of the supplied stores. Treat the whole output directory
as sensitive local benchmark state; publish only reviewed result artifacts.

## Usage

All commands run from this directory.

Self-test (dry-run end to end with assertions; no daemon needed):

```console
python3 run_storage_baseline.py self-test
```

Unit tests:

```console
python3 -m unittest discover -s tests -v
```

Freeze the last released identity. It reads only explicit safe artifacts and
binds the tested binary, schema, workload, corpus, and runtime configuration;
it never derives identity from a live profile:

```console
python3 run_storage_baseline.py freeze \
  --binary /path/to/released/tracedecay \
  --schema-manifest /path/to/released-schema-export \
  --workload workload-s0.json \
  --corpus /path/to/isolated-store-corpus \
  --config /path/to/released-runtime-config \
  --output frozen-identity-v2.json
```

Dry-run the full workload machinery against the synthetic fixture:

```console
python3 run_storage_baseline.py run \
  --workload workload-dry-run.json \
  --input  /path/to/isolated/input-copy \
  --output /path/to/fresh/output-dir
```

S0 evidence run (once the pending product commands are wired; note the frozen
identity is required by `workload-s0.json` and the input must be an explicit
copy of released store fixtures — never the live profile):

```console
python3 run_storage_baseline.py run \
  --workload workload-s0.json \
  --input  /path/to/explicit-store-copy \
  --output /path/to/fresh/output-dir \
  --frozen-identity frozen-identity-v2.json \
  --binary /path/to/released/tracedecay \
  --schema-manifest /path/to/released-schema-export \
  --config /path/to/released-runtime-config
```

Validate a result artifact (schema, counts invariants, comparison outcomes,
absolute-path leak scan):

```console
python3 run_storage_baseline.py validate --result <output>/storage-runtime-baseline-result.json
```

## Workload schema v1

Top level: `schema_version` (1), `workload_id`, `evidence_eligible`,
`store_families`, `phases`,
plus optional `binary`, `frozen_identity`, `environment.version_commands`,
`safety.env` / `safety.env_path_keys`, `defaults`, `metrics`, `platforms`, and
`limitations`. Identifier-like values are safe identifiers; protected child
root environment variables are runner-owned, not workload-configurable.

Phases (`name`, `kind`, explicit `families`):

- `closed_loop` — `setup` (once) then `work` repeated `warmup + repetitions`
  times strictly sequentially; per-operation latency plus `evidence` captures
  and `compare` assertions. Used for `current`, `ten_x`, and `fts`.
- `open_loop` — operations are scheduled at `offered_rate_per_second` for
  `operation_count` operations with an `max_in_flight` admission cap. Excess
  operations are shed (`shed.runner_in_flight_cap`); exit codes map to
  outcomes via `outcome_map` (e.g. admission rejection →
  `shed.command_saturation`); `retryable_outcomes` + `max_retries` drive the
  `retried` count. **Latency is measured from the scheduled issue time**, so
  queueing delay is included and there is no coordinated omission; scheduler
  lag is reported separately as `schedule_lag_ns`.
- `crash` — starts the `work` process in a new POSIX process group, waits for
  `wait_for_file` (or `after_seconds`), then SIGKILLs the group and verifies no
  live group member remains. CPython stdlib has no safe Windows Job Object, so
  Windows crash/tree verification is explicitly `unsupported` and cannot
  produce completed evidence.
- `recovery` — `depends_on` a crash phase and runs `recover` against the same
  crashed store copy, then re-captures evidence and asserts `compare` entries
  (cross-phase references use `phase:name`).
- `backup_restore` — ordered `steps` (backup → verify → restore) into staging
  under the isolated run dir, then digest/count evidence and comparisons.
- `aa_pairs` — re-executes a `closed_loop` `target_phase` `2 * pairs` times on
  fresh store copies (alternating A/B), computes per-pair relative deltas on
  p50 response latency and throughput, and reports the A/A noise floor and the
  regression margin (`noise_floor * margin_multiplier`). Margins are
  per-machine and must be re-baselined per platform.

Command templates are argv arrays with `__INPUT__`, `__OUTPUT__`,
`__RUN_DIR__`, `__FAMILY__`, `__BINARY__`, `__PYTHON__`, `__REPETITION__`
token substitution. `__OUTPUT__` is a per-run isolated output root, not the
top-level artifact directory. Path-bearing placeholders are containment checked.
Evidence captures are `sqlite_logical` (integrity/schema/table-count/optional
FTS row-identity or rank/snippet-result hashes), `logical_file` (synthetic dry-run fixtures only),
and `stdout_redacted` (hash/size/line count only). Raw SQLite-file digest
equality and raw FTS/stdout text are not evidence types. A `null` argv marks a
step pending.

Counts recorded per run: `offered`, `admitted`, `completed`, `failed`,
`retried`, `shed.runner_in_flight_cap`, `shed.command_saturation`, validated
against the invariants `offered == admitted + shed.runner_in_flight_cap` and
`admitted == completed + failed + shed.command_saturation`. Latency summaries
are nearest-rank p50/p95/p99 plus min/max/sample-stddev. Each offered
open-loop request has exactly one terminal ledger record, including scheduled,
admitted, started, and terminal monotonic offsets (explicit `null` admission /
start for runner-shed requests), outcome, attempts, exit/timeout metadata, and
scheduled-to-terminal latency.

## Result artifact (`storage-runtime-baseline-result-v2`)

Written to `<output>/storage-runtime-baseline-result.json`: workload hash,
identity-binding status, normalized platform and process-tree capability,
environment/toolchain block (with redacted tool output), safety guards,
path-independent corpus fingerprint, per-phase/family results, logical or
redacted evidence, overload terminal ledgers, and A/A noise-floor analysis.
`status` is `completed`, `not_evidence`, or `failed_validation`. Only a full,
bound, validation-clean run with supported process-tree verification can be
`completed`; partial/pending/`--only` output is always `not_evidence`.

## Evidence status

- Runner machinery, safety guards, counts invariants, latency recording,
  crash/recovery orchestration, backup/restore comparison, and A/A analysis:
  verified by `self-test` and the 63-test unittest suite on the development
  host (Linux, CPython 3.12).
- Still requiring execution before S0 checkpoint acceptance:
  - wiring the released binary's explicit commands into `workload-s0.json`
    (all product steps are pending by design),
   - capturing `frozen-identity-v2.json` from the released binary/schema,
     workload/corpus/config tuple,
  - the actual current/10x/overload/crash/recovery/FTS/backup baselines and
    A/A noise floors on **Linux, Windows, and macOS** (noise floors are
    per-machine; margins must be recorded per platform),
   - Windows has no safe CPython-stdlib Job Object, so workload execution is
     refused until safe descendant cleanup is added,
   - macOS and Linux without readable mount metadata cannot claim evidence
     until local-filesystem locality can be verified,
  - symlink-alias guard tests skip on platforms/filesystems without symlink
    support (Windows without developer mode).

## Non-goals

No schema migration, no store opening by the runner itself, no live dogfood,
no profile mutation, no network access. The runner orchestrates explicitly
supplied binaries/commands against explicitly supplied store copies and
reports machine-readable evidence; it is not the product runtime.
