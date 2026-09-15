# Issue 753 worker notes

## 2026-09-03 preparation

- Confirmed worktree `/fast/tmp/td-753-worker-gpt56` is on
  `agent/issue-753-gpt56` at untouched base
  `bb48eb5a764850e7324a42aeb8a1369ad699bcba`.
- Read the worktree `AGENTS.md`; production changes must preserve typed
  failures and CAS guards, and all cargo work must use the broker shim.
- TraceDecay MCP tools are not exposed in this subagent host, so repository
  graph lookups use the supported `tracedecay tool ...` CLI fallback.
- Cargo-hauler showed no active or recent request for this worktree before
  baseline testing.
- Salvage branch has eight commits, oldest to newest:
  `b442409f9`, `45481bff3`, `ade3a0e04`, `7ce741815`, `20bb0c277`,
  `11a0cb178`, `7377ab614`, `f06f690f7`.
- Prior lane notes say its latest proof reached lifecycle/runtime issues and
  used four Tokio test workers after two-worker evaluation deadlines; this is
  context only until independently reproduced here.

## 2026-09-03 baseline attempt 1

- Ran the exact requested journey filter through bare cargo; libtest confirmed
  `running 1 test` (1552 filtered out).
- Cargo-hauler queued the request for 14m03s, then compiled for 9m09s.
- The shell-scoped
  `TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE=/fast/tmp/td-753-fastembed-fixture`
  assignment did not propagate through the broker subprocess. The test failed
  immediately at
  `semantic_activation_journey_test.rs:589` with:
  `semantic activation product journey requires
  TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE from distribution acceptance`.
- This is an environment-forwarding failure, not the #753 product
  reproduction. The next attempt will export the fixture variable before
  invoking cargo so the brokered test process receives it.
- TraceDecay CLI graph fallback also failed because the live daemon socket is
  absent. Per the task constraint, the worker did not start or modify the
  operator service and will use narrow source reads only after concrete test
  evidence.

## 2026-09-03 confirmed bb48eb5a7 reproduction

- Exporting the fixture variable made the real product journey run. Libtest
  confirmed `running 1 test` (1552 filtered out).
- Exact failure at
  `crates/tracedecay/src/daemon/production_harness/semantic_activation_journey_test.rs:365:20`:
  `native semantic profile publication failed: ApplicationProblem { problem:
  InvalidRequest { diagnostic: SafeDiagnostic { code:
  "semantic_evaluation.rejected", message: "semantic activation input was
  rejected: packaged native qualification rejected: native qualification
  model identity does not match" }, retry: Never, legal_actions: [] } }`.
- The daemon had reached `semantic_runtime_registered`,
  `code_index_query_authority outcome=mounted`, and
  `semantic_config_selected`; the product journey then failed during native
  semantic profile publication. Test execution itself took 8.83s after a
  103.7s broker wait.
- This is the untouched-base #753 reproduction.

## 2026-09-03 landed typed-state tests

- `cargo test -p tracedecay-semantic
  load_deadline_maps_to_terminal_failed_schedule_state` matched **0 tests**
  on bb48eb5a7 (181 lib tests and one integration test filtered). `git log -S`
  located the missing test in salvage commit `ade3a0e04`; it must be carried
  over and then run non-vacuously.
- The skew-resilience test from `f4515ed9a` matched one test. Its first run
  failed because the fresh worktree lacked `target/debug/tracedecay`, as the
  harness explicitly reported.
- After `cargo build -p tracedecay-cli --bin tracedecay`, exact test
  `stale_client_resilience_test::version_skewed_client_cannot_crash_the_daemon`
  passed: **1 passed**, 13 filtered, 1.77s test time. This confirms the typed
  version-skew refusal/survival proof on bb48eb5a7.

## 2026-09-03 salvage integration

- Recovered an in-progress duplicate cherry-pick after the harness had already
  applied the eight salvage commits once. The first applied sequence is:
  `688a9d9e1`, `c7e9ecb22`, `cc6109560`, `43b2a70ea`, `7564e43fc`,
  `32fb5776e`, `1d88b9406`, `17a5c87ad`.
