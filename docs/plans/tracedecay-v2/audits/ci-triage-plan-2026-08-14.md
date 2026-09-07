# CI failure classification — run 31788759294

Scope: `codex/tracedecay-total-redesign-plan`, failed head `c962cd627`, local head
`2ff144f83`. Analysis is read-only. No Cargo command or test was run.

## Executive result

The supplied count of 484 does not match the log.

- 490 unique names occur in literal `test <name> ... FAILED` lines.
- Nextest adds 11 unique failures with no matching libtest failure line:
  one `ABRT` (stack overflow) and ten `TMT` identities.
- The complete extracted inventory is therefore **501 distinct identities**.
- 24 of the 490 libtest identities passed on retry in every job where they
  failed; 477 identities remained terminally failed/aborted/timed out in at
  least one job.
- Literal libtest identities by job: Dashboard 17, macOS 423, Linux 390.
  macOS/Linux overlap is 340. Windows ran zero tests: its build shard failed
  first, and the Test Windows aggregator reported `skipped`.

The primary partition below is exhaustive and sums to 501. Some diagnostic
families overlap the primary partition (for example a test can log both a busy
WAL checkpoint and an application-surface timeout); those correlations are
listed separately and must not be double-counted as independent failures.

## Method

1. Removed the log's literal `^[[...m` ANSI encoding.
2. Extracted every `test <path> ... FAILED` identity.
3. Parsed nextest `FAIL`, `ABRT`, and `TMT` status lines and added identities
   not emitted by libtest.
4. For each identity, inspected its retry block and captured the panic/error
   text. Appendix A contains one compact evidence line for every identity.
5. Compared the failed head and local head, inspected the named reform
   commits, and checked current production/test symbols.

## Primary class table

| Class | Count | Verdict | Single-concern task |
|---|---:|---|---|
| C01 retry-only failures | 24 | ENV/INFRA | Preserve as flake evidence; reproduce under equivalent load before changing code. |
| C02 Kiro live-handler fixture | 1 | STALE-TEST | Make the fixture provide the restored live-handler daemon boundary or assert the intentional fail-open diagnostic. |
| C03 semantic byte pins | 2 | STALE-TEST | Repin both workload hashes to the one authoritative 2170-chunk corpus. |
| C04 fact-store first touch | 1 | STALE-TEST | Replace removed `fact_store` dispatcher use with the canonical exact tool and daemon-owned profile setup. |
| C05 read-only ResetRequired authority | 1 | STALE-TEST | Expose the actual error, then assert the settled typed reset contract without pinning a superseded authority string. |
| C06 skill/tool discovery coverage | 1 | PRODUCT-BUG | Teach or explicitly internalize all 79 uncovered public tools; replace source-string coverage with behavioral discovery evidence. |
| C07a transcript concurrent CAS | 1 | PRODUCT-BUG | Restore one-winner compare-and-swap for concurrent full transcript batches. |
| C07b transcript summary projection fixtures | 2 | STALE-TEST | Publish summaries through the current verified lineage path or update counts to the intentional non-projection contract. |
| C08 worktree guard authority lifetime | 2 | STALE-TEST | Explicitly close or reuse the first runtime authority before the registry read; retain canonical-root assertions. |
| C09 LCM foreign-session canary | 1 | STALE-TEST | Use a non-secret canary; `sk-proj-...` is correctly rejected by privacy admission before lineage is exercised. |
| C10 update second-writer roster | 1 | STALE-TEST | Align the Cline expectation with the current stock-host canonical component set. |
| C11 socket Git stack overflow | 1 | PRODUCT-BUG | Isolate the recursive preview/apply route causing deterministic SIGABRT. |
| C12 session-registry convergence fixture | 4 | LIKELY-FIXED-BY `42bbaeb6a` + `f88610f33` | Confirm the four directly edited convergence tests under construction-time daemon scope. |
| C13 semantic evaluator paging timeouts | 7 | LIKELY-FIXED-BY `f8fec7b55` + `f282b313d` | Recheck the seven evaluator TMT identities after paged vector commit admission. |
| C14 missing database authority | 22 | STALE-TEST | Convert remaining fixtures to explicit daemon or exclusive-maintenance scopes; never widen ambient test authority. |
| C15 daemon/maintenance overlap | 7 | STALE-TEST | Remove nested incompatible scopes from fixture construction. |
| C16 macOS daemon clean early exit | 54 | ENV/INFRA | Isolate test daemons from nextest/process-group signals and record the terminating signal. |
| C17 configuration reset propagation | 4 | STALE-TEST | Seed the final configuration schema or assert ResetRequired; do not expect in-place migration. |
| C18 daemon disconnect/project retirement | 10 | PRODUCT-BUG | Keep the selected server alive through response settlement and close only after in-flight joins. |
| C19 application surface never mounts | 6 | PRODUCT-BUG | Find the owner that leaves the runtime in `mounting` for the full retry budget. |
| C21 SQLite lease expiry | 9 | PRODUCT-BUG | Renew/shorten bounded transactions so Linux load cannot expire live work. |
| C22 unverifiable summary timestamp | 4 | STALE-TEST | Add authoritative raw-source timestamps to the compression fixtures. |
| C23 memory graph publication conflict | 9 | PRODUCT-BUG | Make curator graph publication idempotent against its own verified head. |
| C24 WAL/graph lock lifecycle | 3 | PRODUCT-BUG | Drain readers/leases before checkpoint, reopen, or close. |
| C25 runtime timeout/deadlock | 22 | PRODUCT-BUG | Split by owner and fix missing completion/cancellation joins; do not raise timeouts. |
| C26 Dashboard response drift | 17 | STALE-TEST | Regenerate/align Dashboard fixtures to current typed envelopes and CAS fields. |
| C-P-AUTOMATION | 12 | PRODUCT-BUG | Repair automation execution/ledger/runtime failures. |
| C-P-DAEMON | 28 | PRODUCT-BUG | Repair daemon ownership, readiness, cancellation, and scheduler failures. |
| C-P-HOST | 19 | PRODUCT-BUG | Repair host mutation/rollback and privacy-admission behavior. |
| C-P-LCM | 7 | PRODUCT-BUG | Repair LCM privacy receipt and retained transport failures. |
| C-P-MCP | 21 | PRODUCT-BUG | Repair MCP dispatch, settlement, and process-tree behavior. |
| C-P-MEMORY | 4 | PRODUCT-BUG | Repair fact identity/lineage behavior. |
| C-P-MISC | 19 | PRODUCT-BUG | Repair isolated typed runtime defects not covered above. |
| C-P-SEARCH | 4 | PRODUCT-BUG | Repair remaining code-index/search activation failures. |
| C-P-SESSION | 1 | PRODUCT-BUG | Repair the remaining session ingestion failure. |
| C-P-STORAGE | 10 | PRODUCT-BUG | Repair storage identity, lock, and durable-state failures. |
| C-P-WORK | 5 | PRODUCT-BUG | Repair work/workflow routing and settlement failures. |
| C-S-AUTOMATION | 16 | STALE-TEST | Align automation DTO/status/count fixtures. |
| C-S-DAEMON | 23 | STALE-TEST | Align daemon contract assertions where the new typed state is intentional. |
| C-S-HOST | 35 | STALE-TEST | Align host/plugin/hook fixture contracts and generated assets. |
| C-S-LCM | 11 | STALE-TEST | Align LCM privacy, redaction, and retained-envelope fixtures. |
| C-S-MCP | 30 | STALE-TEST | Align tool names, schemas, parsers, and affected-test result DTOs. |
| C-S-MEMORY | 1 | STALE-TEST | Align the remaining fact-store expectation. |
| C-S-MISC | 14 | STALE-TEST | Align isolated renamed status/error expectations. |
| C-S-SEARCH | 5 | STALE-TEST | Align remaining search/eval pins and expected states. |
| C-S-SESSION | 5 | STALE-TEST | Align session/temporal fixture counts and hashes. |
| C-S-STORAGE | 10 | STALE-TEST | Align final-schema and storage error expectations. |
| C-S-WORK | 5 | STALE-TEST | Align work/workflow contract fixtures. |

## Hypothesis verification

### Known intentional reforms

All six named reform commits are ancestors of the failed head, not post-run
fixes:

- `c5c0a7663` live Hermes/Kiro handlers: ancestor of `c962cd627`.
- `2ec565ad5` unbound-observer/profile-minting behavior: ancestor.
- `e7a740457` 2170-chunk workload retarget: ancestor.
- `39629c4f5` daemon-pin corpus alignment: ancestor.
- `132b5b0ac` typed ResetRequired settlement: ancestor.
- `79406945a` canonical isolated-path spelling fix: ancestor.

Therefore C02-C05 are expectation/fixture drift at the failed head; they are
not fixed merely because those commits exist locally.

Evidence:

- C02 actual: Kiro exits 0 with `{}` but emits
  `local counter reset daemon call failed`; the test invokes a restored live
  handler without a daemon and still requires empty stderr.
- C03 both tests compare actual
  `sha256:068e...5610` against stale `sha256:a8e1...1347`.
- C04 fails before initialization with `unknown tool: 'fact_store'`; the
  removed broad dispatcher cannot prove first-touch behavior.
- C05 still checks `authority == "graph store"` and does not print the actual
  typed error, while the settlement reform preserves the owning authority's
  ResetRequired state. If adding diagnostic context shows a non-ResetRequired
  variant, reclassify this one test as PRODUCT-BUG rather than weakening it.

### Skill coverage

C06 is not one missing tool. The panic lists **79** uncovered tools:

`tracedecay_github_stack_signal_expand`, `tracedecay_stack_snapshot`,
the five native-integration lifecycle tools, three multi-root tools, five
feedback tools, `tracedecay_affected_tests`, six fact-store read/reason tools,
`tracedecay_memory_status`, `tracedecay_session_refresh`,
`tracedecay_rename_symbol`, `tracedecay_observatory_read`, 32 work tools,
16 workflow-definition/run/handoff tools, and five worktree cleanup/inventory
tools.

Exact uncovered set:

- Stack/native/multi-root:
  `tracedecay_github_stack_signal_expand`, `tracedecay_stack_snapshot`,
  `tracedecay_preflight_native_integration`,
  `tracedecay_approve_native_integration`,
  `tracedecay_apply_native_integration`,
  `tracedecay_native_integration_status`,
  `tracedecay_cancel_native_integration`,
  `tracedecay_multi_root_scope_set_read`,
  `tracedecay_multi_root_scope_set_compare_and_swap`,
  `tracedecay_multi_root_execute`.
- Feedback/memory/session:
  `tracedecay_feedback_diagnostics`, `tracedecay_feedback_get`,
  `tracedecay_feedback_expand`, `tracedecay_feedback_list`,
  `tracedecay_feedback_impact`, `tracedecay_affected_tests`,
  `tracedecay_fact_store_probe`, `tracedecay_fact_store_related`,
  `tracedecay_fact_store_reason`, `tracedecay_fact_store_contradict`,
  `tracedecay_fact_store_get`, `tracedecay_fact_store_list`,
  `tracedecay_memory_status`, `tracedecay_session_refresh`,
  `tracedecay_rename_symbol`, `tracedecay_observatory_read`.
- Work:
  `tracedecay_work_generate_proposal`, `tracedecay_work_create`,
  `tracedecay_work_review_proposal`, `tracedecay_work_accept_proposal`,
  `tracedecay_work_admit_execution`, `tracedecay_work_start_attempt`,
  `tracedecay_work_synthesize`, `tracedecay_work_attempt_status`,
  `tracedecay_work_cancel_attempt`, `tracedecay_work_resume_attempts`,
  `tracedecay_work_retry_attempt`, `tracedecay_work_list_attempts`,
  `tracedecay_work_execution_history`, `tracedecay_work_hydrate_artifacts`,
  `tracedecay_work_retrieve_evidence`, `tracedecay_work_views`,
  `tracedecay_work_experience`, `tracedecay_work_compare_proposal`,
  `tracedecay_work_prepare_graph_mutation`,
  `tracedecay_work_mutate_graph`, `tracedecay_work_topology`,
  `tracedecay_work_topology_metrics`,
  `tracedecay_work_prepare_duplicate_adjudication`,
  `tracedecay_work_adjudicate_duplicate`,
  `tracedecay_work_adjudicate_leak`, `tracedecay_work_pause_run`,
  `tracedecay_work_resume_run`, `tracedecay_work_run_control`,
  `tracedecay_work_placement_preflight`,
  `tracedecay_work_admit_placement`,
  `tracedecay_work_placement_status`,
  `tracedecay_work_release_placement`.
