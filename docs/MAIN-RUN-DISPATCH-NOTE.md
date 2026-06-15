# `src/main.rs::run` — Dispatch Map and Refactor Seams

Status: implementation note (no behavior change). Scope: the binary entrypoint
`async fn run(cli: Cli)` at `src/main.rs:251` and its CLI command dispatch, plus
the startup preamble that precedes the `match command`.

All line numbers reference `src/main.rs` (1721 lines) unless a different file is
named. Graph metrics are from `.tracedecay/tracedecay.db`.

---

## 1. Current dispatch structure

```
main()              src/main.rs:218   parse Cli, spawn worker thread (16 MiB stack), join, exit
 └ async_main()     src/main.rs:240   build multi-thread tokio runtime, block_on(run)
    └ run(cli)      src/main.rs:251   THE DISPATCHER — 1256 lines, 204 branches
```

`run` has three phases:

1. **Early dispatch (251-263).** `None` command → `commands::handle_no_command`.
   `ExtractWorker` is handled *before anything else* (no config load, no network
   ping, no agent checks) — only the token handshake authenticates. The later
   `ExtractWorker` arm in the `match` is `unreachable!`.

2. **Startup preamble (265-368).** Runs for every non-skipped command:
   - `should_skip_startup_maintenance` / `should_skip_agent_install_maintenance`
     gate predicates (1509-1576).
   - First-run notice.
   - `global::try_flush` worldwide-counter flush — **synchronous HTTP** (#84),
     skipped on hot paths.
   - `agents::claude::check_install_stale()`.
   - **Silent-reinstall block (316-368)**: compares `previous_version` /
     `last_installed_version` against the running version and re-runs
     `ag.install()` for every tracked agent on a minor/major bump. Mutates and
     saves `UserConfig` on every qualifying invocation.

3. **`match command` (370-1505).** 30+ arms. Each either delegates to a handler
   or inlines the work.

The dispatch is **shape-complete and exhaustive** (clap derives `Commands`); there
are no fallthroughs or `=> {}` no-ops except the intentional `ExtractWorker`
`unreachable!`.

### Existing extraction precedent (the pattern to follow)

| Command(s) | Already delegated to | Where |
|---|---|---|
| `Tool` | `tool_command::run(project, name, args)` | `src/tool_command.rs:74` (own module) |
| `Serve`/`Dashboard` shared init | `serve::ensure_initialized` | `src/serve.rs:22` |
| `Branch`, `Memory`, `Wipe`, `List`, `Gain` | `commands::handle_{branch,memory,wipe,list,gain}` | `src/commands.rs` |
| `(no command)` | `commands::handle_no_command` | `src/commands.rs:495` |
| `Init`/`Sync` shared indexer | `commands::init_and_index`, `print_sync_doctor` | `src/commands.rs:537,703` |
| Startup maintenance | `global::{try_flush,check_for_update,update_global_db,...}` | `src/global.rs` |
| Hooks | `tracedecay::hooks::hook_*` | lib |

So the established seam is: **fat arm → `pub(crate) async fn handle_X(args...)` in
`commands.rs` (or a dedicated module when it's big enough), leaving `run` as a
thin `match`**. `Tool` and `Serve` already outgrew `commands.rs` and got their
own modules; `Cost`, `Install`, and `Status` are the next candidates to do the
same.

---

## 2. Command inventory

D = delegated (thin arm), I = inlined (fat arm, extraction candidate).

| # | Command | Arm lines | D/I | ~LOC | Notes |
|---|---|---|---|---|---|
| 1 | `Init` | 371-405 | I | 34 | indexes (long), spawns `cloud::fetch_latest_version` thread; update-notice block is duplicated with Sync |
| 2 | `Sync` | 406-519 | I | 113 | heaviest indexer path; inline spinner/ETA closure; duplicate update-notice block |
| 3 | `Status` | 520-692 | I | 172 | **largest arm**; network (worldwide counter + country flags), global DB, accounting ingest, interactive stdin prompt |
| 4 | `Tool` | 693-699 | D | 6 | one-liner |
| 5 | `Install` | 700-871 | I | 171 | interactive picker, fan-out over profiles, fs mutation of agent configs |
| 6 | `Reinstall` | 872-913 | I | 41 | fs mutation |
| 7 | `UpdatePlugin` | 914-979 | I | 65 | detection-driven artifact refresh |
| 8 | `Uninstall` | 980-1029 | I | 49 | fs mutation |
| 9 | `ExtractWorker` | 1030-1034 | D | 4 | early-dispatched; `unreachable!` here |
| 10-27 | 18× `HookXxx` | 1035-1139 | D | 2-4 each | thin; most `process::exit(code)` |
| 28 | `Dashboard` | 1140-1149 | D | 9 | binds port, blocks |
| 29 | `Serve` | 1150-1215 | I | 65 | resolution cascade already partly in `serve.rs`; blocks on stdio MCP loop |
| 30 | `Upgrade` | 1216-1218 | D | 2 | `upgrade::run_upgrade` |
| 31 | `Channel` | 1219-1224 | D | 5 | show/switch |
| 32 | `CurrentCounter` | 1225-1230 | D | 5 | |
| 33 | `ResetCounter` | 1231-1237 | D | 6 | resets counter |
| 34 | `DisableUploadCounter` | 1238-1243 | I | 5 | saves global config |
| 35 | `EnableUploadCounter` | 1244-1249 | I | 5 | saves global config |
| 36 | `Gitignore` | 1250-1280 | I | 30 | 4-way branch (on/off/unknown/None); writes project config |
| 37 | `Doctor` | 1281-1283 | D | 2 | `doctor::run_doctor` |
| 38 | `Cost` | 1284-1440 | I | 156 | network pricing refresh; 4-way output format branching |
| 39 | `Bench` | 1441-1478 | I | 37 | runs retrieval benchmark |
| 40 | `Gain` | 1479-1486 | D | 7 | delegated |
| 41 | `Monitor` | 1487-1492 | D | 5 | blocks (live watcher) |
| 42 | `Branch` | 1493-1495 | D | 2 | delegated |
| 43 | `Memory` | 1496-1498 | D | 2 | delegated |
| 44 | `Wipe` | 1499-1501 | D | 2 | delegated |
| 45 | `List` | 1502-1504 | D | 2 | delegated |

Summary: **~14 inlined arms hold ~860 of the ~1256 lines of `run`**; the rest is
preamble + thin delegated arms.

---

## 3. Top risk / complexity hotspots

Ranked by (complexity × blast radius):

1. **`run` itself — 204 branches, max_nesting 7, 19 loops, 12 returns (lines
   250-1506).** Dominant hotspot; the next-largest function in `main.rs` is
   `main` at 5 branches. Untestable as-is: it is `async`, **private**
   (`async fn run`, not `pub`), runs only inside the spawned runtime, and
   interleaves global state mutation with every branch.

2. **`Status` arm (520-692).** Largest single arm. Mixes a runtime-telemetry
   branch, an interactive stdin prompt (non-interactive early-return),
   network calls with caching (worldwide total 60s cache at 588, country flags
   1800s cache at 606), global-DB upsert, accounting ingest, branch-meta load,
   and two display paths (`short` header vs full table). The network-cache
   blocks are subtle and have no tests.

3. **`Install` arm (700-871).** Two top-level branches (`local` vs not) ×
   (`agent` given vs interactive picker) × profile fan-out. Constructs
   `InstallContext` inline 5+ times. `Reinstall`/`UpdatePlugin`/`Uninstall`
   repeat the `home_dir`/`which_tracedecay`/`InstallContext` boilerplate.

4. **`Cost` arm (1284-1440).** Network call (`pricing::refresh_if_stale`) then
   a 4-way output switch (`export=csv` / `export=json` / `by_model` /
   `by_task` / default) with per-format table formatting. The formatting is
   pure but entangled with the global-DB/network setup.

5. **Silent-reinstall block (316-368).** Runs on *every* non-skipped command.
   Version-predicate logic (`transition_needs_reinstall` /
   `external_upgrade_needs_reinstall`) is computed inline and **not unit
   tested**; a bug here causes spurious agent reinstalls or missed reinstalls
   across every command.

6. **`Sync` spinner/ETA closure (438-471).** Progress-callback math (ETA
   extrapolation) is inline and untested.

---

## 4. Proposed handler boundaries (extraction seams)

Each item keeps behavior identical; only the location of the code changes. Apply
the existing "fat arm → `commands::handle_X`" rule, and graduate to a dedicated
module for the three biggest.

### 4a. Immediate, low-risk (move to `commands.rs`, mirror existing handlers)

- **`handle_init(path, skip_folders) -> Result<()>`** ← `Init` arm (371-405).
- **`handle_sync(path, force, skip_folders, doctor, verbose) -> Result<()>`** ←
  `Sync` arm (406-519). Pull the spinner/ETA closure into a helper
  `fn sync_progress_cb(spinner, start) -> impl FnMut(...)`.
- **`handle_gitignore(path, action) -> Result<()>`** ← `Gitignore` arm (1250-1280).
  The 4-way `match action.as_deref()` becomes a pure, unit-testable function.
- **`handle_upload_counter(enable: bool)`** ← merge
  `DisableUploadCounter` + `EnableUploadCounter` (1238-1249) — identical except
  one bool and the message.
- **`handle_bench(...) -> Result<()>`** ← `Bench` arm (1441-1478).

### 4b. Graduate to dedicated modules (mirrors `tool_command.rs` / `serve.rs`)

- **`src/status_cmd.rs`** ← `Status` arm (520-692). Split internally into:
  - `status_runtime_report(cg, json)` (555-563)
  - `status_worldwide(&mut config, now) -> Option<u64>` (588-604) — pure cache
    logic, **unit-testable**
  - `status_country_flags(&mut config, now)` (606-620)
  - `status_render(cg, gdb, stats, ...)` (621-690)
- **`src/agents_cmd.rs`** ← `Install` + `Reinstall` + `UpdatePlugin` + `Uninstall`
  (700-1029). Extract a shared
  `fn build_install_context(home, bin, profile, project_root, dashboard) ->
  InstallContext` to kill the 5+ inline constructions, and
  `fn resolve_install_runtime() -> Result<(home, bin)>`. This group shares the
  most code and is the highest-value single extraction.
- **`src/cost_cmd.rs`** ← `Cost` arm (1284-1440). Extract
  `fn format_cost_output(summary, range, by_model, by_task, export) -> String`
  as a **pure** function (currently ~100 lines of `println!`/formatting) →
  directly unit-testable without the global DB or network.
- **`serve::run_serve(path, timings)`** ← `Serve` arm (1150-1215) into the
  existing `serve.rs`. The resolution cascade (`ensure_initialized` → mcp roots
  → global db) is already there; only the `DISABLE_TRACEDECAY` guard, scope-prefix
  computation, and the peek/transport setup remain in `run`.

### 4c. Extract preamble logic (testability wins)

- **`agents::maybe_silent_reinstall(&mut user_config) -> bool`** ← silent-reinstall
  block (316-368). Split the **pure predicate**
  `fn needs_reinstall(previous, running, last_installed) -> bool` from the
  mutating reinstall loop. Predicate is unit-testable.
- **`maybe_print_update_notice()`** ← deduplicate the Init (387-404) and Sync
  (501-518) blocks, which are byte-for-byte the same throttling logic.

### 4d. Result

After 4a-4c, `run` becomes: early dispatch (4 lines) + preamble calls + a thin
`match` where every arm is 1-3 lines. Target: `run` under ~120 lines and ~20
branches, all of them dispatch.

---

## 5. Test safety — which branches can / cannot be exercised

### Architectural constraint (read first)

`run`, `commands`, `serve`, `tool_command`, and `global` are declared `mod x;` in
`src/main.rs` → **binary-crate-private**. Their `pub(crate)` handlers are
**unreachable from `tests/`** (a separate integration crate). Today, unit tests
can only live in an in-binary `#[cfg(test)] mod` (the only one today is
`mod startup_tests` at `main.rs:1583`). Integration tests (`tests/*.rs`) exercise
the CLI by **spawning the binary** (`Command::new`, 15 files) or by calling
**lib** functions directly (`tracedecay::...`, e.g. `TraceDecay::open`, 19 files).

Consequence: to make an extracted handler unit-testable from `tests/`, either
(a) keep it `pub(crate)` and add an in-crate `#[cfg(test)]` module, or
(b) move the **pure** portion into the `tracedecay` lib (where `cloud::*`,
`pricing::*`, `metrics::*` already live and are `pub`).

### Safe to exercise now (pure / no side effects)

- `should_skip_startup_maintenance` + `should_skip_agent_install_maintenance`
  + `is_local_install_command` — **already** covered by `mod startup_tests`
  (`main.rs:1583-1713`). Use these as the template.
- `validate_hermes_profile_flags`, `validate_hermes_project_root_flag`,
  `hermes_selected_profile_targets`, `hermes_profile_targets`
  (`main.rs:104-208`) — pure input validation, **currently untested**. Easy wins
  after they stay where they are or move to `commands.rs` with an in-crate test
  module.
- `cloud::is_newer_version` / `is_newer_minor_version` (`src/cloud.rs:263,302`) —
  `pub` in lib, already reachable; the silent-reinstall predicate (4c) should be
  built on them and tested there.
- `accounting::metrics::parse_range` (`metrics.rs:80`) — pub in lib, testable.
- Future `format_cost_output` (4b) and the `gitignore` action matcher (4a) —
  pure; testable once extracted.

### Do NOT invoke these commands in tests (and why)

| Command | Reason not to invoke directly |
|---|---|
| `Serve` | Blocks indefinitely on stdio JSON-RPC loop; needs an MCP client harness (see `tests/mcp_cli_serve_test.rs` for the spawn-and-pipe pattern) |
| `Dashboard` | Binds a TCP port and blocks |
| `Monitor` | Blocks (live watcher) |
| `Upgrade` | Downloads a release and **replaces the running binary** (network + self-mutation) |
| `Channel { Some }` | `switch_channel` — same self-replacement + network |
| `ExtractWorker` | IPC handshake with parent process; runs an indexing worker loop; early-dispatched before any config load |
| `Init`, `Sync` | Heavy multi-language indexing; long runtime; write a full DB to disk |
| `Bench` | Requires an initialized index and runs queries (medium runtime) |
| `Install`, `Reinstall`, `UpdatePlugin`, `Uninstall` | Filesystem mutation of agent configs in the user's real home; `Install` also opens an **interactive** TTY picker |
| `Status` (default path) | Network (`fetch_worldwide_total`, `fetch_country_flags`), global-DB upsert, and an interactive stdin prompt when uninitialized |
| `Cost` | Network (`pricing::refresh_if_stale` LiteLLM fetch) + global-DB ingest |
| `DisableUploadCounter` / `EnableUploadCounter` / `Gitignore { on/off }` | Write global/project config to the real home/cwd |
| `ResetCounter` | Mutates the project-local counter DB |
| `Tool` | Dispatches an MCP tool; per-tool side effects vary (already has its own test suite in `tool_command.rs`-adjacent tests) |
| Hooks (`HookXxx`) | Read agent env / may trigger a sync; several `process::exit(code)`, which aborts the test process |

### Patterns for safely testing the "unsafe" commands

Focused characterization coverage should stay on parse + guard seams for
representative families (for example `tool`, `status`, `install`, `branch`, and
Hermes-only validation helpers), then rely on the existing spawn-the-binary
integration harnesses for arms that block, mutate the user's real home/cwd, or
replace the running binary.

- **Argument validation / gating** (`validate_hermes_*`, the `should_skip_*`
  predicates) — test the pure predicate, never the command.
- **Output formatting** (`Cost`, `Status` table, `Bench`) — extract the formatter
  to a pure fn, feed it a fixture struct, assert on the string.
- **Network cache decisions** (Status worldwide/flags) — extract the cache-math
  predicate, test it with synthetic timestamps.
- **Indexer/serve paths** that must run end-to-end — already covered by the
  spawn-the-binary integration tests (`tests/*`); keep that boundary rather than
  trying to call `run` in-process.

---

## 6. Recommended sequencing

1. Extract pure helpers + add in-crate unit tests (4c, and the 4 untested
   validators) — zero dispatch risk, immediately raises coverage.
2. Move the small config arms (4a) — mechanical, shrinks `run` ~200 lines.
3. Graduate `Cost` → `cost_cmd.rs` (smallest of the big three, biggest pure-fn
   payoff).
4. Graduate `Status` → `status_cmd.rs` (largest arm; do after the pure-helper
   extractions so the cache predicates already exist).
5. Graduate the agent group → `agents_cmd.rs` (highest shared-code payoff;
   touches user-facing install behavior, so pair with the existing agent tests).

Each step is independently shippable and leaves behavior identical.