- Continued rather than aborting/resetting as instructed. The overlapping
  second sequence produced `f01e3e021` and `5c4b781a8`; conflict resolution
  produced `8c31a157f`. Empty duplicates of `ade3a0e04`, `7ce741815`,
  `11a0cb178`, `7377ab614`, and `f06f690f7` were skipped.
- Conflict intent:
  - preserve the registration-time `Arc<Notify>` so a committed activation
    before reconciler installation remains retained;
  - pass that retained wake into the reconciler rather than creating a new
    local notify;
  - retain `Conflict` as retryable canonical reobservation and bare
    `Rejected` as refusal;
  - retain exactly one deferred
    `install_semantic_activation_runtime_owner` implementation and one
    reconciler registration.
- `8c31a157f` removes the superseded reconciler-owned notify/OnceLock wiring
  and a duplicate deferred-owner function introduced by the overlapping
  patches. The active tree has no unresolved conflict markers and only
  untracked `NOTES.md`.

## 2026-09-03 review verdict audit: evidence and CAS guards

- Confirmed both production revision CAS guards remain intact:
  `revalidate_verified_evaluation_target` compares
  `verified.vector_state_revision` with the captured revision, and
  `acquire_vector_publication_lease` compares the graph store's
  `verified_revision` with the expected revision. No weakening was carried.
- Confirmed the code-index snapshot source-manifest guard remains intact.
  Found the salvage had removed the second vector-manifest comparison and
  replaced it with a comment claiming unlike domains; production vector
  publication proves `PublishedVectorGenerationV1::source_manifest_digest`
  is the projection change manifest (or a replay manifest), so the candidate
  must instead be checked against canonical vector-runtime authority.
- Rejected the accepted-profile evidence weakening that allowed any
  well-formed vector digest. Added and observed RED for
  `semantic_runtime::accepted_profile_authority::tests::
  genuine_runtime_evidence_rejects_foreign_vector_generation` (missing wished
  helper), then implemented exact equality while retaining `None` only for
  packaged-portable evidence. GREEN: **1 passed**, 586 filtered.
- Commit: `31f162bab fix(semantic): require the exact vector generation in
  genuine evidence`.
- Added a runtime-minted vector snapshot field for the exact source manifest
  and changed candidate construction to verify both generation and manifest
  against that canonical runtime observation. This compares like authorities
  and preserves replay manifests without deleting the guard.
- RED:
  `semantic_evaluation::lifecycle_tests::
  candidate_vector_identity_requires_runtime_manifest_match` failed because
  the runtime snapshot had no manifest field and the comparison helper did not
  exist. A stale queued attempt later matched zero tests and was not accepted.
- GREEN: the same full-path test passed non-vacuously: **1 passed**, 418
  filtered.
- Commit: `0c5c8cc8a fix(semantic): bind evaluation to vector runtime
  manifest`.

## 2026-09-03 typed deadline/client proofs

- `tracedecay-semantic`
  `scheduling_tests::load_deadline_maps_to_terminal_failed_schedule_state`
  passed non-vacuously: **1 passed**, 181 filtered. It schedules a
  `LoadDeadlineExceeded` failure and waits until the public scheduling handle
  reports terminal `Failed { reason: DeadlineExceeded }`, proving no stale
  `loading`.
- `tracedecay-code-index-runtime`
  `semantic_evaluation::lifecycle_tests::
  evaluation_waits_for_scheduler_admission_and_times_out_typed` passed:
  **1 passed**, 418 filtered. Work denied scheduler admission terminates as
  `DaemonSemanticEvaluationExecutionErrorV1::TimedOut`.
- `tracedecay-daemon-protocol`
  `client::tests::semantic_evaluation_client_maps_typed_application_problems`
  passed: **1 passed**, 56 filtered. Cancelled/deadline daemon outcomes map to
  typed non-retryable client route errors rather than transport ambiguity.
