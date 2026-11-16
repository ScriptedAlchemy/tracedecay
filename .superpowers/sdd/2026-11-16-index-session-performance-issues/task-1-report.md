# Task 1 evidence-only execution report

Goal. Keep foreground session projection available while history waits for real capacity. Preserve owed work and joined cancellation.

Status DONE for the authorized evidence-only portion. Both regressions pass on unchanged BASE production. The coordinator explicitly accepted the foreground baseline pass and authorized the second regression without manufacturing RED. Only tests were needed. This report supersedes the prior NEEDS-DECISION stop.

## Checkout and commits

Worktree `/Users/zackjackson/.t3/worktrees/tracedecay/t3-f18a8309-f8bfb7055e343b2c`.
Branch `t3/session-projection-admission-inventory`.
BASE `e11079d70e40f5baccddbed4c5b80582e039f5d0`.
Prior reviewed HEAD `d3c702fb5128a524dc2d398e5b76afa5e63591fd`.
The refinement commit adds only the status assertions and this report. Its HEAD is supplied in the implementation handoff.

- `b45a8cfe92cc07d0a1125ed6aaaee03c61ba7a00` retains the foreground saturation, FIFO release, and cancellation regression.
- `d3c702fb5128a524dc2d398e5b76afa5e63591fd` adds release-before-wait and closed-signal regression coverage.

The Rust diff from BASE contains only retained_history.rs and registry_tests.rs. Production remains byte-identical to BASE. No production defect was demonstrated, and no scheduler semantics, dependencies, bounds, timeouts, machine configuration, or operator data changed.

## Exact verification

All invocations ran sequentially with invocation-local `--remote_executor= --remote_cache= --jobs=2 --local_resources=cpu=2 --strategy=Rustc=local --strategy=TestRunner=local`. The strategy overrides preserve the prior coordinator-authorized local execution approach against inherited remote-only strategies. No rc file changed.

```sh
REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-session-runtime:unit_test --test_output=all --test_arg=session_temporal_refresh_scheduler::registry::registry_tests::saturated_history_release_race_and_closed_signal_stay_quiescent --test_arg=--exact <local-overrides>
REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay:session_suite --test_output=all --test_arg=session_runtime::retained_history::saturated_history_cancellation_does_not_reenter_or_lose_release --test_arg=--exact --nocache_test_results <local-overrides>
bazel test //crates/tracedecay:session_suite //crates/tracedecay-session-runtime:unit_test --test_output=all --nocache_test_results <local-overrides>
```

Each command exited 0. The logs start at the Bazel invocation and rc-option output. They do not capture the helper prefix or complete expanded argv. The commands above state helper use, and the preceding paragraph states the exact local overrides.

The two single-test logs prove their named test executions. The later task-1-affected-suites.log covers the full affected suites at d3c702fb5128a524dc2d398e5b76afa5e63591fd.

| Evidence | Actual result |
| --- | --- |
| task-1-closed-signal.log | Exact full unit-test path listed as ok. 1 passed; 0 failed; 203 filtered out. Executed 1 out of 1 test. |
| task-1-foreground-executed.log | Exact full integration-test path listed as ok. 1 passed; 0 failed; 426 filtered out. Executed 1 out of 1 test. |
| task-1-affected-suites.log | Unit suite 204 passed; 0 failed; 0 filtered out. Session suite 427 passed; 0 failed; 0 filtered out. Executed 2 out of 2 targets. |

The first foreground helper invocation reused a cached test result in task-1-foreground-proof.log. It was not counted as fresh execution; task-1-foreground-executed.log records the explicit uncached rerun. Earlier execution-configuration failures remain recorded in the prior logs and are not behavioral RED.

The affected-suite output explicitly lists these tests as ok, among the other passing tests.

- `session_temporal_refresh_scheduler::registry::registry_tests::saturated_history_release_race_and_closed_signal_stay_quiescent`
- `session_temporal_refresh_scheduler::worker::tests::refused_history_admission_keeps_its_fifo_turn_across_wait_repolls`
- `session_temporal_refresh_scheduler::registry::registry_tests::a_drain_past_the_grace_joins_the_aborted_worker`
- `session_temporal_refresh_scheduler::registry::registry_tests::a_repeated_retirement_joins_the_retired_worker`
- `session_runtime::retained_history::a_saturated_history_pass_runs_with_the_permit_its_queue_turn_grants`
- `session_runtime::retained_history::saturated_history_admission_defers_the_pass_and_resumes`
- `session_runtime::retained_history::saturated_history_cancellation_does_not_reenter_or_lose_release`

## Behavioral scope and self-review

The retained integration fixture holds shared permits, queues registered project and profile owners, repeats ensure/status/live wakes, and admits and reads a real foreground occurrence while history stays saturated. Release resumes surviving owed history. Retirement joins both a queued owner and active history.

