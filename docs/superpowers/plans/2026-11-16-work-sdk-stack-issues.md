# Work, SDK, and Stack Outstanding Issues Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish lane L5: one lifetime-owned stack scope state (#4436), the remaining native-host acceptance for bounded stack notifications (#4445), independent Work scopes (#4215), retirement of automatic all-PR worktree indexing (#4419), a measured niche-grammar default decision (#4229), and a Node SDK that is a thin napi-rs binding over one shared Rust client (#4226).

**Architecture:** Stack scope state moves into one `Arc`-owned slot per scope, returned to each real owner as a handle; the daemon coordinator keeps only a weak scope index and global preflight admission. Work keeps its single owner journal and global version chain, and a selection's current view becomes a skip-fold projection of the events it covers, with writes rebased onto the global head. PR autotracking is deleted down to a one-shot legacy artifact retirement. The Rust SDK lifecycle client becomes async, the Node package becomes a generated TypeScript facade over a napi-rs addon over that client, and platform addons are staged by the existing release path.

**Tech Stack:** Rust 2024, Tokio, rusqlite runtime, reqwest (async), napi-rs, Bazel (rules_rust, rules_js), TypeScript, Vitest, pnpm, schemars codegen.

**Spec:** `docs/superpowers/specs/2026-11-16-outstanding-issues-delivery-design.md`

## Global Constraints

