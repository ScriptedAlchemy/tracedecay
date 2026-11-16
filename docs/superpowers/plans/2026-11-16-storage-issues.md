# L2 Storage Issues Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver #4876, #4513, #4152, #4616, and #4099, in that order, as focused storage-correctness and retention slices. Each slice is reproduced first on isolated fixtures through its production route and closed only by cited, non-vacuous evidence. #4099's real-profile 24-hour acceptance is drafted here and executed later, only after the user approves it separately.

**Architecture:** Five PRs, one per issue. Each starts from the current `origin/master` in the L2 lane worktree.

- **#4876** narrows repository-identity candidates in the profile registry, so a shard registered for a sibling linked worktree no longer claims a new checkout. It also makes the remaining genuine conflicts name their remedy in the tool error and in the hook status line.
- **#4513** pins safety for an external reader of a live daemon's `sessions.db` through a real daemon journey, then reproduces the reported `sqlite3` CLI workflow on an isolated profile.
- **#4152** removes the last connection-local TEMP tables from projection-rebuild activation and moves the unlinked-temp census behind one process-descriptor authority that also works on macOS.
- **#4616** pins sealed-generation collection through the daemon, isolates an unsafe text artifact from the rest of recovery, gives busy-store retention bounded progress, names the blocking refusal in Doctor, and proves the storage report's memory does not scale with generation count.
- **#4099** moves profile-wide hook-analytics rotation onto the daemon retention tick, adds the ingest-growth regression check, retires the footprint family for the store L1 deletes, and drafts the real-profile runbook.

**Tech Stack:** Rust 2024 through Bazel only. SQLite via rusqlite (bundled) and the `tracedecay-rusqlite-runtime` writer/reader authority. `tracing` spans with `TRACEDECAY_SPAN_TIMINGS=1`. `libc` (`proc_pidinfo`/`proc_pidfdinfo` on macOS, `/proc/self/fd` on Linux). Git linked-worktree fixtures.

**Spec:** `docs/superpowers/specs/2026-11-16-outstanding-issues-delivery-design.md`

## Global Constraints

- **Bazel only.** Every build, test, check, run, and measurement goes through Bazel (`bazel test //<target> --test_output=errors`, `bazel run //crates/tracedecay-cli:tracedecay -- …`). Never invoke `cargo`. This plan adds no Rust dependency. If one becomes necessary, escalate before running `CARGO_BAZEL_REPIN=workspace bazel fetch --repo=@c` and `bazel run //scripts:gen_builds`.
- **Non-zero evidence.** Every name-filtered run goes through `scripts/require-exact-test.sh` with the full module path. Quote the reported executed count in the lane report. A run that reports `0 passed` is a failure, not evidence. Bazel prints a passing test's log only with `--test_output=all`, so helper runs and any run whose counts are cited use that flag.
- **Reproduce first.** Each defect task runs its new test, or its measured reproduction, red on the unmodified tip before any production edit. If the reproduction does not fail, stop that slice and report `needs-decision` to the coordinator with the attempted conditions. Do not write a speculative fix.
- **No compatibility, no backups.** A shape change migrates every caller and deletes the old one in the same slice. No aliases, dual writes, migrations, or secondary copies of project, profile, or operator data.
- **Released stores.** The profile registry (`global.db`) is a released relational store. #4876 admits and converges its existing rows under the 2026-09-16 admission exception: rows are narrowed at resolution time and never rewritten, deleted, or reset. No other task changes a persisted shape.
- **Typed production failures.** No `unwrap`, `expect`, `panic`, silent fallback, empty success, or swallowed error in production code. Tests may assert and unwrap.
- **Retention safety.** No retention change may remove a generation a live reader holds, a durable descriptor target, or a foreign-owned entry. All collection runs through the daemon maintenance path.
- **Limits.** Task 9 introduces one new bound, `GENERATION_STORE_BUSY_DEFERRALS_BEFORE_WAIT = 3`. It is the behavior of Task 9's single commit, whose body carries the measured deferral counts, and the coordinator reviews it as a bound change. No existing timeout, deadline, or budget is raised.
- **Generated sources.** No task here changes a schemars schema or the operation registry. Each PR runs `pnpm run contracts:check` from `dashboard/` as verification. If it reports drift, a step changed a contract and must stop and regenerate with `pnpm run contracts:generate` in its own step.
- **Isolated state.** Every daemon, profile, home, and project lives under a lane-owned temporary directory (`IsolatedHome` in tests, an explicit `HOME` and profile root for measured runs). Never read or write `~/.tracedecay`, `~/.claude`, `~/.codex`, `~/.agents`, or installed services. The installed TraceDecay daemon is out of scope and must not be touched.
- **Commits.** `<type>(<scope>): <subject>`, header ≤ 72 characters, commitlint-clean, no issue numbers. Issue numbers appear only in PR bodies, as `Fixes #N` when that PR alone meets every criterion and `Refs #N` otherwise.
- **Heavy runs.** Tasks 3 (Step 5), 6, 11, and 13 are heavy or measured. Take a coordinator slot first. Rerun any timing-sensitive failure in isolation before treating it as a regression.
- **Cross-lane gates.**
  - **Start gate:** the planning PR has merged and the coordinator has created the L2 worktree.
  - **#4876 → L3 #4861 and L5 #4419:** #4876 must merge before L3's #4861 inventory slice and L5's #4419 can integrate. Report the merge SHA to the coordinator immediately.
  - **#4616 → L3 #4533:** #4616 must merge before L3's #4533 can integrate.
  - **#4099 integration gate:** the #4099 PR integrates only after L1's #4448 and #4427 have merged. Task 14 starts only after #4448 merges.
  - **No cross-lane edits:** no task edits L3's code-index format, segment layout, or session admission; L5's SDK sources; or L1's registered-store schema.

## Review Focus

1. **A worktree-registered shard that is the repository's only authority.** Narrowing sibling-worktree candidates must never strand a repository whose first enrollment came from a linked worktree. A new worktree must still resolve to that sole authority rather than minting a second project. Pinned by `sole_worktree_registered_authority_still_claims_a_new_worktree` (Task 1, Step 1).
2. **Retention under a live reader or a racing publication.** Collection must leave the generation a reader holds and every durable descriptor target in place. A text artifact that became live after its retention receipt was written must be restored, not deleted, and must not block the other candidates. Pinned by `reader_held_previous_generation_survives_scheduled_retention` (Task 7, Step 1) and `committed_receipt_with_relivened_artifact_restores_it_and_finishes_recovery` (Task 8, Step 1).
3. **A retention pass waiting on a busy store when the daemon shuts down.** The new bounded wait must observe cancellation and its deadline, so it can neither hang shutdown nor hold the maintenance owner. Pinned by `busy_store_wait_observes_shutdown_cancellation` (Task 9, Step 1).
4. **An external SQLite client that is last to close a WAL store across a daemon restart.** A default-mode (read-write-capable) reader that closes after the daemon stops may checkpoint or remove `-wal`/`-shm`. The restarted daemon must then admit the store intact or refuse it with a typed error, never report `disk I/O error` or `file is not a database`. Pinned by `external_default_mode_reader_across_daemon_restart_preserves_sessions` (Task 3, Step 1).
5. **Projection-rebuild activation rewritten without TEMP sets.** The rewritten deletes decide which `lcm_raw_messages` rows survive a rebuild. A wrong correlation silently deletes retained transcript rows. Pinned by `rebuild_activation_clears_exactly_the_unretained_outputs` (Task 4, Step 1).

---

