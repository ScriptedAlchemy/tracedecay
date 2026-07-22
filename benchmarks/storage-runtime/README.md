# SQLite storage runtime — Phase S0 frozen evidence / baseline harness

This directory is the Phase S0 delivery barrier for the SQLite storage runtime
plan (`sqlite_storage_runtime_a482f404.plan.md`). It freezes distinct released
product/evidence binary identities plus schema identity and defines the current,
10x, open-loop overload, crash,
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

- `run_storage_baseline.py` — small CLI and compatibility facade. Python 3.10+.
- `requirements.txt` — maintained runtime/test libraries: `jsonschema` for
  strict plan/result/receipt and product-adapter schemas, `psutil` for
  process-tree timeout cleanup and CPU/RSS/I/O observations, and `pytest` for
  the benchmark harness.
- `runner_contract.py` / `workload_model.py` — artifact constants, errors,
  latency/count models, and fail-closed workload schema validation.
- `safe_paths.py` / `profile_safety.py` — recursive path validation, safe
  copying/publication, live-profile isolation, child environments, and local
  filesystem checks.
- `process_execution.py` — placeholder containment, subprocess execution,
  process-tree cleanup, redacted streams, and environment identity capture.
- `run_context.py` / `phase_execution.py` — isolated per-run state and the
  closed/open-loop, crash/recovery, backup/restore, and A/A phase engines.
- `evidence_validation.py` / `freeze_identity.py` / `runner_commands.py` —
  logical evidence, artifact validation, frozen identity binding, and command
  orchestration.
- `product_adapter.py` — stdlib-only, fail-closed construction of the real
  `tracedecay tool --project <explicit-copy> search|message_search --args <json>`
  FTS probes. It accepts only an explicit binary, fixture, sandbox, and family;
  validates the source fixture, makes a fresh private copy beneath the sandbox
  for every invocation, and replaces inherited HOME/profile roots before
  invoking the product.
- `workload-s0.json` — workload schema v1: the frozen S0 baseline definition
  (families, phases, counts invariants, A/A policy, platform matrix). All
  product steps are pending; this file is the contract, not evidence.
- `workload-dry-run.json` — workload schema v1: fully executable dry-run using
  stdlib python one-liners against the synthetic fixture below.
- `fixtures/dry-run-input/` — synthetic, runner-owned fixture. It contains no
  TraceDecay store files, no product schema, and no protocol fields.
- `tests/test_safety_and_processes.py`, `tests/test_models_and_evidence.py`,
  `tests/test_freeze_and_validation.py`, and `tests/test_runner_end_to_end.py`
  — behavior-focused stdlib `unittest` suites covering recursive filesystem
  safety, identity binding, result lifecycle, output atomicity, process trees,
  platform/network guards, redaction, and dry-run orchestration.
- `tests/test_product_adapter.py` — exact argv/environment construction and
  fail-closed adapter tests; product execution is mocked and never dogfoods a
  profile. Standalone adapter results are explicitly marked `not_evidence`.
- `soak/scheduler.py`, `soak/executor.py`, `soak/schemas.py`,
  `soak/trends.py`, and `soak/evidence.py` — deterministic frozen plans,
  code-allowlisted execution via `asyncio.create_subprocess_exec`, strict
  schemas, campaign-duration/cadence trend gates, and fail-closed promotion.
  Plan JSON contains workload and gate IDs only; it cannot supply argv.

Hyperfine and pytest-benchmark are intentionally not used: they measure
short-lived command timing but do not own the long-lived copied store or its
provenance. Locust and k6 are also excluded because their network/load model
does not preserve this local fixture identity and process boundary.

## Product command audit

The checked-in CLI has real JSON MCP-tool entry points for graph text search
(`search`) and stored session-message search (`message_search`).
`product_adapter.py` constructs those exact commands without provider fields or
implicit profile discovery. A qualifying isolated input copy must contain
`storage-runtime-fixture-v1.json`:

```json
{
  "schema_version": 1,
  "project_root": "project",
  "profile_root": "profile",
  "fts_queries": {"graph": "fixture-owned query", "session": "fixture-owned query"},
  "s11": {
    "database": "profile/storage-evidence.sqlite3",
    "binding": {
      "shard_id": {
        "brain_id": "brain.storage-evidence",
        "profile_id": "profile.storage-evidence",
        "scope": {"kind": "project", "project_id": "project.storage-evidence"}
      },
      "incarnation": 1,
      "authority_epoch": 1
    },
    "evidence_tables": ["evidence_rows"]
  }
}
```

Both relative roots must stay inside the copy. Query text is deliberately
fixture-owned: the adapter does not fabricate session/provider protocol. The
repository currently has real provider-native transcript JSON fixtures and
synthetic runner fixtures, but no released graph/session database fixture
satisfying this manifest. Therefore the FTS workload template remains pending
even though its adapter command construction is real and tested.

S6 now supplies the concrete maintenance, Doctor, corruption/repair,
quarantine, online-backup, restore-publication, and backup/restore orchestration
APIs. S11 binds them to three fixed typed-evidence gate IDs:

- `storage-runtime-maintenance-doctor-v1` requires
  `MaintenanceCoordinator`, `SqliteMaintenanceDriver`, and
  `SqliteDoctorHealthLane`.
- `storage-runtime-crash-recovery-repair-v1` additionally requires
  `SqliteCorruptionProbe`, `SqliteRepairDriver`, and
  `FilesystemQuarantineStore`.
