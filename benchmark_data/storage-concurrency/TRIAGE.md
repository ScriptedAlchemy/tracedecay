# Focused failure triage

Follow-up to commit `c081b413c6a37be0f7f48fe9147b9840fa4dfd99`, 2026-10-09.
The full-workspace outcomes in [workspace-verification.txt](workspace-verification.txt)
remain 135 passed targets, 12 failed, and one timed out. Filtered reruns do not
reclassify those targets as passing. All commands used the same Bazel CI
configuration and existing timeouts; no assertions or production budgets changed.

After the four schema failures and two host-tool targets, the unresolved group
actually contains **nine targets and eighteen failing cases**, plus the separate
code-index timeout. MCP's complete failing case list was not retained: the
shared test.log was overwritten by its filtered host-tool rerun. The BEP still
preserves MCP's full-target failure (619.781 s, exit 101). The absence of that
original case list is a limitation, not evidence that the remaining cases passed.

## Exact isolated controls on the fixed reader code

Each case below was run uncached in its own invocation:

```sh
bazel test --config=ci //crates/<target> \
  --test_arg=<full-test-name> --test_arg=--exact \
  --nocache_test_results --test_output=all --repo_env=DO_NOT_TRACK=1
```

All invocations also supplied `TRACEDECAY_AST_GREP_BIN` for the isolated
repository-pinned `@ast-grep/cli@0.44.0` host installation. No blanket tracing
was supplied to CLI tests that require empty stderr. Each reported one executed
test; none was a zero-test green. The existing tests retained internal thread/
Tokio concurrency.

| Target / exact test | Outcome | Test duration |
| --- | --- | --- |
| `//crates/tracedecay-hooks:unit_test` / `delivery_spool::tests::concurrent_writers_on_a_slow_disk_publish_every_receipt_within_budget` | PASS (1 passed) | 0.09s |
| `//crates/tracedecay-lsp:lsp_suite` / `analyzer_runtime::broker_refresh::an_exhausted_restart_budget_stops_the_refresh_lane_from_respawning` | PASS (1 passed) | 1.10s |
| `//crates/tracedecay-application:application_suite` / `observability_rollup_convergence::idle_producer_converges_dirty_days_into_application_readable_fragments` | PASS (1 passed) | 2.12s |
| `//crates/tracedecay-application:application_suite` / `observability_producer_shutdown::shutdown_drains_every_offered_owner_fact` | PASS (1 passed) | 1.62s |
| `//crates/tracedecay-code-index-retention:unit_test` / `code_index_generations::tests::graph_replay_pool_lock_tests::execute_cancels_held_pool_acquire_before_any_exposure` | PASS (1 passed) | 0.01s |
| `//crates/tracedecay:unit_test` / `daemon::broker_stream_transport_tests::rmcp_receive_waits_for_full_close_after_request_half_close` | FAIL (1 failed) | 0.45s |
| `//crates/tracedecay:unit_test` / `daemon::tests::replay::client_identity_startup_replays_retained_profile_receipts` | PASS (1 passed) | 0.79s |
| `//crates/tracedecay-cli:core_cli_suite` / `cli_boundary::shipped_binary_stops_quietly_when_a_pipeline_reader_exits` | PASS (1 passed) | 6.56s |
| `//crates/tracedecay-cli:core_cli_suite` / `tool_daemon_test::daemon_socket_is_owner_only` | PASS (1 passed) | 0.43s |
| `//crates/tracedecay-cli:core_cli_suite` / `tool_daemon_test::daemon_sigterm_exits_while_authenticated_project_client_is_connected` | PASS (1 passed) | 1.36s |
| `//crates/tracedecay-cli:core_cli_suite` / `tool_daemon_test::daemon_sigterm_stops_an_in_flight_open_at_the_next_store_boundary` | PASS (1 passed) | 1.47s |
| `//crates/tracedecay:hooks_lsp_suite` / `hook_spool_drain_test::a_failed_receipt_drain_is_reported_on_status_and_clears_when_the_spool_drains` | PASS (1 passed) | 1.59s |
| `//crates/tracedecay:transcript_ingest_suite` / `codex_compaction::codex_post_compact_hook_commits_app_server_summary_through_daemon_effect` | PASS (1 passed) | 3.04s |
| `//crates/tracedecay-cli:host_journeys_suite` / `host_lifecycle_cli_acceptance::droid_uninstall_restores_the_exact_pre_install_tree` | FAIL (1 failed) | 0.92s |
| `//crates/tracedecay-cli:host_journeys_suite` / `host_lifecycle_cli_acceptance::codex_uninstall_restores_the_exact_pre_install_tree` | FAIL (1 failed) | 2.29s |
| `//crates/tracedecay-cli:host_journeys_suite` / `host_lifecycle_cli_acceptance::sweep_outcomes::doctor_names_the_drifted_host_component_and_its_remedy` | FAIL (1 failed) | 0.45s |
| `//crates/tracedecay-cli:host_journeys_suite` / `host_lifecycle_cli_acceptance::uninstall_keeps_preexisting_and_foreign_occupied_directories` | FAIL (1 failed) | 2.26s |
| `//crates/tracedecay-cli:host_journeys_suite` / `host_lifecycle_cli_acceptance::exact_restore::uninstall_restores_the_exact_pre_install_home_on_every_host` | FAIL (1 failed) | 34.42s |
| `//crates/tracedecay-code-index-runtime:unit_test` / `code_index_scheduler::registry::convergence_park_tests::a_seat_wait_answers_a_parked_worker_with_its_park` | PASS (1 passed) | 5.37s |