- Workflow:
  `tracedecay_workflow_register_definition`,
  `tracedecay_workflow_activate_definition`,
  `tracedecay_workflow_retire_definition`,
  `tracedecay_workflow_reject_definition`,
  `tracedecay_workflow_validate_definition`,
  `tracedecay_workflow_get_definition`,
  `tracedecay_workflow_list_definitions`,
  `tracedecay_workflow_definition_history`,
  `tracedecay_workflow_diff_definition`,
  `tracedecay_workflow_handoff_issue`,
  `tracedecay_workflow_handoff_redeem`,
  `tracedecay_workflow_start_run`, `tracedecay_workflow_pause_run`,
  `tracedecay_workflow_resume_run`, `tracedecay_workflow_cancel_run`,
  `tracedecay_workflow_get_run`.
- Worktree:
  `tracedecay_worktree_inventory`,
  `tracedecay_worktree_cleanup_inspect`,
  `tracedecay_worktree_cleanup_confirm`,
  `tracedecay_worktree_cleanup_remove`,
  `tracedecay_worktree_cleanup_reconcile`.

This is a real discoverability gap, although the current test's body-string
scan is not acceptable as the final behavioral acceptance mechanism.

### macOS path hypothesis is disproved

At failed head `c962cd627`, `under_isolated_root` already:

1. tries raw `starts_with`;
2. canonicalizes the root through the deepest existing ancestor;
3. canonicalizes the candidate path the same way;
4. compares the canonical spellings.

That is exactly the `/var` -> `/private/var` settlement from `79406945a`.
Moreover, the errors are not macOS-only:

- missing managed-daemon/maintenance authority: 90 failed attempts across
  26 tests (macOS 48, Linux 42);
- daemon/maintenance overlap: 28 failed attempts across seven tests
  (macOS 14, Linux 14).

The remaining issue is fixture scope ownership, not path spelling. The
post-run `f88610f33` diff confirms this diagnosis by keeping a
`DaemonDatabaseScope` alive in the four convergence fixtures.

### Environment/lifecycle families

| Signature | Raw occurrences | Distinct tests | OS conclusion | Classification |
|---|---:|---:|---|---|
| daemon exits status 0 before socket | 107 | 54 | macOS only | ENV/INFRA signal leakage |
| WAL checkpoint `busy=1` | 34 | 15 | macOS + Linux | secondary lifecycle symptom |
| `application.surface.unavailable` | 95 | 17 | macOS + Linux | PRODUCT-BUG unless a stale reset fixture precedes it |
| configuration persisted shape reset | 16 | 4 | macOS + Linux | STALE-TEST fixture |
| daemon closed connection | 17 | 6 | macOS + Linux | PRODUCT-BUG |
| retained server retired | 14 | 12 | macOS + Linux | PRODUCT-BUG |
| child did not exit within 20s | 5 | 3 | Linux only | PRODUCT-BUG/process leak |
| SQLite transaction lease expired | 26 | 16 | Linux only | PRODUCT-BUG for terminal failures; retry-pass subset is ENV/INFRA |

`run_foreground_unix` has no ordinary status-0 pre-socket return for these
fresh-home fixtures: after excluding the fresh-home-inapplicable account
deletion resume, a clean return comes from its Ctrl-C/SIGTERM select branch.
The macOS-only, empty-stderr pattern therefore points to test-runner/process-
group signal leakage, not a product startup rejection.

The WAL line is often only logged by the daemon while the test later fails for
another reason. `checkpoint_result` truthfully returns busy/incomplete; the
dispatch task is lifecycle drainage, not weakening the error.

### Named residual families

- Transcript concurrent full batches: both writers return `Ok(())`; this is a
  real split-brain/CAS defect.
- Transcript late-cursor and stale-higher tests: raw rows commit but summary
  count remains zero. Their synthetic summary bypasses the current immutable
  lineage publication contract; align the fixture before declaring a storage
  regression.
- Worktree canonical-root pair: both fail before their canonical-root
  assertions because a prior `TraceDecay` authority is still incompatible
  with `HostAdmissionTestRuntimeV1::profile`; these fixtures use `drop` instead
  of the explicit consuming `TraceDecay::close`.
- LCM canary: `FOREIGN_CANARY` is
  `sk-proj-lineage-foreign-canary-1234567890`, intentionally secret-shaped;
  privacy admission blocks setup before foreign-lineage disclosure is tested.
- Update second-writer: `host_owns_canonical_component_set` derives from all
  stock host kinds with non-empty default components; Cline is now in that
  canonical set, while the test still puts it with Zed/Roo/Kilo.
- Nextest-only abort:
  `daemon::tests::socket::socket_git_preview_apply_replay_and_pre_admission_problems_are_canonical`
  deterministically stack-overflows and SIGABRTs on both macOS and Linux.

## Post-run commits that plausibly remove failures

- `42bbaeb6a`: production now schedules historical schema convergence instead
  of leaving it pending forever.
- `f88610f33`: maintenance mode is fixed at registry construction and the four
  convergence tests now hold daemon database scope for the fixture lifetime.
- `5a00ee063`..`f282b313d`: semantic evaluation now exposes source errors,
  isolates source projection, uses an identity receipt, pages durable vector
  commits, and admits page-local change-set digests.
- `2ff144f83`: rustfmt-only checkpoint; it plausibly clears the Format job.

These are **likely-fixed**, not verified: this task forbids Cargo runs.

## Non-test CI blockers

1. **Windows compile — PRODUCT-BUG, still open.**
   `crates/tracedecay-usecases/src/retention/code_index_generations/scope_quarantine.rs:422`
   calls `creation_time()` through the wrong metadata extension trait on
   Windows (`cap_fs_ext::Metadata` implements `MetadataExt`, not
   `OsMetadataExt`). The Windows test job consequently ran zero tests.
2. **Clippy — PRODUCT-BUG, still open.**
   - `crates/tracedecay-policy/src/work_loop.rs:515`: 8 arguments.
   - `crates/tracedecay-store/src/memory/project_memory/mod.rs:116`: 9 arguments.
   - `crates/tracedecay-store/src/memory/project_memory/curation/operations.rs:226`:
     redundant closure.
   No post-run commit touches those files.
3. **Format — LIKELY-FIXED-BY `2ff144f83`.**
   The failed paths (`paths_and_io.rs`, `semantic_evaluation.rs`,
   `hook_cmd.rs`, `grafeo_restart_acceptance.rs`) are in the post-run rustfmt
   delta.

## Ordered fix dispatch

1. Windows compile (`creation_time`) so the Windows shard can execute.
2. C11 deterministic stack overflow; it is a process abort, not assertion drift.
3. C16 macOS test-daemon signal isolation; it masks 54 identities.
4. Recheck C12 on the four directly changed convergence tests.
5. Recheck C13 semantic TMT tests after `f8fec7b55`/`f282b313d`.
6. C14/C15 fixture authority cutover; keep explicit production scopes.
7. C18/C19/C24/C25 daemon settlement, mounting, checkpoint, and join defects.
8. C21 Linux transaction lease expiry.
9. C07a transcript CAS.
10. C23 memory graph publication conflict.
11. C08 worktree authority lifetime.
12. C06 public-tool discovery/skill coverage.
13. C17/C22/C09 stale reset/timestamp/canary fixtures.
14. C02-C05 and C10 narrow stale reform expectations.
15. C26 Dashboard typed contract/CAS fixture alignment.
16. C-S-MCP and C-S-HOST generated tool/host contract alignment.
17. Remaining subsystem product lanes, then remaining stale-test lanes.
18. Clippy cleanup.
19. Run narrow non-vacuous checks per lane, then a fresh complete main-CI run.

## Class membership index

Appendix A is the affected-test list. The following selectors make the primary
partition reproducible:

- C01 contains these 24 retry-only identities:
  `agents::context_scout_model::tests::configured_model_measures_usage_when_backend_omits_token_counts`,
  `agents::context_scout_model::tests::denied_backend_surfaces_denied_not_unavailable`,
  `agents::context_scout_model::tests::disconnected_backend_surfaces_disconnect_not_unavailable`,
  `agents::context_scout_model::tests::production_adapter_sends_only_bounded_candidates_and_retains_usage`,
  `daemon::code_index_scheduler::tests::registry_feeds_publications_and_bounded_freshness_reads`,
  `daemon::service::invocation::tests::lsp_tests::lsp_disconnect_expiry_settles_unacknowledged_outbound_as_dropped`,
  `daemon::tests::bootstrap::remote_account_deletion_joins_admitted_open_before_enumeration_and_reconciles_restart`,
  `exact_sql::tests::authority::long_lease_transaction_renews_its_lease_after_successful_bounded_steps`,
  `global_db::upsert_session_message_preserves_oversized_text_losslessly`,
  `jobs::concurrent_manual_job_triggers_do_not_double_execute`,
  `lcm_payload::delete_external_payload_rejects_referenced_payload_without_hash_verification`,
  `lcm_payload::replay_and_successor_reuse_reject_mutated_payload_global_authority`,
  `lcm_payload::summary_publication_binds_external_payload_manifest_and_sanitization_receipt`,
  `lcm_query::expand::expand_returns_sliced_raw_summary_and_payload_content_with_ranges`,
  `lcm_query::status::status_reports_payload_gc_run_metadata_after_apply`,
  `lcm_raw::transcript_ingest_preserves_lossless_raw_content`,
  `persistence_failure_cannot_be_rewritten_as_a_clean_terminal`,
  `runtime::lcm::payload::rollback_tests::direct_store_failure_rolls_back_metadata_and_payload_file`,
  `session_temporal_benchmark::tests::fixture_refresh_persists_progress_before_measurement`,
  `tool_daemon_test::cursor_after_shell_missing_daemon_exits_promptly_without_children`,
  `tool_daemon_test::status_json_requests_compact_daemon_payload_noninteractively`,
  `tool_daemon_test::tool_cli_invokes_mcp_tool_through_daemon_socket`,
  `tool_daemon_test::tool_cli_rejects_truncated_json_rpc_response_without_hanging`,
  `tool_daemon_test::tool_cli_skips_daemon_notifications_until_matching_response`.
- C02-C11 are the exact named tests in their table descriptions.
- C12 is the four `session_registry::tests` entries directly changed by
  `f88610f33`: daemon admission, duplicate attach, convergence checkpoint, and
  degraded convergence.
- C13 is the six nextest-only `candidate_output::tests` TMT entries plus
  `packaged_assets::tests::packaged_evaluator_runs_against_an_unrelated_project`.
- C14-C24 select Appendix entries by their literal evidence phrase:
  missing database authority, scope overlap, status-0 daemon exit,
  configuration reset, daemon-close/retired-server, application-surface
  unavailable, transaction expiry, unverifiable timestamp, memory graph
  publication conflict, and WAL/graph lock respectively. Earlier exact classes
  win when an identity matches more than one selector.
- C25 selects remaining terminal timeout/deadline/`Elapsed` identities.
- C26 is every Dashboard identity.
- Remaining identities are partitioned by subsystem prefix and then by
  evidence: expected-value/schema/name/DTO drift is `C-S-*`; runtime errors,
  conflicts, unavailable states, lock failures, and invariant violations are
  `C-P-*`.

## Appendix A — all failing identities and compact error snippets

Format: `identity [jobs] — first useful panic/error text`. `<TMP>`, `<HASH>`,
and `<N>` normalize unstable values. `nextest` marks failures that had no
`test ... FAILED` line.