The new unit fixture owns one optional watch sender and its receiver. The production worker subscribes before invoking the retained ingestor. Its first callback advances the watch before returning a no-progress retry, so its second callback proves the release survived publication before waiting. The second callback drops the sole sender before returning another no-progress retry. The test confirms channel closure, repeatedly polls the closed release future as pending, drives foreground ensure wakes through the registry, joins idle, and requires the callback count to stay at two. Retirement completes within its liveness bound, records cancellation, removes the worker, and leaves the count at two. No sleep or timing window establishes ordering. Existing production timeouts were not changed.

Model the Domain kept closure represented by dropping the sole sender. Test Behavior, Not Implementation selected resumed callbacks, settled foreground wakes, and joined retirement as observations. Prove It Works selected actual nonzero execution rather than compilation or cached output. Self-review and simplification found no added comments, compatibility code, or production machinery. rustfmt, git diff --check, and commitlint passed. The tracked worktree is clean.

## Open acceptance and release

Parent independent review is required before any PR. Explicit no-delegation leaves that review with the coordinator. No PR was opened or pushed. Nothing was merged, deleted, or closed.

Task 2 inventory integration, L2 registry, and L6 discovery were untouched. Task 1 broader measured performance acceptance remains open. No CPU/RSS, transaction, warning-frequency, multi-provider historical import, benchmark, full-workspace sweep, or long acceptance was run. These proofs do not reproduce the reported CPU or warning amplification and imply no measured performance improvement. Issue #4861 remains open.

All owned Bazel invocations exited normally. The authorized local slot is released. No owned test invocation remains active; shared infrastructure and peer processes were left untouched. The append-only decisions.tsv records the ruling, verification, and release.

## Minor review resolution

The discarded status tuple now checks all three historical owners. Each must report stale historical convergence or the typed `history_admission_saturated` retry, with convergence still `Converging`. Repeated wakes may expose pending convergence, so the assertion does not require `HistoricalRetry` at every read or incorrectly require `HistoricalBlocked`. The existing callback assertions still require no historical ingest while capacity is held. Production and the benign match arm remain unchanged.

The following commands run sequentially on the refinement tree. The first uses `REQUIRE_EXACT_TEST_COUNT=1` and the exact-test helper. Both disable test-result reuse. The logs capture Bazel output, not the environment prefix or complete argv.

```sh
REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay:session_suite --test_output=all --test_arg=session_runtime::retained_history::saturated_history_cancellation_does_not_reenter_or_lose_release --test_arg=--exact --nocache_test_results --remote_executor= --remote_cache= --jobs=2 --local_resources=cpu=2 --strategy=Rustc=local --strategy=TestRunner=local
bazel test //crates/tracedecay:session_suite //crates/tracedecay-session-runtime:unit_test --test_output=all --nocache_test_results --remote_executor= --remote_cache= --jobs=2 --local_resources=cpu=2 --strategy=Rustc=local --strategy=TestRunner=local
```

`task-1-minor-saturation.log` records the exact test as `ok` and `1 passed; 0 failed; 0 ignored; 0 measured; 426 filtered out`. No compiler or runtime warning appears. Bazel emits its test-size advisory, `There were tests whose specified size is too big`, after the successful result. This is not a warning-free Bazel transcript.

The earlier single-test logs and full-suite log remain evidence for the prior reviewed HEAD. The new `task-1-minor-affected-suites.log` covers the refinement tree.

The refinement suite run exited 0 with `204 passed; 0 failed` and `427 passed; 0 failed`, both with zero filtered tests. Bazel reports `Executed 2 out of 2 tests: 2 tests pass.` Neither suite transcript contains a compiler or runtime warning. These are focused behavioral results, not performance measurements.

Model the Domain kept the existing typed states as the assertion authority. Test Behavior, Not Implementation selected observable owner status alongside the existing callback and foreground checks. Prove It Works required an uncached exact helper run and the affected suites. Simplification retained one local assertion loop and added no helper, comments, or production code. The parent owns the scoped re-review. No delegation, PR, push, merge, or issue edit occurred.

The first Bazel rustfmt check found only the convergence assertion's line wrapping. The correction changes whitespace only. `task-1-minor-rustfmt-final.log` records the successful rerun of `bazel build //crates/tracedecay:rustfmt_check_2024 --remote_executor= --remote_cache= --jobs=2 --local_resources=cpu=2 --strategy=Rustc=local --strategy=TestRunner=local`. The affected suites above ran before that whitespace correction. The final exact helper command repeats the command above on the formatted source and writes `task-1-minor-saturation-final.log`. `task-1-minor-commitlint.log` records the valid commit header. `git diff --check` passes.

The final formatted-source exact run exited 0 with `1 passed; 0 failed; 0 ignored; 0 measured; 426 filtered out`. No compiler or runtime warning appears. Bazel repeats the test-size advisory recorded above. All owned Bazel invocations have exited. The one local slot is released. Minor resolution and slot release are appended through the decision-log helper.