- These are future execution steps. Writing this plan authorized no branch, build, test, daemon start, install, issue edit, or operator-data access. Execute only in the L5 worktree the coordinator assigns, on branches cut from current `origin/master` (baseline `58ccb41d66`). One branch and PR per task; a later task may start locally while the earlier PR is in review, but never stacks on it.
- Order is fixed: Task 1 (#4436), 2 (#4445), 3 (#4215), 4 (#4419), 5 (#4229), 6-8 (#4226). Tasks 6-8 start after Task 5 against the operation registry on `master` at that time.
- **Bazel only.** `cargo` is never invoked. Name-filtered libtest runs go through `scripts/require-exact-test.sh` with the full module path and must show a nonzero count. The helper reads the libtest summary, which Bazel prints for a passing test only with `--test_output=all`, so every helper run and every run whose counts or test names are cited uses that flag. JavaScript packages use their pnpm scripts; Rust, codegen, and SDK/plugin Bazel targets use Bazel. After Rust dependency or feature changes: `CARGO_BAZEL_REPIN=workspace bazel fetch --repo=@c`, then `bazel run //scripts:gen_builds`, committed with the manifest change.
- **Generated sources are never hand-edited** (`dashboard/src/contracts/`, `sdks/typescript/src/operations.ts` and `types.ts`, `crates/tracedecay-sdk/src/operations.rs`, `dashboard/codegen/schemas/`). Regenerate with `cd dashboard && pnpm run contracts:generate`, verify with `pnpm run contracts:check`, and resolve rebase conflicts in them by regenerating. Regeneration is always its own checkbox step.
- **No compatibility code.** A shape change migrates every caller and deletes the old one in the same task. No shims, aliases, dual writes, readers for old shapes, backups, or rollback copies. Persisted shapes either need no change (Tasks 1-3, 5-8) or return the typed reset path (Task 4).
- **Seams.** L5 owns Work backend semantics, the stack coordinator, PR-branch demand (#4419), grammar profiles, and the Node SDK plus generated SDK sources. L5 does not edit the project/worktree registry (L2), the code-index generation format (L3), or any hand-written dashboard path (L4), and other lanes only regenerate SDK outputs. Task 4's dashboard consumers belong to L4 Task 4 (Gate G3). Task 4 edits the shared configuration registry files only under Gate G2.
- No `unwrap`, `expect`, `panic`, silent fallback, or empty success in production code. No `allow(dead_code)`. No timeout, bound, or budget changes without measurement in their own commit. Measured work (Tasks 4 and 5) runs alone in a coordinator-granted slot, and reports come from `tracing` spans (`TRACEDECAY_SPAN_TIMINGS=1`) or real resource sampling.
- Tests use isolated home, profile, project, and session inputs and the shared production identity authorities. Nothing reads or mutates `~/.tracedecay`, `~/.claude`, `~/.codex`, `~/.agents`, or the installed daemon.
- Commits: `<type>(<scope>): <subject>`, header at most 72 characters, no issue numbers (those go in the PR body as `Fixes #N` only when the PR meets every criterion, otherwise `Refs #N`). Run the repository's `simplify` pass before each non-test, non-doc commit.

### Start and merge gates

- **G1 (#4419 merge):** L2 #4876 has merged and Task 4 is rebased and re-verified onto it. Analysis and implementation may precede the merge.
- **G2 (#4419 registry files):** L6's #4416 also retires a key from `crates/tracedecay-global-db/src/configuration/registry.rs`, `crates/tracedecay-domain/src/configuration.rs`, `crates/tracedecay-domain/src/lib.rs`, and the configuration store tests. Task 4 edits those files only after its branch is rebased onto a `master` that contains L6's merged #4416 PR. Analysis and Steps 1-6 may precede that. If that PR has not merged when Task 4 reaches Step 7, Task 4 pauses there and L5 starts Task 5.
- **G3 (#4419 dashboard child):** Task 4's regenerated contracts remove `ProjectSettingsPayloadV1.pr_autotrack`, `ProjectEditableSettingsV1.sync`, `ProjectSettingsPatch.sync`, and the `SyncSettingsV1`, `SyncSettingsPatch`, `PrAutoTrackPayloadV1`, and `PrAutoTrackEntryV1` types. The L4-owned settings editor and fixtures read them, so dashboard checks on Task 4's branch stay red until L4 Task 4 lands. L4 branches from Task 4's pushed tip and opens a child PR against Task 4's branch. The coordinator merges that child into Task 4's branch, then merges Task 4 (design, Across lanes). Task 4 edits no hand-written dashboard path, only regenerates generated outputs by command, and never resolves a dashboard conflict.
- **G4 (#4226 merge):** Tasks 6-8 do not merge until every contract-changing prerequisite PR from any lane has merged or been dispositioned with no pending contract change. Task 8 ends with the rebase, regeneration, and conformance rerun that gate requires.

### Settled rulings

These are decided in the design's Decisions resolved. None is a gate, and none goes back to the user.

- **#4419 scope and settings.** Retiring automatic all-PR indexing, the user-visible `tracedecay branch autotrack` command, and its two settings is the issue's explicit goal, and the design approves it. A configuration snapshot that holds a key the registry no longer defines is typed configuration `ResetRequired` (`converge_registry_shape`; `bd64621c12` set the precedent), so every snapshot that holds either autotrack key resets, and setters refuse both keys as unknown. No admission-path convergence, value mapping, or silent preservation of the retired settings exists.
- **#4436 restart.** No production reader of persisted open drift intervals exists at baseline (`restore_open_drift_interval` has only test callers). Task 1 ports the handle-level restore and its test. It adds no production consumer to claim closure. Its PR says `Refs #4436` and names the restart criterion as the open gap, and the issue stays open.
- **#4229 decision.** The Step 7 decision rule in Task 5 turns the measurements into opt-in or a completed no-change. The coordinator verifies the evidence and escalates to the user only a genuine product preference that the evidence cannot settle. If demand or quality evidence is not obtainable without operator data, the issue stays open.
- **#4226 customization and dependencies.** The user chose to remove `ClientOptions.fetch` in 0.16.0, with the typed `TraceDecayInvalidOptionError` and a README migration section. The design accepts `napi`, `napi-derive`, and `napi-build`. They follow the repository dependency rules: maintained releases declared in `[workspace.dependencies]`, the Bazel repin and `gen_builds` output committed with the manifest, no overlapping library, and removal with their last caller. The addon must build and pass on every OS and architecture in the existing `.github/workflows/release.yml` matrix. A platform that cannot build is a blocker to report, not a matrix change.

## Review Focus

1. **Hidden-entity disclosure through cross-scope dependencies (Task 3):** a narrow selection that references, collides with, or modifies an entity authored under a scope it cannot see must be refused without confirming that the entity exists. Pinned by `hidden_dependency_is_denied_without_disclosing_the_entity` (Task 3, Step 7), with `concurrent_scoped_writers_conflict_only_within_one_aggregate` for the compare-and-swap boundary.
2. **Late cleanup against a successor scope registration (Task 1):** a read or owner that outlives its scope's retirement must not remove or publish into a later registration of the same scope. Pinned by `late_retired_owner_cannot_alter_successor_scope` (Task 1, Step 5).
3. **Destructive retirement of old managed PR artifacts (Task 4):** dirty, foreign, leased, or hand-edited worktrees and refs must be preserved and reported, never recursively deleted. Pinned by `dirty_foreign_or_leased_legacy_pr_artifacts_are_preserved_and_reported` (Task 4, Step 6).
4. **Expired or replayed stack notices reaching a host (Task 2):** a retired signal must neither expand as empty success nor deliver again, including after a restart and beside a pending recipient. Pinned by `expired_stack_notice_expansion_is_concealed_and_never_redelivered` and `retired_signal_replay_after_restart_does_not_redeliver` (Task 2, Steps 1 and 4).
5. **Native cancellation and event-loop blocking (Task 7):** aborting an in-flight request or closing a stream early must close the connection, and a stalled read must not stop the Node event loop. Pinned by `aborted_requests_close_the_socket_and_never_block_the_event_loop` (Task 7, Step 5).

---

### Task 1: #4436 lifetime-owned stack scope state

**Files:**
- Modify: `crates/tracedecay-application/src/stack_coordinator.rs`
- Modify: `crates/tracedecay-application/src/advisory/github_runtime/owner.rs`
- Modify: `crates/tracedecay-application/src/advisory/github_runtime/network/stack_network.rs`
- Modify: `crates/tracedecay-application/src/advisory/runtime/local_cycle_tests.rs`
- Modify: `crates/tracedecay-agent-hosts/src/native_integration/stack_runtime.rs`
- Modify: `crates/tracedecay-agent-hosts/src/native_integration/registry.rs`
- Modify: `crates/tracedecay-agent-hosts/src/native_integration/mod.rs`
- Modify: `crates/tracedecay/src/daemon/project_open_owners.rs`
- Modify: `crates/tracedecay/src/daemon/project_open_owners/advisory_runtime.rs`
- Test: `crates/tracedecay-application/tests/application_suite/github_stack_coordinator.rs`
- Test: `crates/tracedecay-application/tests/application_suite/github_stack_coordinator/preflight_tests.rs`
- Test: `crates/tracedecay-agent-hosts/src/native_integration/journey_tests.rs`

**Interfaces consumed:** `ResolvedScope`, `GitHubStackedPullRequestPolicyV1`, `StackCircuitPolicyV1`, `GitHubStackProviderReadV1`, `OptionalStackPreflightPort`, `StackCoordinatorStore`, `StackDeliveryAuthorizationPort`, `native_runtime`.

**Interfaces produced:**
- `#[derive(Clone)] pub struct StackScopeHandleV1` wrapping `Arc<ScopeSlot>`. The slot owns what is scope-local today: registration id and policy, current observation, open drift intervals, circuit policy and circuit state, and the debounce dedupe map.
- `DaemonGitHubStackCoordinatorV1::register_scope(&self, scope: &ResolvedScope, policy: GitHubStackedPullRequestPolicyV1, circuit: StackCircuitPolicyV1) -> Result<StackScopeHandleV1, GitHubStackCoordinatorErrorV1>`. It joins a live slot for the scope digest, replaces the registration id only when the policy changed, and otherwise creates a slot.
- `StackScopeHandleV1::{scope() -> &ResolvedScope, authorize_provider_read() -> Result<Option<GitHubStackProviderReadV1>, GitHubStackCoordinatorErrorV1>, observe_policy(provider, source_binding, observed_at), observe_provider(read, provider, outcome, source_binding, observed_at), restore_open_drift_interval(&GitHubStackDriftObservationV1), enqueue_transition(store, authorization, signal)}`. Each has today's parameters and results minus the scope argument.
- `DaemonGitHubStackCoordinatorV1::optional_preflight(&self, scope: &StackScopeHandleV1, preflight, request, cancellation, now)`. Global and per-repository admission and in-flight sharing stay on the coordinator.
- `#[cfg(any(test, feature = "test-helpers"))] DaemonGitHubStackCoordinatorV1::retained_scope_count(&self) -> usize`.
- `pub fn github_stack_circuit_policy() -> Result<StackCircuitPolicyV1, StackCoordinatorErrorV1>`, re-exported from `native_integration` and returning the policy `DaemonGitHubStackRuntimeV1::mount` builds today.
- `DaemonGitHubStackRuntimeV1::mount` and `DaemonNativeIntegrationOwner::mount_github_stack_runtime` take a `StackScopeHandleV1` in place of `Arc<DaemonGitHubStackCoordinatorV1>`; `GitHubReviewRuntimeOwnerConfigV1.stack_coordinator` becomes `stack_scope: StackScopeHandleV1`.
- Deleted: `register_circuit_policy`, `CircuitKey`, the coordinator's `observation_states`, `circuit_policies`, `circuits`, and `dedupe` maps, and every caller that rebuilt the scope key to join them.

- [ ] **Step 1: Add `retired_scopes_release_all_state_once_every_owner_and_read_settles`** to `github_stack_coordinator.rs`: register 1,000 distinct scopes through `register_scope` and drop each handle, asserting `retained_scope_count() == 0`; then register one scope, call `authorize_provider_read`, drop the handle while the read is held, assert the count is 1, then drop the read and assert 0.
- [ ] **Step 2: Run** `scripts/require-exact-test.sh bazel test //crates/tracedecay-application:application_suite --test_output=all --test_arg=github_stack_coordinator::retired_scopes_release_all_state_once_every_owner_and_read_settles --test_arg=--exact`; expect RED as a compile failure on `StackScopeHandleV1` (the baseline has no retirement path and four independently keyed maps).
- [ ] **Step 3: Add `independent_scopes_do_not_share_circuit_state_but_share_global_admission`** to `preflight_tests.rs`: open scope A's circuit with threshold-many failed preflights, assert scope B still admits, then fill `MAX_DAEMON_PREFLIGHTS` across scopes and assert the next distinct request is `Saturated` for every scope.
- [ ] **Step 4: Add `concurrent_identical_provider_publications_have_one_drift_transition` against handles** by porting the existing 24-thread test to `StackScopeHandleV1::observe_provider`, keeping its assertions on exactly one stack-tip transition and one opening interval.
- [ ] **Step 5: Add `late_retired_owner_cannot_alter_successor_scope`**: take a provider read, drop every handle, register the same scope with a changed policy, publish the successor's enabled snapshot, then complete the old read and assert `Superseded` with the successor's observation and intervals unchanged; repeat with an unchanged policy and assert the old read publishes only into the joined live slot.
- [ ] **Step 6: Add `dropping_the_mounted_stack_runtime_retires_its_scope_state`** to `journey_tests.rs`: mount the runtime through the existing journey harness, enqueue and expand one signal, drop the runtime and review owner, and assert the coordinator's `retained_scope_count()` is 0 while the delivery store still reports the settled recipient.
- [ ] **Step 7: Add `restarted_scope_restores_a_persisted_open_drift_interval`** by porting `restarted_owner_refuses_pre_restart_reads_and_settles_restored_drift` to `StackScopeHandleV1::restore_open_drift_interval`; this pins the handle-level contract only, because no production path reads persisted open drift intervals (Settled rulings, #4436 restart). Steps 3-7 are expected RED with Step 1's compile failure until Step 8 lands.
- [ ] **Step 8: Implement the slot and handle** in `stack_coordinator.rs` per the Interfaces: `scopes: Mutex<BTreeMap<ManifestDigest, Weak<ScopeSlot>>>` replaces the four maps, `Drop for ScopeSlot` removes its index entry only if that entry's `Weak` is dead, and `GitHubStackProviderReadV1` holds an `Arc<ScopeSlot>` clone so an accepted in-flight read keeps the slot alive. No lock is held across network or storage I/O.
- [ ] **Step 9: Migrate callers**: `owner.rs`, `stack_network.rs`, `local_cycle_tests.rs`, `stack_runtime.rs`, `registry.rs`, `mod.rs`, `project_open_owners.rs`, and `advisory_runtime.rs`. `project_open_owners.rs` is the single `register_scope` caller; it passes `github_stack_circuit_policy()?` and hands clones of one handle to the stack-runtime mount and the review owner config; delete the old `register_circuit_policy` call in `mount`.
- [ ] **Step 10: Run** `bazel test //crates/tracedecay-application:application_suite --test_output=errors`, `bazel test //crates/tracedecay-application:unit_test --test_output=errors`, and `bazel test //crates/tracedecay-agent-hosts:unit_test --test_output=errors`; expect all pass with the Step 1, 3, 4, 5, 6, and 7 tests each listed as run and a nonzero total.
- [ ] **Step 11: Run** `bazel test //crates/tracedecay:daemon_suite --test_output=errors` and `bazel build //crates/tracedecay-cli:tracedecay`; expect pass and a clean build proving the daemon composition compiles against the handle.
- [ ] **Step 12: Commit** `simplify(stack): own scope state by the mounted lifetime`. The PR body lists the removed maps (`observation_states`, `circuit_policies`, `circuits`, `dedupe`), `CircuitKey`, `register_circuit_policy`, and the repeated scope-key lookups. It says `Refs #4436` and states the gap: restart restores open drift intervals only at the handle level, because nothing in production reads them from durable storage.

### Task 2: #4445 native-host acceptance for bounded stack notifications

Terminal retirement, its frontier, the replay guard, and the daemon maintenance unit already exist on `master` (merged #4826). This task closes what that PR left unproven: the native-host expansion route, replay through the mounted runtime after restart, the documented guarantee, and the measured footprint.

**Files:**
- Modify: `crates/tracedecay-global-db/src/stack_delivery.rs`
- Test: `crates/tracedecay-agent-hosts/src/native_integration/journey_tests.rs`
- Test: `crates/tracedecay-global-db/src/stack_delivery_tests.rs`

**Interfaces consumed:** `DaemonGitHubStackRuntimeV1::{enqueue_from_preflight, expand}`, `GitHubStackSignalExpandPortError::Concealed`, `RegisteredGlobalDb::{retire_github_stack_terminal_payloads_before, github_stack_signal, github_stack_recipient_state, pending_github_stack_deliveries}`, the native store `read_receipt_by_digest`.

**Interfaces produced:** No new API. The expansion contract stays: a retired signal returns `Err(Concealed)`, because that variant intentionally merges absent and unauthorized handles and the terminal-recipient rows needed to prove a recipient are deleted by design. A documented 14-day guarantee is added to the module docs of `stack_delivery.rs`.

- [ ] **Step 1: Add `expired_stack_notice_expansion_is_concealed_and_never_redelivered`** to `journey_tests.rs`: seed a settled `IntegrationCommitted` signal issued 15 days before `now` for one recipient and a pending signal for a second recipient, retire with `retire_github_stack_terminal_payloads_before`, then assert the first recipient's `expand` returns `Err(Concealed)`, the native receipt is still readable by digest, the second recipient's pending signal still expands, and no recipient row was recreated.
- [ ] **Step 2: Run** `scripts/require-exact-test.sh bazel test //crates/tracedecay-agent-hosts:unit_test --test_output=all --test_arg=native_integration::journey_tests::expired_stack_notice_expansion_is_concealed_and_never_redelivered --test_arg=--exact`; expect a nonzero count. This pins existing store behavior at the host route, so a first-run pass is expected; if it fails, the failure is the defect to fix in `stack_runtime.rs::expand_request` before Step 5.
- [ ] **Step 3: Add `authorization_loss_after_retirement_is_concealed_not_recorded`** to `stack_delivery_tests.rs`: retire a signal, call `record_github_stack_authorization_loss` for it, and assert the call neither resurrects recipient rows nor changes the retired payload.
- [ ] **Step 4: Add `retired_signal_replay_after_restart_does_not_redeliver`** to `journey_tests.rs`: after Step 1's retirement, drop the runtime and registry, remount over the same store, call `enqueue_from_preflight` with the identical signal, and assert it succeeds as a replay with zero pending deliveries for that signal.
- [ ] **Step 5: Run** the two new `journey_tests` names and `authorization_loss_after_retirement_is_concealed_not_recorded` through `scripts/require-exact-test.sh` with `bazel test //crates/tracedecay-agent-hosts:unit_test` and `bazel test //crates/tracedecay-global-db:unit_test`; expect each count nonzero and passing.
- [ ] **Step 6: Document the retention guarantee** in the `stack_delivery.rs` module docs: the 14-day inspection window, that a retired signal keeps only its identity fingerprint, that expansion of a retired signal is `Concealed`, and that native previews and receipts are not touched.
- [ ] **Step 7: Measure the footprint** by running `bazel test //crates/tracedecay-global-db:unit_test --test_output=all --test_arg=stack_delivery_tests::terminal_compaction_preserves_pending_peers_and_refuses_replay_collision --test_arg=--exact` under `scripts/require-exact-test.sh` and `bazel test //crates/tracedecay:daemon_suite --test_output=all --test_arg=terminal_housekeeping`; record the printed payload bytes, retired and remaining row counts, and index counts in the PR body, from test output only.
- [ ] **Step 8: Commit** `docs(stack): state the settled-notice retention guarantee` for Step 6, and `test(stack): cover retired notice expansion on the native host route` for Steps 1-4 as one commit. The PR says `Refs #4445`; the coordinator closes it only after confirming the merged #4826 evidence plus this PR cover every acceptance line.

### Task 3: #4215 independent Work scopes

**Chosen semantics.** The owner journal, its global version chain, publications, and checkpoints are unchanged, so no persisted shape changes and no migration or reset applies. A selection's current view is the skip-fold of the journal events that selection covers, labelled with the global version of its last covered event. A mutation compares the selection's head token, validates against that projection, rebases version-bound changes onto the global head, validates them there, and appends. A change that applies to the projection but not to the global graph is a typed `CrossScopeDependency` refusal that names no entity. Historical (as-of) reads keep their explicit version semantics. Current reads still disclose how many events they skipped.

**Files:**
- Modify: `crates/tracedecay-rusqlite-runtime/src/work_product.rs`
- Modify: `crates/tracedecay-rusqlite-runtime/src/work_product/read.rs`
- Modify: `crates/tracedecay-rusqlite-runtime/src/work_product/history.rs`
- Modify: `crates/tracedecay-rusqlite-runtime/src/work_product/events.rs`
- Modify: `crates/tracedecay-domain/src/work_product/graph.rs`
- Modify: `crates/tracedecay-contracts/src/work_product/mutation.rs`
- Modify: `crates/tracedecay-contracts/src/work_product/types.rs`
- Modify: `crates/tracedecay-contracts/src/work_evidence.rs`
- Modify: `crates/tracedecay-contracts/src/work_attempt/product_admission.rs`
- Modify: `crates/tracedecay-daemon-service/src/invocation/work/outcome.rs`
- Modify: `crates/tracedecay-daemon-service/src/invocation/work/preparation.rs`
- Test: `crates/tracedecay-rusqlite-runtime/tests/rusqlite_suite/work_product_graph_authority.rs`
- Test: `crates/tracedecay-contracts/tests/contracts_suite`
- Test: `crates/tracedecay-daemon-service/src/invocation/work/outcome.rs` (module tests)

**Interfaces consumed:** `WorkProductSelectionScopeV1`, `WorkProductEventV1::{authorized_relation_scopes, expected_graph_version, result_graph_version}`, `WorkProductGraphV1::{new, apply, version}`, `VerifiedWorkGraphVersionV1`, `WorkGraphSelectionCoverageV1`, `RegisteredWorkProductServicesV1`.

**Interfaces produced:**
- `rusqlite-runtime`: `covered_projection(selection: &WorkProductSelectionScopeV1, journal: Vec<WorkProductJournalEntryV1>, through: Option<WorkProductEventSequenceV1>) -> Option<(Vec<WorkProductJournalEntryV1>, WorkGraphSelectionCoverageV1)>`, replacing `covered_prefix`. It keeps every covered entry, returns `Partial { covered_events, excluded_events, first_excluded_sequence }` when any were skipped, and `load_covered_journal` folds the projection with the version relabelled to each covered event's global result version.
- `domain`: `WorkGraphChangeV1::rebased_onto(self, head: WorkGraphVersionV1) -> Self`, which rewrites only `based_on_version` fields and leaves every other field unchanged.
- `contracts`: `WorkProductApplicationErrorV1::CrossScopeDependency` replaces `SelectionCoverageIncomplete`. The daemon maps it to `ApplicationProblem::invalid_request("work.cross_scope_dependency", "The change depends on work outside the selection's authorized scopes")`.
- A mutation's `expected_authority` token is the selection's own head. The appended event's `expected_graph_version` is the global head the serialized writer observed.

- [ ] **Step 1: Add the reproduction `unrelated_scoped_event_does_not_block_independent_no_git_mutation`** to `work_product_graph_authority.rs`, replacing `a_mutation_over_a_partially_covered_selection_is_refused_by_name`: create no-Git work, append a repository-scoped event, then add a second no-Git task pinned to the no-Git head; assert it commits, the no-Git view lists both no-Git tasks and omits the repository task, and the published global chain is still version-contiguous.
- [ ] **Step 2: Run** `scripts/require-exact-test.sh bazel test //crates/tracedecay-rusqlite-runtime:rusqlite_suite --test_output=all --test_arg=work_product_graph_authority::unrelated_scoped_event_does_not_block_independent_no_git_mutation --test_arg=--exact`; expect RED with `SelectionCoverageIncomplete`, the baseline defect.
- [ ] **Step 3: Add `later_authorized_events_appear_in_a_current_narrow_view`**: with a hidden earlier event, assert the no-Git current read includes a later no-Git event and its disclosure reports one excluded event, while an as-of read at the version before the later event returns the earlier head.
- [ ] **Step 4: Add `current_reads_are_not_presented_as_a_stale_prefix`**: assert the head of a narrow current view is the last covered event's global version, never an earlier covered prefix, for sequences interleaved across two scopes.
- [ ] **Step 5: Add `concurrent_scoped_writers_conflict_only_within_one_aggregate`**: from two threads, submit two no-Git mutations against the same head and assert exactly one commits and the other is `VersionConflict`; submit one no-Git and one repository mutation concurrently and assert both commit.
- [ ] **Step 6: Add `rebase_rewrites_only_version_bound_fields`** to the domain graph tests: apply `rebased_onto` to an `ExecutionAdmitted` and an `AcceptedAttemptLinked` change and assert only `based_on_version` differs, and that a `TaskAdded` change is returned unchanged.
- [ ] **Step 7: Add `hidden_dependency_is_denied_without_disclosing_the_entity`**: under a repository selection create task `task.hidden`; under no-Git attempt a change whose dependency, identity, or hierarchy collides with `task.hidden`; assert `CrossScopeDependency` and that the refusal and the problem body are byte-identical to the refusal for a never-created id, so existence is not observable.
- [ ] **Step 8: Add `restart_and_denied_access_preserve_scope_boundaries`**: restart the registered store between writes, then read and mutate under an unauthorized selection and assert `NotFoundOrNotAuthorized` with no projection content, and the authorized narrow view is unchanged by the restart.
- [ ] **Step 9: Run** `bazel test //crates/tracedecay-rusqlite-runtime:rusqlite_suite //crates/tracedecay-domain:unit_test --test_output=errors`; expect RED for the Step 3, 4, 5, 6, 7, and 8 tests (the baseline refuses on partial coverage and has no `rebased_onto`), each listed as failing, not skipped.
- [ ] **Step 10: Implement `covered_projection`, the relabelled skip-fold, and `rebased_onto`** in `work_product.rs`, `read.rs`, `history.rs`, and `graph.rs`, and use `covered_projection` for current and as-of reads. A covered event that fails to apply to the projection is a typed corrupt-journal fault, never an empty or partial graph.
- [ ] **Step 11: Implement the mutation path** in `mutation.rs` and `events.rs`: remove both `is_partial` refusals; under the serialized append compare the token to the projection head, validate the change on the projection, rebase it, validate it on the global graph, and map a global-only failure to `CrossScopeDependency`; delete `SelectionCoverageIncomplete` and migrate `types.rs`, `work_evidence.rs`, `product_admission.rs`, `outcome.rs`, and `preparation.rs`.
- [ ] **Step 12: Run** `bazel test //crates/tracedecay-rusqlite-runtime:rusqlite_suite --test_output=errors`, `bazel test //crates/tracedecay-contracts:contracts_suite --test_output=errors`, `bazel test //crates/tracedecay-domain:unit_test --test_output=errors`, and `bazel test //crates/tracedecay-daemon-service:unit_test --test_output=errors`; expect all pass with the Step 1, 3, 4, 5, 6, 7, and 8 tests run.
- [ ] **Step 13: Run** `bazel test //crates/tracedecay-rusqlite-runtime:work_product_read_scaling --test_output=errors`; expect pass, proving current reads over complete coverage keep the checkpointed path and no full-history replay is reintroduced there.
- [ ] **Step 14: Regenerate** with `cd dashboard && pnpm run contracts:generate`.
- [ ] **Step 15: Verify generation** with `cd dashboard && pnpm run contracts:check`; expect exit 0 and any generated diff committed in the same commit.
- [ ] **Step 16: Run** `bazel test //crates/tracedecay:daemon_suite //crates/tracedecay:mcp_suite --test_output=errors` to confirm the Work routes still pass through the daemon and MCP.
- [ ] **Step 17: Commit** `feat(work): keep unrelated scoped events from freezing current work`. Notify L4 through the coordinator when merged so #4223 rebases and re-measures; the PR says `Fixes #4215` only if the coordinator confirms every criterion is evidenced.

### Task 4: #4419 retire automatic all-PR worktree indexing

**Files:**
- Create: `crates/tracedecay/src/daemon/branch_worktrees.rs` (the manual-branch functions moved out of `pr_autotrack.rs`)
- Create: `crates/tracedecay/src/daemon/branch_worktrees/tests.rs`
- Create: `crates/tracedecay/src/daemon/legacy_pr_retirement.rs`
- Delete: `crates/tracedecay/src/daemon/pr_autotrack.rs`, `pr_autotrack/runtime.rs`, `pr_autotrack/tests.rs`
- Modify: `crates/tracedecay/src/daemon.rs`, `daemon/bootstrap.rs`, `daemon/engine.rs`, `daemon/engine/shutdown.rs`, `daemon/branch_add.rs`, `daemon/branch_admin.rs`, `daemon/project_composition.rs`
- Modify: `crates/tracedecay-application/src/pr_tracking.rs` and `pr_tracking/worktrees.rs`
- Modify: `crates/tracedecay-cli/src/cli.rs`, `crates/tracedecay-cli/src/commands/branch.rs`
- Modify (Gate G2): `crates/tracedecay-domain/src/configuration.rs`, `crates/tracedecay-domain/src/lib.rs`, `crates/tracedecay-global-db/src/configuration/registry.rs`, `crates/tracedecay-global-db/src/configuration/store/tests/mod.rs`
- Modify: `crates/tracedecay-configuration/src/config/mod.rs`, `config/model.rs`, `crates/tracedecay-api/src/configuration.rs`, `crates/tracedecay-application/src/settings_control.rs`, `crates/tracedecay-contracts/src/settings_preview.rs`
- Modify: `crates/tracedecay-dashboard-api/src/settings_api.rs`, `state.rs`, `test_transport.rs`, `events_api.rs`, `authority_tests.rs`
- Modify: `crates/tracedecay/src/mcp/server.rs`, `mcp/server/construction.rs`, `mcp/server/graph_tool_owner.rs`, `mcp/tools/handlers/mod.rs`, `dispatch_groups.rs`, `dashboard.rs`
- Modify: `crates/tracedecay/src/doctor.rs`, `doctor/tests.rs`
- Create (Gate G2): `crates/tracedecay-global-db/src/configuration/store/tests/retired_autotrack_keys.rs`, `crates/tracedecay-global-db/src/configuration/store/tests/fixtures/configuration-registry-autotrack.sql`
- Regenerate only by command: `dashboard/codegen/schemas/dashboard-contracts.schema.json`, `dashboard/src/contracts/`, `sdks/typescript/src/`, `crates/tracedecay-sdk/src/operations.rs`
- No dashboard source, story, or test fixture. L4 Task 4 owns those (Gate G3).
- Test: `crates/tracedecay/src/daemon/legacy_pr_retirement.rs` (module tests), `crates/tracedecay/src/daemon/branch_worktrees/tests.rs`, `crates/tracedecay-application/tests/pr_tracking.rs`, `crates/tracedecay-global-db/src/configuration/store/tests/retired_autotrack_keys.rs`, `crates/tracedecay-cli` parse tests, `crates/tracedecay/src/doctor/tests.rs`

**Interfaces consumed:** `ManagedPr`, `PrAutotrackState`, `load_state`, `save_state`, `untrack_pr` (moved into `legacy_pr_retirement.rs` with its `remove_pr_store` and worktree cleanup helpers), `StoreAdministration`, `CodeIndexSchedulerRegistryV1`, `PrCommandControl`, `acquire_manual_branch_lifecycle`.

**Interfaces produced:**
- `legacy_pr_retirement::retire_legacy_pr_worktrees(administration: StoreAdministration, schedulers: CodeIndexSchedulerRegistryV1, cancellation: CancellationToken) -> LegacyPrRetirementReport`, with `LegacyPrRetirementReport { retired: Vec<String>, preserved: Vec<PreservedPrArtifact> }` and `PreservedPrArtifact { label: String, worktree: PathBuf, tracking_ref: String, reason: PreservedReason }`; `PreservedReason` is `Dirty`, `Foreign`, `Leased`, or `Cancelled`. One bounded pass over registered projects that have a `pr-autotrack.json`; no timer, discovery, last-poll map, or new-track limit.
- `doctor::legacy_pr_artifacts_finding(data_root: &Path) -> Option<String>`, listing each remaining managed worktree path and ref and the malformed-state case.
- The branch-worktree functions keep their names and signatures and move to `branch_worktrees.rs` with spans renamed `daemon.branch_worktrees.*`.
- Deleted: `PrAutotrackTask`, `spawn_with_administration`, `reconcile_project_with_administration`, `track_pr`, `sweep_orphan_pr_worktrees`, `MAX_NEW_TRACKS_PER_CYCLE`, `discover_open_prs_with_control` and its parsers, `DiscoveredPr`, `PrDiscovery`, `ReconcileReport`, `sync.auto_track_pr_branches.v1`, `sync.auto_track_pr_poll_secs.v1`, `SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY`, `SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY`, `MIN_AUTO_TRACK_PR_POLL_SECS`, `PrAutoTrackPayloadV1`, `PrAutoTrackEntryV1`, the dashboard-api `SyncSettingsV1`, `tracedecay-api` `SyncSettingsPatch`, and `settings_control` `SyncSettingsPatchV1` (empty once both keys go), the dashboard `pr_autotrack_reader` plumbing, and the `tracedecay branch autotrack` command.
- Generated contracts (the exact interface L4 Task 4 consumes): `ProjectSettingsPayloadV1` loses `pr_autotrack`, `ProjectEditableSettingsV1` and `ProjectSettingsPatch` lose `sync`, and `SyncSettingsV1`, `SyncSettingsPatch`, `PrAutoTrackPayloadV1`, and `PrAutoTrackEntryV1` disappear from `dashboard/src/contracts/`.

- [ ] **Step 1: Add the reproduction `enabled_profile_with_many_open_prs_creates_checkouts_today`** to the existing `pr_autotrack/tests.rs`: with a fake `gh` on `PATH` listing 12 same-repo open PRs and the setting enabled, run one reconcile and assert 10 managed worktrees plus one `gh` call per cycle, recorded through the fake's invocation log; this is the before-measurement.
- [ ] **Step 2: Run** `scripts/require-exact-test.sh bazel test //crates/tracedecay:unit_test --test_output=all --test_arg=daemon::pr_autotrack::tests::enabled_profile_with_many_open_prs_creates_checkouts_today --test_arg=--exact`; expect pass with a nonzero count, proving the feature creates the checkouts today (this is the defect reproduction).
- [ ] **Step 3: Add `bootstrap_starts_no_pr_poll_task_for_legacy_enabled_profiles`**: build the daemon engine over a store holding an enabled legacy `pr-autotrack.json` and 12 fake open PRs, bootstrap, and assert no `gh` invocation, no new worktree, and no poll task.
- [ ] **Step 4: Add `explicit_branch_request_still_activates_and_reports_honest_state`** in `branch_worktrees/tests.rs`, covering the three outcomes: ready, unavailable scheduler, and refused head. Assert a refused or cancelled activation publishes no usable generation and leaves an existing worktree intact.
- [ ] **Step 5: Add `legacy_managed_prs_retire_through_the_lifecycle_authority`**: with two clean managed PRs recorded, `retire_legacy_pr_worktrees` removes both stores, worktrees, tracking branches, and refs, empties `pr-autotrack.json`, and leaves shared facts and session history untouched.
- [ ] **Step 6: Add `dirty_foreign_or_leased_legacy_pr_artifacts_are_preserved_and_reported`**: with one dirty worktree, one whose recorded path differs from the owned `pr-worktrees/pr-N` path, one under a held branch lifecycle lease, and one cancelled mid-pass, assert each is preserved byte-for-byte with its matching `PreservedReason`, remains in the state file, and appears in `legacy_pr_artifacts_finding`; no recursive deletion by name prefix.
- [ ] **Step 7: Add `snapshot_holding_retired_autotrack_keys_is_reset_required`** in `store/tests/retired_autotrack_keys.rs` (registered in `store/tests/mod.rs`). Before any production edit, capture `fixtures/configuration-registry-autotrack.sql`: a byte-exact dump of an isolated store canonically initialized by the unmodified registry, documented with its source tip as the revision-3 fixture is. The revision-3 fixture cannot serve, because it already fails closed on its retired `query.default_collection.v1` entry. The test seeds the dump, converges it through `converge_registered_registry_shape`, and asserts typed configuration `ResetRequired`, unchanged stored entries, and that a setter call for either autotrack key is refused as unknown (Settled rulings, #4419 scope and settings).
- [ ] **Step 8: Run** `bazel test //crates/tracedecay:unit_test //crates/tracedecay-global-db:unit_test --test_output=errors`; expect RED for the Step 3, 4, 5, 6, and 7 tests (the baseline still polls, has no retirement pass, converges the captured store cleanly, and accepts both keys), each listed as failing, not skipped.
- [ ] **Step 9: Move the manual-branch functions** and their tests into `branch_worktrees.rs` and `branch_worktrees/tests.rs`, switch `branch_add.rs` and `branch_admin.rs` to the new module path, and delete `pr_autotrack.rs`, `pr_autotrack/runtime.rs`, and `pr_autotrack/tests.rs` once Steps 1-2 are recorded as evidence.
- [ ] **Step 10: Implement `retire_legacy_pr_worktrees`** and replace the bootstrap spawn with one native task joined at shutdown through the engine slot renamed `legacy_pr_retirement_task`; remove the poll loop plumbing in `engine.rs`, `shutdown.rs`, and `bootstrap.rs`, then delete the discovery and reconcile code from `pr_tracking.rs`.
- [ ] **Step 11: Retire the settings and surfaces**: remove both keys and their constants, defaults, parsing, patch fields, and `MIN_AUTO_TRACK_PR_POLL_SECS`; delete the then-empty `SyncSettingsV1`, `SyncSettingsPatch`, and `SyncSettingsPatchV1`; remove `PrAutoTrackPayloadV1`, `PrAutoTrackEntryV1`, the reader plumbing through dashboard API and MCP, the `branch autotrack` clap variants and handler, and replace `pr_autotrack_state_failure` with `legacy_pr_artifacts_finding`. Do not add the keys to any additive-convergence list, alias, or value mapping.
- [ ] **Step 12: Run** `bazel test //crates/tracedecay:unit_test //crates/tracedecay-application:pr_tracking //crates/tracedecay-application:unit_test //crates/tracedecay-global-db:unit_test //crates/tracedecay-configuration:unit_test //crates/tracedecay-dashboard-api:unit_test //crates/tracedecay-cli:tracedecay_unit_test --test_output=errors`; expect all pass with the Step 3, 4, 5, 6, and 7 tests run and the old autotrack tests gone.
- [ ] **Step 13: Run** `bazel test //crates/tracedecay:daemon_suite //crates/tracedecay:dashboard_api_test //crates/tracedecay-cli:core_cli_suite --test_output=errors`; expect pass through the real daemon, dashboard handler, and CLI routes.
- [ ] **Step 14: Regenerate** with `cd dashboard && pnpm run contracts:generate`.
- [ ] **Step 15: Verify generation** with `cd dashboard && pnpm run contracts:check`; expect exit 0. Then run `cd dashboard && pnpm run typecheck` and record its errors. They must name only the L4 Task 4 paths (the settings editor under `dashboard/src/workspaces/settings/` and its tests). That list is the hand-off for Gate G3. An error in any other path is a Task 4 defect to fix before Step 17.
- [ ] **Step 16: Measure** (alone, in a granted slot) by re-running the Step 1 fixture against the final tree and recording, from the fake `gh` log and `git worktree list`, that background `gh` calls and created worktrees dropped from the Step 2 counts to zero; report both counts in the PR body.
- [ ] **Step 17: Commit** `feat(branch): retire automatic all-PR worktree indexing`. The commit removes a user-visible command and resets configuration, so it uses a release-note type, not `simplify`. Its body carries the upgrade-impact paragraph: a configuration holding `sync.auto_track_pr_branches.v1` or `sync.auto_track_pr_poll_secs.v1` returns `ResetRequired`; reset the project configuration and re-apply custom settings. Push, open the PR with `Refs #4419` and the Step 15 error list, and report the tip SHA to the coordinator for L4 Task 4. The PR lists the removed task, state-file writer, settings, and exclusive files.
- [ ] **Step 18: Re-verify after the Gate G3 fold.** Once the coordinator has merged L4 Task 4's child PR into this branch and Gates G1 and G2 hold, run `cd dashboard && pnpm run contracts:check && pnpm run typecheck && pnpm test && pnpm run build` and rerun the Step 12 and Step 13 targets on the combined tip; expect exit 0 and a nonzero Vitest count. The PR switches to `Fixes #4419` only on the coordinator's confirmation.

### Task 5: #4229 measured opt-in niche grammar sets

**Files:**
- Modify (only if the Step 7 rule selects opt-in): `crates/tracedecay/Cargo.toml`, `crates/tracedecay-code-index/Cargo.toml`, `crates/tracedecay-code-extraction/Cargo.toml`, `crates/tracedecay-cli/Cargo.toml`, `crates/tracedecay-code-index/src/languages.rs`, and the supported-language documentation page.
- Modify (same condition): the release workflow or inventory script that names artifact profiles.
- Test: `crates/tracedecay-code-index/src/languages.rs` (module tests), `crates/tracedecay-code-extraction/tests/main`.

**Interfaces consumed:** the existing `lite`, `medium`, `full`, and `lang-*` features and the conditional `ts_provider` registration.

**Interfaces produced:** if opt-in, the default build enables `lite` plus the grammars the evidence justifies, `full` remains the explicit extras profile, and a language omitted from the default artifact reports a typed unsupported coverage state. If no change, none; the task produces only the closure evidence.

- [ ] **Step 1: List the per-profile Bazel targets** with `bazel query 'kind(rust_library, //crates/tracedecay-code-extraction:all)'` and `bazel query 'kind(rust_test, //crates/tracedecay-code-extraction:all)'`; expect the generated `lite`, `medium`, and `full` feature variants to appear, and record their labels for Steps 2-4.
- [ ] **Step 2: Measure build cost** (alone, in a granted slot): for each profile, set the `crates/tracedecay/Cargo.toml` default features, run `bazel run //scripts:gen_builds`, then `bazel clean` and `bazel build //crates/tracedecay-cli:tracedecay --profile=<tmp>/<profile>.json` for the clean time, and rebuild once for the warm time. Repeat the clean and warm builds once more per profile so Step 7 can compare each difference with the run-to-run spread; record every run.
- [ ] **Step 3: Measure artifact and startup cost** by recording `stat` size of `bazel-bin/crates/tracedecay-cli/tracedecay` per profile and the wall time of `tracedecay --version` and of daemon-less `tracedecay status` on an isolated empty project, five runs each.
- [ ] **Step 4: Measure extraction quality** by running the Step 1 extraction test labels for the core and extras variants with `--test_output=errors`; record per-language pass counts, and run each niche language's fixture through `tracedecay` index on the isolated project to record whether the resolved-edge count is nonzero.
- [ ] **Step 5: Revert the investigative overrides**, restore the committed default features, rerun `bazel run //scripts:gen_builds`, and confirm `git diff --stat` shows no measurement edits; nothing from Steps 2-4 is committed as an artifact.
- [ ] **Step 6: Gather demand evidence** from the repository's own tracker and benchmark corpora only: count open and closed issues mentioning each niche language (`gh issue list --search "<language>" --state all --json number --jq length`) and which languages `benchmark_data` fixtures exercise. No operator or telemetry data is used.
- [ ] **Step 7: Apply the decision rule** to the Step 2-6 table. A grammar set moves to opt-in only when both conditions hold:
  - removing it from the default lowers artifact size, clean or warm build time, or startup by more than that metric's run-to-run spread (artifact size is deterministic);
  - it lacks demonstrated demand (no tracker issue other than #4229 names its languages, and no `benchmark_data` fixture exercises them) or lacks demonstrated quality (a failing Step 4 extraction test, or zero resolved edges on its fixture).

  Every other set stays default. If no set qualifies, the outcome is no change. Record the rule's inputs and outcome in the PR body or closure draft, and send them to the coordinator, who verifies the evidence. The only escalation to the user is a genuine product preference the rule cannot settle, such as evidence that splits within one set. Do not wait on any other decision.
- [ ] **Step 8: If the rule yields no change,** post the criterion map (fixture, machine, spans or sampling, before and after, demand evidence, quality evaluation) as the closure comment draft, make no code commit, and stop. The issue closes as completed only if every requested measurement is cited (design closure rule 7).
- [ ] **Step 9: If opt-in, add `omitted_grammar_reports_unsupported_not_empty_success`** to the `languages.rs` module tests, gated `#[cfg(not(feature = "lang-cobol"))]`, asserting a `.cob` file returns the typed unsupported/limited coverage state and never a fabricated empty semantic success.
- [ ] **Step 10: If opt-in, add `extras_profile_registers_and_extracts_every_advertised_grammar`** gated `#[cfg(feature = "lang-cobol")]`, iterating the extras language list and asserting each registers a provider and extracts its fixture symbol.
- [ ] **Step 11: If opt-in, run** each test through `scripts/require-exact-test.sh` against the default-feature unit test target and the extras variant label from Step 1; expect the omitted test RED before Step 12 and both GREEN after.
- [ ] **Step 12: If opt-in, change the default features** in the four manifests so the default exposes exactly the set Step 7 kept, keep `full` as the extras profile, and rerun `bazel run //scripts:gen_builds`.
- [ ] **Step 13: If opt-in, update** the supported-language documentation and release artifact metadata so each artifact states its languages and limits, with no semantic capability claimed for a registered-only parser.
- [ ] **Step 14: If opt-in, run** `bazel test //crates/tracedecay-code-extraction:unit_test //crates/tracedecay-code-index:unit_test //crates/tracedecay-code-index:code_index_suite --test_output=errors` and `bazel build //crates/tracedecay-cli:tracedecay`; commit `feat(code-index): make niche grammar sets opt-in`.

### Task 6: #4226 async Rust SDK lifecycle core

**Files:**
- Modify: `crates/tracedecay-sdk/Cargo.toml`, `src/client.rs`, `src/observe.rs`, `src/request_control.rs`, `src/semantic.rs`, `src/lib.rs`
- Test: `crates/tracedecay-sdk/tests/sdk_suite/client.rs`, `production_daemon.rs`, `semantic_replay.rs`, `facade.rs`, `remote_journey.rs`, and `crates/tracedecay-sdk/tests/remote_client_proxy.rs` (its local lifecycle client)
- Modify (callers): `crates/tracedecay/tests/runtime_acceptance_suite/runtime_surface_acceptance.rs`, `crates/tracedecay/tests/transport_acceptance_suite/typed_terminal_restart_acceptance/transport_boundaries.rs`, `crates/tracedecay/tests/daemon_suite/advanced_workflow_journey_test.rs`, `crates/tracedecay/tests/daemon_suite/advanced_workflow_journey/task_session.rs`, `crates/tracedecay/tests/daemon_suite/advanced_workflow_journey/daemon_fixture.rs`, `crates/tracedecay/src/daemon/production_harness/configuration_idempotency_journey_test.rs`

`remote_client.rs` (the enrolled-remote blocking client used by `tracedecay remote` and daemon remote join) is a separate authority and is not changed; it imports only `ClientError`, `ProblemError`, and `problem_error` from `client.rs`, which keep their shapes.

**Interfaces consumed:** `reqwest` async client (workspace dependency, `stream` feature), workspace `futures-util` and `tokio`, `OperationRequestOptions`, `StreamOptions`, `is_terminal_event`.

**Interfaces produced:** `async fn` versions of `Client::{execute, execute_mcp, execute_with_options, cancel_operation, stream_operation}` with today's parameters and results; `McpToolTransport::call_tool` returning a boxed `Send` future; `OperationStream::next(&mut self) -> Option<Result<StreamEvent, ClientError>>` replacing the `Iterator` impl. Dropping a future or an `OperationStream` closes its transport and requests no server-side cancellation. `reqwest::blocking` is no longer used by `Client`.

- [ ] **Step 1: Add the conformance tests** to `sdk_suite/client.rs` as async tests against the fixture server: `every_terminal_event_ends_the_stream` (a table over `is_terminal_event`'s terminal set including `unavailable`), `reconnects_are_bounded_across_connection_failures`, `dropping_an_operation_stream_closes_its_connection`, `cancelling_a_request_future_closes_the_socket`, and `sequence_numbers_above_two_pow_53_round_trip_exactly`.
- [ ] **Step 2: Run** `bazel test //crates/tracedecay-sdk:sdk_suite --test_output=errors`; expect RED as a compile failure (the tests await a blocking API), which is the baseline for the port.
- [ ] **Step 3: Update the Cargo manifest**: switch `reqwest` to `["json", "rustls", "stream"]`, add workspace `futures-util` and `tokio` (`rt`, `time`, `sync`), and keep `rustls` and `tempfile` for tests.
- [ ] **Step 4: Repin and regenerate the build files** with `CARGO_BAZEL_REPIN=workspace bazel fetch --repo=@c` and then `bazel run //scripts:gen_builds`; expect a clean exit and a diff confined to the lockfile and generated `BUILD.bazel` files.
- [ ] **Step 5: Port the client** in `client.rs`, `observe.rs`, `request_control.rs`, and `semantic.rs` to async, with SSE framing, bounded reconnect, and terminal classification in one place; keep the bounded-frame limits and typed `ClientError` variants unchanged.
- [ ] **Step 6: Migrate every in-repo caller** of `Client` in the listed files to `.await`. Find them with `rg -n 'tracedecay_sdk::client::(Client\b|\{[^}]*\bClient\b)' crates`, which also matches brace imports; the compile in Step 7 is the proof that no blocking caller remains.
- [ ] **Step 7: Run** `bazel test //crates/tracedecay-sdk:sdk_suite //crates/tracedecay-sdk:unit_test //crates/tracedecay-sdk:remote_client_proxy --test_output=errors`; expect all pass with the five Step 1 tests run.
- [ ] **Step 8: Run** `bazel test //crates/tracedecay:runtime_acceptance_suite //crates/tracedecay:transport_acceptance_suite //crates/tracedecay:daemon_suite --test_output=errors`; expect pass through the real daemon.
- [ ] **Step 9: Regenerate** with `cd dashboard && pnpm run contracts:generate`, then verify with `pnpm run contracts:check`; expect exit 0 and no generated diff, proving the async port did not touch the contract authority.
- [ ] **Step 10: Commit** `feat(sdk): run the operation lifecycle client on an async core`.

### Task 7: #4226 napi-rs binding and thin TypeScript facade

**Files:**
- Create: `crates/tracedecay-sdk-node/Cargo.toml`, `build.rs`, `src/lib.rs`, `src/client.rs`, `src/stream.rs`
- Modify: `Cargo.toml` (workspace member), `scripts/bazel/gen_builds.py`, `scripts/BUILD.bazel` (`gen_builds_test`)
- Modify: `sdks/typescript/src/client.ts`, `src/index.ts`, `package.json`, `BUILD.bazel`, `README.md`
- Create: `sdks/typescript/src/errors.ts`, `src/native.ts`
- Delete: `sdks/typescript/src/request-control.ts` and the SSE, retry, terminal-classification, and canonical-receipt code in `client.ts`
- Modify (only if the facade needs new generated descriptors): `crates/tracedecay-sdk/src/codegen.rs`
- Test: `sdks/typescript/test/client.test.ts`, `client.body.test.ts`, `stream-cursor.test.ts`, `stream-disposal-error.test.ts`, `sse-framing.test.ts`, `native-event-loop.test.ts`, `crates/tracedecay-sdk-node/src/lib.rs` (module tests), `scripts/bazel` generator test

**Interfaces consumed:** Task 6's async `Client`, the generated `operations.ts` descriptors and types.

**Interfaces produced:**
- `#[napi] struct NativeClient` with `#[napi(constructor)] new(options: NativeClientOptions) -> napi::Result<Self>`; `#[napi] async fn execute(&self, operation: String, request_json: String, options: NativeRequestOptions, signal: Option<AbortSignal>) -> napi::Result<String>`; `#[napi] fn open_stream(&self, operation_id: String, options: NativeStreamOptions) -> napi::Result<NativeStream>`; `NativeStream::{next() -> Option<NativeStreamEvent> (async), close()}`. Stream sequence numbers cross as decimal strings and the facade returns the existing public type. MCP callbacks use a `ThreadsafeFunction`.
- TypeScript: `createClient(options): TraceDecayClient` and the typed error classes keep their public names; `ClientOptions.fetch` is removed with a typed `TraceDecayInvalidOptionError` for callers who pass it, a package version bump from 0.15.0 to 0.16.0, and a README migration section (the explicit versioned migration for the one customization a native transport cannot honor).
- `gen_builds.py` emits a `rust_shared_library` for a `crate-type = ["cdylib"]` package and a genrule that copies it to `tracedecay-sdk.<platform>.node`.

- [ ] **Step 1: Add the generator failure test** `cdylib_package_generates_a_shared_library_target` to the `gen_builds_test` fixtures, using a minimal fixture workspace with one `cdylib` crate.
- [ ] **Step 2: Run** `bazel test //scripts:gen_builds_test --test_output=errors`; expect RED because the generator emits only `rust_library` and `rust_binary`.
- [ ] **Step 3: Teach `gen_builds.py` the cdylib case** and rerun the Step 2 command; expect it to pass with a nonzero test count.
- [ ] **Step 4: Add the TypeScript conformance suite** against the not-yet-built addon: terminal states, bounded reconnect, typed errors, numeric boundaries above `Number.MAX_SAFE_INTEGER`, early iterator `return()`, backpressure, and shutdown, ported from the existing tests so the same assertions run through the native client.
- [ ] **Step 5: Add `aborted_requests_close_the_socket_and_never_block_the_event_loop`** to `native-event-loop.test.ts`: against a local server that stalls a response and a stream, run a 10 ms `setInterval` counter and assert at least 20 ticks elapse during a 300 ms stalled read, then abort and assert the server observes the connection close within 200 ms, for both `execute` with an `AbortSignal` and early `return()` of a stream.
- [ ] **Step 6: Run** `bazel test //sdks/typescript:unit_test --test_output=errors`; expect RED because no addon exists, with the Step 4 and Step 5 tests listed as failing, not skipped.
- [ ] **Step 7: Create the addon crate** with `napi`, `napi-derive`, and `napi-build` (Settled rulings, #4226 customization and dependencies), the Interfaces above, and a Tokio runtime owned by the addon; add the workspace member and run `CARGO_BAZEL_REPIN=workspace bazel fetch --repo=@c`.
- [ ] **Step 8: Regenerate the build files** with `bazel run //scripts:gen_builds`; expect a diff confined to the lockfile, root build metadata, and `crates/tracedecay-sdk-node/BUILD.bazel`.
- [ ] **Step 9: Build the addon** with `bazel build //crates/tracedecay-sdk-node:tracedecay_sdk_node`; expect a shared library artifact and the staged `.node` copy.
- [ ] **Step 10: Cut the facade over**: rewrite `client.ts` to build methods from the generated descriptors and call `NativeClient`, load the addon through `native.ts`, move the typed error classes to `errors.ts`, and delete `request-control.ts` and the superseded SSE, retry, and receipt code.
- [ ] **Step 11: Wire the addon into the Bazel test** by adding the `.node` target to `sdks/typescript/BUILD.bazel` `data` for `unit_test`, `dist`, and `typed_terminal_live`.
- [ ] **Step 12: Run** `bazel test //sdks/typescript:unit_test //sdks/typescript:typecheck --test_output=errors`; expect all Step 4 and Step 5 tests pass.
- [ ] **Step 13: Run** `bazel test //crates/tracedecay-sdk-node:unit_test //crates/tracedecay-sdk:sdk_suite --test_output=errors` and `bazel run //sdks/typescript:typed_terminal_live`; expect pass against a real daemon, covering every terminal state end to end.
- [ ] **Step 14: Measure the net change** in handwritten TypeScript, new Rust, dependency count, and installed package size with `git diff --shortstat origin/master...` on the handwritten paths and `bazel cquery` for the dependency delta; record the numbers in the PR body.
- [ ] **Step 15: Regenerate** with `cd dashboard && pnpm run contracts:generate`, then verify with `pnpm run contracts:check`; expect exit 0.
- [ ] **Step 16: Commit** `feat(sdk): bind the Node client to the shared Rust core`.

### Task 8: #4226 platform addon packaging and distribution conformance

**Files:**
- Create: `scripts/stage-sdk-native-packages.py`, `plugin/chatgpt-extension/test/native-addon.test.ts`
- Modify: `sdks/typescript/package.json`, `sdks/typescript/test/installed-package.test.ts`, `scripts/check-sdk-publish-workflow.py`, the SDK publish workflow under `.github/workflows`, `plugin/chatgpt-extension/scripts/build.mjs`, `plugin/chatgpt-extension/BUILD.bazel`, `scripts/assemble-release-artifacts.sh`, `scripts/check-packaged-plugin-manifests.py`

**Interfaces consumed:** Task 7's staged `.node` artifact, the release workflow's existing OS/architecture matrix (Settled rulings, #4226 customization and dependencies), the existing `npm pack` publish path.

**Interfaces produced:** per-platform packages `@tracedecay/sdk-<platform>-<arch>` listed as `optionalDependencies` of `@tracedecay/sdk`, each carrying exactly one `tracedecay-sdk.node`; `scripts/stage-sdk-native-packages.py <artifact-root> <output-root>` builds those package directories; the packaged ChatGPT plugin carries the host platform's addon at `embedded/native/<platform>-<arch>/tracedecay-sdk.node`, staged by release assembly rather than committed, with `server.mjs` resolving it by that path.

- [ ] **Step 1: Add `offline_plugin_loads_the_staged_addon_without_workspace_node_modules`** to `native-addon.test.ts`: copy `embedded/` plus a staged addon into an empty directory with no `node_modules`, launch `server.mjs` against the isolated daemon fixture, and assert a tool call succeeds and the process resolves the addon from `embedded/native/`.
- [ ] **Step 2: Add the installed-package assertions** to `installed-package.test.ts`: the packed `@tracedecay/sdk` tarball contains no TypeScript client runtime beyond the facade, lists the per-platform `optionalDependencies`, and a clean consumer install on the current platform loads the matching addon and completes one real daemon call.
- [ ] **Step 3: Run** `bazel test //plugin/chatgpt-extension:serve_client_test --test_output=errors` and `cd sdks/typescript && pnpm run test:installed`; expect RED for both new assertions because nothing stages or resolves a platform addon yet.
- [ ] **Step 4: Add `scripts/stage-sdk-native-packages.py`** and the `optionalDependencies` entries, with a unit test of the staged layout beside it.
- [ ] **Step 5: Update `plugin/chatgpt-extension/scripts/build.mjs`** to keep the bundle single-file by marking the native loader external, and leave the `--check` comparison to `app.html` and `server.mjs` only.
- [ ] **Step 6: Update release assembly and the manifest check** (`assemble-release-artifacts.sh`, `check-packaged-plugin-manifests.py`) so each packaged plugin carries and verifies its platform addon, and update the SDK publish workflow and `check-sdk-publish-workflow.py` for the per-platform packages.
- [ ] **Step 7: Run** `python3 scripts/check-sdk-publish-workflow.py` and `python3 scripts/check-packaged-plugin-manifests.py`; expect exit 0 with the new package set verified.
- [ ] **Step 8: Run** `bazel test //plugin/chatgpt-extension:adapter_test //plugin/chatgpt-extension:serve_client_test //plugin/chatgpt-extension:bridge_lifecycle_test //plugin/chatgpt-extension:typecheck --test_output=errors`, `cd plugin/chatgpt-extension && pnpm run check:embedded`, and `cd sdks/typescript && pnpm run test:installed`; expect all pass, including the two Step 1-2 additions.
- [ ] **Step 9: Regenerate** with `cd dashboard && pnpm run contracts:generate`, then verify with `pnpm run contracts:check`, after the Gate G4 rebase; expect exit 0 and any diff from other lanes' merged contract changes committed.
- [ ] **Step 10: Rerun** `bazel test //sdks/typescript:unit_test //crates/tracedecay-sdk:sdk_suite //crates/tracedecay-sdk-node:unit_test --test_output=errors` and `bazel build //...` after the rebase; expect all pass with nonzero counts.
- [ ] **Step 11: Commit** `build(sdk): stage platform addons for npm and plugin delivery`, and open the PR only once Gate G4 is satisfied. The PR says `Refs #4226`; the coordinator closes it after confirming every acceptance line, including the interruption and event-loop evidence from Task 7.