## Original-condition comparison of all six persistent failures

Restored only the original `leased < lease_ceiling` admission condition,
ran the six persistent exact cases uncached, and restored the fixed condition
in `finally`. Every case still failed with the same error class. The source is
restored byte-for-byte to the committed fix. This is a production-condition
ablation, not a second checkout or a rollback copy.

| Exact control above | Fixed condition | Original condition | Persistent diagnostic |
| --- | --- | --- | --- |
| root-half-close | FAIL | FAIL | macOS socket shutdown(Both): code57 NotConnected |
| host-droid | FAIL | FAIL | Extra Library/Caches/com.apple.python nodes in isolated home |
| host-codex | FAIL | FAIL | Same Python home cache pollution |
| host-doctor | FAIL | FAIL | OS-temporary project root refused before doctor invocation |
| host-foreign | FAIL | FAIL | Python cache nodes violate exact preexisting/foreign tree assertion |
| host-all-restore | FAIL | FAIL | Python cache nodes remain after host uninstall |

The exact test names corresponding to these control labels are in the first
table. No fixture assertion was weakened and no application behavior was changed
to accommodate them. Together with the prior four runtime-core schema controls,
ten persistent failed cases reproduce with the original production condition.

## Source diagnoses and bounded interpretation

- `daemon/broker_stream_transport_tests.rs:29` reunites a half-closed Unix socket
  and calls shutdown(Both); macOS returns NotConnected. This case never opens a
  database. Its cleanup assertion is independent of reader admission.
- Host fixture `IsolatedCli::command` sets PYTHONDONTWRITEBYTECODE=1 at
  `tracedecay-cli/tests/host_journeys_suite/host_lifecycle_cli_acceptance.rs:284`.
  `tracedecay-agent-hosts/src/agents/host_cli.rs:210` clears the admitted child's
  environment and sets HOME; Python stand-ins lose that flag and populate
  Apple's home caches. An outer Bazel environment override cannot repair the
  cleared child boundary. Preserve the exact-tree assertion; an eventual
  fixture fix should control the stand-in interpreter's cache policy.