- `storage-runtime-backup-restore-v1` requires `BackupRoot`,
  `FilesystemBackupStore`, `SqliteOnlineBackupDriver`,
  `RestorePublicationAuthority`, and `BackupRestoreOrchestrator`.

The allowlisted `storage-runtime-s11-product-gates-v1` workload calls only the
product-facing `storage-runtime-evidence --gate <fixed-id>` binary target from
`tracedecay-rusqlite-runtime`; the root package does not link the runtime before
cutover. The runner passes its exact product commit and copied-fixture SHA-256,
plus the separately frozen product/evidence binary SHA-256 identities. The
adapter invokes only the evidence binary for these gates; it never treats the
released TraceDecay CLI as that executable. The evidence binary recomputes the
fixture fingerprint before executing real S5/S6 capabilities. Output must
satisfy strict per-gate JSON schemas and bind the copied fixture, product
commit, tested product binary, emitting evidence binary, and logical SQLite
evidence.

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
- `product_adapter.py` applies the same boundary before its FTS and fixed S11
  product-gate commands:
  it refuses fixture/sandbox overlap and live/default/custom profile roots,
  recursively `lstat`s the source fixture (including hardlink and replacement
  checks), then invokes the released product CLI only for product probes and
  the distinct `storage-runtime-evidence` binary only for fixed S11 gates,
  always against a fresh copied fixture and a
  runner-owned CWD/HOME/config/cache/temp environment. S11 accepts only the
  three code-owned gate IDs above, validates exact API-binding lists and typed
  outcomes, and publishes private JSON with the runner's
  create-new/no-follow atomic writer.

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
python3 -m pytest -q tests
```

Install the benchmark-local dependencies using the environment's Python
package manager and `requirements.txt`.

Freeze the last released identity. It reads only explicit safe artifacts and
separately binds the tested product binary, evidence adapter binary, schema,
workload, corpus, and runtime configuration;
it never derives identity from a live profile:

```console
python3 run_storage_baseline.py freeze \
  --product-binary /path/to/released/tracedecay \
  --evidence-binary /path/to/storage-runtime-evidence \
  --product-commit-sha <released-product-commit> \
  --schema-manifest /path/to/released-schema-export \
  --workload workload-s0.json \
  --corpus /path/to/isolated-store-corpus \
  --config /path/to/released-runtime-config \
  --output frozen-identity-v3.json
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
  --frozen-identity frozen-identity-v3.json \
  --product-binary /path/to/released/tracedecay \
  --evidence-binary /path/to/storage-runtime-evidence \
  --schema-manifest /path/to/released-schema-export \
  --config /path/to/released-runtime-config
```

Validate a result artifact (schema, counts invariants, comparison outcomes,
absolute-path leak scan):

```console
python3 run_storage_baseline.py validate --result <output>/storage-runtime-baseline-result.json
```

Create and execute a frozen soak plan. `soak-run` resolves the plan's fixed
`storage-runtime-s11-product-gates-v1` ID through a code allowlist; arbitrary
executables, shell strings, package installers, and network commands are not
accepted from JSON:

```console
python3 run_storage_baseline.py soak-plan \
  --seed 7 --duration-seconds 3600 \
  --current-rate 1 --ten-x-rate 10 --overload-rate 100 \
  --crash-count 10 --restore-rehearsals 3 \
  --workload-id storage-runtime-s11-product-gates-v1 \
  --output soak-plan.json

python3 run_storage_baseline.py soak-run \
  --plan soak-plan.json \
  --product-binary /path/to/released/tracedecay \
  --evidence-binary /path/to/storage-runtime-evidence \
  --fixture /path/to/explicit-fixture-copy \
  --frozen-identity frozen-identity-v3.json \
  --family graph \
  --output /path/to/fresh/soak-output
```

Both execution and evaluation default to acceptance mode and exit nonzero when
the artifact cannot qualify as evidence. `--mode lint` validates and writes
artifacts while allowing a zero exit for planning/review workflows. Until the
evidence binary implements all three fixed adapter commands and qualifying
copied fixtures are supplied, missing or failed gates remain
`not_run`/`not_evidence` and acceptance exits nonzero:

```console
python3 run_storage_baseline.py soak-evaluate \
  --baseline /abs/linux.json --baseline /abs/windows.json \
  --baseline /abs/macos.json --plan soak-plan.json \
  --result /abs/storage-runtime-soak-result.json \
  --output soak-assessment.json --mode acceptance
```

## Workload schema v1

Top level: `schema_version` (1), `workload_id`, `evidence_eligible`,
`store_families`, `phases`,
plus optional `product_binary`, `evidence_binary`, `frozen_identity`,
`environment.version_commands`,
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
`__RUN_DIR__`, `__FAMILY__`, `__PRODUCT_BINARY__`, `__EVIDENCE_BINARY__`,
`__PYTHON__`, `__REPETITION__`
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
  crash/recovery orchestration, backup/restore comparison, A/A analysis, and
  copied-fixture product-adapter boundary: verified by `self-test` and the
  unittest suite on the development host (Linux, CPython 3.12).
- Still requiring execution before S0 checkpoint acceptance:
  - supplying qualifying copied fixtures for healthy maintenance, a
    ready-signalled crash plus diagnosed repair/quarantine and healthy reopen,
    and online backup/verified restore with a newer canonical publication,
   - capturing `frozen-identity-v3.json` from the distinct product/evidence
     binaries plus schema/workload/corpus/config tuple,
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