### Task 1: #4876 Sibling worktree shards do not claim a new checkout

**Files:**
- Create: `crates/tracedecay/tests/storage_suite/sibling_worktree_shard_resolution_test.rs`
- Modify: `crates/tracedecay/tests/storage_suite/main.rs` (add `mod sibling_worktree_shard_resolution_test;`)
- Modify: `crates/tracedecay-global-db/src/project_registry.rs`

**Interfaces:**
- Consumes: `RegisteredGlobalDb::upsert_code_project(&str, &Path, Option<&Path>, Option<&str>, Option<&str>) -> Result<CodeProjectRecord>`.
- Consumes: `RegisteredGlobalDb::resolve_project_store_by_identity(&Path, Option<&Path>) -> Result<Option<ProjectStoreResolution>>`.
- Consumes: `tracedecay_runtime_core::worktree::{git_common_dir, is_linked_worktree}`.
- Consumes: `StoreInstanceUpsert` and the `git`/`repo_with_worktrees` fixture pattern from `project_identity_collapse_test.rs`.
- Produces (private, `project_registry.rs`): `async fn registered_canonical_root(executor, project_id: &str) -> Result<Option<PathBuf>>`, generic over the same executor bound that `alias_project_id` accepts, so both the write transaction and the read snapshot use it.
- Produces (private): `async fn narrow_sibling_worktree_authorities(executor, candidates: BTreeSet<String>, from_common_dir: &BTreeSet<String>, requested_root: &Path) -> Result<BTreeSet<String>>`. It drops a candidate only if all of the following hold:
  - it was contributed solely by the `git_common_dir` match;
  - its registered canonical root is an existing linked worktree (`is_linked_worktree`) different from `requested_root`;
  - at least one candidate that does not meet the first two conditions remains.

- [ ] **Step 1: Write the released-shape reproduction and the sole-authority guard**

Build a primary checkout `main/` with three linked worktrees, `wt1/`, `wt2/`, and `wt3/` (detached). Run `tracedecay init` in `main/` under an `IsolatedHome`, recording `proj_a`. Stop the daemon. Open `<profile>/global.db` with `rusqlite` and seed the released 0.0.74 shape. For each of `proj_b` and `proj_c`:
- a `code_projects` row with `canonical_root` set to `wt1`/`wt2` and `git_common_dir` set to `main/.git`;
- a `ProjectRoot` alias for `wt1`/`wt2`;
- a store instance written through `StoreInstanceUpsert`.

Write `new_worktree_with_two_sibling_worktree_shards_resolves_to_the_primary_store`. It runs `tracedecay tool active_project --args '{}'` with `cwd = wt3` and asserts:
- exit 0;
- the reported project id is `proj_a`;
- `projects search` still lists exactly `proj_a`, `proj_b`, and `proj_c` (no row rewritten or deleted).

Write `sole_worktree_registered_authority_still_claims_a_new_worktree`. Here only `proj_b` is registered, with root `wt1`. Calling `upsert_code_project` for `wt3` must return `proj_b`, and no new project row may appear.

- [ ] **Step 2: Run both and observe the reproduction RED**

Run: `scripts/require-exact-test.sh bazel test //crates/tracedecay:storage_suite --test_output=all --test_arg=sibling_worktree_shard_resolution_test::new_worktree_with_two_sibling_worktree_shards_resolves_to_the_primary_store --test_arg=--exact`

Expected: `1 failed`. The tool exits nonzero with a `ResetRequired`/`conflicting project authorities [proj_a, proj_b, proj_c]` or `project_identity_conflict` error. Record which one.

Run the same command for `sibling_worktree_shard_resolution_test::sole_worktree_registered_authority_still_claims_a_new_worktree`. Expected: `1 passed`; it guards against over-narrowing. If the first test passes, stop and report #4876 as not reproduced on `master` (`needs-decision`).

- [ ] **Step 3: Narrow common-dir candidates at both resolution sites**

In `project_registry.rs`, add `registered_canonical_root` and `narrow_sibling_worktree_authorities`. Then:
- In `upsert_code_project`, record which ids came from the `SELECT project_id FROM code_projects WHERE git_common_dir = ?1` query. Apply the narrowing before the `existing_authorities.len() > 1` check.
- In `project_ids_by_identity`, record the id from the `GitCommonDir` alias and apply the same narrowing before returning.

Leave every row untouched; narrowing is read-time admission only.

- [ ] **Step 4: Run GREEN and the existing identity suites**

Run both exact commands from Step 2. Expected: each reports `1 passed`.

Run: `bazel test //crates/tracedecay:storage_suite //crates/tracedecay-global-db:unit_test --test_output=errors`. Expected: PASS, including `project_identity_collapse_test` and `worktree_canonical_root_guard_test`.

- [ ] **Step 5: Commit**

```bash
git add crates/tracedecay-global-db/src/project_registry.rs crates/tracedecay/tests/storage_suite/sibling_worktree_shard_resolution_test.rs crates/tracedecay/tests/storage_suite/main.rs
git commit -m "fix(registry): keep sibling worktree shards from claiming new checkouts"
```

### Task 2: #4876 Genuine conflicts name their remedy in the tool error and hook status

**Files:**
- Modify: `crates/tracedecay-global-db/src/project_registry.rs` (conflict messages)
- Modify: `crates/tracedecay-agent-hosts/src/ports/hook_runtime.rs`
- Modify: `crates/tracedecay-agent-hosts/src/hooks/steering.rs`
- Modify: `crates/tracedecay-agent-hosts/src/hooks/mod.rs`
- Modify: `crates/tracedecay-agent-hosts/src/hooks/cursor.rs`
- Modify: `crates/tracedecay-agent-hosts/src/hooks/codex.rs`
- Modify: `crates/tracedecay-project/src/runtime_ports.rs`
- Modify: `crates/tracedecay-agent-hosts/tests/hooks_boundary.rs`
- Test: `crates/tracedecay/tests/storage_suite/sibling_worktree_shard_resolution_test.rs`
- Test: `crates/tracedecay/tests/hooks_lsp_suite/hooks_test.rs`