- The doctor fixture's `cli.run(["init"])` in `sweep_outcomes.rs:925` uses an
  OS-temporary project. Production rejects that location as a durable authority;
  doctor is never reached. Fixing the fixture must not permit temporary
  production authorities.
- Spool receipt publication, LSP Python startup, and retention journal/pool
  filesystem locking do not use the modified ReaderPool. Their isolated tests
  pass with original budgets. The retention whole-operation check measured
  33.27ms in the broad run and passes in 0.01s as an isolated test; neither
  observation justifies changing its20ms cancellation assertion.
- Both application deadline cases pass in isolation. Daemon replay, socket wait,
  SIGTERM cases, and Codex compaction also pass. These results show the original
  broad-run failures were not reproduced under isolation, not that every
  full-target failure is pre-existing. Indirect background invariant scans
  exist (`crates/tracedecay-global-db/src/schema_contract/invariants.rs:178`); no failing storage
  regression was reproduced. Production shutdown allows45s while the selected
  CLI shutdown cases require3s; no deadline was increased.
- The receipt-drain case passes alone. Source inspection finds status reads the
  last completed sweep (`crates/tracedecay/src/daemon/hook_v2_replay_consumer.rs:421`), while newly appended
  receipts do not invalidate that status. A previous Drained state can therefore
  precede acknowledgement of the newly added receipt. This is a plausible
  existing race, not an experimentally established cause of the broad-run failure.
- Catalog pipeline printing can panic on a broken pipe before any database is
  opened. The exact case passed in isolation; its broad-run failure remains
  unreproduced rather than silently ignored.

## Code-index timeout controls

The original target timed out at300.087s (exit142,594 tests). The explicit
failure was `convergence_park_tests::a_seat_wait_answers_a_parked_worker_with_its_park`;
that exact case passes alone in5.37s. Among the reported >60s tests, the corpus
cases eventually completed; only
`branch_publication_tests::mid_wait_branch_publication_surfaces_terminal_publication_park`
lacked completion in the original log. It passes alone in0.97s (1passed,
593filtered). The latter command added `--local_test_jobs=1` and
`--test_arg=--test-threads=1`, retaining the configured300s test timeout.

The branch test bounds pending metadata discovery, then awaits publication
without an outer test-body timeout (`branch_publication_tests.rs:305`). The
production loop checks the injected terminal park and watches serving changes;
its hard deadline is30minutes, idle deadline20s (`branch_publication.rs:31`).
We did not change these budgets or claim a deterministic deadlock. The original
full-target timeout was not reproduced by either exact control.

Artifact construction/reads use direct rusqlite connections
(`tracedecay-query/src/retrieval/lexical/projection/artifact.rs:228`,
`artifact/reader.rs:460`), bypassing the modified ReaderPool. Broad tests build
1001-/512-/384-file corpora concurrently, so host and fixture pressure remain a
possible cause. The full594-test target was not rerun or reclassified as passing.

## MCP Git/import controls

Each of these exact cases passes alone (1passed,626filtered), using
`--test_arg=--test-threads=1 --local_test_jobs=1` and the pinned host tool:

- `mcp_cli_serve_test::no_explicit_path_auto_initializes_unindexed_git_cwd`: test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 626 filtered out; finished in 10.86s
- `mcp_cli_serve_test::initialize_roots_auto_initializes_unindexed_git_repo`: test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 626 filtered out; finished in 1.42s
- `mcp_handler_test::session_search_test::scheduled_session_import_makes_the_final_codex_source_searchable`: test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 626 filtered out; finished in 6.09s
- `mcp_handler_test::session_search_test::cursor_record_with_a_dispatch_is_one_searchable_message`: test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 626 filtered out; finished in 2.03s

Raw Git initialization in these cases occurs before production harness/database
construction. Immediate Bad-file-descriptor errors there do not originate in
those fixtures acquiring ReaderPool capacity. Resource pressure remains a
hypothesis; these controls do not reconstruct the missing full failure list.