- `advanced_workflow_journey_test::mounted_fan_out_recovers_then_synthesizes_and_hands_off` [Linux/macOS] — advanced Work TaskSession journey requires the byte-pinned FastEmbed fixture in TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE
- `agent_cmd::tests::codex_core_rollback_restores_generated_agent_exports_byte_for_byte` [Linux/macOS] — called `Result::unwrap()` on an `Err` value: NativeUpdateRequired
- `agent_cmd::tests::codex_native_removed_retry_cleans_receipt_owned_source` [Linux/macOS] — called `Result::unwrap()` on an `Err` value: bundle ownership marker conflicts or is ambiguous
- `agent_cmd::tests::kimi_canonical_component_set_fails_before_direct_host_mutation` [Linux/macOS] — assertion failed: error does not contain `host capability is unsupported`
- `agent_cmd::tests::kiro_context_mcp_apply_converges_without_rollback` [macOS] — Kiro Install apply did not converge after an interrupted host-bundle operation
- `agent_cmd::tests::opencode_core_rollback_restores_every_registration_side_effect` [Linux/macOS] — `StalePreview` at host component registration
- `agents::codex::mcp_registry::tests::add_and_remove_preserve_an_operator_owned_peer_server` [macOS] — host add changed Codex-owned peer state and left host state unaccepted
- `agents::context_scout_model::tests::configured_model_measures_usage_when_backend_omits_token_counts` [Linux] — `DeadlineExceeded`
- `agents::context_scout_model::tests::denied_backend_surfaces_denied_not_unavailable` [Linux] — `Err(DeadlineExceeded)` vs `Err(Denied)`
- `agents::context_scout_model::tests::disconnected_backend_surfaces_disconnect_not_unavailable` [Linux] — `Err(DeadlineExceeded)` vs `Err(Disconnected)`
- `agents::context_scout_model::tests::production_adapter_sends_only_bounded_candidates_and_retains_usage` [Linux] — `DeadlineExceeded`
- `agents::copilot::tests::add_and_remove_preserve_an_operator_owned_peer_server` [macOS] — host add changed peer MCP servers and left host state unaccepted
- `agents::host_cli::tests::env_shebang_interpreter_is_resolved_before_ambient_path_is_cleared` [macOS] — interpreter command bytes differ despite the same rendered command
- `agents::kiro::tests::add_and_remove_preserve_an_operator_owned_peer_server` [macOS] — forced host add changed peer MCP servers and left host state unaccepted
- `agents::kiro::tests::rollback_refuses_a_foreign_registry_write_after_cli_apply` [macOS] — fake native add returned `StorageFailure`
- `agents::path_normalize_tests::path_lookup_preserves_non_unicode_parent_components` [macOS] — `Illegal byte sequence`
- `analytics_api::tests::diagnostics_summary_aggregates_real_hook_completed_rows_safely` [Linux/macOS] — `"unavailable"` vs `"measured"`
- `api::automation_outcomes_endpoint_returns_live_read_only_outcomes` [Dashboard] — expected activated skill outcome; response contains facts but no skills
- `api::holographic_dashboard_endpoints_return_seeded_payloads` [Dashboard] — graph-assist coverage is null, expected `"complete"`
- `api::holographic_fact_detail_returns_full_content_and_entities` [Dashboard] — expected `linked_entities`
- `api::lcm_endpoints_cover_seeded_fts_and_like_fallback` [Dashboard] — null vs true
- `api::lcm_project_store_wins_over_global_accounting_override` [Dashboard] — null vs `"profile_sharded"`
- `api::lcm_serves_project_session_store_without_global_override` [Dashboard] — null vs `"profile_sharded"`
- `application_surface::tests::catalog_bound_compatibility_tools_resolve_before_retained_dispatch` [Linux/macOS] — live catalog-bound compatibility-tool set differs from expected set
- `authentic_callback_to_all_delivery_surfaces` [Linux/macOS] — LSP surface absent
- `authority_tests::a_selected_project_answers_feedback_and_work_reads_and_nothing_else_by_post` [Linux/macOS] — `None` vs `Some(Work)`
- `authority_tests::application_routes_are_active_project_only` [Linux/macOS] — HTTP 404 vs 204
- `authority_tests::dashboard_user_job_run_without_automation_authority_fails_closed` [Linux/macOS] — HTTP 500 vs 503
- `authority_tests::graph_overview_returns_the_canonical_dashboard_envelope` [Linux/macOS] — `"unknown"` vs `"ready"`
- `authority_tests::memory_status_returns_the_canonical_dashboard_envelope` [Linux/macOS] — `"error"` vs `"ready"`
- `automation::automation_run_artifact_api_serves_verified_sidecar_payloads` [Dashboard] — malformed automation ledger `started_at`
- `automation::final_self_improvement_smoke_covers_autonomous_curation_and_skill_deployment` [Dashboard] — settings patch lacks `expected_revision_id`
- `automation::jobs::effect_receipt::tests::file_write_failure_after_open_is_a_bound_partial_effect` [macOS] — post-open failure was not a partial effect
- `automation_jobs::dashboard_user_job_history_appears_only_after_retained_settlement` [Dashboard] — user-job backend never reached admitted execution
- `automation_skills::managed_skills_are_dashboard_controllable_with_direct_activation` [Dashboard] — disable did not export to Claude
- `backend::classifies_backend_failures_for_retry_policy` [Linux/macOS] — `Disconnected` vs `Unavailable`
- `backend::failure_disposition_heals_stale_recorded_retryability` [Linux/macOS] — `Some(Disconnected)` vs `Some(Unavailable)`
- `cancellation_before_relational_cas_keeps_the_prior_head_current` [macOS] — `Conflict`
- `cancelling_generic_tool_reaps_child_and_closes_request` [Linux/macOS] — daemon exited status 0 before accepting connections
- `candidate_output::tests::candidate_bytes_match_direct_production_calls` [nextest] — TMT after 360s
- `candidate_output::tests::direct_outputs_cover_train_and_validation` [nextest] — TMT after 360s
- `candidate_output::tests::evaluation_rejects_optional_stage_status_that_disagrees_with_profile` [nextest] — TMT after 360s
- `candidate_output::tests::published_corpus_maps_production_source_occurrences` [Linux/macOS] — `Unavailable(ReadFailed)` vs `Complete`
- `candidate_output::tests::query_phrase_and_historical_queries_reach_their_checked_in_anchors` [Linux/macOS] — checked-in historical anchor absent
- `candidate_output::tests::rerank_profiles_remain_pending_when_no_rerank_measurement_ran` [nextest] — TMT after 360s
- `candidate_output::tests::resource_evidence_enforces_state_budgets_and_exact_catalog` [nextest] — TMT after 360s
- `candidate_output::tests::semantic_profiles_do_not_claim_a_comparison_when_only_fallback_ran` [nextest] — TMT after 360s
- `claude_plugin_bundle_test::claude_agents_allow_only_live_read_only_mcp_tools` [Linux/macOS] — automation-auditor grants `tracedecay_analytics`, whose live `readOnlyHint` is not true
- `claude_plugin_schema_test::claude_bundle_hooks_config_matches_the_claude_hooks_schema` [Linux/macOS] — Claude hooks schema rejects `PostCompact`
- `cli_args_contract_test::arg_catalog_table_flags_exist_in_tool_schemas` [Linux/macOS] — catalog documents `--keywords` absent from context schema and documents removed `fact_store`
- `cli_args_contract_test::managed_skill_guidance_matches_automatic_activation` [Linux/macOS] — managed-skill approval/review-queue guidance is stale
- `cli_mcp_and_http_dispatch_the_same_callable_contracts` [Linux/macOS] — `github_stack_signal_expand` lacks parity-golden accounting
- `cli_non_interactive_test::automation_config_enable_writes_canonical_project_setting_noninteractively` [Linux/macOS] — application surface unavailable, then not-found/not-authorized
- `cli_non_interactive_test::automation_config_set_rejects_unimplemented_external_backend` [Linux/macOS] — stderr lacks `unknown automation backend`
- `cli_non_interactive_test::automation_config_set_writes_complete_canonical_project_setting_noninteractively` [Linux/macOS] — application surface unavailable, then not-found/not-authorized
- `cli_non_interactive_test::branch_add_tracks_the_branch_on_the_single_project_store` [Linux/macOS] — code-index scheduler unavailable for branch activation
- `cli_non_interactive_test::branch_gc_preserves_profile_shard_without_repository_evidence` [Linux/macOS] — configuration persisted shape requires reset
- `cli_non_interactive_test::branch_list_reads_profile_sharded_branch_meta` [Linux/macOS] — fixture project never mounted
- `cli_non_interactive_test::branch_remove_deletes_branch_db_from_profile_shard` [Linux/macOS] — configuration persisted shape requires reset
- `cli_non_interactive_test::branch_remove_deletes_branch_local_memory_without_cutover_receipt` [Linux/macOS] — configuration persisted shape requires reset
- `cli_non_interactive_test::branch_removeall_deletes_profile_shard_branch_dbs` [Linux/macOS] — configuration persisted shape requires reset
- `cli_non_interactive_test::fact_store_curate_records_backend_disabled_skip_and_preserves_read_only_inspection` [Linux/macOS] — null vs `"memory_curator"`
- `cli_non_interactive_test::gitignore_reads_effective_config_for_primary_and_linked_worktrees` [Linux] — application surface unavailable while reading effective configuration
- `cli_non_interactive_test::init_skips_gitignore_prompt_when_stdin_not_a_terminal` [Linux/macOS] — initialization stderr no longer matches the expected noninteractive text
- `cli_non_interactive_test::install_codex_automation_enables_daemon_owned_project_configuration_noninteractively` [Linux/macOS] — `Text file busy`
- `cli_non_interactive_test::list_all_reports_orphan_manifest_reconstructable_store` [macOS] — daemon exited status 0 before accepting connections
- `cli_non_interactive_test::list_all_reports_profile_sharded_store_without_stale_label` [macOS] — daemon exited status 0 before accepting connections
- `cli_non_interactive_test::list_all_uses_registry_profile_shard_when_enrollment_marker_missing` [macOS] — daemon exited status 0 before accepting connections
- `cli_non_interactive_test::projects_context_resolves_linked_worktree_path_by_git_common_dir` [macOS] — daemon exited status 0 before accepting connections
- `cli_non_interactive_test::projects_context_resolves_project_id_and_path` [macOS] — daemon exited status 0 before accepting connections
- `cli_non_interactive_test::projects_list_json_reads_global_registry` [macOS] — daemon exited status 0 before accepting connections
- `cli_non_interactive_test::projects_search_text_matches_registered_alias` [macOS] — daemon exited status 0 before accepting connections
- `cli_non_interactive_test::sessions_search_omits_absent_optional_filters_and_preserves_provider` [macOS] — daemon exited status 0 before accepting connections
- `cli_non_interactive_test::sessions_unfinished_lists_workflow_state_evidence` [Linux/macOS] — daemon exited status 0 before accepting connections
- `cli_non_interactive_test::status_reports_uninitialized_project_without_creating_it` [macOS] — daemon exited status 0 before accepting connections
- `cli_non_interactive_test::status_surfaces_split_identity_conflict_without_suggesting_init` [Linux/macOS] — daemon exited status 0 before accepting connections
- `cli_non_interactive_test::storage_report_prints_registered_store_size_and_unregistered_backlog` [Linux/macOS] — report JSON is empty/EOF
- `cli_non_interactive_test::storage_report_uses_active_daemon_authority_without_hanging` [macOS] — daemon exited status 0 before accepting connections
- `cli_non_interactive_test::wipe_all_removes_profile_sharded_store_and_global_row` [macOS] — shard `tracedecay.db` remains
- `cli_non_interactive_test::wipe_all_removes_registry_backed_profile_shard_without_enrollment_marker` [macOS] — registry-backed profile shard remains
- `code_diagnostics::code_diagnostics_dashboard_api_exposes_engines_and_applies_settings` [Dashboard] — HTTP 503 vs 200
- `codex_compaction::codex_post_compact_hook_commits_app_server_summary_through_daemon_effect` [Linux/macOS] — daemon exited status 0 before accepting connections
- `codex_goals::codex_workflow_lifecycle_secret_content_is_sanitized_before_persistence` [Linux/macOS] — secret-bearing goal was not sanitized before persistence
- `codex_response_items::codex_goal_response_item_is_cataloged_as_context` [Linux/macOS] — LCM privacy sanitizer receipt construction failed
- `codex_response_items::codex_response_item_skips_developer_messages_and_keeps_reasoning_summaries` [Linux/macOS] — LCM privacy sanitizer receipt construction failed
- `codex_usage::codex_structured_events_produce_full_row_mix` [Linux/macOS] — LCM privacy sanitizer receipt construction failed
- `commands::storage::wipe_target_tests::registered_project_paths_preserve_non_unicode_roots` [macOS] — expected non-Unicode path absent
- `compare_reports_unmeasured_semantic_and_rerank_stages_as_pending` [Linux/macOS] — `"fail"` vs `"pending"`
- `config::tests::runtime_configuration_cutover::resolve_runtime_configuration_pins_registered_project_when_cache_is_cold` [Linux/macOS] — configuration ResetRequired: no canonical revision
- `config::validation_rejects_zero_scheduler_tick_secs` [Linux/macOS] — error no longer contains `scheduler_tick_secs`
- `cursor::cursor_pre_compact_without_native_payload_is_read_only_and_unavailable` [Linux/macOS] — daemon exited status 0 before accepting connections
- `cursor::cursor_transcript_ingest_retries_after_mid_batch_db_failure` [Linux/macOS] — required projection-audit trigger missing
- `cursor_composer::composer_envelope_todo_secret_is_sanitized_before_persistence` [Linux/macOS] — secret-bearing todo was not sanitized
- `cursor_native_extension_receipt_matches_embedded_assets` [Linux/macOS] — `Corrupt` vs `Current`
- `daemon::automation_effect::journal::tests::cancellation_observed_under_lock_leaves_foreign_reservation_pending` [Linux/macOS] — curation receipt missing `accepted_operations`
- `daemon::automation_effect::journal::tests::durable_journal_rejects_swapped_partial_receipts_before_write` [Linux/macOS] — curation receipt missing `accepted_operations`
- `daemon::automation_effect::journal::tests::durable_journal_reports_changed_project_owner_as_a_conflict` [Linux/macOS] — prepared-effect binding now rejected as inconsistent
- `daemon::automation_effect::journal::tests::durable_journal_reports_changed_scope_identity_as_a_conflict` [Linux/macOS] — recovery problem now rejected as inconsistent
- `daemon::automation_effect::journal::tests::durable_journal_reports_changed_task_identity_as_a_conflict` [Linux/macOS] — recovery problem now rejected as inconsistent
- `daemon::automation_effect::journal::tests::foreign_reservation_recovery_persists_exact_partial_terminal` [Linux/macOS] — curation receipt missing `accepted_operations`
- `daemon::automation_effect::journal::tests::pending_index_survives_physical_reopen_and_closes_after_terminal` [Linux/macOS] — curation receipt missing `accepted_operations`
- `daemon::automation_effect::journal::tests::physical_reopen_rejects_a_corrupt_swapped_terminal` [Linux/macOS] — corrupt fixture rejected during construction as inconsistent
- `daemon::automation_effect::journal::tests::project_open_repairs_corrupt_append_intent_at_clean_eof_without_pending_journals` [Linux/macOS] — corrupt append-intent path missing
- `daemon::automation_effect::journal::tests::reserved_admission_conflict_preserves_recovery_index` [Linux/macOS] — prepared-effect binding rejected as inconsistent
- `daemon::automation_effect::journal::tests::reserved_read_removes_an_orphan_terminal_sidecar` [Linux/macOS] — expected reservation read error did not occur
- `daemon::automation_effect::journal::tests::retained_projector_panic_finishes_recovery_before_releasing_task_lock` [nextest] — TMT after 360s
- `daemon::automation_effect::journal::tests::terminal_admission_conflict_preserves_existing_cleanup_authority` [Linux/macOS] — prepared-effect binding rejected as inconsistent
- `daemon::automation_effect::projection::tests::all_noop_curation_projects_accepted_effects_without_mutation_or_anchors` [Linux/macOS] — canonical all-noop receipt contains another owner's fact
- `daemon::branch_admin::tests::profile_bootstrap_preserves_future_spool_reset_without_retry_mapping` [Linux/macOS] — profile identity root permissions are not 0700
- `daemon::code_index_scheduler::activation_tests::cold_mount_defers_sealed_decode_and_truth_verification_to_the_retained_owner` [macOS] — retained owner did not activate the sealed generation
- `daemon::code_index_scheduler::branch_generations::tests::mounted_store_diffs_two_clean_exact_commit_generations` [Linux/macOS] — exact-generation read timed out
- `daemon::code_index_scheduler::ignored_dependencies_tests::cancellation_tests::admitted_source_read_observes_live_cancellation_between_chunks` [macOS] — unexpected `IgnoredDependency(SymlinkEscape)`
- `daemon::code_index_scheduler::tests::registry_feeds_publications_and_bounded_freshness_reads` [Linux] — initial publication timed out
- `daemon::git_transactions::native::tests::apply_rematerializes_exact_commit_input_after_executor_restart` [Linux/macOS] — preview is `Unsupported`
- `daemon::git_transactions::native::tests::files_ref_backend_exposes_no_destination_publication_window` [Linux/macOS] — typed preview is `Unsupported`
- `daemon::git_transactions::native::tests::native_blockers_never_mint_a_preview_from_stale_caller_state` [Linux/macOS] — stale materialization did not return `StalePreview`
- `daemon::git_transactions::native::tests::snapshot_capture_agrees_across_symlink_repository_root_aliases` [Linux/macOS] — drifted preview CAS did not report stale preview
- `daemon::http_application_tests::daemon_http_authenticated_operations_cancel_and_resume_through_canonical_owner` [Linux/macOS] — cancel returned HTTP 503
- `daemon::invocation_executor::controlled_invocation_tests::in_process_effect_without_settlement_returns_reset_required` [Linux/macOS] — authoritative join timed out
- `daemon::lcm_effects::tests::codex_and_cursor_daemon_adapters_commit_exact_authoritative_summaries` [Linux/macOS] — `"needs_summary"` vs `"ok"`
- `daemon::lcm_effects::tests::compression_producer_apply_read_and_rollback_stay_one_authority` [Linux/macOS] — relation reads reconstructed a pending projection
- `daemon::production_harness::configuration_idempotency_journey_test::configuration_set_has_cli_mcp_http_sdk_parity_and_replays_after_restart` [Linux/macOS] — first CLI configuration effect is not-found/not-authorized
- `daemon::production_harness::configuration_idempotency_journey_test::credential_effect_uses_the_durable_request_operation_digest` [Linux/macOS] — typed tool payload is invalid JSON
- `daemon::production_harness::configuration_idempotency_journey_test::user_profile_configuration_batch_has_cli_dashboard_parity_after_restart` [Linux/macOS] — session relation graph database remains locked on restart
- `daemon::production_harness::generation_retention_test::linked_worktree_scope_retention_crash_replay_and_pure_inventory_journey` [Linux/macOS] — distribution FastEmbed fixture missing
- `daemon::production_harness::generation_retention_test::mounted_daemon_maintenance_retains_activation_lease_and_converges_after_restart` [Linux/macOS] — committed query-only profile did not expose known-empty retention authority
- `daemon::production_harness::semantic_activation_journey_test::public_semantic_activation_rollback_and_exact_retry_preserve_graph_authority` [Linux/macOS] — distribution FastEmbed fixture missing
- `daemon::project_open_owners::code_index_reads::ignored_dependency_admission_tests::writable_binding_returns_only_after_exact_scope_generation_is_warm_and_serving` [Linux/macOS] — code graph projection had not completed activation
- `daemon::query_authority_provider::tests::activation_tests::committed_query_routes_install_and_rollback_as_one_revision` [macOS] — active vector generation is `None`
- `daemon::retained_owner::memory_target::tests::selected_project_opens_its_exact_read_only_store_not_the_active_store` [macOS] — `Unavailable`
- `daemon::retained_owner::session::retained_effect_tests::retained_begin_and_join_report_partial_effect_and_restart_recovers_same_operation` [Linux/macOS] — manifest digest mismatch
- `daemon::retained_owner::session::retained_effect_tests::retained_cancel_reports_partial_effect_with_canonical_cancelled_receipt` [Linux/macOS] — manifest digest mismatch
- `daemon::scheduler::combined_effect::tests::conflicting_reflector_abandons_only_the_fresh_skill_reservation` [macOS] — 0 vs 1
- `daemon::scheduler::combined_effect::tests::conflicting_skill_abandons_only_the_fresh_reflector_reservation` [macOS] — 0 vs 1
- `daemon::scheduler::combined_effect::tests::partial_replay_reuses_prior_scheduler_skip_without_current_publication` [macOS] — duplicate project authority
- `daemon::service::invocation::tests::dispatch_tests::feedback_handles_fail_closed_without_an_owner` [Linux/macOS] — absent owner returns `Unavailable`, not expected application problem shape
- `daemon::service::invocation::tests::dispatch_tests::multi_root_payloads_are_not_served_by_the_per_project_service` [Linux/macOS] — response is not `InvalidRequest`
- `daemon::service::invocation::tests::lsp_tests::lsp_disconnect_expiry_settles_unacknowledged_outbound_as_dropped` [Linux] — observability persistence deadline
- `daemon::service::invocation::tests::project_lifecycle_tests::recovery_quiescence_retires_only_the_selected_projects_lsp_owners` [Linux/macOS] — authorized-root scope set is invalid
- `daemon::service::project_runtime::observability_tests::each_producer_lifetime_uses_a_disjoint_ordered_stream` [Linux/macOS] — delivery settlement recorder already running
- `daemon::service::project_runtime::observability_tests::exact_profile_routing_collapses_linked_roots_without_crossing_profiles` [Linux/macOS] — expected distinct brain IDs but both are `brain.test-runtime`
- `daemon::session_sync::tests::cancel_in_alias_activation_gap_mirrors_primary_terminal_receipt` [Linux/macOS] — project cannot bind a foreign session shard
- `daemon::store_runtime::session_registry::code_graph::seals::tests::project_replay_pool_serializes_same_digest_from_distinct_sources` [macOS] — `Conflict`
- `daemon::store_runtime::session_registry::code_graph::seals::tests::replay_seal_publish_preserves_foreign_existing_destination` [Linux/macOS] — `Corrupt` vs `Conflict`
- `daemon::store_runtime::session_registry::project_memory_relation_graph_contract_tests::registered_memory_relation_graph_survives_restart_and_isolates_topologies` [Linux/macOS] — mounted graph reconciliation did not settle
- `daemon::store_runtime::session_registry::remote_recovery::publication::tests::failed_phase_transition_preserves_the_previous_active_fence` [Linux/macOS] — rollback unexpectedly required
- `daemon::store_runtime::session_registry::remote_recovery::publication::tests::stale_mounted_runtime_rejects_the_replacement_identity` [Linux/macOS] — error lacks `mounted identity`
- `daemon::store_runtime::session_registry::remote_recovery::publication::tests::unverified_destination_is_quarantined_before_retained_rollback_is_restored` [Linux/macOS] — expected file missing
- `daemon::store_runtime::session_registry::tests::background_convergence_commits_the_durable_authority_checkpoint` [Linux/macOS] — missing managed-daemon/maintenance authority
- `daemon::store_runtime::session_registry::tests::background_convergence_failure_remains_observable_as_degraded` [Linux/macOS] — missing managed-daemon/maintenance authority
- `daemon::store_runtime::session_registry::tests::cached_project_sessions_reject_conflicting_enrollment_authority` [Linux/macOS] — missing managed-daemon/maintenance authority
- `daemon::store_runtime::session_registry::tests::daemon_admission_returns_while_historical_convergence_is_blocked` [Linux/macOS] — missing managed-daemon/maintenance authority
- `daemon::store_runtime::session_registry::tests::duplicate_project_attaches_schedule_one_historical_convergence` [Linux/macOS] — missing managed-daemon/maintenance authority
- `daemon::store_runtime::session_registry::tests::existing_profile_memory_uses_final_schema_and_canonical_linked_lineage` [Linux/macOS] — missing managed-daemon/maintenance authority
- `daemon::store_runtime::session_registry::tests::profile_sessions_mount_rejects_incompatible_schema_through_registered_runtime` [Linux/macOS] — missing managed-daemon/maintenance authority
- `daemon::store_runtime::session_registry::tests::profile_sessions_mount_uses_the_durable_profile_identity_and_profile_pin` [Linux/macOS] — missing managed-daemon/maintenance authority
- `daemon::store_runtime::session_registry::tests::project_sessions_mount_uses_typed_enrollment_and_is_idempotent` [Linux/macOS] — missing managed-daemon/maintenance authority
- `daemon::store_runtime::session_registry::tests::read_only_worktree_mount_never_recreates_a_deleted_database` [Linux/macOS] — missing managed-daemon/maintenance authority
- `daemon::tests::bootstrap::account_tombstone_denies_projectless_memory_and_profile_automation` [macOS] — account deletion fails with authority unavailable
- `daemon::tests::bootstrap::daemon_restart_resumes_account_tombstone_without_ordinary_admission` [macOS] — missing managed-daemon/maintenance authority
- `daemon::tests::bootstrap::direct_tool_cache_miss_returns_warming_while_project_opens_in_background` [Linux/macOS] — bounded warming response timed out
- `daemon::tests::bootstrap::linked_route_reuses_primary_authority_while_shadow_writer_is_held` [Linux/macOS] — routes do not resolve one retained server
- `daemon::tests::bootstrap::mcp_bootstrap_catalog_bypasses_project_writer_gate` [Linux/macOS] — initialize waits on writer gate
- `daemon::tests::bootstrap::portable_broker_bootstrap_bypasses_project_writer_gate` [Linux/macOS] — portable initialize waits on writer gate
- `daemon::tests::bootstrap::production_composition_dashboard_persists_project_settings_over_http` [Linux/macOS] — settings patch lacks `idempotency_key`
- `daemon::tests::bootstrap::production_composition_harness_dispatches_application_invocations_in_process` [Linux/macOS] — `tracedecay_storage_status` exceeds absolute deadline
- `daemon::tests::bootstrap::production_composition_harness_reads_retained_profile_analytics_authority` [Linux/macOS] — retained analytics event absent
- `daemon::tests::bootstrap::production_composition_harness_shutdown_allows_immediate_profile_reopen` [Linux/macOS] — session relation graph remains locked
- `daemon::tests::bootstrap::production_composition_harness_wires_cross_project_resolver` [Linux/macOS] — top-level `project_path` rejected as a selector
- `daemon::tests::bootstrap::production_composition_mounts_core_query_without_optional_stage_evaluation` [Linux/macOS] — core query authority never becomes ready
- `daemon::tests::bootstrap::project_open_shutdown_retains_noncooperative_task_until_retry_joins_it` [Linux/macOS] — zero-deadline shutdown unexpectedly succeeds
- `daemon::tests::bootstrap::remote_account_deletion_joins_admitted_open_before_enumeration_and_reconciles_restart` [Linux] — account tombstone persistence timed out
- `daemon::tests::bootstrap::unenrolled_ambient_directory_is_rejected_before_project_warmup` [macOS] — authority error masks missing-enrollment error
- `daemon::tests::bootstrap::unenrolled_leaf_is_rejected_from_cache_and_direct_open` [macOS] — authority error masks missing-enrollment error
- `daemon::tests::handshake::daemon_refreshes_once_only_after_generation_change` [Linux/macOS] — socket client lacks managed-daemon/maintenance authority
- `daemon::tests::handshake::initialized_ack_preserves_pending_catalog_refresh_notification` [Linux/macOS] — socket client lacks managed-daemon/maintenance authority
- `daemon::tests::invocation_ownership::committed_project_invocation_routes_mounted_operations` [Linux/macOS] — Work route returns not-found/not-authorized
- `daemon::tests::multi_root_journey::authenticated_multi_root_journey_reaches_scope_set_storage` [Linux/macOS] — journey thread panics
- `daemon::tests::ownership::fresh_committed_project_open_mounts_feedback_before_lsp` [Linux/macOS] — committed Git identity has no feedback cycle
- `daemon::tests::ownership::released_automation_tombstone_allows_one_eventual_replacement` [Linux/macOS] — replacement owner-key mismatch panic
- `daemon::tests::remote_project_recovery::recovery_quiesces_only_a_and_remounts_its_retry_route` [Linux/macOS] — graph runtime close conflicts
- `daemon::tests::replay::client_identity_startup_replays_retained_profile_receipts` [macOS] — 0 vs 1
- `daemon::tests::replay::projectless_user_session_setup_failure_returns_json_rpc_error` [Linux/macOS] — authenticated profile mismatch
- `daemon::tests::rmcp_route::portable_production_route_selects_rmcp_after_initialize` [Linux/macOS] — cancelled route does not terminate
- `daemon::tests::rmcp_route::production_rmcp_cancels_registered_and_pre_registration_requests` [Linux/macOS] — cancelled requests do not terminate
- `daemon::tests::rmcp_route::selected_target_rmcp_flushes_response_and_full_disconnect_cancels_target` [Linux/macOS] — daemon and maintenance scopes overlap
- `daemon::tests::rmcp_route::unix_production_route_selects_rmcp_only_after_initialize` [Linux/macOS] — cancelled route does not terminate
- `daemon::tests::runtime_identity::concurrent_same_identity_worktrees_keep_exact_server_and_scheduler_bindings` [Linux/macOS] — follow-up sees code graph unavailable instead of retaining linked route
- `daemon::tests::scheduler_config::cached_project_reconciles_cli_enabled_automation_without_cache_probe` [Linux/macOS] — scheduler key remains when expected absent
- `daemon::tests::scheduler_config::daemon_scheduler_discovery_without_work_does_not_wait_for_writer_gate` [Linux/macOS] — read-only scheduler discovery waits on writer gate
- `daemon::tests::scheduler_config::daemon_scheduler_skips_stale_owner_key_after_rekey` [Linux/macOS] — scheduler starts under a stale owner key
- `daemon::tests::scheduler_config::disabled_scheduler_reconcile_cannot_acknowledge_an_owner_that_then_exits` [Linux/macOS] — `RunningNotified` vs `Started`
- `daemon::tests::scheduler_config::fresh_v2_project_starts_the_required_automation_scheduler` [Linux/macOS] — scheduler key absent
- `daemon::tests::scheduler_config::profile_reconcile_broadcasts_to_cached_projects_without_opening_uncached_projects` [macOS] — projectless profile reconcile sees authenticated profile mismatch
- `daemon::tests::socket::daemon_linked_worktree_route_repairs_primary_identity_and_keeps_alias` [Linux/macOS] — linked-root path remains canonical instead of primary path
- `daemon::tests::socket::socket_client_requires_user_storage_scope_without_project` [Linux/macOS] — projectless handshake message changed
- `daemon::tests::socket::socket_git_preview_apply_replay_and_pre_admission_problems_are_canonical` [nextest] — stack overflow; SIGABRT on both attempts
- `daemon::tests::socket::user_session_read_bypasses_unregistered_project_route` [Linux/macOS] — user-session read times out
- `dashboard_project_settings_commit_through_the_daemon_control_plane` [Linux/macOS] — daemon exited status 0 before accepting connections
- `dashboard_user_settings_replay_through_application_restart` [Linux/macOS] — daemon exited status 0 before accepting connections
- `direct_lifecycle_entry_points_retain_production_authority` [Linux/macOS] — graph database locked by another process
- `dropped_reservation_releases_its_fence_after_caller_cancellation` [macOS] — `Conflict`
- `duplicate_receipt_corrections_choose_the_latest_revision_across_anchors_and_fragment_order` [Linux/macOS] — `IncompatibleFragments`
- `embedded_component_sets_complete_lifecycle_for_all_supported_hosts` [Linux/macOS] — `ArtifactContentMismatch`
- `every_cursor_carrying_code_operation_mints_and_spends_a_continuation` [Linux/macOS] — code-symbol search remains `application.surface.unavailable` after ~60s
- `every_journey_operation_binds_to_cli_and_mcp_and_withholds_http` [Linux/macOS] — operation count 11 vs 6
- `exact_project_profile_identity_reuses_one_persistent_handle` [macOS] — `Conflict`
- `exact_search_does_not_wait_for_semantic_projection` [Linux/macOS] — `tracedecay init` rejects stale `--quiet`
- `exact_sql::tests::authority::long_lease_transaction_renews_its_lease_after_successful_bounded_steps` [macOS] — `TransactionExpired`
- `exact_verified_generation_lease_blocks_retirement_until_activation_releases_it` [macOS] — `Conflict`
- `expired_deadline_does_not_open_or_close_a_registered_store` [macOS] — `Conflict`
- `explorer::explorer_query_coordinates_real_sources_without_inventing_a_merge` [Dashboard] — `"unavailable"` vs `"ready"`
- `explorer::explorer_session_routes_reuse_lcm_size_and_read_context_authority` [Dashboard] — `"unknown"` vs `"ready"`
- `fact_merge_hydration::contradictions_are_recorded_explicitly_in_lineage` [Linux/macOS] — `FactNotFound` returned where storage error expected
- `fact_merge_hydration::failed_fact_batch_rolls_back_identity_assertion_anchor_and_lineage` [Linux/macOS] — `FactNotFound` returned after staged writes
- `fact_merge_hydration_test::contradictions_are_recorded_explicitly_in_lineage` [Linux/macOS] — `FactNotFound` returned where storage error expected
- `fact_merge_hydration_test::failed_fact_batch_rolls_back_identity_assertion_anchor_and_lineage` [Linux/macOS] — `FactNotFound` returned after staged writes
- `fixture_authority_test::committing_a_fixture_tree_never_stages_enrollment_state` [Linux/macOS] — daemon and maintenance scopes overlap
- `fixture_authority_test::enrolled_layout_comes_from_the_opened_graph` [Linux/macOS] — daemon and maintenance scopes overlap
- `fixture_authority_test::one_profile_serves_two_projects_with_distinct_stores` [Linux/macOS] — daemon and maintenance scopes overlap
- `generic_tool_accepts_slow_byte_stream` [Linux/macOS] — daemon exited status 0 before accepting connections
- `generic_tool_accepts_split_json_rpc_frame` [macOS] — daemon exited status 0 before accepting connections
- `generic_tool_handles_concurrent_requests_without_crosstalk` [Linux/macOS] — daemon exited status 0 before accepting connections
- `generic_tool_preserves_late_reply_within_response_grace` [Linux/macOS] — daemon exited status 0 before accepting connections
- `generic_tool_rejects_semantic_truncation_envelope_without_output` [macOS] — daemon exited status 0 before accepting connections
- `generic_tool_rejects_truncated_frame_without_output` [Linux/macOS] — daemon exited status 0 before accepting connections
- `generic_tool_rejects_unrepresentable_deadline` [macOS] — daemon exited status 0 before accepting connections
- `git_index_transactions::tests::configured_merge_diff_and_filter_drivers_are_preview_only` [Linux/macOS] — external drivers still present
- `global_db::open_at_upgrades_existing_global_db_with_analytics_events_table` [Linux/macOS] — session-temporal ResetRequired replaces upgrade
- `global_db::open_at_upgrades_existing_sessions_table_with_parent_columns` [Linux/macOS] — session-temporal ResetRequired replaces upgrade
- `global_db::search_session_messages_git_scoped_by_branch_with_hyphen_term` [Linux/macOS] — ProjectSessions authority required
- `global_db::upsert_session_message_externalizes_tool_payload_without_indexing_body_or_metadata` [Linux/macOS] — session-message upsert returns false
- `global_db::upsert_session_message_preserves_oversized_text_losslessly` [Linux] — session-message upsert returns false
- `global_registry_test::project_tokens_saved_schema_and_queries_still_work` [macOS] — byte-distinct path vectors render identically but compare unequal
- `graph_store_survives_reopen_and_preserves_superseded_generations` [Linux/macOS] — externalized state serialized before sealing
- `hook_lifecycle_lease_test::native_hook_captures_only_bound_transport_spool_records` [Linux/macOS] — captured hook count 0 vs 1
- `hook_replay_test::replayed_provider_hooks_record_attributed_rows_and_bridge_to_analytics_events` [Linux/macOS] — Codex prompt hook exits 1
- `hooks::codex::tests::codex_session_context_resolves_global_only_and_preserves_nudge` [Linux/macOS] — global-only repo reports `Generic` vs `Initialized`
- `hooks::tests::daemon_tool_json_returns_project_warming_without_retrying` [Linux/macOS] — hook daemon call retries warming until timeout
- `host_admission_test::host_ingress_binds_provenance_to_authoritative_project_and_replays_stably` [Linux/macOS] — observation remains `AcceptedForReplay/Pending`
- `host_admission_test::registered_profile_runtime_is_required_and_mismatch_never_falls_back` [Linux/macOS] — expected committed persisted outcome absent
- `host_admission_test::registered_project_runtime_is_exact_and_revocation_never_falls_back` [Linux/macOS] — expected committed persisted outcome absent
- `immediate_concurrent_and_repeated_opens_publish_one_callable_owner` [Linux/macOS] — test-results surface remains unavailable after ~60s
- `interrupted_convergence_serves_the_prior_snapshot_and_replays_identically` [macOS] — `Conflict`
- `jobs::concurrent_manual_job_triggers_do_not_double_execute` [macOS] — automation lock parent missing
- `jobs::user_job_delivers_output_to_file_and_records_ledger` [Linux/macOS] — null output path
- `labeled_byte_record_entities_reach_a_verified_head` [macOS] — `Conflict`
- `lcm_bridge::generated_skill_mirrors_session_context_retrieval_contract` [Linux/macOS] — generated skill lacks `begin` marker
- `lcm_bridge::generated_tools_bridge_preserves_message_kwargs_in_json_args` [Linux/macOS] — generated subprocess bridge changes message kwargs
- `lcm_compression::frontier::late_summary_projection_failure_rolls_back_payload_files_and_canonical_rows` [Linux/macOS] — `SummarySourceUnavailable(unverifiable_timestamp)`
- `lcm_compression::overflow::overflow_recovery_keeps_preserved_objective_scaffold_when_evicting_tail` [Linux/macOS] — session-message upsert returns false
- `lcm_compression::replay::idless_compression_replay_does_not_reingest_existing_raw_messages` [Linux/macOS] — `SummarySourceUnavailable(unverifiable_timestamp)`
- `lcm_compression::tool_transactions::bounded_leaf_chunk_backs_off_before_multi_tool_transaction` [Linux/macOS] — `SummarySourceUnavailable(unverifiable_timestamp)`
- `lcm_compression::tool_transactions::budget_and_overflow_replay_never_split_tool_transaction` [Linux/macOS] — `SummarySourceUnavailable(unverifiable_timestamp)`
- `lcm_compression::tool_transactions::fresh_tail_boundary_keeps_multi_tool_transaction_atomic_and_shrinking` [Linux/macOS] — `SummarySourceUnavailable(unverifiable_timestamp)`
- `lcm_dag::summary_expansion_marks_external_raw_sources_without_silent_empty_content` [Linux] — SQLite transaction lease expired
- `lcm_payload::api_alias_assignments_redact_apikey_and_apitoken` [Linux/macOS] — privacy sanitizer receipt construction failed
- `lcm_payload::delete_external_payload_rejects_referenced_payload_without_hash_verification` [Linux] — SQLite transaction lease expired
- `lcm_payload::denies_cross_session_payload_expansion` [Linux] — SQLite transaction lease expired
- `lcm_payload::denies_expansion_after_message_updates_to_new_payload_ref` [Linux] — SQLite transaction lease expired
- `lcm_payload::externalizes_large_tool_payload_with_recoverable_ref` [Linux] — SQLite transaction lease expired
- `lcm_payload::lcm_ingest_uses_the_canonical_privacy_detector_without_local_policy` [Linux/macOS] — redacted content lacks `canonicallcmcanary`
- `lcm_payload::private_key_redaction_cannot_be_disabled_by_local_metadata` [Linux/macOS] — privacy sanitizer receipt construction failed
- `lcm_payload::quoted_password_assignment_redacts_full_quoted_value` [Linux/macOS] — canonical credential redaction marker absent
- `lcm_payload::replay_and_successor_reuse_reject_mutated_payload_global_authority` [Linux] — SQLite transaction lease expired
- `lcm_payload::sensitive_redaction_is_canonical_lossy_and_not_indexed` [Linux/macOS] — canonical credential redaction marker absent
- `lcm_payload::summary_publication_binds_external_payload_manifest_and_sanitization_receipt` [Linux] — SQLite transaction lease expired
- `lcm_query::describe::describe_gives_session_overview_without_full_payload_bodies` [Linux] — SQLite transaction lease expired
- `lcm_query::describe::describe_node_and_external_payload_return_metadata_without_body_leaks` [Linux] — SQLite transaction lease expired
- `lcm_query::expand::expand_cross_session_external_row_can_hydrate_payload_via_two_step_expand` [Linux] — SQLite transaction lease expired
- `lcm_query::expand::expand_returns_sliced_raw_summary_and_payload_content_with_ranges` [Linux] — SQLite transaction lease expired
- `lcm_query::status::status_reports_payload_gc_run_metadata_after_apply` [Linux] — SQLite transaction lease expired
- `lcm_query::status::status_reports_schema_frontier_payload_and_debt_counts` [Linux] — SQLite transaction lease expired
- `lcm_raw::transcript_ingest_preserves_lossless_raw_content` [Linux] — SQLite transaction lease expired
- `lcm_summary_lineage_review::immutable_summary_lineage_rejects_foreign_session_canary_sources_without_disclosure` [Linux/macOS] — secret-shaped canary session-message upsert returns false
- `mcp::scope::tests::dotdot_request_resolves_the_same_worktree_scope_as_daemon_authority` [Linux/macOS] — dotdot spelling resolves a different scope identity
- `mcp::scope::tests::symlink_request_resolves_the_same_worktree_scope_as_daemon_authority` [Linux/macOS] — symlink spelling resolves a different scope identity
- `mcp::server::connection::cancellable_queue_tests::cancellation_during_route_resolution_reaches_selected_live_target` [macOS] — cancellation response times out
- `mcp::server::hook_boundary_failure_matrix_tests::matrix_backpressure_overflow_rejects_before_reconcile_without_pending_growth` [Linux] — count 2 vs 1
- `mcp::server::hook_boundary_failure_matrix_tests::matrix_daemon_unavailable_without_broker_skips_reconcile_and_frontier` [Linux] — unavailable path opens reconcile sink
- `mcp::server::hook_boundary_failure_matrix_tests::matrix_identical_notifications_are_distinct_without_frontier_corruption` [Linux] — count 3 vs 2
- `mcp::server::hook_boundary_failure_matrix_tests::matrix_unavailable_then_success_keeps_sticky_retained_failure_frontier` [Linux] — count 2 vs 1
- `mcp::server::host_admission_tests::add_branch_at_replay_rejects_stale_branch_after_switch` [Linux] — count 1 vs 0
- `mcp::server::host_admission_tests::add_branch_at_replay_rejects_stale_root_after_adversarial_replace` [Linux] — stale root writes
- `mcp::server::host_admission_tests::add_branch_at_restart_replay_rejects_common_dir_drift` [Linux] — count 1 vs 0
- `mcp::server::host_admission_tests::add_branch_at_restart_replay_rejects_symlink_swap` [Linux] — count 1 vs 0
- `mcp::server::host_admission_tests::add_branch_replay_rejects_stale_branch_after_delayed_switch` [Linux] — count 1 vs 0
- `mcp::server::host_admission_tests::add_branch_restart_replay_rejects_stale_branch_after_switch` [Linux] — count 1 vs 0
- `mcp::server::host_admission_tests::cancelled_canonical_attempt_is_recovered_and_replayed` [Linux] — count 0 vs 1
- `mcp::server::host_admission_tests::commit_before_ack_replays_once_and_acknowledges_exact_duplicate` [Linux] — count 2 vs 1
- `mcp::server::host_admission_tests::durable_route_survives_unavailable_effect_for_same_connection_retry` [Linux] — `Committed` vs `Unavailable`
- `mcp::server::host_admission_tests::malformed_semantic_payload_is_explicit_and_quarantined_across_reopen` [Linux] — effect was attempted unexpectedly
- `mcp::server::host_admission_tests::malformed_source_does_not_starve_valid_sibling_source` [Linux] — count 2 vs 1
- `mcp::server::host_admission_tests::oversized_event_is_rejected_before_canonical_attempt` [Linux] — effect was attempted unexpectedly
- `mcp::server::host_admission_tests::quarantine_releases_active_capacity_then_full_fails_closed` [Linux] — count 1 vs 0
- `mcp::server::host_admission_tests::sync_current_branch_replay_rejects_stale_branch_after_delayed_switch` [Linux] — count 1 vs 0
- `mcp::server::host_admission_tests::sync_current_branch_restart_replay_rejects_stale_branch_after_switch` [Linux] — count 1 vs 0
- `mcp::server::host_admission_tests::unsupported_payload_version_is_retryable_and_retained_across_reopen` [Linux] — effect was attempted unexpectedly
- `mcp::server::lcm_claude_recall_tests::lcm_expand_query_returns_every_matching_claude_message` [Linux/macOS] — retained transport unavailable
- `mcp::server::lcm_claude_recall_tests::lcm_expand_reads_every_live_raw_message_store_id` [Linux/macOS] — retained transport unavailable
- `mcp::server::lcm_claude_recall_tests::lcm_grep_finds_a_term_stored_in_exactly_one_message` [Linux/macOS] — retained transport unavailable
- `mcp::server::lcm_claude_recall_tests::lcm_grep_returns_every_matching_claude_message` [Linux/macOS] — retained transport unavailable
- `mcp::tools::definitions::tests::catalog_filter_preserves_non_catalog_tools_and_filters_catalog_bindings` [Linux/macOS] — legacy production tools are no longer discoverable
- `mcp::tools::handlers::analysis::unmounted_files::rust::tests::a_non_utf8_file_name_is_reported_without_panicking` [macOS] — `Illegal byte sequence`
- `mcp::tools::handlers::configuration_dispatch_tests::available_configuration_effect_reaches_canonical_executor` [Linux/macOS] — effect admission does not win settlement before daemon invocation
- `mcp::tools::handlers::configuration_dispatch_tests::every_other_configuration_effect_reaches_the_authoritative_daemon_executor` [Linux/macOS] — configuration-unset does not claim settlement
- `mcp::tools::handlers::context_scout_control_dispatch_tests::context_scout_pause_and_resume_preserve_caller_idempotency_keys` [Linux/macOS] — context-scout pause does not claim settlement
- `mcp::tools::handlers::dispatch_tests::a_warm_call_is_unaffected_by_the_ceiling` [Linux/macOS] — warm context sees code index unavailable
- `mcp::tools::handlers::dispatch_tests::graph_reader_selector_dispatch_policy_is_allowlisted` [Linux/macOS] — required selector keys are null
- `mcp::tools::handlers::dispatch_tests::graph_tools_reject_blank_node_ids_and_zero_depth_with_typed_errors` [Linux/macOS] — blank node ID maps to noncanonical occurrence instead of expected typed error
- `mcp::tools::handlers::dispatch_tests::unavailable_user_lcm_effect_is_rejected_before_profile_store_open` [Linux/macOS] — error does not contain expected `unknown`
- `mcp::tools::handlers::dispatch_tests::user_lcm_doctor_reports_a_missing_store_without_opening_it` [Linux/macOS] — profile retained authority unavailable
- `mcp::tools::handlers::retained_timeout_dispatch_tests::fact_store_curate_forwards_only_bounds_and_preserves_canonical_success` [Linux/macOS] — zero-second dispatch ceiling
- `mcp::tools::handlers::retained_timeout_dispatch_tests::fact_store_curate_pre_commit_cancellation_does_not_mutate` [Linux/macOS] — zero-second dispatch ceiling
- `mcp::tools::handlers::retained_timeout_dispatch_tests::fact_store_curate_rejects_a_partial_receipt_from_another_scope` [Linux/macOS] — zero-second dispatch ceiling
- `mcp::tools::handlers::workflow::affected_tests_tests::cancellation_retains_results_completed_before_the_later_test` [Linux/macOS] — `"invalid_test_identity"` vs `"cargo"`
- `mcp::tools::handlers::workflow::affected_tests_tests::directly_changed_test_file_dispatches_each_full_test_identity` [Linux/macOS] — dispatched test list is null
- `mcp::tools::handlers::workflow::affected_tests_tests::nested_source_module_dispatches_the_crate_relative_test_identity` [Linux/macOS] — dispatched test list is null
- `mcp::tools::handlers::workflow::affected_tests_tests::reported_passing_and_failing_tests_complete_with_observed_results` [Linux/macOS] — completion state is null
- `mcp::tools::handlers::workflow::affected_tests_tests::timed_out_test_runner_returns_a_terminal_receipt` [Linux/macOS] — `"invalid_test_identity"` vs `"cargo"`
- `mcp::tools::handlers::workflow::affected_tests_tests::vacuous_or_nonzero_test_output_is_a_failed_terminal` [Linux/macOS] — `"invalid_test_identity"` vs `"cargo"`
- `mcp::tools::handlers::workflow::test_runner::tests::deadline_terminates_and_reaps_the_complete_test_process_tree` [Linux] — child marker missing
- `mcp::tools::plugin_conformance_tests::plugin_tool_mentions_resolve_to_registered_tools` [Linux/macOS] — plugin mentions removed session-refresh/work tools
- `mcp::tools::plugin_conformance_tests::readme_mcp_allowlist_matches_read_only_tools` [Linux/macOS] — README allowlist differs from live `readOnlyHint=true` set
- `mcp::tools::plugin_conformance_tests::registered_tools_are_referenced_by_the_plugin_bundle` [Linux/macOS] — many registered tools unreferenced by Cursor plugin
- `mcp_cli_serve_test::explicit_initialized_path_ignores_initialize_roots` [macOS] — daemon exited status 0 before accepting connections
- `mcp_cli_serve_test::initialize_roots_auto_initializes_unindexed_git_repo` [Linux/macOS] — daemon exited status 0 before accepting connections / child timeout
- `mcp_cli_serve_test::initialize_roots_decode_file_uri_localhost_and_percent_escapes` [macOS] — daemon exited status 0 before accepting connections
- `mcp_cli_serve_test::no_explicit_path_auto_initializes_unindexed_git_cwd` [Linux/macOS] — daemon exited status 0 before accepting connections / child timeout
- `mcp_cli_serve_test::no_explicit_path_prefers_discovered_cwd_over_initialize_roots` [Linux/macOS] — daemon exited status 0 before accepting connections / child timeout
- `mcp_cli_serve_test::no_explicit_path_prefers_initialize_roots_over_global_fallback` [macOS] — daemon exited status 0 before accepting connections
- `mcp_cli_serve_test::no_explicit_path_without_roots_still_uses_global_fallback` [macOS] — daemon exited status 0 before accepting connections
- `mcp_cli_serve_test::serve_daemon_proxy_reports_daemon_disconnect_as_json_rpc_error` [nextest] — TMT after 360s
- `mcp_cli_serve_test::serve_stdio_smokes_automation_run_artifact_view` [macOS] — daemon exited status 0 before accepting connections
- `mcp_cli_serve_test::serve_stdio_smokes_managed_skill_list_and_view` [macOS] — daemon exited status 0 before accepting connections
- `mcp_cli_serve_test::serve_with_reachable_daemon_proxies_before_opening_explicit_project` [macOS] — serve does not connect before project resolution
- `mcp_cli_serve_test::serve_without_daemon_socket_reports_daemon_unavailable` [macOS] — command unexpectedly succeeds
- `mcp_cli_serve_test::unexpanded_template_path_prefers_initialize_roots_over_discovered_cwd` [macOS] — daemon exited status 0 before accepting connections
- `mcp_configuration_write_persists_and_rejects_stale_cas` [Linux/macOS] — daemon exited status 0 before accepting connections
- `mcp_handler_test::admin_test::project_registry_tools_missing_registry_carries_stable_shape` [Linux/macOS] — registered selection unresolved before dispatch
- `mcp_handler_test::lcm_test::lcm_expand_query_context_max_tokens_is_independent_of_max_tokens` [Linux/macOS] — retained transport unavailable
- `mcp_handler_test::lcm_test::lcm_grep_rejects_invalid_scope` [Linux/macOS] — generic invalid retained request replaces expected argument error
- `mcp_handler_test::lcm_test::lcm_grep_rejects_invalid_scope_without_searching_all_sessions` [Linux/macOS] — generic invalid retained request replaces expected argument error
- `mcp_handler_test::lcm_test::lcm_load_session_missing_store_uses_typed_empty_messages_without_creating_sessions_db` [Linux/macOS] — messages null vs empty array
- `mcp_handler_test::lcm_test::lcm_read_only_tools_return_not_ingested_without_creating_sessions_db` [Linux/macOS] — retained transport unavailable
- `mcp_handler_test::lcm_test::lcm_status_cli_bridge_accepts_json_args` [Linux/macOS] — retained authority unavailable
- `mcp_handler_test::retrieve_truncation_test::retrieve_tool_reports_missing_and_expired_handles_actionably` [Linux/macOS] — `Option::unwrap()` on `None`
- `mcp_handler_test::schema_test::exact_memory_tool_definitions_exclude_legacy_payload_aliases` [Linux/macOS] — schema type is `["number","null"]`, expected `"number"`
- `mcp_handler_test::schema_test::schema_required_arguments_match_representative_handler_parsers` [Linux/macOS] — route availability masks missing-parameter parser error
- `mcp_handler_test::session_search_test::message_search_rejects_all_registered_with_project_selector` [Linux/macOS] — unresolved registered selection masks selector rejection
- `mcp_handler_test::session_search_test::message_search_rejects_invalid_scope` [Linux/macOS] — generic invalid retained request replaces expected error
- `mcp_handler_test::session_search_test::message_search_rejects_unsupported_project_scope` [Linux/macOS] — call unexpectedly succeeds
- `mcp_handler_test::status_runtime_test::test_status` [Linux/macOS] — branch diagnostics absent
- `memory_curation::automatic_fact_receipt_endpoints_expose_terminal_applied_and_quarantined_receipts` [Dashboard] — count 2 vs 1
- `memory_curation::retained_admin_journey_commits_add_update_feedback_and_remove` [Dashboard] — tombstone read does not preserve expected feedback lineage
- `memory_curation::retained_mutations_deny_foreign_project_scope_without_a_receipt` [Dashboard] — `"runtime"` vs `"application"`
- `memory_curator::backend_failures::memory_curator_runner_ledgers_malformed_backend_output` [Linux/macOS] — count 0 vs 1
- `memory_curator::backend_failures::memory_curator_runner_records_noop_fallback_when_backend_run_task_fails` [Linux/macOS] — verified memory graph publication conflicted
- `memory_curator::manual_trigger::manual_memory_curator_runs_when_scheduling_and_task_are_disabled` [Linux/macOS] — count 0 vs 1
- `memory_curator::memory_curator_persists_transient_transient_success_retry_receipt` [Linux/macOS] — verified memory graph publication conflicted
- `memory_curator::memory_curator_quarantines_legacy_output_after_bounded_repair_exhaustion` [Linux/macOS] — count 0 vs 2
- `memory_curator::memory_curator_repairs_then_applies_validated_ops_and_records_ledger` [Linux/macOS] — verified memory graph publication conflicted
- `memory_curator::memory_curator_runner_applies_validated_ops_under_apply_policy` [Linux/macOS] — verified memory graph publication conflicted
- `memory_curator::memory_curator_runner_artifacts_block_handoff_without_validation_examples` [Linux/macOS] — verified memory graph publication conflicted
- `memory_curator::memory_curator_runner_artifacts_mark_handoff_ready_for_accepted_only_examples` [Linux/macOS] — verified memory graph publication conflicted
- `memory_curator::memory_curator_runner_auto_applies_validated_operations` [Linux/macOS] — verified memory graph publication conflicted
- `memory_curator::memory_curator_stops_before_backend_or_apply_when_caller_is_interrupted` [Linux/macOS] — error lacks `interrupted`
- `memory_curator::pagination::memory_curator_resumes_from_the_durable_next_page_cursor` [Linux/macOS] — verified memory graph publication conflicted
- `memory_curator::scheduler_memory_curator_applies_validated_ops_automatically` [Linux/macOS] — verified memory graph publication conflicted
- `memory_eval_test::eval_memory_feedback_trust` [Linux/macOS] — daemon exited status 0 before accepting connections
- `memory_eval_test::eval_memory_multiturn_continuity` [Linux/macOS] — daemon exited status 0 before accepting connections
- `memory_eval_test::eval_memory_no_pollution` [Linux/macOS] — daemon exited status 0 before accepting connections
- `memory_eval_test::eval_memory_ranking_feedback_promotes` [Linux/macOS] — daemon exited status 0 before accepting connections
- `memory_eval_test::eval_memory_ranking_morphology` [Linux/macOS] — daemon exited status 0 before accepting connections
- `memory_eval_test::eval_memory_ranking_retrieval_reinforcement` [Linux/macOS] — daemon exited status 0 before accepting connections
- `memory_eval_test::eval_memory_ranking_supersession` [Linux/macOS] — daemon exited status 0 before accepting connections
- `memory_eval_test::eval_memory_ranking_trust_bias` [Linux/macOS] — daemon exited status 0 before accepting connections
- `memory_eval_test::eval_memory_secret_rejection` [Linux/macOS] — daemon exited status 0 before accepting connections
- `memory_eval_test::eval_memory_skip_local` [Linux/macOS] — daemon exited status 0 before accepting connections
- `memory_eval_test::eval_memory_supersede_without_dup` [Linux/macOS] — daemon exited status 0 before accepting connections
- `multi_connection_test::split_brain_is_rejected_and_unavailable_daemon_fails_closed_until_restart` [Linux/macOS] — daemon exited status 0 before opening socket
- `multi_connection_test::twelve_mcp_cli_and_hook_clients_share_one_daemon_profile_store_owner` [Linux/macOS] — daemon exited status 0 before opening socket
- `native_host_event_fixtures_execute_provider_admission_paths` [Linux/macOS] — Hermes supported stdout is `{}` instead of empty
- `observation_store::legacy_idempotency_column_rows_migrate_before_reads_and_writes` [Linux/macOS] — session-temporal ResetRequired replaces migration
- `observation_workflow_projection::workflow_projection_rolls_back_rebuilds_restarts_and_audits` [Linux/macOS] — injected projection failure remains `RetryDeferred`
- `operation_family_executes_through_cli_mcp_and_http` [Linux/macOS] — retained project server retired during init
- `packaged_assets::tests::packaged_evaluator_runs_against_an_unrelated_project` [nextest] — TMT after 360s
- `packaged_host_ingest_delivers_a_registered_advisory_cycle` [macOS] — advisory cycle fails `feedback-document-outside-root`
- `persisted_topology_wakes_idle_rollup_after_unrelated_queue_tail` [Linux/macOS] — topology source does not bypass five-minute idle poll
- `persistence_failure_cannot_be_rewritten_as_a_clean_terminal` [Linux] — count 0 vs 2
- `post_retention_corrections_join_retained_bounded_evidence_exactly` [Linux/macOS] — `IncompatibleFragments`
- `pr_autotrack_test::discovery_classifies_same_repo_and_fork_pull_heads` [Linux/macOS] — daemon and maintenance scopes overlap
- `pr_autotrack_test::failed_discovery_is_not_reported_as_an_empty_success` [Linux/macOS] — daemon and maintenance scopes overlap
- `pr_autotrack_test::reconciliation_without_scheduler_fails_before_git_or_state_mutation` [Linux/macOS] — daemon and maintenance scopes overlap
- `pre_cancelled_application_has_cli_mcp_http_parity` [macOS] — daemon exited status 0 before accepting connections
- `primitive_config_markdown_json_parity` [Linux/macOS] — daemon exited status 0 before accepting connections
- `production_lsp_negotiates_and_projects_canonical_context` [Linux/macOS] — daemon exited status 0 before accepting connections
- `production_primitive_code_routes_have_cli_mcp_http_parity` [Linux/macOS] — daemon exited status 0 before accepting connections
- `production_primitive_reads_agree_across_mcp_http_and_cli` [Linux/macOS] — daemon authority wait timed out
- `production_project_open_serves_a_paginated_symbol_graph_read` [Linux/macOS] — symbol search remains unavailable after ~60s
- `profile_memory_scope_uses_exact_profile_authority` [macOS] — `Conflict`
- `profile_sessions_scope_uses_exact_profile_authority` [macOS] — `Conflict`
- `profile_storage_reset_test::branch_open_rejects_a_mismatched_maintenance_profile` [Linux/macOS] — error text changed from `branch snapshot open` to `branch open`
- `profile_storage_reset_test::incompatible_profile_store_requires_reset_without_in_place_changes` [Linux/macOS] — ResetRequired authority/reason differs
- `profile_storage_reset_test::trace_decay_open_branch_uses_shared_profile_store` [Linux/macOS] — branch tracking is absent
- `project_open_application_boundary` [Linux/macOS] — daemon exited status 0 before accepting connections
- `project_session_and_code_scopes_keep_distinct_locator_authority` [macOS] — `Conflict`
- `projects::project_scoped_plugin_routes_read_selected_project_store` [Dashboard] — selected-project graph coverage contract differs
- `public_executable_routes_are_served_by_the_production_daemon` [macOS] — daemon exited status 0 before publishing authority
- `report_tests::baseline_report_is_self_validating_but_not_activation_evidence` [Linux/macOS] — activation now requires a passing direct evaluation
- `research_anchors::authorized_resolution_rejects_unknown_fields_and_incoherent_states` [Linux/macOS] — incoherent wire value decodes
- `research_anchors::canonical_retrieval_anchor_rejects_payload_unknown_fields_and_claimed_ids` [Linux/macOS] — unknown-field diagnostic changed
- `reset_required_is_retained_until_an_explicit_reopen` [macOS] — `Conflict`
- `reset_required_shape_is_recreated_fresh_and_republished_from_the_manifest` [macOS] — `Conflict`
- `restart_reverification_installs_once_and_steady_reads_need_no_authority` [macOS] — `Conflict`
- `retained_surfaces::sdk::results::automation::curation::tests::all_noop_receipt_retains_acceptance_without_fabricating_mutations_or_anchors` [Linux/macOS] — noncanonical ManifestDigest
- `retained_surfaces::sdk::results::tests::automation_terminal_selects_only_its_exact_result_variant` [Linux/macOS] — terminal DTO matches no retained-surface variant
- `retention::orphan_stores::tests::unregistered_collection_rejects_profile_contained_data_root_symlink` [Linux/macOS] — `InspectFailed` vs `OutsideProfile`
- `retention::storage_report::tests::an_unreadable_store_reports_size_with_unsampled_free_pages` [Linux/macOS] — offline snapshot lacks managed-daemon/maintenance authority
- `retention::storage_report::tests::full_profile_report_creates_no_entries_under_the_profile_root` [Linux/macOS] — offline snapshot lacks managed-daemon/maintenance authority
- `retention::storage_report::tests::full_profile_total_includes_session_and_generation_files` [Linux/macOS] — offline snapshot lacks managed-daemon/maintenance authority
- `retention::storage_report::tests::report_sizes_every_registered_store_and_counts_unregistered_dirs` [Linux/macOS] — offline snapshot lacks managed-daemon/maintenance authority
- `retention::storage_report::tests::sizing_a_store_copies_nothing_and_leaves_no_scratch` [Linux/macOS] — offline snapshot lacks managed-daemon/maintenance authority
- `retention::storage_report::tests::storage_report_preserves_the_exact_live_global_database_family` [Linux/macOS] — offline snapshot lacks managed-daemon/maintenance authority
- `retention::storage_report::tests::unregistered_sizing_does_not_follow_symlinks` [Linux/macOS] — offline snapshot lacks managed-daemon/maintenance authority
- `retention_cleanup_failure_keeps_cancel_fence_until_retry` [nextest] — TMT after 360s
- `run_ledger::run_ledger_limit_and_malformed_lines_are_handled` [Linux/macOS] — malformed ledger row lacks status
- `run_ledger::run_ledger_rejects_legacy_rfc3339_with_subsecond_micros` [Linux/macOS] — legacy timestamp is no longer rejected
- `runtime::git_correlation::backfill::bounded::native::resume::tests::non_utf8_local_ref_is_sealed_without_fabricated_branch_text` [macOS] — child command fails
- `runtime::git_correlation::backfill::bounded::tests::non_utf8_canonical_worktree_resumes_exactly_then_fails_typed_publish` [macOS] — `Illegal byte sequence`
- `runtime::hermes::tests::hermes_reader_supports_non_utf8_database_paths` [macOS] — SQLite cannot open non-Unicode path
- `runtime::lcm::payload::rollback_tests::direct_store_failure_rolls_back_metadata_and_payload_file` [Linux] — `TransactionExpired`
- `runtime::opencode::tests::durable_sql_frontier_reaches_rows_beyond_a_poisoned_pass_after_restart` [Linux] — count 0 vs 1
- `runtime::opencode::tests::steady_state_restart_keeps_high_water_without_per_row_durability_reads` [Linux] — count 2 vs 1
- `runtime::opencode::tests::wal_part_append_replaces_the_durable_message_with_complete_content` [Linux] — unwraps missing value
- `scheduler::config_validates_scheduler_idle_and_lock_bounds` [Linux/macOS] — validation no longer mentions `min_idle_secs`
- `selected_project_source_route_survives_physical_daemon_restart` [macOS] — daemon exited status 0 before accepting connections
- `serve_template_path_test::literal_template_without_daemon_fails_closed_before_mcp_handshake` [macOS] — command unexpectedly succeeds
- `session_ingest_tests::cancelled_user_pass_reports_partial_coverage` [Linux/macOS] — deferred units 11 vs 9
- `session_reflector::session_reflector_runner_skips_when_task_is_disabled` [Linux/macOS] — count 1 vs 0
- `session_temporal_benchmark::tests::contract_matches_checked_in_artifacts` [Linux/macOS] — implementation hash mismatch
- `session_temporal_benchmark::tests::fixture_refresh_persists_progress_before_measurement` [Linux/macOS] — root refresh deadline exceeded
- `session_temporal_benchmark::tests::fresh_benchmark_db_provisions_key_for_rank_and_hydration` [Linux/macOS] — root refresh deadline exceeded
- `shutdown_terminal_linearizes_after_concurrent_admission` [Linux] — observability shutdown deadline
- `skill_lint_cursor_test::cursor_skill_references_resolve` [Linux/macOS] — six references name removed session-refresh/work tools
- `skill_targets_test::lifecycle_export_sweep_deploys_and_retracts_across_detected_agents` [Linux/macOS] — only Cursor exports; expected Claude and Cursor
- `skill_targets_test::lifecycle_export_sweep_isolates_per_agent_failures` [Linux/macOS] — Claude failure not reported
- `skill_targets_test::uninstall_all_removes_inverse_order_legacy_orphan_and_slugged_block` [Linux/macOS] — managed-skill prompt markers unbalanced
- `skill_targets_test::uninstall_all_removes_legacy_orphan_alongside_slugged_block` [Linux/macOS] — managed-skill prompt markers unbalanced
- `skill_targets_test::uninstall_repairs_legacy_orphan_end_without_claiming_user_text` [Linux/macOS] — managed-skill prompt markers unbalanced
- `skill_usage_test::repeated_skill_patches_recommend_improvement_review` [Linux/macOS] — `"repair_candidate"` vs `"patch_review"`
- `skill_usage_test::stale_scoring_explains_archive_candidates_and_exclusions` [Linux/macOS] — `"archive_candidate"` vs `"archive_review"`
- `skill_writer::skill_writer_runner_skips_when_task_is_disabled` [Linux/macOS] — count 1 vs 0
- `sqlite_writer_uses_production_wal_normal_policy` [Linux/macOS] — count 2 vs 1
- `stack_snapshot_decodes_into_the_typed_journey_request` [Linux/macOS] — `InvalidSurfaceRequest`
- `stale_binding_cannot_close_or_rebind_the_registered_store` [macOS] — `Conflict`
- `stdio_bridge_exits_successfully_after_client_shutdown_and_exit` [macOS] — daemon exited status 0 before accepting connections
- `storage_resolver_test::init_and_open::trace_decay_init_registers_default_profile_shard_globally` [Linux/macOS] — legacy checkout did not migrate to durable repository identity
- `temporal_application::canonical_digest_binds_every_semantic_input_and_excludes_resume_ephemera` [Linux/macOS] — `ProfileIdentityWithoutProject`
- `temporal_derived_evidence::frozen_temporal_page_returns_projected_occurrences_and_lineage` [Linux/macOS] — count 0 vs 1
- `tests::default_validation_uses_byte_pinned_activation_workload` [Linux/macOS] — workload hash mismatch
- `the_dashboard_work_surface_answers_who_worked_on_a_task_on_both_published_mounts` [macOS] — daemon exited status 0 before publishing authority
- `the_parity_golden_accounts_for_every_catalog_operation` [Linux/macOS] — eight application operations unaccounted
- `the_work_surface_answers_real_requests_on_both_published_mounts` [macOS] — daemon exited status 0 before publishing authority
- `tool_command::tests::array_value_collected_via_repetition` [Linux/macOS] — `--keywords` removed from context schema
- `tool_command::tests::bare_boolean_flag_at_end_of_args_defaults_to_true` [Linux/macOS] — bare `--include-code` now requires a value
- `tool_command::tests::bare_boolean_flag_before_next_flag_does_not_swallow_it` [Linux/macOS] — `raw_json` false
- `tool_command::tests::boolean_flag_with_explicit_value_after_it_is_still_consumed` [Linux/macOS] — string `"false"` vs boolean false
- `tool_command::tests::boolean_flag_with_invalid_explicit_value_still_errors` [Linux/macOS] — invalid `"maybe"` parses successfully
- `tool_command::tests::coerces_boolean_flag` [Linux/macOS] — string `"true"` vs boolean true
- `tool_command::tests::dispatch_routing_keys_bypass_unknown_key_gate` [Linux/macOS] — `--project-root` removed from `fact_store_list`
- `tool_command::tests::fact_feedback_bare_helpful_flag_does_not_swallow_note_flag` [Linux/macOS] — `--helpful` removed from `fact_feedback`
- `tool_command::tests::finalize_arrays_splits_csv` [Linux/macOS] — unwraps missing array value
- `tool_daemon_test::configuration_tool_cli_persists_effects_and_fails_on_stale_cas` [Linux/macOS] — observed configuration state never becomes available
- `tool_daemon_test::cursor_after_shell_missing_daemon_exits_promptly_without_children` [macOS] — retained project server retired during init
- `tool_daemon_test::daemon_first_touch_uses_registered_runtime_without_rewriting_legacy_config` [Linux/macOS] — daemon closes connection before result
- `tool_daemon_test::daemon_project_cache_is_scoped_by_client_identity` [Linux/macOS] — daemon closes connection before result
- `tool_daemon_test::daemon_project_handshake_uses_client_profile_identity` [Linux/macOS] — daemon closes connection before result
- `tool_daemon_test::daemon_project_handshake_uses_registered_remote_store_after_rename` [Linux/macOS] — daemon closes connection before result
- `tool_daemon_test::daemon_project_handshake_uses_registry_backed_profile_store_without_marker` [Linux/macOS] — daemon closes connection before result
- `tool_daemon_test::daemon_reuses_project_engine_across_tool_clients` [Linux/macOS] — first status call counts 2 vs 1
- `tool_daemon_test::daemon_sigterm_exits_while_authenticated_project_client_is_connected` [Linux/macOS] — daemon socket wait times out
- `tool_daemon_test::hermes_read_only_preflight_keeps_project_lcm_grep_available` [Linux/macOS] — project open completes without publishing a server
- `tool_daemon_test::kiro_hooks_capture_prompt_boundary_and_type_post_tool_use_unsupported` [Linux/macOS] — restored Kiro handler writes daemon-unavailable counter-reset stderr
- `tool_daemon_test::status_json_requests_compact_daemon_payload_noninteractively` [Linux/macOS] — retained project server retired during init
- `tool_daemon_test::tool_cli_invokes_mcp_tool_through_daemon_socket` [macOS] — daemon authority wait times out
- `tool_daemon_test::tool_cli_rejects_truncated_json_rpc_response_without_hanging` [macOS] — retained project server retired during init
- `tool_daemon_test::tool_cli_skips_daemon_notifications_until_matching_response` [macOS] — retained project server retired during init
- `tool_first_touch_test::fact_store_creates_profile_store_on_first_touch` [Linux/macOS] — removed `fact_store` tool is unknown
- `tool_skill_coverage_test::every_mcp_tool_is_taught_by_at_least_one_bundled_skill` [Linux/macOS] — 79 MCP tools unreferenced in canonical skill view
- `tracedecay::lifecycle::tests::nonempty_wrong_schema_read_only_open_returns_reset_required` [Linux/macOS] — ResetRequired authority is not `"graph store"`
- `tracedecay_test::daemon_tool_str_replace_updates_source` [Linux/macOS] — source edit now requires fresh idempotency key and preview expected-state
- `transcript_store::concurrent_full_batches_converge_without_split_brain_or_partial_writes` [Linux/macOS] — both concurrent full batches return `Ok(())`
- `transcript_store::late_cursor_failure_rolls_back_every_transcript_write_then_retries` [Linux/macOS] — summary count 0 vs 1 after retry
- `transcript_store::stale_higher_batch_is_rejected_until_reparsed_from_durable_cursor` [Linux/macOS] — summary count 0 vs 1 after reparse
- `update_cmd::tests::canonical_component_set_hosts_are_not_refreshed_by_a_second_writer` [Linux/macOS] — Cline now owns a canonical component set
- `verified_generations_keep_old_reads_dependencies_and_leases_stable` [macOS] — `Conflict`
- `workflow_json_preserves_a_typed_application_problem_envelope` [macOS] — count 0 vs 1
- `workload_fixture::semantic_workload_and_incremental_fixture_are_byte_exact` [Linux/macOS] — workload hash mismatch
- `worktree_canonical_root_guard_test::opening_from_linked_worktree_keeps_canonical_root_on_primary` [Linux/macOS] — incompatible database authority before canonical-root assertion
- `worktree_canonical_root_guard_test::stale_worktree_canonical_root_heals_on_next_touch` [Linux/macOS] — incompatible database authority before healing assertion
