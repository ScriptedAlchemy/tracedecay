# L6 Follow-up issue closure implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close #4593, #4416, #4423, and #4440 on current `master` with cited, non-vacuous evidence, adding only the production-journey pins and the one residual simplification each issue still needs. Deliver #4374's fixture measurement and operator runbook draft, and leave #4374 `pending-long-acceptance`.

**Architecture:** Four of the five issues already have their production change merged (#4801, #4549/#4790, #4817, #4860/#4825). Each task therefore pins the issue's acceptance criteria through the real route that owns them, adds the one missing pin or residual simplification, and produces a criterion map the coordinator can close from. #4374 is the only issue whose acceptance is a measurement, and that measurement needs the operator's actual `~/.codex` corpus of about 2,010 rollouts. Its task builds a sized fixture and records span-attributed numbers as supporting evidence, then drafts the runbook for the coordinator's separately approved, read-only operator run (design, Long-running acceptance). The fixture never closes #4374.

**Tech Stack:** Rust 2024 (Bazel only), `tracing` spans with `TRACEDECAY_SPAN_TIMINGS=1`, React/TypeScript dashboard (pnpm + vitest), git submodule fixtures.

**Spec:** `docs/superpowers/specs/2026-11-16-outstanding-issues-delivery-design.md`

## Global constraints

- **Bazel only.** Every build, test, check, and run goes through Bazel. Never invoke `cargo`. pnpm is only for `dashboard/` (`pnpm run typecheck`, `pnpm test`, `pnpm run build`, `pnpm run contracts:check`).
- **Green and red runs.** A green named run is `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test <one target> --test_output=all --test_arg=<full module path> --test_arg=--exact`. The helper proves only a pass: when the command fails it exits with that status and checks no signature. An expected red invokes Bazel directly as `bazel test <one target> --test_output=all --test_arg=<full module path> --test_arg=--exact`. It counts as red only when Bazel exits nonzero and the log shows the step's exact signature: a rustc `error[E…]` line naming the stated missing symbol, or `test <full module path> ... FAILED` with the stated assertion text and `test result: FAILED. 0 passed; 1 failed`. No substring filter is evidence. Every unfiltered target run whose counts or test names are recorded uses `--test_output=all`, because Bazel prints a passing test's log only with that flag. Quote counts and signature lines in the lane report.
- **No compatibility, no backups.** A shape change migrates every caller and deletes the old shape in the same slice. No aliases, dual writes, migrations, or secondary copies of project or operator data.
- **Typed failures only.** No `unwrap`, `expect`, `panic`, silent fallback, empty success, or `allow(dead_code)` in production code. Tests may assert and unwrap.
- **Generated sources are never hand-edited.** `dashboard/src/contracts/`, `sdks/typescript/src/`, and `crates/tracedecay-sdk/src/operations.rs` are regenerated with `pnpm run contracts:generate` and verified with `pnpm run contracts:check`. No task in this plan changes a schema or an operation (Task 2's setting key and pin are not generated contract types), so no regeneration is expected; if a task's diff reaches a generated file, regenerate instead of editing it and run `pnpm run contracts:check` from `dashboard/`.
- **Isolated state.** Every daemon, profile, home, and project used for reproduction or measurement lives under a lane-owned temporary directory with explicit `HOME`, profile, and socket overrides. Never read or write `~/.tracedecay`, `~/.codex`, `~/.claude`, `~/.agents`, or installed services. The #4374 operator-corpus run is the coordinator's, not the lane's (Task 5, Step 7).
- **Commits.** `<type>(<scope>): <subject>`, header ≤ 72 characters, commitlint-clean. No issue numbers in commit subjects; issue numbers appear only in the PR body as `Refs #N`.
- **Green-first verification tasks.** Tasks 1, 3, and 4 close already-merged fixes. Their new pins are expected to **pass** on current `master`; a red result means the merged fix does not cover that criterion and is a **blocker to report**, never something to patch inside the task. Task 2 carries the red-first type change; Task 5's first evidence is the measurement itself.
- **Cross-lane gates.** #4374 (Task 5) shares session discovery and admission paths with L3's #4861 and #4756, which are still open. Partial fixes are on `master` at `58ccb41d66` (#4864 for #4861; #4857 and #4793 for #4756), but L3's program PRs for both issues have not merged. Task 5 may build its fixture and take a first measurement on a branch from `master`. It integrates only after both L3 PRs merge, and it rebases and repeats Step 2 on that `master`. No task in this plan edits L3's discovery module, L2's registry resolution, or L5's SDK sources.
- **Shared paths.** Task 2 holds the configuration registry files (`crates/tracedecay-global-db/src/configuration/registry.rs`, `crates/tracedecay-domain/src/configuration.rs`, `crates/tracedecay-domain/src/lib.rs`, and the configuration store tests) first. L5 #4419 edits them only after Task 2's PR merges. Task 2's new `dashboard/src/viz/proximity/ProximityPanel.dom.test.tsx` is an L6-created file under L4's dashboard area; L4 does not edit it while #4416 is open. Task 3 is the last of three owners of `crates/tracedecay-application/src/advisory/runtime/local_cycle_tests.rs` (design, Shared files): L1 #4448 edits it first and L5 #4436 second. Task 3 starts only after both PRs have merged, on a branch from the `master` that contains them. If they have not merged when Task 3 is next, run Task 4 first.
- **Heavy runs.** Tasks 4 and 5 are measured runs. Take a coordinator slot, run them alone, and never loosen a bound to make a number look better.

## Review focus

1. **A registered submodule whose binding is revoked between discovery and read.** A parent gitlink removed, a `.git` marker retargeted, or a child worktree replaced must refuse the read, not serve the previous child bytes. Pinned by `lazy_submodule_walk_refuses_retargeted_marker` and `submodule_revocation_during_capture_refuses_snapshot_and_preserves_publication` (Task 1).
2. **A proximity read whose graph generation moved under it.** A read that resolves encounter addresses against a generation the code index no longer holds must report stale or partial, never a clean zero. Pinned by `expired_evidence_returns_typed_stale_result` and `read_page_preserves_encounter_intervals_and_relation_evidence` (Task 2).
3. **A local editor cycle with a configured provider that never answers.** With CI configured and no retained evidence, a provider read that blocks forever must not consume the local cycle's deadline or cancel its diagnostics, and the cycle must never claim a complete pass. Pinned by `local_cycle_completes_with_a_configured_provider_that_never_answers` (Task 3).
4. **A configuration that still carries the retired numeric threshold.** It must return typed configuration `ResetRequired`, and a setter for the retired key must be refused as unknown. No value maps to the new boolean policy. Pinned by `snapshot_holding_the_retired_proximity_threshold_is_reset_required` (Task 2).
5. **A TypeScript project whose compiler is absent, unreadable, or unfamiliar.** The read must report unavailable or incomplete coverage, never republish a clean snapshot from guessed configuration semantics. Pinned by `an_unreadable_configuration_is_a_failure_until_repaired` and `generated_output_files_are_not_project_members` (Task 4).
6. **A Codex corpus re-offered after a restart, with appends and archives.** Unchanged rollouts must not be re-read, an appended window must still be admitted, and an archived rollout must not replay the pass forever. Pinned on the fixture by `codex_reoffer_measurement` and the existing `sessions` re-offer regressions (Task 5). Acceptance on the operator's real corpus is the coordinator's separately approved run, so this pin is supporting evidence only.

---

### Task 1: #4593 submodule opt-in evidence closure

**Files:**
- Test: `crates/tracedecay-code-index/src/source_walk.rs` (add to the existing `mod tests`)
- Test: `crates/tracedecay-code-index/src/grep_search.rs` (add to the existing `mod tests`)
- Test: `crates/tracedecay-code-index/src/ast_grep_search.rs` (add to the existing `mod tests`)

**Interfaces:**
- Consumes: `IndexPathPolicyV1::new(Vec<String>, Vec<String>) -> Result<Self, _>`, `IndexPathPolicyV1::with_submodules(bool) -> Self`, `IndexPathPolicyV1::includes_submodules() -> bool`.
- Consumes: `crate::source_walk::submodules::tests::fixture() -> tempfile::TempDir` (parent repo at `<tmp>/parent`, registered submodule at `app/`, child files `first.rs` and `nested/second.rs`).
- Consumes: `crate::source_walk::source_walk(&Path, Option<&str>, &IndexPathPolicyV1) -> Result<SourceWalk, SourceWalkError>`.
- Consumes: `crate::grep_search::search_tree_with_cancel(&Path, &GrepSearchQuery, &IndexPathPolicyV1, impl Fn() -> bool + Sync) -> Result<GrepSearchResult, GrepSearchError>`; `GrepSearchResult.hits: Vec<GrepSearchHit>` with `GrepSearchHit.file: Arc<str>`.
- Consumes: `crate::ast_grep_search::search_tree(&Path, &str, Option<&str>, Option<&str>, usize, &IndexPathPolicyV1) -> Result<AstGrepSearchResult, AstGrepSearchError>`; `AstGrepSearchResult.matches: Vec<AstGrepSearchMatch>` with `AstGrepSearchMatch.file: Arc<str>`.
- Produces: nothing. This task adds coverage only.

- [ ] **Step 1: Confirm the #4801 merge commit is on `master`**

Run: `git merge-base --is-ancestor 97caca9f0e53be3649019a469bbb165a1e55ebce HEAD && echo ancestor`

Expected: prints `ancestor`. Anything else means the fix is not on the branch under test; stop and report it.

- [ ] **Step 2: Write the linked-worktree pin**

Add to `mod tests` in `crates/tracedecay-code-index/src/source_walk.rs`, using the module's existing `fixture_git(root, args)` and `walked_files(root, policy)` helpers:

```rust
#[test]
fn linked_worktree_inside_the_parent_stays_pruned_under_submodule_opt_in() {
    let fixture = crate::source_walk::submodules::tests::fixture();
    let parent = fixture.path().join("parent");
    fixture_git(&parent, &["worktree", "add", "-q", "linked"]);
    let policy = IndexPathPolicyV1::new(Vec::new(), Vec::new())
        .expect("empty policy")
        .with_submodules(true);
    let walked = walked_files(&parent, &policy);
    assert!(walked.contains(&PathBuf::from("parent.rs")), "{walked:?}");
    assert!(walked.contains(&PathBuf::from("app/first.rs")), "{walked:?}");
    assert!(
        !walked.iter().any(|path| path.starts_with("linked")),
        "a linked worktree is not a registered submodule: {walked:?}"
    );
}
```

- [ ] **Step 3: Run the linked-worktree pin**

Run: `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-code-index:unit_test --test_output=all --test_arg=source_walk::tests::linked_worktree_inside_the_parent_stays_pruned_under_submodule_opt_in --test_arg=--exact`

Expected: PASS with `1 passed; 0 failed`. A failure means the opt-in admits a linked worktree; stop and report it as a blocker with the failing assertion.

- [ ] **Step 4: Write the grep parity pin**

Add to `mod tests` in `crates/tracedecay-code-index/src/grep_search.rs`:

```rust
#[test]
fn submodule_sources_are_searchable_only_under_the_opt_in() {
    let fixture = crate::source_walk::submodules::tests::fixture();
    let parent = fixture.path().join("parent");
    let mut query = query("pub fn first");
    query.max_results = 10;
    let disabled = search_tree_with_cancel(
        &parent,
        &query,
        &no_exclusions(),
        || false,
    )
    .expect("default grep");
    assert!(
        disabled.hits.iter().all(|hit| !hit.file.starts_with("app/")),
        "{:?}",
        disabled.hits
    );
    let enabled = search_tree_with_cancel(
        &parent,
        &query,
        &no_exclusions().with_submodules(true),
        || false,
    )
    .expect("opt-in grep");
    assert!(
        enabled.hits.iter().any(|hit| hit.file.as_ref() == "app/first.rs"),
        "{:?}",
        enabled.hits
    );
}
```

- [ ] **Step 5: Run the grep parity pin**

Run: `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-code-index:unit_test --test_output=all --test_arg=grep_search::tests::submodule_sources_are_searchable_only_under_the_opt_in --test_arg=--exact`

Expected: PASS with `1 passed; 0 failed`. A failure means grep does not share the submodule walker; report it as a blocker.

- [ ] **Step 6: Write the ast-grep parity pin**

Add to `mod tests` in `crates/tracedecay-code-index/src/ast_grep_search.rs`:

```rust
#[test]
fn submodule_sources_are_parsed_only_under_the_opt_in() {
    let fixture = crate::source_walk::submodules::tests::fixture();
    let parent = fixture.path().join("parent");
    let disabled = search_tree(&parent, "fn first()", Some("rust"), None, 10, &no_exclusions())
        .expect("default ast-grep");
    assert!(
        disabled.matches.iter().all(|hit| !hit.file.starts_with("app/")),
        "{:?}",
        disabled.matches
    );
    let enabled = search_tree(
        &parent,
        "fn first()",
        Some("rust"),
        None,
        10,
        &no_exclusions().with_submodules(true),
    )
    .expect("opt-in ast-grep");
    assert!(
        enabled.matches.iter().any(|hit| hit.file.as_ref() == "app/first.rs"),
        "{:?}",
        enabled.matches
    );
}
```

- [ ] **Step 7: Run the ast-grep parity pin**

Run: `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-code-index:unit_test --test_output=all --test_arg=ast_grep_search::tests::submodule_sources_are_parsed_only_under_the_opt_in --test_arg=--exact`

Expected: PASS with `1 passed; 0 failed`. A failure means ast-grep does not share the submodule walker; report it as a blocker.

- [ ] **Step 8: Run the submodule revocation pins**

Run each pin through the helper by its exact full name, one command per test:

- `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-code-index:unit_test --test_output=all --test_arg=source_walk::submodules::tests::lazy_submodule_walk_refuses_retargeted_marker --test_arg=--exact`
- `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-code-index:unit_test --test_output=all --test_arg=source_walk::submodules::tests::lazy_submodule_walk_refuses_removed_staged_gitlink --test_arg=--exact`
- `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-code-index-runtime:unit_test --test_output=all --test_arg=code_index_scheduler::tests::index_path_policy_tests::submodule_revocation_during_capture_refuses_snapshot_and_preserves_publication --test_arg=--exact`
- `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-code-index-runtime:unit_test --test_output=all --test_arg=code_index_scheduler::tests::index_path_policy_tests::retained_graph_checks_require_a_successful_submodule_probe_each_time --test_arg=--exact`

Expected: each reports `1 passed; 0 failed`. A failure means the merged fix does not refuse a revoked binding; report it as a blocker with the failing assertion.

- [ ] **Step 9: Run the #4801 behavioral targets on current `master`**

Run: `bazel test //crates/tracedecay-code-index:unit_test //crates/tracedecay-code-index-runtime:unit_test //crates/tracedecay-configuration:unit_test --test_output=all`

Expected: all three targets PASS. Record each target's executed test count from its `test result:` line; a target reporting `0 passed` is not evidence. This unfiltered log also lists `source_walk::tests::nested_repository_is_not_part_of_the_parent_source_tree` and `unmounted_files::tests::audit_reports_nested_repository_exclusion_without_claiming_child_code` as `ok`, which corroborates the Step 11 rows that cite them.

- [ ] **Step 10: Commit**

```bash
git add crates/tracedecay-code-index/src/source_walk.rs \
  crates/tracedecay-code-index/src/grep_search.rs \
  crates/tracedecay-code-index/src/ast_grep_search.rs
git commit -m "test(code-index): pin submodule opt-in boundaries"
```

- [ ] **Step 11: Draft the closure comment in the lane report**

The report contains one row per #4593 request, each naming the file and test that proves it on `master`: opt-in setting default off (`crates/tracedecay-configuration/src/config/mod.rs`), descent only into gitlink-plus-`.gitmodules` children (`submodules.rs` tests), linked-worktree and nested-clone pruning (Step 2 and `nested_repository_is_not_part_of_the_parent_source_tree`), shared-walker coverage for grep, ast-grep, and unmounted files (Steps 4 and 6, plus `audit_reports_nested_repository_exclusion_without_claiming_child_code`), and the skip hint surfaced through `unmounted_files` (`EcosystemAudit.note`, which `audit_project` fills from the walk's skipped-nested-repository diagnostic for every ecosystem). The comment cites PR #4801, merge commit `97caca9f0e53be3649019a469bbb165a1e55ebce`, and the Step 9 counts. The coordinator closes the issue.

---

### Task 2: #4416 retire the numeric proximity threshold

The root no-compatibility rule wins over the issue's request to map old threshold values (design Decision 10). This task deletes the numeric setting, its pin, and its bounds completely. One boolean project setting is the explicit hint policy the issue asks for, and no existing setting governs neighborhood hints. A configuration that still holds the retired key returns typed configuration `ResetRequired`.

**Files:**
- Modify: `crates/tracedecay-application/src/advisory/proximity_runtime.rs` (pin type, evaluation entry points, contribution builder, and the existing `mod tests`)
- Modify: `crates/tracedecay-application/src/advisory/mod.rs` (re-export)
- Modify: `crates/tracedecay-application/src/feedback/cycle_production.rs`
- Modify: `crates/tracedecay-domain/src/feedback/proximity.rs` (the setting key constant)
- Modify: `crates/tracedecay-domain/src/configuration.rs` (`CONFIGURATION_SETTING_KEYS_V1`)
- Modify: `crates/tracedecay-domain/src/lib.rs`, `crates/tracedecay-contracts/src/advisory.rs`, `crates/tracedecay-contracts/src/lib.rs` (re-exports)
- Modify: `crates/tracedecay-global-db/src/configuration/registry.rs` (definition, range validation, default and maximum constants, and its tests)
- Modify: `crates/tracedecay-global-db/src/configuration/store/tests/mod.rs` (register the reset test)
- Create: `crates/tracedecay-global-db/src/configuration/store/tests/retired_proximity_threshold.rs`
- Create: `crates/tracedecay-global-db/src/configuration/store/tests/fixtures/configuration-registry-proximity-threshold.sql`
- Modify (test callers): `crates/tracedecay-configuration/src/configuration/operations.rs`, `crates/tracedecay/src/mcp/tools/handlers/configuration_batch_behavior_tests.rs`, `crates/tracedecay/tests/runtime_acceptance_suite/advisory_runtime_acceptance.rs`
- Test: `dashboard/src/viz/proximity/ProximityPanel.dom.test.tsx` (create)

**Interfaces:**
- Produces: `pub const PROXIMITY_RELATIONSHIP_HINTS_SETTING_KEY_V1: &str = "feedback.proximity.relationship_hints.v1"` in `crates/tracedecay-domain/src/feedback/proximity.rs`, registered in `ConfigurationRegistry::core` as a project `ConfigurationValueKindV1::Boolean` setting with default `ConfigurationValueV1::Boolean(true)`. The default keeps the shipped behavior: threshold 7,000 admits the 7,500 neighborhood candidates.
- Produces: `pub struct ProximityPolicyPinV1 { pub configuration_revision: ConfigurationRevisionId, pub configuration_digest: ManifestDigest, pub relationship_hints: bool }` with `from_current_configuration(&ConfigurationCurrentStateV1) -> Option<Self>` (reads only the boolean setting), `new(ConfigurationRevisionId, ManifestDigest, bool) -> Option<Self>`, and `validate(&self) -> bool`.
- Produces: `ProximityRuntimeOwnerV1::evaluate_with_policy_pin(&self, context: &RequestContext, request: &ProximityEvaluationRequestV1, policy: &ProximityPolicyPinV1) -> ProximityRuntimeOutcomeV1`.
- Removes: `PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1` and the `feedback.proximity.risk_threshold` registry definition, `DEFAULT_PROXIMITY_RISK_THRESHOLD_BASIS_POINTS_V1`, `MAX_PROXIMITY_RISK_THRESHOLD_BASIS_POINTS_V1`, the registry's threshold range check, `RELATIONSHIP_HINT_MAX_THRESHOLD_BASIS_POINTS_V1`, `ProximityThresholdPinV1` with `value_basis_points` and `admits_relationship_hints`, and `evaluate_with_threshold_pin`. No `from_legacy_threshold` or other value mapping exists, and the new key joins no additive-convergence list.
- Consumes: the registry convergence in the public `GlobalDbConfigurationControlStore::converge_registered_registry_shape`, which returns `ResetRequired` for a snapshot that holds a key the registry no longer defines.

- [ ] **Step 1: Write the failing policy-pin test**

Migrate the existing `configuration(revision, threshold: Option<u64>)` helper in `mod tests` of `proximity_runtime.rs` to `configuration(revision, relationship_hints: Option<bool>)`, writing `PROXIMITY_RELATIONSHIP_HINTS_SETTING_KEY_V1` as `ConfigurationValueV1::Boolean`. Then add:

```rust
#[test]
fn policy_pin_reads_the_relationship_hint_setting() {
    let default = configuration("configuration.proximity-policy.default", None);
    let disabled = configuration("configuration.proximity-policy.disabled", Some(false));
    let default_pin =
        ProximityPolicyPinV1::from_current_configuration(&default).expect("default policy pin");
    let disabled_pin =
        ProximityPolicyPinV1::from_current_configuration(&disabled).expect("disabled policy pin");
    assert!(default_pin.relationship_hints, "the registry default admits relationship hints");
    assert!(!disabled_pin.relationship_hints);
    assert_ne!(default_pin.configuration_digest, disabled_pin.configuration_digest);
    assert!(default_pin.validate() && disabled_pin.validate());
}
```

- [ ] **Step 2: Write the failing reset test and run it red**

Before any production edit, capture `fixtures/configuration-registry-proximity-threshold.sql`: a byte-exact dump of an isolated store canonically initialized by the unmodified registry, documented with its source tip as the revision-3 fixture is. The revision-3 fixture cannot serve, because it already fails closed on its retired `query.default_collection.v1` entry. In `store/tests/retired_proximity_threshold.rs` (registered as `mod retired_proximity_threshold;` in `store/tests/mod.rs`), add `snapshot_holding_the_retired_proximity_threshold_is_reset_required`. It seeds the dump and calls the public `GlobalDbConfigurationControlStore::converge_registered_registry_shape`. It then takes the error with `.expect_err("a snapshot holding feedback.proximity.risk_threshold must return configuration ResetRequired")`, asserts that error is typed configuration `ResetRequired`, asserts the stored entries are unchanged, and asserts that a setter call for `feedback.proximity.risk_threshold` is refused as unknown. It names the retired key only as a string, so it compiles on the unmodified tree.

Run it before Step 3 adds a test that names a new symbol to the same target:

`bazel test //crates/tracedecay-global-db:unit_test --test_output=all --test_arg=configuration::store::tests::retired_proximity_threshold::snapshot_holding_the_retired_proximity_threshold_is_reset_required --test_arg=--exact`

Expected red: Bazel exits nonzero, and the log shows `test configuration::store::tests::retired_proximity_threshold::snapshot_holding_the_retired_proximity_threshold_is_reset_required ... FAILED`, the panic text `a snapshot holding feedback.proximity.risk_threshold must return configuration ResetRequired`, and `test result: FAILED. 0 passed; 1 failed`. The baseline registry still defines the threshold, so convergence succeeds. Any other failure means the fixture is wrong; fix it before continuing.

- [ ] **Step 3: Write the failing registry test**

In `crates/tracedecay-global-db/src/configuration/registry.rs`, rename `mod proximity_threshold_tests` to `mod proximity_relationship_hints_tests` and replace its `proximity_threshold_has_one_bounded_project_default` with:

```rust
#[test]
fn proximity_relationship_hints_is_one_boolean_project_default() {
    let registry = ConfigurationRegistry::core().expect("registry");
    let key = SettingKey::new(PROXIMITY_RELATIONSHIP_HINTS_SETTING_KEY_V1).expect("key");
    let definition = registry.definition(&key).expect("definition");
    assert_eq!(definition.value_kind, ConfigurationValueKindV1::Boolean);
    assert_eq!(definition.default_value, ConfigurationValueV1::Boolean(true));
    assert_eq!(definition.scope, SettingScopeV1::Project);
    let retired = SettingKey::new("feedback.proximity.risk_threshold").expect("key");
    assert!(registry.definition(&retired).is_err(), "the numeric threshold is retired");
}
```

- [ ] **Step 4: Run the new type and key tests to verify they fail**

Run each directly:

- `bazel test //crates/tracedecay-application:unit_test --test_output=all --test_arg=advisory::proximity_runtime::tests::policy_pin_reads_the_relationship_hint_setting --test_arg=--exact`
- `bazel test //crates/tracedecay-global-db:unit_test --test_output=all --test_arg=configuration::registry::proximity_relationship_hints_tests::proximity_relationship_hints_is_one_boolean_project_default --test_arg=--exact`

Expected red: each Bazel run exits nonzero before any test runs. The application log shows a rustc `error[E…]` line naming `ProximityPolicyPinV1`, and every rustc error it reports is in `advisory/proximity_runtime.rs`. The global-db log shows a rustc `error[E…]` line naming `PROXIMITY_RELATIONSHIP_HINTS_SETTING_KEY_V1`, and every rustc error it reports is in `configuration/registry.rs`. An error in any other file is not this red.

- [ ] **Step 5: Replace the numeric setting and pin**

Replace `PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1` with `PROXIMITY_RELATIONSHIP_HINTS_SETTING_KEY_V1` in `proximity.rs`, `configuration.rs`, and the domain and contracts re-exports. In `registry.rs`, register the boolean definition and delete the threshold definition, its range check, and both threshold constants. In `proximity_runtime.rs`, rename `ProximityThresholdPinV1` to `ProximityPolicyPinV1`, replace `value_basis_points` with `relationship_hints`, read the boolean in `from_current_configuration`, drop the range check from `new` and `validate`, delete `RELATIONSHIP_HINT_MAX_THRESHOLD_BASIS_POINTS_V1` and `admits_relationship_hints`, rename `evaluate_with_threshold_pin` to `evaluate_with_policy_pin`, and read `policy.relationship_hints` in the contribution builder.

- [ ] **Step 6: Migrate every remaining caller and test**

Update `advisory/mod.rs` and `cycle_production.rs` (field `threshold_pin` becomes `policy_pin`) and `advisory_runtime_acceptance.rs` to `ProximityPolicyPinV1` and `evaluate_with_policy_pin`. Rename `threshold_pin_is_immutable_in_cycle_and_refreshes_next_cycle` to `policy_pin_is_immutable_in_cycle_and_refreshes_next_cycle`, with `Some(false)` as its drifted configuration. Make `encounter_policy_preserves_exact_identity_and_hint_admission` iterate over `[true, false]`. Delete `relationship_hints_follow_the_legacy_threshold_mapping`, whose only subject is the numeric mapping. Move `get_resolves_a_registered_setting_missing_from_the_snapshot_to_its_default` in `operations.rs` to the boolean key. Move `configuration_batch_behavior_tests.rs` to another registered unsigned project setting, because the batch behavior under test does not depend on proximity.

- [ ] **Step 7: Run the new tests to verify they pass**

Run each through the helper:

- `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-application:unit_test --test_output=all --test_arg=advisory::proximity_runtime::tests::policy_pin_reads_the_relationship_hint_setting --test_arg=--exact`
- `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-global-db:unit_test --test_output=all --test_arg=configuration::registry::proximity_relationship_hints_tests::proximity_relationship_hints_is_one_boolean_project_default --test_arg=--exact`
- `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-global-db:unit_test --test_output=all --test_arg=configuration::store::tests::retired_proximity_threshold::snapshot_holding_the_retired_proximity_threshold_is_reset_required --test_arg=--exact`

Expected: each reports `1 passed; 0 failed`.

- [ ] **Step 8: Write the read-page retention test**

Add to `mod tests` in `crates/tracedecay-application/src/advisory/proximity_runtime.rs`:

```rust
#[tokio::test]
async fn read_page_preserves_encounter_intervals_and_relation_evidence() {
    let (scope, context) = scope_and_context();
    let evidence = encounter_evidence(&scope);
    let expected = evidence.encounter.clone();
    let authority: SharedCanonicalProximityEvidenceAuthorityV1 =
        Arc::new(FixedEvidence(Some(CanonicalProximityEvidenceBatchV1 {
            evidence: vec![evidence],
            coverage: ProximityCoverageV1::Complete,
            source_generation: CodeGenerationId::new("generation.proximity-read").unwrap(),
            observed_at: UtcMicros(1),
            expires_at: UtcMicros(4),
            omissions: Vec::new(),
        })));
    let runtime = FeedbackProximityReadRuntimeV1::new(scope, authority).unwrap();
    let result = runtime
        .read(&context, &FeedbackProximityReadRequestV1 { observed_at: UtcMicros(2) })
        .await;
    let FeedbackProximityReadResultV1::Complete { page } = result else {
        panic!("expected a complete read: {result:?}");
    };
    assert_eq!(page.encounters, vec![expected]);
}
```

`FixedEvidence` is a test-only `CanonicalProximityEvidenceAuthorityV1` returning the batch it holds; add it beside the existing test doubles in that module.

- [ ] **Step 9: Run the read-page test**

Run: `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-application:unit_test --test_output=all --test_arg=advisory::proximity_runtime::tests::read_page_preserves_encounter_intervals_and_relation_evidence --test_arg=--exact`

Expected: PASS with `1 passed; 0 failed`. This pins retained read-page behavior, so a first-run pass is expected; a failure is a defect to report.

- [ ] **Step 10: Write the Loom panel test**

Create `dashboard/src/viz/proximity/ProximityPanel.dom.test.tsx` asserting that a complete read with one `overlapping_edit` encounter renders the encounter interval (`formatMicrosUtc` of `interval.start` and `interval.end`), the relation label from `proximityLabel`, and no element whose text matches `/risk|score|threshold/i`.

- [ ] **Step 11: Run the dashboard tests**

Run: `pnpm exec vitest run --reporter=verbose src/viz/proximity/ProximityPanel.dom.test.tsx` from `dashboard/`

Expected: exit 0. The verbose log lists the new test as passed, and the summary reports a nonzero `passed` count and `0 failed` for this file. This pins retained Loom behavior, so a first-run pass is expected.

- [ ] **Step 12: Run each cited retained test by its exact name**

Every test the closure map cites runs through the helper by its full name, one command each, with `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test <target> --test_output=all --test_arg=<path> --test_arg=--exact`:

- `//crates/tracedecay-application:unit_test`: `advisory::proximity_runtime::tests::expired_evidence_returns_typed_stale_result`, `advisory::proximity_runtime::tests::encounter_policy_preserves_exact_identity_and_hint_admission`, `advisory::proximity_runtime::tests::policy_pin_is_immutable_in_cycle_and_refreshes_next_cycle`, `advisory::proximity_runtime::authority::tests::empty_neighborhoods_do_not_issue_additional_graph_reads`, `advisory::proximity_runtime::authority::tests::shared_caller_does_not_read_discarded_dependency_hops`, `advisory::proximity_runtime::authority::tests::dependency_neighborhood_reuses_adjacency_between_distinct_seeds`, and `advisory::runtime::local_cycle_tests::local_cycle_admits_retained_evidence_only_for_exact_scope_grant_and_policy`.
- `//crates/tracedecay-configuration:unit_test`: `configuration::operations::tests::get_resolves_a_registered_setting_missing_from_the_snapshot_to_its_default`.
- `//crates/tracedecay:unit_test`: `mcp::tools::handlers::configuration_batch_behavior_tests::configuration_batch_applies_both_settings_and_refuses_the_other_inputs`, the module's only test.

Expected: each reports `1 passed; 0 failed`.

- [ ] **Step 13: Run the owning targets**

Run: `bazel test //crates/tracedecay-application:unit_test //crates/tracedecay-application:application_suite //crates/tracedecay-domain:unit_test //crates/tracedecay-global-db:unit_test //crates/tracedecay-configuration:unit_test //crates/tracedecay:unit_test //crates/tracedecay:runtime_acceptance_suite --test_output=all`, then `pnpm run contracts:check` from `dashboard/`.

Expected: all PASS, each target's `test result:` line reports a nonzero passed count and `0 failed`, and `contracts:check` reports no drift. Record the counts, the retained types (`ProximityContributionV1`, `ProximityInclusionV1`, `ProximityRelationPathV1`), and the removed setting, constants, and field (`value_basis_points`) in the lane report.

- [ ] **Step 14: Commit**

```bash
git add crates/tracedecay-application/src/advisory/proximity_runtime.rs \
  crates/tracedecay-application/src/advisory/mod.rs \
  crates/tracedecay-application/src/feedback/cycle_production.rs \
  crates/tracedecay-domain/src/feedback/proximity.rs \
  crates/tracedecay-domain/src/configuration.rs \
  crates/tracedecay-domain/src/lib.rs \
  crates/tracedecay-contracts/src/advisory.rs \
  crates/tracedecay-contracts/src/lib.rs \
  crates/tracedecay-global-db/src/configuration/registry.rs \
  crates/tracedecay-global-db/src/configuration/store/tests/mod.rs \
  crates/tracedecay-global-db/src/configuration/store/tests/retired_proximity_threshold.rs \
  crates/tracedecay-global-db/src/configuration/store/tests/fixtures/configuration-registry-proximity-threshold.sql \
  crates/tracedecay-configuration/src/configuration/operations.rs \
  crates/tracedecay/src/mcp/tools/handlers/configuration_batch_behavior_tests.rs \
  crates/tracedecay/tests/runtime_acceptance_suite/advisory_runtime_acceptance.rs \
  dashboard/src/viz/proximity/ProximityPanel.dom.test.tsx
git commit -m "feat(proximity): replace the risk threshold with a hint setting" \
  -m "Upgrade impact: a project configuration that holds feedback.proximity.risk_threshold now returns configuration ResetRequired. Reset the project configuration and re-apply custom settings; feedback.proximity.relationship_hints.v1 controls neighborhood hints."
```

The commit removes a user-visible setting and resets configuration, so it uses a release-note type, not `simplify`. After it merges, report the merge to the coordinator, because L5 #4419 edits the same registry files only after this PR merges.

- [ ] **Step 15: Draft the closure comment in the lane report**

The report maps each #4416 acceptance line to evidence: no graph-impact call on the encounter read (the `impact` entry point has no proximity caller; the neighborhood read counts are pinned by `empty_neighborhoods_do_not_issue_additional_graph_reads`, `shared_caller_does_not_read_discarded_dependency_hops`, and `dependency_neighborhood_reuses_adjacency_between_distinct_seeds`), exact-encounter identity and coverage (Step 8 and `encounter_policy_preserves_exact_identity_and_hint_admission`), retained permission, freshness, generation, and configuration checks (`expired_evidence_returns_typed_stale_result`, `local_cycle_admits_retained_evidence_only_for_exact_scope_grant_and_policy`, `policy_pin_is_immutable_in_cycle_and_refreshes_next_cycle`), historical feedback readability (no durable source data is deleted by this task), and Loom retention (Step 10). The legacy threshold-mapping line is superseded by the user's ruling (design Decision 10); the map says so and cites Steps 1 to 3 in its place. It cites merged #4549 and #4790, the Step 12 exact runs, and the Step 13 counts. The coordinator closes the issue.

---

### Task 3: #4423 local feedback isolation evidence closure

**Gate:** start only after L1 #4448 and L5 #4436 have merged, on a branch from the `master` that contains both (Global constraints, Shared paths).

**Files:**
- Test: `crates/tracedecay-application/src/advisory/runtime/local_cycle_tests.rs` (the file is the `advisory::runtime::local_cycle_tests` module itself, with its tests at file level; add one `Harness` helper, migrate the two existing held-read tests to it, and add the new test)

**Interfaces:**
- Consumes: `Harness::open() -> Harness`, `Harness::local(&str) -> AdvisoryCycleRequest`, `Harness::explicit(&str) -> AdvisoryCycleRequest`, `Harness::context(bool) -> RequestContext`, `Harness::control(&RequestContext) -> AdvisoryCycleControl`, `Harness::remote_reads() -> (usize, usize)`, and the `completed` and `provider_state` helpers in the same file.
- Consumes: `AdvisoryRuntime::run_once(&RequestContext, AdvisoryCycleControl, AdvisoryCycleRequest) -> Result<AdvisoryCycleOutcome, FeedbackCycleRuntimeError>`.
- Produces (test-only, same file): `Harness::hold_next_ci_provider_read(&self) -> (oneshot::Receiver<()>, oneshot::Sender<()>)`. It arms `HeldLiveCiArchive` so its next `read_record` call holds open, and returns both ends the test owns: the receiver resolves when that CI provider read arrives, and sending on or dropping the sender releases it. The archive keeps the opposite ends, as it does today. This replaces the manual `ci_archive.arrived`/`ci_archive.release` setup in `local_cycle_publishes_retained_evidence_while_an_explicit_refresh_is_held` and `remote_and_local_cancellation_are_isolated`.

- [ ] **Step 1: Add the held-read helper and the silent-provider pin**

Add to `impl Harness` in `crates/tracedecay-application/src/advisory/runtime/local_cycle_tests.rs`:

```rust
fn hold_next_ci_provider_read(&self) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
    let (arrived_tx, arrived) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    *self.ci_archive.arrived.lock().unwrap() = Some(arrived_tx);
    *self.ci_archive.release.lock().unwrap() = Some(release_rx);
    (arrived, release)
}
```

Replace the four setup lines in `local_cycle_publishes_retained_evidence_while_an_explicit_refresh_is_held` with `let (arrived, release) = harness.hold_next_ci_provider_read();`, and those in `remote_and_local_cancellation_are_isolated` with `let (arrived, _release) = harness.hold_next_ci_provider_read();`. Their assertions do not change. Then add at file level:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_cycle_completes_with_a_configured_provider_that_never_answers() {
    let harness = Arc::new(Harness::open().await);
    let (arrived, release) = harness.hold_next_ci_provider_read();
    let explicit = {
        let harness = Arc::clone(&harness);
        tokio::spawn(async move {
            let context = harness.context(true);
            let control = harness.control(&context).await;
            harness
                .runtime
                .run_once(&context, control, harness.explicit("explicit-silent"))
                .await
        })
    };
    arrived.await.expect("explicit refresh reached its CI read");
    let before = harness.remote_reads();
    let context = harness.context(true);
    let control = harness.control(&context).await;
    let (cycle, contributions) = completed(
        harness
            .runtime
            .run_once(&context, control, harness.local("local-silent"))
            .await
            .unwrap(),
    );
    assert_eq!(harness.remote_reads(), before, "the local cycle read no provider");
    assert_eq!(
        provider_state(&contributions, AdvisoryProviderV1::Ci),
        ProviderEvaluationStateV1::Unavailable,
        "a configured provider without retained evidence is unavailable, not clean"
    );
    assert!(
        !matches!(
            cycle.execution.cycle.termination,
            FeedbackCycleTerminationV1::Clean | FeedbackCycleTerminationV1::StaleReplanRequired
        ),
        "{:?}",
        cycle.execution.cycle.termination
    );
    assert!(cycle.execution.publication.is_some(), "local feedback is published");
    release.send(()).unwrap();
    let _ = explicit.await.unwrap();
}
```

- [ ] **Step 2: Run the silent-provider pin and the migrated held-read tests**

Run each through the helper, one command per test:

- `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-application:unit_test --test_output=all --test_arg=advisory::runtime::local_cycle_tests::local_cycle_completes_with_a_configured_provider_that_never_answers --test_arg=--exact`
- `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-application:unit_test --test_output=all --test_arg=advisory::runtime::local_cycle_tests::local_cycle_publishes_retained_evidence_while_an_explicit_refresh_is_held --test_arg=--exact`
- `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-application:unit_test --test_output=all --test_arg=advisory::runtime::local_cycle_tests::local_cycle_admits_retained_evidence_only_for_exact_scope_grant_and_policy --test_arg=--exact`
- `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-application:unit_test --test_output=all --test_arg=advisory::runtime::local_cycle_tests::remote_and_local_cancellation_are_isolated --test_arg=--exact`

Expected: each reports `1 passed; 0 failed`. A failure of the first means the local cycle joins a configured provider read; report it as a blocker. A failure of a migrated test means the helper changed its setup; fix the helper, not the assertion.

- [ ] **Step 3: Run the owning targets**

Run: `bazel test //crates/tracedecay-application:unit_test //crates/tracedecay-application:application_suite //crates/tracedecay:hooks_lsp_suite --test_output=all`

Expected: all PASS, and each target's `test result:` line reports a nonzero passed count and `0 failed`. Record the counts, the deleted orchestration surface (`AdvisoryRemoteReadMode::LocalFeedback` never constructs `AdvisoryRemoteEvidenceV1::Refresh`; no CI discovery call in the local preamble), and the before/after provider-call counts asserted by `local_cycle_publishes_retained_evidence_while_an_explicit_refresh_is_held` (`(0, 0)` local, `(1, ≥1)` explicit).

- [ ] **Step 4: Commit**

```bash
git add crates/tracedecay-application/src/advisory/runtime/local_cycle_tests.rs
git commit -m "test(feedback): pin local cycles against a silent provider"
```

- [ ] **Step 5: Draft the closure comment in the lane report**

The report maps each #4423 requirement to evidence: local analysis proceeds while a remote read is held (`local_cycle_publishes_retained_evidence_while_an_explicit_refresh_is_held`), a configured-but-silent provider never blocks or cleans a local cycle (Step 1), retained evidence attaches only for exact scope, grant, and policy (`local_cycle_admits_retained_evidence_only_for_exact_scope_grant_and_policy`), local and remote cancellation are isolated (`remote_and_local_cancellation_are_isolated`), and explicit refresh still obtains updated data (the same held test's explicit cycle). It cites merged #4817, the Step 2 exact runs, and the Step 3 counts. The coordinator closes the issue.

---

### Task 4: #4440 compiler-owned TypeScript evidence closure

**Files:**
- Test: `crates/tracedecay-application/src/diagnostics_producer/compiler_conformance_tests.rs` (add to the existing module)
- Test: `crates/tracedecay-application/src/diagnostics_producer/installed_compiler_tests.rs` (add the manual measurement test)

**Interfaces:**
- Consumes: `InstalledCompilerProject::installing(PinnedTypeScript, &[(&str, &str)]) -> InstalledCompilerProject` with `project.root: PathBuf`, `project.run(&str, &mut TypeScriptProducerStateV1) -> CompilerProducerRunV1`, and `project.findings(&str) -> Vec<(String, String)>`.
- Consumes: `PinnedTypeScript::{Seven, Five}` and the module's `RELEASES: [PinnedTypeScript; 2]`.
- Consumes: `producer(&TypeScriptProducerStatusHandleV1) -> TypeScriptProducerStateV1` and `owner(&InstalledCompilerProject, &TypeScriptProducerStatusHandleV1, &str) -> Option<(String, Option<TypeScriptProjectCheckV1>)>` from `compiler_conformance_tests`.
- Consumes: `TypeScriptProducerStatusHandleV1::default()`.
- Produces: nothing. This task adds coverage and a measurement.

- [ ] **Step 1: Write the generated-output membership pin**

Add to `compiler_conformance_tests.rs`:

```rust
#[tokio::test]
async fn generated_output_files_are_not_project_members() {
    for release in RELEASES {
        let project = InstalledCompilerProject::installing(
            release,
            &[
                (
                    "tsconfig.json",
                    r#"{"compilerOptions":{"outDir":"dist","rootDir":"src"},"include":["src"]}"#,
                ),
                ("src/index.ts", "export const value: number = 1;\n"),
                ("dist/index.js", "export const value = 1;\n"),
            ],
        );
        let status = TypeScriptProducerStatusHandleV1::default();
        let mut state = producer(&status);
        let run = project.run("generation.generated-output", &mut state).await;
        assert!(
            matches!(run, CompilerProducerRunV1::Published { checked: 1, .. }),
            "{release:?}: {run:?}"
        );
        assert!(
            owner(&project, &status, "src/index.ts").is_some(),
            "{release:?}: the source is a project member"
        );
        assert!(
            owner(&project, &status, "dist/index.js").is_none(),
            "{release:?}: a generated output is not a project member"
        );
    }
}
```

- [ ] **Step 2: Run the generated-output pin**

Run: `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-application:unit_test --test_output=all --test_arg=diagnostics_producer::compiler_conformance_tests::generated_output_files_are_not_project_members --test_arg=--exact`

Expected: PASS with `1 passed; 0 failed`. A failure means a generated output is owned by the project; report it as a blocker.

- [ ] **Step 3: Write the manual preparation-cost measurement**

Add to `installed_compiler_tests.rs`:

```rust
#[tokio::test]
#[ignore = "manual measurement: preparation cost and incremental reuse"]
async fn measure_preparation_cost_and_incremental_reuse() {
    for release in RELEASES {
        let project = InstalledCompilerProject::installing(
            release,
            &[
                (
                    "tsconfig.json",
                    r#"{"compilerOptions":{"composite":true,"outDir":"dist"},"include":["src"]}"#,
                ),
                ("src/a.ts", "export const a: number = 1;\n"),
                ("src/b.ts", "export const b: number = 2;\n"),
            ],
        );
        let status = TypeScriptProducerStatusHandleV1::default();
        let mut state = producer(&status);
        let cold = std::time::Instant::now();
        let cold_run = project.run("generation.measure.1", &mut state).await;
        let cold_elapsed = cold.elapsed();
        let warm = std::time::Instant::now();
        let warm_run = project.run("generation.measure.2", &mut state).await;
        let warm_elapsed = warm.elapsed();
        println!(
            "{release:?} cold={cold_elapsed:?} warm={warm_elapsed:?} cold_run={cold_run:?} warm_run={warm_run:?}"
        );
        assert!(
            warm_elapsed <= cold_elapsed,
            "{release:?}: reuse must not cost more than a cold check"
        );
    }
}
```

- [ ] **Step 4: Run the measurement and record the numbers**

Run: `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-application:unit_test --test_output=all --test_arg=diagnostics_producer::installed_compiler_tests::measure_preparation_cost_and_incremental_reuse --test_arg=--exact --test_arg=--ignored --test_arg=--nocapture`

Expected: PASS with `1 passed; 0 failed` and a printed `cold=… warm=…` line. Record both durations, the compiler release, and the machine in the lane report. A warm run slower than cold is a finding to report, not a bound to loosen.

- [ ] **Step 5: Run the owning targets**

Run: `bazel test //crates/tracedecay-application:unit_test //crates/tracedecay-lsp:unit_test //crates/tracedecay-lsp:lsp_suite --test_output=all`

Expected: all PASS, each target's `test result:` line reports a nonzero passed count and `0 failed`, and the log lists `an_unreadable_configuration_is_a_failure_until_repaired`, `membership_is_the_program_the_compiler_checked`, and the new generated-output pin as `ok`. Record the counts.

- [ ] **Step 6: Commit**

```bash
git add crates/tracedecay-application/src/diagnostics_producer/compiler_conformance_tests.rs \
  crates/tracedecay-application/src/diagnostics_producer/installed_compiler_tests.rs
git commit -m "test(diagnostics): pin generated output membership"
```

- [ ] **Step 7: Draft the closure comment in the lane report**

The report maps each #4440 acceptance line to evidence: monorepo references and unbuilt/out-of-date references (`unbuilt_references_are_not_published_as_the_referencing_projects_findings`, `out_of_date_references_invalidate_the_referencing_projects_check`), relative and `${configDir}` extends (`a_config_dir_base_owns_and_admits_each_extending_projects_sources`), inherited include/exclude and generated outputs (Step 1), malformed and unreadable configs (`an_unreadable_configuration_is_a_failure_until_repaired`), toolchain upgrades (`a_same_version_compiler_change_is_checked_afresh`, `a_restarted_producer_does_not_replay_a_replaced_compilers_build_info`), and preparation cost with incremental reuse (Step 4). It cites merged #4860 and #4825 and the Step 5 counts. The coordinator closes the issue.

---

### Task 5: #4374 Codex re-offer fixture measurement and operator runbook draft

The fixture measurement in Steps 1 to 4 is supporting evidence. #4374's acceptance needs the operator's actual `~/.codex` corpus, which only the coordinator's separately approved run can measure (design, Long-running acceptance). This task never reads `~/.codex`.

**Files:**
- Create: `crates/tracedecay-sessions/src/runtime/ingest/codex_reoffer_measurement_tests.rs`
- Modify: `crates/tracedecay-sessions/src/runtime/ingest.rs` (add `#[cfg(test)] mod codex_reoffer_measurement_tests;` beside the existing test modules)

**Interfaces:**
- Consumes: `crate::runtime::hosts::codex::CodexSource::with_home(&Path) -> CodexSource` (roots at `<home>/.codex`).
- Consumes: `CodexSource::discover_transcript_paths_with_frontier(TranscriptDiscoveryBounds, CodexDiscoveryFrontier) -> TranscriptIngestResult<CodexDiscoveryPass>`; `CodexDiscoveryPass.report: FileDiscoveryReport` with `paths: Vec<PathBuf>`, and `CodexDiscoveryPass.next_frontier: CodexDiscoveryFrontier`; `CodexDiscoveryFrontier::initial()`.
- Consumes: `crate::runtime::hosts::codex::try_admit_codex_jsonl_observations_for_project_window(&Path, &Path, ProjectId, &dyn HostAdmission, u64, &ObservationCancellation) -> TranscriptIngestResult<CodexJsonlAdmissionProgress>` with `bytes_consumed`, `frames_persisted`, `resumed`, `source_deferred`.
- Consumes: `crate::runtime::hosts::codex::try_admit_codex_jsonl_observations_for_profile_with_admission_and_cancellation(&Path, Option<&str>, &[PathBuf], &dyn HostAdmission, Option<u64>, &ObservationCancellation) -> TranscriptIngestResult<CodexJsonlAdmissionProgress>`.
- Consumes: the in-memory admission fixture and rollout writer already used by `crates/tracedecay-sessions/src/runtime/hosts/codex/tests.rs`.
- Produces: `fn codex_rollout_corpus(home: &Path, count: usize) -> Vec<PathBuf>` (test-only fixture builder) and the measurement test `codex_reoffer_measurement`.

- [ ] **Step 1: Write the corpus builder and the measurement test**

Create `crates/tracedecay-sessions/src/runtime/ingest/codex_reoffer_measurement_tests.rs` with a `codex_rollout_corpus(home: &Path, count: usize) -> Vec<PathBuf>` that writes `count` rollouts under `<home>/.codex/sessions/2026/07/` totalling about 7.9 MB (about 3.9 KB each), each with a `session_meta` line carrying a distinct `cwd` under one project root, and a `#[tokio::test] #[ignore = "manual measurement: 2,010-rollout re-offer"] async fn codex_reoffer_measurement()` that:

1. builds 2,010 rollouts and one project root;
2. times a cold project-scope pass: `discover_transcript_paths_with_frontier(TranscriptDiscoveryBounds::default_walk(), CodexDiscoveryFrontier::initial())` plus the per-file loop over `pass.report.paths`, summing `std::time::Instant` around `try_admit_codex_jsonl_observations_for_project_window` separately from the loop total;
3. times a warm project-scope pass over the same corpus starting from `pass.next_frontier`;
4. times a profile-scope pass with `try_admit_codex_jsonl_observations_for_profile_with_admission_and_cancellation`, then a second profile-scope pass that simulates a restart by rebuilding the `CodexSource` and starting from `CodexDiscoveryFrontier::initial()`;
5. prints one line per phase: `phase=<name> files=<n> total_ms=<n> admit_ms=<n> per_file_us=<n>`;
6. asserts the warm project pass admits zero new bytes and the second profile pass re-offers zero files.

- [ ] **Step 2: Run the measurement**

Run: `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-sessions:unit_test --test_output=all --test_arg=runtime::ingest::codex_reoffer_measurement_tests::codex_reoffer_measurement --test_arg=--exact --test_arg=--ignored --test_arg=--nocapture`

Expected: PASS with `1 passed; 0 failed` and four `phase=…` lines. Record every line, the machine, and the Bazel target in the lane report.

- [ ] **Step 3: Attribute the per-file cost**

For each phase, subtract `admit_ms` from `total_ms` and record the remainder as the between-files cost. Compare it with the issue's reported baseline (about 62 s total, about 28 ms per file, admission under 1 s for 4,020 reads). If the remainder is still dominated by a per-file durable write, record the span that owns it (`sessions.ingest.project.codex.convergence_settle` or the cursor write inside `try_admit_codex_jsonl_observations_for_project_window`) and report it as a `needs-decision` escalation with the numbers; do not implement a speculative batching change in this task.

- [ ] **Step 4: Run the owning targets**

Run: `bazel test //crates/tracedecay-sessions:unit_test --test_output=all`

Expected: PASS, the `test result:` line reports a nonzero passed count and `0 failed`, and the log lists the existing re-offer regressions (`sessions` cold-cache, repeated re-offer, provenance, and rewrite-failure cases) as `ok`. Record the count and those test names.

- [ ] **Step 5: Commit**

```bash
git add crates/tracedecay-sessions/src/runtime/ingest.rs \
  crates/tracedecay-sessions/src/runtime/ingest/codex_reoffer_measurement_tests.rs
git commit -m "test(sessions): measure codex re-offer cost"
```

- [ ] **Step 6: Draft the status comment in the lane report**

The report states the fixture's measured project-scope and profile-scope per-file costs, the restart result, the between-files remainder, and the span that owns it, beside the issue's 2,010-rollout baseline. It labels every number as fixture evidence, not acceptance. It cites merged #4380 (profile-scope replay), #4806 (skips empty JSONL scope resolution on re-offer), #4864 (#4861 history admission), #4857 and #4793 (#4756 directory and projection bounds), L3's merged program PRs for #4861 and #4756, and the Step 4 count. The PR says `Refs #4374`. The lane marks #4374 `pending-long-acceptance` and never drafts it as closed.

- [ ] **Step 7: Draft the operator-corpus runbook (report only; never executed by the lane)**

Put the draft in the lane-end report for the coordinator, who presents it to the user for separate approval at execution time. Commit nothing for it. The draft has these sections:

1. **Binary.** The final merged `master` SHA and `bazel build //crates/tracedecay-cli:tracedecay`. The install location and method stay explicit fields that the coordinator fills and the user approves. The draft proposes none.
2. **Measured route.** The operator's normal TraceDecay authority re-offers the real `~/.codex` corpus through the canonical Codex discovery and admission path, as it does in normal use. The draft names the spans to read (`sessions.ingest.project.codex.*` and the profile-scope re-offer spans that Step 3 attributed) and how they are captured. Any restart, or any environment change such as `TRACEDECAY_SPAN_TIMINGS=1` on the service, is its own line item with how it is undone. It proposes no second daemon.
3. **Read and write boundaries.** `~/.codex` is read in place and never written, moved, renamed, deleted, or copied. The normal authority writes its own profile as normal use does. The coordinator writes only its own sample files, outside `~/.codex` and outside the profile.
4. **Samples.** Rollout file count and total eligible bytes, the cold and restart re-offer pass wall times, per-file cost, admission time, the between-files remainder, and the daemon PID's CPU time and RSS. Counts and timings only, never transcript content or rollout text.
5. **Comparison.** The same columns as Step 3's fixture table and the issue's baseline (about 62 s total, about 28 ms per file, admission under 1 s for 4,020 reads).
6. **Stop conditions.** Verbatim from the design's #4374 operator-corpus measurement section.
7. **Forbidden actions.** No copy, backup, snapshot, fixture, or excerpt of `~/.codex` or of data derived from it. No write under `~/.codex`. No racing daemon. No repair without a new approval.
8. **Closure evidence.** The recorded samples, the binary SHA, and the comparison. If the run is not approved or does not complete, the issue stays open with the fixture evidence and what remains.

Mark #4374 `pending-long-acceptance` in the lane report, with the draft attached and the statement "not executed; requires the user's separate approval". Do not run any part of it.