## Complete MCP target control

The one MCP-only control used the pinned host tool, four libtest threads,
one Bazel test job and the unchanged 900-second timeout:

```sh
bazel test --config=ci //crates/tracedecay:mcp_suite \
  --test_env=TRACEDECAY_AST_GREP_BIN=<isolated-host-tool>/ast-grep \
  --test_arg=--test-threads=4 --local_test_jobs=1 \
  --nocache_test_results --test_output=all --repo_env=DO_NOT_TRACK=1
```

It failed: **384 passed, 243 failed, 627 executed**, 553.05s test duration
(555.436s Bazel elapsed), test exit101/Bazel exit3. Every one of the243
failure blocks contains `Bad file descriptor (os error 9)` from subprocess
spawning. The first completed failure was
`graph_query_test::relation_page_cost::a_default_callees_page_resolves_dispatch_without_reading_every_callee`,
while initializing Git at `crates/tracedecay/tests/common/fixture.rs:535`.
278 cases had completed successfully before that first failure. The first
failed fixture had not yet opened its production harness; earlier concurrent
fixtures can still affect process resources or state. This does not prove
that the reader change caused or did not cause the accumulated failure.

[The case inventory](mcp-controlled-failures.json) records all failed names
and error locations, the retained log digest, command and result. The full
log and BEP remain outside the checkout in the task evidence directory;
this control did not repeat the workspace suite.

A separate host API probe duplicated a valid `/dev/null` descriptor to number
10240. `fstat` succeeded; `posix_spawn_file_actions_adddup2` returned EBADF9,
while the same action on descriptor3 returned0. Both owned descriptors and the
spawn-actions object were released immediately. This demonstrates the host's
spawn boundary without raising limits, opening thousands of files or launching
children. Apple's implementation rejects descriptors at its compile-time
OPEN_MAX bound ([spawn source](https://github.com/apple-oss-distributions/xnu/blob/main/libsyscall/wrappers/spawn/posix_spawn.c#L1691),
[limit definition](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/syslimits.h#L101)).
The pinned Rust process implementation uses these actions for child stdio
([Rust source](https://github.com/rust-lang/rust/blob/b940084d7eb6a299eb4bfeb8e34901bc051e7ac4/library/std/src/sys/process/unix/unix.rs)).

Live numeric descriptor observations during the smaller graph-query control
were855/max856, then2073/max2120; the full failed control had no descriptor
sample. This supports investigating resource accumulation but does not prove
that the243 errors crossed the boundary. Several fixtures rely on harness
Drop spawning asynchronous shutdown on a terminating per-test Tokio runtime
(`crates/tracedecay/src/daemon/production_harness.rs:1275`), rather than joining
cleanup. This is an existing source path to investigate, not a demonstrated
leak. The reader fix tightens admission without raising pool capacity or
changing connection opening/shutdown. An indirect scheduling effect is not
ruled out. No fork fallback, limit change or fixture assertion change was added.


Three uncached boundary controls completed on the fixed code, with the pinned
host tool and unchanged900-second timeout:

| Selection | Threads | Executed / result | Test duration |
| --- | --- | --- | --- |
| first failed full test name, `--exact` | 1 | 1PASS,626filtered | 2.74s |
| immediately preceding `name_first_lexical_search_discloses_preferred_symbol_route` plus first failed full name, both filters with `--exact` | 1 | 2PASS,625filtered | 3.33s |
| `mcp_handler_test::graph_query_test::` group | 4 | 56PASS,571filtered | 96.11s |

The group includes all44 graph-query failures from the full control. None
reproduced within that smaller process; the preceding pair also did not poison
it. This narrows the failure to broader accumulated state/load rather than an
individually failing graph-query assertion. It does not establish a baseline
full-target failure or rule out an indirect effect of changed scheduling.
No further full-target rerun or unrelated production lifecycle change was made.