**Interfaces:**
- Produces: `pub enum ProjectRootResolutionV1 { Resolved(PathBuf), Unregistered, Conflict { project_ids: Vec<String> } }` in `ports/hook_runtime.rs`. `ProjectRootResolver` now returns a future of `ProjectRootResolutionV1` instead of `Option<PathBuf>`.
- Produces: `HookWorkspaceStatus::RegistryConflict` with key `"registry_conflict"`, and `fn registry_conflict_status_line(project_ids: &[String]) -> String` in `steering.rs`.
- Produces: conflict messages that end with `inspect with \`tracedecay --dry-run projects forget <id>\` for each id: <ids>`. This covers both the `upsert_code_project` `ResetRequired` and the `project_identity_conflict` route error.
- Consumes: `resolve_project_root_with_identity` in `tracedecay-project/src/runtime_ports.rs` (now maps the registry's `project_identity_conflict` route error to `Conflict`).

- [ ] **Step 1: Write the two-primary conflict tests**

In `sibling_worktree_shard_resolution_test.rs`, add `two_primary_claimants_report_the_forget_remedy`. Seed `proj_a` plus `proj_d`, both with the canonical root `main/`, and run `tracedecay tool active_project` from `wt3`. Assert:
- exit nonzero;
- stderr names both ids;
- stderr contains `tracedecay --dry-run projects forget`.

In `hooks_test.rs`, add `codex_prompt_in_conflicting_checkout_names_registry_conflict`. It drives the Codex user-prompt hook with `cwd = wt3` against the same profile. Assert the context contains `registry_conflict`-status text naming both ids and the forget command, and does not contain `run \`tracedecay init\``.

- [ ] **Step 2: Run RED**

Run: `scripts/require-exact-test.sh bazel test //crates/tracedecay:storage_suite --test_output=all --test_arg=sibling_worktree_shard_resolution_test::two_primary_claimants_report_the_forget_remedy --test_arg=--exact`

Expected: `1 failed`, because the error lacks the forget command.

Run: `scripts/require-exact-test.sh bazel test //crates/tracedecay:hooks_lsp_suite --test_output=all --test_arg=hooks_test::codex_prompt_in_conflicting_checkout_names_registry_conflict --test_arg=--exact`

Expected: `1 failed`, because the context contains the `UNINDEXED_STATUS_LINE` init advice.

- [ ] **Step 3: Implement typed conflict resolution for hooks**

1. Change `ProjectRootResolver` to return `ProjectRootResolutionV1`.
2. Map `TraceDecayError::ProjectRoute` with reason `project_identity_conflict` (and the registry `ResetRequired` conflict) to `Conflict { project_ids }` in `resolve_project_root_with_identity`. Map every other error to the existing typed failure path, never to `Unregistered`.
3. Migrate every call site in `hooks/mod.rs`, `hooks/cursor.rs`, `hooks/codex.rs`, and `tests/hooks_boundary.rs`. Delete the `Option` form.
4. `codex_workspace_status` returns `RegistryConflict` for a conflict, and the prompt context emits `registry_conflict_status_line`.
5. Append the forget remedy to both registry conflict messages.

- [ ] **Step 4: Run GREEN and the hook suites**

Run both exact commands from Step 2. Expected: `1 passed` each.

Run: `bazel test //crates/tracedecay:hooks_lsp_suite //crates/tracedecay-agent-hosts:unit_test //crates/tracedecay-agent-hosts:hooks_boundary //crates/tracedecay:storage_suite --test_output=errors`

Expected: PASS.

Then run `pnpm run contracts:check` in `dashboard/`. Expected: no drift.

- [ ] **Step 5: Commit, open the PR, and report the gate**

```bash
git add crates/tracedecay-global-db/src/project_registry.rs crates/tracedecay-agent-hosts crates/tracedecay-project/src/runtime_ports.rs crates/tracedecay/tests/storage_suite/sibling_worktree_shard_resolution_test.rs crates/tracedecay/tests/hooks_lsp_suite/hooks_test.rs
git commit -m "fix(hooks): name registry conflicts and their remedy in session hints"
```

Open one PR for Tasks 1–2 with `Fixes #4876`. Its criterion map covers four points:
- resolution narrowed by each shard's registered root;
- the sole-authority guard;
- the remedy named in both the error and the hint;
- the issue's "Also" notes. `migrate consolidate` and `migrate registry-gc` do not exist on `master`. Cite the built CLI refusing them (`bazel run //crates/tracedecay-cli:tracedecay -- migrate --help` exits nonzero on an unrecognized subcommand under an isolated `HOME`), not a source search, and name the `RegistryGc` admin action as the current surface.

The real multi-worktree check on the operator's host is a named operator step in the lane report, not run by the lane. After merge, notify the coordinator that the #4876 gate for L3 #4861 and L5 #4419 is open.

### Task 3: #4513 External readers of a live daemon `sessions.db`

**Files:**
- Create: `crates/tracedecay/tests/daemon_suite/external_sqlite_reader_test.rs`
- Modify: `crates/tracedecay/tests/daemon_suite/main.rs` (add `mod external_sqlite_reader_test;`)

**Interfaces:**
- Consumes: `IsolatedHome`, `spawn_tracedecay_daemon_logged`, `daemon_socket_path`, and `poll_until` from `tests/common`.
- Consumes: `initialize_tracedecay`, `commit_all`, `git`, and `stop_daemon_gracefully` from `daemon_suite/code_index_journey.rs`.
- Consumes: the `project_stores(home, project) -> (graph_db, sessions_db)` layout pattern from `store_shutdown_checkpoint_test.rs`, and that file's session-ingest driver for daemon writes.
- Produces: test fn `external_sqlite_reader_test::external_sqlite_child_probe`, which returns immediately unless `TRACEDECAY_TEST_EXTERNAL_SQLITE_CHILD=readonly_uri|default_open` and `TRACEDECAY_TEST_EXTERNAL_SQLITE_PATH=<sessions.db>` are set. The parent re-executes `std::env::current_exe()` with `--exact external_sqlite_reader_test::external_sqlite_child_probe`, the same pattern `daemon_suite/main.rs` uses for its missing-CLI child. The child opens the store with the bundled rusqlite, which is the same SQLite build the daemon links. It holds a read transaction across `SELECT count(*) FROM sqlite_master` and a full scan of the largest table for `TRACEDECAY_TEST_EXTERNAL_SQLITE_HOLD_MS`, prints `rows:<n>`, and exits 0.

- [ ] **Step 1: Write the live-reader and restart journeys**

`external_readonly_reader_during_ingest_never_breaks_the_daemon`:
- start the daemon and drive the session ingest;
- while the ingest runs, launch the `readonly_uri` child three times, each holding for 500 ms;
- keep ingesting after each child exits.

Assert:
- every child prints a non-zero `rows:`;
- the daemon log contains neither `disk I/O error` nor `file is not a database`;
- a final ingest write succeeds;
- after a graceful stop, `PRAGMA integrity_check` on `sessions.db` returns `ok`.

`external_default_mode_reader_across_daemon_restart_preserves_sessions`:
- launch the `default_open` child with a 3 s hold;
- stop the daemon gracefully while the child still holds, so the child is the last connection to close;
- wait for the child to exit, then restart the daemon and drive one more ingest.

Assert:
- the restarted daemon admits the store, or refuses it with a typed `ResetRequired` that names `sessions.db`;
- no `disk I/O error` or `file is not a database` appears;
- `integrity_check` returns `ok`;
- the pre-stop message count is preserved.

- [ ] **Step 2: Run both tests (reproduce first)**

Run: `scripts/require-exact-test.sh bazel test //crates/tracedecay:daemon_suite --test_output=all --test_arg=external_sqlite_reader_test::external_readonly_reader_during_ingest_never_breaks_the_daemon --test_arg=--exact`

Then run the same command for `external_sqlite_reader_test::external_default_mode_reader_across_daemon_restart_preserves_sessions`.

Record each outcome and count. This is a darwin host, so these are native macOS Bazel results; record the platform. A pass is the regression pin for the merged #4803/#4805 fixes. A failure is the reproduction: capture the daemon log and the failing pragma, then go to Step 4.

- [ ] **Step 3: Reproduce the reported workflow with the real `sqlite3` CLI**

Take a coordinator slot. Run `bazel build //crates/tracedecay-cli:tracedecay`. With an isolated `HOME`, use the built binary to init a fixture repository and start `daemon run` in the foreground. Drive session ingest. While the daemon writes, run `/usr/bin/sqlite3 -readonly <profile>/projects/<id>/sessions.db 'select count(*) from sqlite_master;'` ten times, then `/usr/bin/sqlite3 <same path> '.tables'` (default mode) ten times. Record:
- `sqlite3 --version`;
- the daemon log lines matching `disk I/O|not a database|ResetRequired`;
- `PRAGMA integrity_check` after a graceful stop.

Remove the isolated state afterwards.

- [ ] **Step 4: Disposition**

If Steps 2 and 3 are clean, commit the tests:

```bash
git add crates/tracedecay/tests/daemon_suite/external_sqlite_reader_test.rs crates/tracedecay/tests/daemon_suite/main.rs
git commit -m "test(sqlite): cover external readers of live daemon session stores"
```

Open a PR with `Refs #4513`, carrying the criterion map and the Step 3 transcript summary. The coordinator decides closure, because the issue's "identify which setting allows it" may be answered by the merged #4803/#4805.

If either step reproduces corruption or `disk I/O error`, do not commit a fix in this task. Report `needs-decision` with:
- the failing mode;
- the SQLite version;
- the setting implicated (from the log, `PRAGMA journal_mode`, `wal_autocheckpoint`, `journal_size_limit`, and sidecar identity checks);
- the red test.

The coordinator then amends this plan with a fix task.

### Task 4: #4152 Projection-rebuild activation without connection TEMP tables

**Files:**
- Create: `crates/tracedecay-global-db/src/observation_projection/rebuild/temp_ownership_tests.rs`
- Modify: `crates/tracedecay-global-db/src/observation_projection/rebuild.rs` (`clear_active_projection`; add `#[cfg(test)] mod temp_ownership_tests;`)

**Interfaces:**
- Consumes: `clear_active_projection(conn: &impl Executor, generation: &str) -> ProjectionStoreResult<()>`. Its signature is unchanged; it is called from the rebuild activation at `rebuild.rs` `clear_active_projection(transaction, &job.generation)`.
- Consumes: indexes `idx_observation_projection_provenance_output` and `idx_observation_projection_provenance_global_output` from `observation_projection/schema.rs`.
- Produces: no new symbols. The two `temp.observation_projection_rebuild_*_outputs` tables are deleted.

- [ ] **Step 1: Write the activation-semantics and TEMP-residue tests**

`rebuild_activation_clears_exactly_the_unretained_outputs` seeds, through the registered schema on an on-disk store opened with `PRAGMA temp_store = FILE`:
- 2,000 current-version provenance rows with `message_created = 1`;
- 300 of them also carrying an older projector version (retained);
- 200 staged in `observation_projection_rebuild_messages` for the job generation;
- matching `lcm_raw_messages` rows.

It runs `clear_active_projection` in a write transaction and commits. It then asserts that the set of surviving `lcm_raw_messages` `(provider, message_id)` pairs equals an independently computed expected set (all rows minus created, unretained, unstaged ones), and that the current-version provenance rows are exactly the retained ones.

`rebuild_activation_leaves_no_temp_schema_on_the_writer_connection` runs the same activation, then asserts on the same connection that `SELECT count(*) FROM temp.sqlite_master` returns `0`.

- [ ] **Step 2: Run RED**

Run: `scripts/require-exact-test.sh bazel test //crates/tracedecay-global-db:unit_test --test_output=all --test_arg=observation_projection::rebuild::temp_ownership_tests::rebuild_activation_leaves_no_temp_schema_on_the_writer_connection --test_arg=--exact`

Expected: `1 failed` with `temp.sqlite_master` count `2`.

Run the same command for `observation_projection::rebuild::temp_ownership_tests::rebuild_activation_clears_exactly_the_unretained_outputs`. Expected: `1 passed`; it pins current semantics before the rewrite.

- [ ] **Step 3: Rewrite activation with correlated index-backed subqueries**

Delete the `CREATE TEMP TABLE`/`INSERT INTO temp.*` statements. Express "retained" as the `EXISTS` over `observation_projection_provenance` with a different `projector_version`, and "cleared" as the created, unretained, unstaged predicate, directly inside the `lcm_raw_messages` and provenance `DELETE`s.

Before committing, confirm on the test store that `EXPLAIN QUERY PLAN` for each `DELETE` shows `SEARCH … USING INDEX idx_observation_projection_provenance_*` and no `USE TEMP B-TREE`. Record the plan text in the commit body.

- [ ] **Step 4: Run GREEN and the projection suite**

Run both exact commands from Step 2. Expected: `1 passed` each.

Run: `bazel test //crates/tracedecay-global-db:unit_test //crates/tracedecay:session_suite --test_output=errors`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/tracedecay-global-db/src/observation_projection/rebuild.rs crates/tracedecay-global-db/src/observation_projection/rebuild/temp_ownership_tests.rs
git commit -m "fix(projection): activate rebuilds without connection TEMP tables"
```

### Task 5: #4152 Unlinked SQLite temp census on Linux and macOS

**Files:**
- Create: `crates/tracedecay-runtime-core/src/process_fds.rs`
- Modify: `crates/tracedecay-runtime-core/src/lib.rs` (`pub mod process_fds;`)
- Modify: `crates/tracedecay-maintenance/src/retention/footprint.rs` (`census_sqlite_temp_spill`)
- Modify: `crates/tracedecay-daemon-service/src/doctor_kernel.rs` (`profile_footprint_read`)
- Modify: `crates/tracedecay-cli/tests/c_library_allocator.rs` (caller migration)

**Interfaces:**
- Produces: `pub struct UnlinkedOpenFileV1 { pub file_name: String, pub bytes: u64, pub identity: (u64, u64) }` and `pub enum ProcessFdCensusErrorV1 { Unsupported, Unreadable(String) }`.
- Produces: `pub fn unlinked_open_files_with_prefix(prefix: &str) -> Result<Vec<UnlinkedOpenFileV1>, ProcessFdCensusErrorV1>`:
  - Linux reads `/proc/self/fd` targets ending in ` (deleted)`.
  - macOS enumerates `PROC_PIDLISTFDS` and, for each vnode, reads `PROC_PIDFDVNODEPATHINFO`, keeping entries with `vst_nlink == 0`.
  - Other targets return `Unsupported`.
  - Results are deduplicated by `(dev, ino)`.
- Changes: `census_sqlite_temp_spill(observed: &[UnlinkedOpenFileV1], bound_overrides: &BTreeMap<String, u64>) -> Option<ProfileFootprintEntryV1>`. The `fd_directory: &Path` form is deleted with all its callers.
- Changes: `profile_footprint_read` calls `unlinked_open_files_with_prefix("etilqs_")`. `Unsupported` contributes no entry. `Unreadable` contributes the existing unavailable-entry count, never a zero-byte entry.

- [ ] **Step 1: Write the platform census test**

In `process_fds.rs` `mod tests`, write `open_unlinked_sqlite_temp_file_is_counted_once`:
- create `etilqs_census_probe` in a temp dir and write 3 MiB;
- open it twice (two descriptors), then unlink it;
- call `unlinked_open_files_with_prefix("etilqs_")`.

Assert exactly one entry with `bytes >= 3 * 1024 * 1024`. After both descriptors drop, assert no entry remains.

- [ ] **Step 2: Run RED**

Run: `scripts/require-exact-test.sh bazel test //crates/tracedecay-runtime-core:unit_test --test_output=all --test_arg=process_fds::tests::open_unlinked_sqlite_temp_file_is_counted_once --test_arg=--exact`

Expected: build failure (`process_fds` does not exist), which counts as red. Record that on this darwin host the existing census returns `None`.

- [ ] **Step 3: Implement `process_fds` and migrate the census**

1. Implement the Linux and macOS readers with `libc`. Each `unsafe` block carries a `// SAFETY:` invariant naming the buffer length contract.
2. Change `census_sqlite_temp_spill` to take observed files and migrate its tests in `footprint.rs`. The fake `/proc` fixtures become `UnlinkedOpenFileV1` vectors, so the existing over-bound and dedup assertions are kept.
3. Migrate `doctor_kernel.rs` and `c_library_allocator.rs`.
4. Delete the `fd_directory` path form.

- [ ] **Step 4: Run GREEN and the affected targets**

Run the Step 2 command. Expected: `1 passed`.

Run: `bazel test //crates/tracedecay-runtime-core:unit_test //crates/tracedecay-maintenance:unit_test //crates/tracedecay-daemon-service:unit_test //crates/tracedecay-cli:c_library_allocator --test_output=errors`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/tracedecay-runtime-core/src/process_fds.rs crates/tracedecay-runtime-core/src/lib.rs crates/tracedecay-maintenance/src/retention/footprint.rs crates/tracedecay-daemon-service/src/doctor_kernel.rs crates/tracedecay-cli/tests/c_library_allocator.rs
git commit -m "feat(storage): census unlinked SQLite temp files on macOS"
```

### Task 6: #4152 Page-corruption causality reproduction and PR

**Files:**
- None (measured reproduction and report only)

**Interfaces:**
- Consumes: `//crates/tracedecay-cli:c_library_allocator`, `//crates/tracedecay-cli:process_allocator_heaps`, and Task 4's `temp_ownership_tests`.

- [ ] **Step 1: Run the allocator and rebuild stress under the shipped allocator**

Take a coordinator slot. Run:

`bazel test //crates/tracedecay-cli:c_library_allocator //crates/tracedecay-cli:process_allocator_heaps --runs_per_test=20 --test_output=errors`

Then run:

`bazel test //crates/tracedecay-global-db:unit_test --test_filter=observation_projection::rebuild::temp_ownership_tests --runs_per_test=50 --test_output=all`

Expected: every run passes, and each run's log reports `2 passed`; a run reporting `0 passed` invalidates the evidence. Record run counts, platform, and any `integrity_check` or segfault output.

- [ ] **Step 2: Open the #4152 PR and record the disposition**

Open one PR for Tasks 4–5 with `Refs #4152`. Its criterion map has two items:
- **Item 2, large unlinked temp files:** spill sources are the #4853 merged carry repairs, Task 4 removes the remaining connection-local TEMP tables, and Task 5 adds the macOS census. Doctor accounting covers this as `sqlite_temp_spill`.
- **Item 1, memory corruption:** not reproduced under the Step 1 conditions, with the mimalloc owner-heap removal (#4173) cited.

Report #4152 as `needs-decision`. The coordinator chooses whether item 1 stays open, or closes as not reproducible with these conditions. Never close it as completed.

### Task 7: #4616 Sealed generations collected through the daemon

**Files:**
- Create: `crates/tracedecay/tests/daemon_suite/sealed_generation_retention_test.rs`
- Modify: `crates/tracedecay/tests/daemon_suite/main.rs` (add `mod sealed_generation_retention_test;`)

**Interfaces:**
- Consumes: `initialize_tracedecay`, `commit_all`, `exact_identity`, `wait_for_terminal_generation`, `tool`, `stop_daemon_gracefully`, and `RECEIPT_TIMEOUT` from `code_index_journey.rs`.
- Consumes: `try_acquire_code_generation_store_read_lock` and `scoped_code_index_store_root` from `tracedecay_code_index_retention::code_index_generations`.
- Consumes: `MaintenanceCadence::retry_delay` and `RetentionConfig::default().interval_hours`, as in `sealed_generation_crash_test.rs`.
- Consumes: the store's `tracedecay.sealed/` directory (`SEALED_GRAPH_DIRECTORY` in `retention/storage_report.rs`).

- [ ] **Step 1: Write the N > 3 journey and the reader-held guard**

`superseded_sealed_generations_collect_to_live_plus_one`:
1. Init a repository and publish five generations, one commit each, waiting for each terminal generation.
2. Wait at most `cadence.retry_delay() + RECEIPT_TIMEOUT` for scheduled maintenance.
3. Assert that `tracedecay.sealed/` holds at most two generation directories, one of which is the live head.
4. Assert that a `search` for the last commit's anchor symbol returns it.

`reader_held_previous_generation_survives_scheduled_retention`:
1. Publish three generations.
2. Hold `try_acquire_code_generation_store_read_lock` on the store while a fourth generation publishes and the maintenance window elapses.
3. Assert the generation directory published before the fourth still exists while the lock is held.
4. Release the lock, wait one more window, and assert the sealed directory count is at most two.

- [ ] **Step 2: Run (reproduce first) and record the blocking stage**

Run: `scripts/require-exact-test.sh bazel test //crates/tracedecay:daemon_suite --test_output=all --test_arg=sealed_generation_retention_test::superseded_sealed_generations_collect_to_live_plus_one --test_arg=--exact`

Run the same command for `sealed_generation_retention_test::reader_held_previous_generation_survives_scheduled_retention`.

Record the counts and the daemon-log retention events (`retention_degraded` with `failure=` and `graph_superseded_replays_retired` with `retained=`/`pending=`). If both pass, criterion 1 is met on `master` and the tests are its regression pins. If the first fails with `generation_store_busy`, Task 9 owns the fix. If it fails with `retention_plan_failed … durable descriptor target`, Task 8 owns it. Any other failure stops #4616 and goes to the coordinator as `needs-decision` with the log.

- [ ] **Step 3: Commit the pins only if they are green**

If both tests passed in Step 2:

```bash
git add crates/tracedecay/tests/daemon_suite/sealed_generation_retention_test.rs crates/tracedecay/tests/daemon_suite/main.rs
git commit -m "test(retention): pin sealed generation collection through the daemon"
```

If either is red, leave both files uncommitted. They are committed with the task that turns them green: Task 8 Step 5 for a descriptor-target refusal, Task 9 Step 5 for a busy store. No commit on the branch carries a known-red test.

### Task 8: #4616 One relivened text artifact does not stall recovery

**Files:**
- Create: `crates/tracedecay-code-index-retention/src/code_index_generations/tests/text_artifact_recovery_tests.rs`
- Modify: `crates/tracedecay-code-index-retention/src/code_index_generations/tests.rs` (register beside `graph_replay_release_tests`)
- Modify: `crates/tracedecay-code-index-retention/src/code_index_generations/text_artifacts.rs`

**Interfaces:**
- Consumes: `recover_pending_text_artifact_transaction_unlocked(&Path)`, `cleanup_committed_text_artifact_transaction(&Path, &CodeTextArtifactRetentionTransactionV1)`, `scope_text_artifact_marks`, `FileQuarantine::{recover, restore, remove_committed, remove_empty_stage}`, and the generation fixture in `code_index_generations/fixture.rs`.
- Changes: `cleanup_committed_text_artifact_transaction(..) -> Result<CommittedTextArtifactCleanupV1, CodeGenerationRetentionErrorV1>`. It restores each committed candidate that a current scope marks as a durable target, removes the rest, and empties the stage.
- Produces: `pub struct CommittedTextArtifactCleanupV1 { pub removed: Vec<String>, pub restored_live: Vec<String> }`, and a new field `restored_live_text_artifacts: Vec<String>` on `CodeGenerationRetentionReportV1`, which every constructor sets.
- Deletes: `ensure_text_artifact_transaction_liveness` as a whole-transaction refusal.
- Changes: callers log `retention_text_artifact_restored_live` with the restored names, and no longer return `UnsafeState`.

- [ ] **Step 1: Write the relivened-artifact recovery test**

`committed_receipt_with_relivened_artifact_restores_it_and_finishes_recovery`:
1. Stage a text-artifact retention transaction with three completed candidates and write its durable receipt.
2. Before cleanup, publish a scope pointer naming candidate 2 (simulating a sibling publish).
3. Run `recover_pending_text_artifact_transaction_unlocked`, then plan and execute a second retention with two further superseded generations.

Assert:
- candidate 2 is present byte-identical in the completed-artifact directory;
- candidates 1 and 3 are absent;
- the transaction journal is cleared;
- the second retention collects its generations.

- [ ] **Step 2: Run RED**

Run: `scripts/require-exact-test.sh bazel test //crates/tracedecay-code-index-retention:unit_test --test_output=all --test_arg=code_index_generations::tests::text_artifact_recovery_tests::committed_receipt_with_relivened_artifact_restores_it_and_finishes_recovery --test_arg=--exact`

Expected: `1 failed` with `refused unsafe state: text-artifact retention recovery would remove a durable descriptor target`.

- [ ] **Step 3: Implement per-candidate restoration**

Rewrite `cleanup_committed_text_artifact_transaction` to compute the marks once. For each candidate: `quarantine.restore` it if marked and not `Staging`, otherwise `remove_committed`. Then `remove_empty_stage` and return `CommittedTextArtifactCleanupV1`. Migrate both callers (the in-pass path near the receipt write and `recover_pending_text_artifact_transaction_unlocked`), and include `restored_live` in the retention report the maintenance pass logs.

- [ ] **Step 4: Run GREEN and the retention crate**

Run the Step 2 command. Expected: `1 passed`.

Run: `bazel test //crates/tracedecay-code-index-retention:unit_test //crates/tracedecay-maintenance:unit_test --test_output=errors`

Expected: PASS.

- [ ] **Step 5: Commit**

If Task 7's pins were held back because of a descriptor-target refusal and now pass (rerun its Step 2 commands), add them to this commit.

```bash
git add crates/tracedecay-code-index-retention/src/code_index_generations
git commit -m "fix(retention): restore live text artifacts instead of stalling"
```

### Task 9: #4616 Bounded progress for retention on a busy generation store

**Files:**
- Create: `crates/tracedecay-maintenance/src/store_maintenance/busy_store_progress_tests.rs`
- Modify: `crates/tracedecay-maintenance/src/store_maintenance/mod.rs` (`run_code_generation_retention`; register the test module)
- Modify: `crates/tracedecay-maintenance/src/telemetry.rs` (`StoreTelemetrySamplingRegistry`)
- Modify: `crates/tracedecay-code-index-retention/src/code_index_generations.rs` (`execute_code_generation_retention_cancellable`, `recover_code_generation_retention_cancellable`)
- Modify: `crates/tracedecay-code-index-retention/src/code_index_generations/locking.rs` (make `acquire_code_generation_store_lock_checked` `pub`)

**Interfaces:**
- Produces: `pub enum StoreLockAdmissionV1 { TryOnce, WaitUntil(Instant) }` in `code_index_generations.rs`. It is a new parameter of `execute_code_generation_retention_cancellable` and `recover_code_generation_retention_cancellable`; `WaitUntil` uses `acquire_code_generation_store_lock_checked(store_root, deadline, is_cancelled)`.
- Produces: `StoreTelemetrySamplingRegistry::record_generation_store_busy(&self, store_root: &Path) -> u32` and `clear_generation_store_busy(&self, store_root: &Path)`.
- Produces: `const GENERATION_STORE_BUSY_DEFERRALS_BEFORE_WAIT: u32 = 3` in `store_maintenance/mod.rs`. `run_code_generation_retention` passes `WaitUntil(lease deadline)` once the count reaches it, and clears the count on any non-busy outcome.

- [ ] **Step 1: Write the sustained-writer and shutdown tests**

`retention_progresses_under_sustained_publication_lock_pressure`:
- a writer thread loops: acquire `acquire_code_generation_store_lock`, signal `held`, hold 50 ms, release, sleep 20 ms;
- the test waits for `held` before each of five retention attempts on a fixture store with three superseded generations.

Assert:
- attempts 1–3 return the busy deferral;
- some attempt among 4–5 collects the superseded generations;
- the writer never observes its lock lost.

`busy_store_wait_observes_shutdown_cancellation`:
- the writer holds the lock indefinitely;
- after three deferrals, the fourth attempt starts, and the test cancels its `CancellationToken` after 100 ms.

Assert the attempt returns `Cancelled` within 1 s and that no generation was removed.

- [ ] **Step 2: Run RED**

Run: `scripts/require-exact-test.sh bazel test //crates/tracedecay-maintenance:unit_test --test_output=all --test_arg=store_maintenance::busy_store_progress_tests::retention_progresses_under_sustained_publication_lock_pressure --test_arg=--exact`

Expected: `1 failed`; all five attempts defer with `generation_store_busy`.

- [ ] **Step 3: Implement the bounded wait (behavior commit)**

Add `StoreLockAdmissionV1` and the counter. Migrate every caller of the two retention entry points (`rg -n "execute_code_generation_retention_cancellable|recover_code_generation_retention_cancellable" crates`) in this slice.

- [ ] **Step 4: Run GREEN, the shutdown guard, and the daemon pins**

Run the Step 2 command, then the same command for `store_maintenance::busy_store_progress_tests::busy_store_wait_observes_shutdown_cancellation`. Expected: `1 passed` each.

Run: `bazel test //crates/tracedecay-maintenance:unit_test //crates/tracedecay-code-index-retention:unit_test //crates/tracedecay:daemon_suite --test_output=errors`

Expected: PASS, including Task 7's pins.

- [ ] **Step 5: Commit with the bound's evidence**

If Task 7's pins were held back as red (Task 7 Step 3), add them to this commit now that they pass.

```bash
git add crates/tracedecay-maintenance/src crates/tracedecay-code-index-retention/src
git commit -m "fix(retention): wait for a busy generation store after deferrals"
```

Put the measurement in the commit body: the deferral count observed in Task 7 Step 2 and the Step 4 attempt count. Flag the constant for coordinator bound review in the lane report.

### Task 10: #4616 Doctor names the refusal behind an over-bound store

**Files:**
- Modify: `crates/tracedecay-daemon-service/src/doctor_kernel.rs` (`collect_code_generation_retention_findings`)
- Modify: `crates/tracedecay-maintenance/src/retention/footprint.rs` (project-store finding text)
- Test: `crates/tracedecay-daemon-service/src/doctor_kernel/tests.rs`

**Interfaces:**
- Consumes: `plan_code_generation_retention_with_verification` errors (`CodeGenerationRetentionErrorV1::{UnsafeState, Conflict, TargetHeld}`) and `footprint_findings(&ProfileFootprintV1) -> DoctorStorageFamilyReadV1`.
- Produces: an over-bound project-store finding whose dry-run plan is refused. It carries `retention blocked: <error display>` followed by the existing supported-command text (`tracedecay --dry-run projects forget <id>`, then `--yes`).

- [ ] **Step 1: Write the Doctor guidance test**

`over_bound_store_with_refused_retention_names_refusal_and_command`:
- a fixture project store over a 1 MiB `store_soft_budgets_bytes` override, whose code-index store has a transaction journal that makes the plan return `UnsafeState`;
- run the Doctor storage family read.

Assert the project finding contains `retention blocked:`, the refusal text, and `tracedecay --dry-run projects forget`.

- [ ] **Step 2: Run RED**

Run: `scripts/require-exact-test.sh bazel test //crates/tracedecay-daemon-service:unit_test --test_output=all --test_arg=doctor_kernel::tests::over_bound_store_with_refused_retention_names_refusal_and_command --test_arg=--exact`

Expected: `1 failed`; the finding lacks `retention blocked:`.

- [ ] **Step 3: Thread the refusal into the finding**

`collect_code_generation_retention_findings` returns the refusal for the store. The footprint finding for the matching `projects/<id>` key appends it before the command text.

- [ ] **Step 4: Run GREEN and commit**

Run the Step 2 command. Expected: `1 passed`.

Run: `bazel test //crates/tracedecay-daemon-service:unit_test //crates/tracedecay-maintenance:unit_test --test_output=errors`. Expected: PASS.

```bash
git add crates/tracedecay-daemon-service/src/doctor_kernel.rs crates/tracedecay-daemon-service/src/doctor_kernel/tests.rs crates/tracedecay-maintenance/src/retention/footprint.rs
git commit -m "fix(doctor): name the retention refusal behind over-bound stores"
```

### Task 11: #4616 Storage report memory independent of generation count

**Files:**
- Create: `crates/tracedecay-maintenance/src/retention/storage_report/scale_tests.rs`
- Modify: `crates/tracedecay-maintenance/src/retention/storage_report.rs` (`#[cfg(test)] mod scale_tests;`)

**Interfaces:**
- Consumes: `build_project_storage_report_from_daemon` and `build_storage_report(&Path)`, the existing seeding helpers in `storage_report.rs` tests (`seed_global_db`, `seed_graph_db`), and the generation fixture in `tracedecay_code_index_retention::code_index_generations::fixture` (feature `test-helpers`).
- Consumes: `tracedecay_runtime_core::resident_memory` for the process resident sample.

- [ ] **Step 1: Write the scale test**

`storage_report_resident_growth_is_independent_of_generation_count` builds one profile with 5 superseded generations and one with 70. Each generation has a 700 MiB sparse segment file, so it uses no disk blocks. For each profile, run `build_storage_report` and sample peak resident bytes during the call.

Assert:
- both reports complete with a size coverage of complete or partial;
- each report's logical byte total equals the sum of the fixture's logical sizes;
- the 70-generation resident delta is at most 1.5× the 5-generation delta. This is a scale-independence property, not a tuned budget.

- [ ] **Step 2: Run (reproduce first)**

Take a coordinator slot. Run: `scripts/require-exact-test.sh bazel test //crates/tracedecay-maintenance:unit_test --test_output=all --test_arg=retention::storage_report::scale_tests::storage_report_resident_growth_is_independent_of_generation_count --test_arg=--exact`

Record both resident deltas and the `TRACEDECAY_SPAN_TIMINGS=1` close timings of `maintenance.footprint.census`.

If it passes, criterion 5 is met on `master` and the test is its pin. If it fails, stop and report `needs-decision` with the span that scales. That span names the accumulating owner, and the coordinator adds the fix task.

- [ ] **Step 3: Commit the pin**

```bash
git add crates/tracedecay-maintenance/src/retention/storage_report.rs crates/tracedecay-maintenance/src/retention/storage_report/scale_tests.rs
git commit -m "test(storage): bound storage report memory across generation counts"
```

- [ ] **Step 4: Re-verify #4616 and open the PR**

Run: `bazel test //crates/tracedecay:daemon_suite //crates/tracedecay-maintenance:unit_test //crates/tracedecay-code-index-retention:unit_test //crates/tracedecay-daemon-service:unit_test --test_output=errors`

Expected: PASS, including both Task 7 pins.

Run `pnpm run contracts:check`. Expected: no drift.

Open the PR for Tasks 7–11 with `Refs #4616`. Its criterion map covers the five criteria with test names and counts. Criterion 3 and the multi-day busy-monorepo observation stay `pending-long-acceptance` on the operator machine. After merge, notify the coordinator that the L3 #4533 gate is open.

### Task 12: #4099 Profile-wide hook analytics rotate from the retention tick

**Files:**
- Modify: `crates/tracedecay/src/daemon/maintenance.rs` (new pass inside the retention tick; tests in its `mod tests`)

**Interfaces:**
- Consumes: `tracedecay_application::analytics_bridge::{hook_import_sources(&Path, Option<&Path>) -> Result<Vec<HookImportSource>>, import_hook_analytics(&RegisteredGlobalDb, Vec<HookImportSource>) -> HookImportOutcome}`. Rotation happens inside the existing `import_source` once a fully imported file exceeds `HOOK_ANALYTICS_ROTATE_BYTES` (64 MiB).
- Produces: `async fn run_hook_analytics_rotation(profile_root: &Path, registry: &RegisteredGlobalDb, project_roots: &[PathBuf]) -> HookImportOutcome`, called once per retention tick for the profile-wide file and each registered project root. It logs `retention_hook_analytics` with `imported`, `rotated_sources`, and `errors`.

- [ ] **Step 1: Write the rotation and concurrency tests**

`retention_tick_rotates_unsettled_profile_hook_analytics`:
- write 70 MiB of valid hook rows to `<profile>/hook_analytics.jsonl` with no settlement having run;
- run one retention tick.

Assert:
- the file length is below 1 MiB;
- `analytics_events` contains every written row exactly once.

`concurrent_settlement_and_maintenance_imports_count_each_row_once`:
- run the tick's import and `import_hook_analytics` (the settlement route) concurrently on the same 70 MiB file.

Assert every row is counted once.

- [ ] **Step 2: Run RED**

Run: `scripts/require-exact-test.sh bazel test //crates/tracedecay:unit_test --test_output=all --test_arg=daemon::maintenance::tests::retention_tick_rotates_unsettled_profile_hook_analytics --test_arg=--exact`

Expected: `1 failed`; the file is still 70 MiB.

- [ ] **Step 3: Add the pass to the retention tick**

Call `run_hook_analytics_rotation` from the tick, after the LCM payload GC pass, with the registry the tick already holds. A registry-unavailable state logs `retention_degraded pass=hook_analytics` and continues the tick.

- [ ] **Step 4: Run GREEN and commit**

Run the Step 2 command, then the same command for `daemon::maintenance::tests::concurrent_settlement_and_maintenance_imports_count_each_row_once`. Expected: `1 passed` each.

Run: `bazel test //crates/tracedecay:unit_test --test_output=errors`. Expected: PASS.

```bash
git add crates/tracedecay/src/daemon/maintenance.rs
git commit -m "fix(maintenance): rotate hook analytics from the retention tick"
```

### Task 13: #4099 Ingest growth trips a named Doctor finding

**Files:**
- Create: `crates/tracedecay/tests/daemon_suite/storage_growth_finding_test.rs`
- Modify: `crates/tracedecay/tests/daemon_suite/main.rs` (add `mod storage_growth_finding_test;`)

**Interfaces:**
- Consumes: `tracedecay doctor --json` (`Doctor { json: bool }`), the storage family findings in that document, the `[retention] store_soft_budgets_bytes` owner setting (key text asserted in `dashboard_api_test/storage.rs`), and the session-ingest driver from `store_shutdown_checkpoint_test.rs`.

- [ ] **Step 1: Write the growth regression with its negative control**

`ingest_growth_past_a_bound_trips_a_named_finding`:
1. Set `store_soft_budgets_bytes` for the project's `projects/<id>` key to its measured size plus 256 KiB.
2. Run `tracedecay doctor --json` and assert no over-bound finding for that key (negative control).
3. Ingest transcripts until the store grows more than 1 MiB.
4. Run `doctor --json` again.

Assert:
- an over-bound finding names `projects/<id>` with measured and bound bytes;
- the document lists per-store sizes for the project store, `global.db`, `user-sessions.db`, and every present WAL;
- `storage.table_growth` for `tracedecay.db` and `global.db` is not `unavailable` on the second sample.

- [ ] **Step 2: Run (reproduce first)**

Take a coordinator slot. Run: `scripts/require-exact-test.sh bazel test //crates/tracedecay:daemon_suite --test_output=all --test_arg=storage_growth_finding_test::ingest_growth_past_a_bound_trips_a_named_finding --test_arg=--exact`

Expected: `1 passed`. This is criterion 3's suite check, and its negative control proves the assertion is falsifiable.

If the `table_growth` assertion fails, stop and report `needs-decision` with the typed `TableGrowthUnavailableReasonV1` the document carries. Any other failure is a red reproduction to report with the Doctor JSON.

- [ ] **Step 3: Commit**

```bash
git add crates/tracedecay/tests/daemon_suite/storage_growth_finding_test.rs crates/tracedecay/tests/daemon_suite/main.rs
git commit -m "test(storage): trip a named finding when ingest outgrows a bound"
```

### Task 14: #4099 Retire the footprint family for the store L1 deletes, then open the PR

**Gate:** start only after L1's #4448 has merged. Rebase onto the `master` that contains it.

**Files:**
- Modify: `crates/tracedecay-maintenance/src/retention/footprint.rs` (delete `ProfileFamilyV1::ResponseHandles`, its bound arm, its label arm, and its `response-handles` classification)
- Modify: `crates/tracedecay-cli/src/commands/storage.rs` only if its fixed profile path list still names `response-handles`. Delete that entry.

**Interfaces:**
- Consumes: L1's merged removal of the `response-handles/` writer (`RESPONSE_HANDLES_DIRECTORY` absent from `crates/tracedecay-runtime-core/src/storage`).
- Changes: a `response-handles/` directory left on a released profile classifies as `ProfileFamilyV1::Other`.

- [ ] **Step 1: Write the classification test**

In `footprint.rs` tests, write `retired_response_handle_directory_is_reported_as_unowned`. It census-walks a profile containing `response-handles/a.json` and asserts the entry's family is `Other`.

- [ ] **Step 2: Run RED**

Run: `scripts/require-exact-test.sh bazel test //crates/tracedecay-maintenance:unit_test --test_output=all --test_arg=retention::footprint::tests::retired_response_handle_directory_is_reported_as_unowned --test_arg=--exact`

Expected: `1 failed`; the family is `ResponseHandles`.

- [ ] **Step 3: Delete the family**

Remove the variant and every arm that matches it. Run `rg -n "ResponseHandles|response-handles" crates/tracedecay-maintenance crates/tracedecay-daemon-service crates/tracedecay-cli/src` and confirm no reference remains outside L1's refusal path. Also confirm that no other footprint family maps to a path L1's merged #4427 deleted. Cite L1's PR diff file list.

- [ ] **Step 4: Run GREEN and commit**

Run the Step 2 command. Expected: `1 passed`.

Run: `bazel test //crates/tracedecay-maintenance:unit_test //crates/tracedecay-daemon-service:unit_test //crates/tracedecay-cli:core_cli_suite --test_output=errors`. Expected: PASS.

```bash
git add crates/tracedecay-maintenance/src/retention/footprint.rs crates/tracedecay-cli/src/commands/storage.rs
git commit -m "simplify(storage): drop footprint family for retired response handles"
```

- [ ] **Step 5: Open the #4099 PR after the L1 gate**

Confirm that L1's #4448 and #4427 have both merged. Rebase, rerun the Task 12–14 targets, and run `pnpm run contracts:check`.

Open the PR for Tasks 12–14 with `Refs #4099`. Its criterion map covers the three needs:

- **Need 1, size observable.** Task 13, plus #4794 merged growth streaming.
- **Need 2, retention and GC.** One row per family, each with the pass and test that already cover it on `master`:
  - deleted-worktree project stores: `orphan_store_gc_days`, `retention/orphan_stores`;
  - LCM payloads: `retention_lcm_payload_gc`;
  - WAL checkpointing: the checkpoint driver plus the `Wal` bound;
  - projection history: graph-replay retirement;
  - hook analytics: Task 12.

  `user-sessions.grafeo` and `response-handles/` no longer exist on `master` and are recorded as such.
- **Need 3, regression check.** Task 13.

The 24-hour acceptance is `pending-long-acceptance` (Task 15).

### Task 15: #4099 Real-profile 24-hour runbook draft (report only; never executed by the lane)

**Files:**
- None committed. The draft goes into the L2 lane-end report for the coordinator, who presents it to the user for separate approval at execution time.

**Interfaces:**
- Consumes: final merged `master` SHA, `default_bound_bytes` per `ProfileFamilyV1`, `PROFILE_TOTAL_BOUND_BYTES`, `tracedecay doctor --json`, and `tracedecay storage report --json`.

- [ ] **Step 1: Draft the runbook sections**

Write these sections:

1. **Binary.** Final `master` commit SHA, built with `bazel build //crates/tracedecay-cli:tracedecay`. The install location and method are left as explicit fields the coordinator fills and the user approves. The draft proposes none.
2. **Authority.** The operator's existing daemon and service run as they are. The draft lists any install, upgrade, or restart as a separate line item, with how it is undone. It proposes no second daemon.
3. **Read/write boundaries.** The product writes `~/.tracedecay` through normal use. The coordinator only runs read-only sampling commands.
4. **Sampling.** `tracedecay doctor --json` and `tracedecay storage report --json` at t0, every 2 h, and at ≥ 24 h, saved outside the profile.
5. **Per-store bounds table.** From `default_bound_bytes`: project store 10 GiB, graph container 4 GiB, WAL 4× soft limit, profile registry 4 GiB, user sessions 8 GiB, user memory 1 GiB, LCM payloads 16 GiB, hook analytics 256 MiB, maintenance 64 MiB, SQLite temp spill 1 GiB, profile total 64 GiB.
6. **Stop conditions.** Verbatim from the design's Long-running acceptance section.
7. **Forbidden actions.** No copy, backup, reset, manual deletion, config edit, or racing daemon.
8. **Closure evidence.** Per-store sizes present in every sample, and no store past its bound without a named finding.

- [ ] **Step 2: Hand off**

Mark #4099 `pending-long-acceptance` in the lane report, with the draft attached and the statement "not executed; requires the user's separate approval". Do not run any part of it.

---

## Self-review

- **Spec coverage.** Every L2 issue has its order (#4876 → #4513 → #4152 → #4616 → #4099), reproduce-first steps, and closure through a coordinator-reviewed criterion map. The seams are respected: L2 owns registry resolution and retention; L3's format is consumed only through retention's public interface; L1's removals gate Task 14. #4099's real-profile runbook is drafted (Task 15) and never executed.
- **Placeholder scan.** No TBDs. Non-reproduction and unexpected-mechanism outcomes go to a named `needs-decision` disposition instead of an unspecified fix.
- **Type consistency.** These names are used identically everywhere they appear: `ProjectRootResolutionV1`, `HookWorkspaceStatus::RegistryConflict`, `UnlinkedOpenFileV1`, `ProcessFdCensusErrorV1`, `CommittedTextArtifactCleanupV1`, `StoreLockAdmissionV1`, `GENERATION_STORE_BUSY_DEFERRALS_BEFORE_WAIT`, and `run_hook_analytics_rotation`.
- **Review Focus.** Each of the five items names a test that is written in a numbered step of its owning task.
