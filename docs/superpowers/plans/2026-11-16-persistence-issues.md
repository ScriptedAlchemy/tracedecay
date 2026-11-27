# L1 Persistence implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move response handles, automation run-state, and saved-token accounting into the admitted `RegisteredGlobalDb` project/profile stores while deleting their independent persistence authorities and proving production behavior under concurrency, failure, and restart.

**Architecture:** Deliver three complete, sequential issue slices: response handles, automation run-state, then saved-token accounting. Each slice installs schema in the existing registered store, cuts over every production caller through async APIs bound to the admitted shard, deletes its old persistence owner in the same commit, and verifies real production journeys and failures. Response pages use checksummed Unicode-scalar chunks; automation uses indexed current/transition/staged rows plus a small retained external-effect journal; accounting uses per-response reservations and resettable epochs, with identity-bearing savings observations.

**Tech Stack:** Rust 2024, Tokio, `RegisteredGlobalDb`, daemon-owned rusqlite/SQLite transactions and indexes, `tracing`, MCP/CLI/dashboard contracts, Bazel, Vitest/TypeScript contract generation, pnpm only for dashboard package scripts.

**Spec:** `docs/superpowers/specs/2026-11-16-outstanding-issues-delivery-design.md`

## Global constraints

- Deliver in this order: #4448, #4427, then #4447. Each task is one coherent issue PR and one coherent commit. Start the next issue from the merged previous issue; cross-lane dependencies are start/merge gates, never permission to edit another lane's files.
- L1 is the first owner of five shared files (design, Shared files): `crates/tracedecay/src/daemon/project_open_owners.rs`, `crates/tracedecay-application/src/advisory/runtime/local_cycle_tests.rs`, and `crates/tracedecay/src/daemon/maintenance.rs` (Task 1), `crates/tracedecay/src/mcp/server.rs` (Tasks 1 and 3), and `crates/tracedecay-dashboard-api/src/state.rs` (Task 2). L5 #4436, L6 #4423, L2 #4099, and L5 #4419 integrate their edits to those files only after the L1 PR that edits them merges. Report each of those merge SHAs to the coordinator at once.
- The 2026-09-16 exception applies only to genuinely released relational stores and rows. Preserve existing relational data through real additive admission; never import graph metadata. JSON/JSONL has no compatibility reader, migration, dual write, backup, or secondary copy.
- A legacy response-handle JSON root produces feature-scoped `TraceDecayError::project_route("response_handle_legacy_json_refused", false, ...)`, not a broad profile/project reset. The response-handle feature reports that its legacy cache is not carried forward while unrelated source/session stores remain mounted and usable; the JSON bytes are untouched. Automation JSONL equivalently produces `project_route("automation_run_legacy_jsonl_refused", false, ...)` only for run-state operations, never a broad reset or deletion.
- Use the existing `RegisteredGlobalDb` and exact admitted `StoreRuntimeBindingV1.shard_id`; do not add a SQLite database, global content-hash authorization, an ORM, another accounting authority, or a process-global lifecycle cache. Complete all callers and old-authority deletion in the owning issue task.
- All new store APIs are async. Migrate sync render, CLI, feedback/LSP, and retained-settlement callers coherently; never use `block_on`, a hidden runtime, or a fallback writable connection.
- Preserve typed errors, redaction, project/profile attribution, UTF-8 character paging, lifecycle history, cancellation ownership, monitor/analytics/cost behavior, and existing accounting opt-out semantics. Local registered totals and optional savings history/analytics need not share one transaction.
- Tests use isolated home/profile/project/session inputs and shared production identity/enrollment authorities. Cover restart, wrong scope, concurrency, reorder, rollback, and cancellation where applicable. Named failing tests must fail a real behavior assertion or unresolved production symbol, never merely rely on a missing libtest filter.
- Run builds, tests, checks, and codegen only through the prescribed Bazel/dashboard paths. A green named run is `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test <one target> --test_output=all --test_arg=<full module path> --test_arg=--exact`; the helper proves only a pass and never an expected red, because it exits with a failing command's status and checks no signature. An expected red invokes Bazel directly, `bazel test <one target> --test_output=all --test_arg=<full module path> --test_arg=--exact`, and counts only when Bazel exits nonzero and the log shows the step's exact signature: rustc `error[E…]` lines naming the stated missing symbol, all in the stated file, or `test <full module path> ... FAILED` with the stated assertion text and `test result: FAILED. 0 passed; 1 failed`. A compile red covers every new test in its target, so assertion reds in the same target run before a test there names a new symbol. No substring filter is evidence. Broad/heavy Bazel suites and measurement runs require coordinator slots under the design's at-most-two default; timing-sensitive checks rerun in isolation without loosening any bound. Measurements yield raw evidence, not speedup claims.
- Never hand-edit `dashboard/src/contracts/`, `sdks/typescript/src/`, or `crates/tracedecay-sdk/src/operations.rs`; regenerate and check them in separate steps when implicated. Bazel prints a passing test's log only with `--test_output=all`; the helper runs below use it, and so does every unfiltered target run whose counts are cited. Commits use conventional headers of at most 72 characters without issue numbers and stage only the task's owned paths.

## Review focus

- A copied handle/run ID presented to a different logical shard must not become authorization, and retired JSON/JSONL must remain byte-identical after feature-scoped refusal: Task 1 test `foreign_namespace_and_legacy_json_are_refused_without_touching_bytes`; Task 2 test `wrong_project_and_legacy_jsonl_refuse_without_touching_bytes`.
- Multibyte text crossing a storage chunk/page boundary, a damaged chunk, or cleanup racing renewal must return exact text or a typed refusal: Task 1 tests `utf8_chunks_tamper_and_renewal_races_recover_exactly` and `cleanup_removes_only_expired_rows_in_the_admitted_namespace`.
- An external effect committed before its exact terminal is published must neither replay the effect nor fabricate success after any crash/cancellation boundary: Task 2 test `external_effect_crash_and_cancellation_boundaries_settle_once`.
- Reset can slip between reservation and completion, an identity/delta or observation can conflict, and an unsupported/null/missing request identity must never become a shared accounting key: Task 3 tests `pre_reset_reservation_conflict_and_overflow_keep_exact_epoch_state` and `identity_unavailable_never_credits_positive_savings`.
- Disconnect/restart and writer-stage failure can strand reservations and history while later responses commit and graph metadata contains a different-looking total: Task 3 tests `disconnect_restart_pending_stages_and_graph_totals_replay_once` and `savings_history_repairs_once_without_duplicate_rows`.

---

### Task 1: Complete response-handle registered-store cutover (#4448)

**Production and generated contract files:**
- Create: `crates/tracedecay-global-db/src/response_handles.rs`
- Create: `crates/tracedecay-global-db/src/response_handles/tests.rs`
- Modify: `crates/tracedecay-global-db/src/lib.rs`
- Modify: `crates/tracedecay-global-db/src/schema_stages.rs`
- Modify: `crates/tracedecay-mcp/src/response_handles.rs`
- Modify: `crates/tracedecay-mcp/src/tool_context.rs`
- Modify: `crates/tracedecay-mcp/src/handlers/retrieve.rs`
- Modify: `crates/tracedecay-mcp/src/handlers/graph_tool.rs`
- Modify: `crates/tracedecay-mcp/src/handlers/support.rs`
- Modify: `crates/tracedecay-mcp/src/handlers/edit.rs`
- Modify: `crates/tracedecay-mcp/src/handlers/grep.rs`
- Modify: `crates/tracedecay-mcp/src/handlers/ast_grep.rs`
- Modify: `crates/tracedecay-mcp/src/handlers/graph/search.rs`
- Modify: `crates/tracedecay-mcp/src/handlers/graph/context_markdown.rs`
- Modify: `crates/tracedecay-mcp/src/handlers/info/status.rs`
- Modify: `crates/tracedecay-mcp/src/tools/render.rs`
- Modify: `crates/tracedecay-mcp/src/tools/render/ranked_tail.rs`
- Modify: `crates/tracedecay-mcp/src/application_output/tool_result.rs`
- Modify: `crates/tracedecay-application/src/feedback/concrete.rs`
- Modify: `crates/tracedecay-application/src/lsp_runtime/feedback_source.rs`
- Modify: `crates/tracedecay-application/src/lsp_runtime/managed_test_runs.rs`
- Modify: `crates/tracedecay-daemon-service/src/invocation/registrars.rs`
- Modify: `crates/tracedecay/src/daemon/project_open_owners.rs`
- Modify: `crates/tracedecay/src/daemon/maintenance.rs`
- Modify: `crates/tracedecay/src/mcp/tools/handlers/application_surface.rs`
- Modify: `crates/tracedecay/src/mcp/tools/handlers/dispatch_groups.rs`
- Modify: `crates/tracedecay/src/mcp/server.rs`
- Modify: `crates/tracedecay-cli/src/tool_command.rs`
- Modify: `crates/tracedecay-cli/src/tool_command/args.rs`
- Modify: `crates/tracedecay-cli/src/commands/storage.rs`
- Modify: `crates/tracedecay-runtime-core/src/storage.rs`
- Modify: `crates/tracedecay-runtime-core/src/storage/paths_and_io.rs`
- Modify: `crates/tracedecay-contracts/src/retrieval/project_info_surface.rs`
- Modify: `crates/tracedecay-session-memory/src/response_handles.rs` (replace only the old root-lock/file implementation with `ResponseHandleStore`)
- Modify: `crates/tracedecay-session-memory/src/lib.rs`
- Regenerate only by command: `dashboard/src/contracts/`, `sdks/typescript/src/`, `crates/tracedecay-sdk/src/operations.rs`

**Test, fixture, and module registration files:**
- Create: `crates/tracedecay-session-memory/src/response_handles/tests.rs`
- Create: `crates/tracedecay-application/tests/application_suite/feedback_test.rs`
- Modify: `crates/tracedecay-application/tests/application_suite/main.rs` (register `mod feedback_test;` before the suite's test modules)
- Test: `crates/tracedecay-application/src/advisory/runtime/local_cycle_tests.rs`
- Test: `crates/tracedecay-application/src/advisory/host_delivery_consume_tests.rs`
- Test: `crates/tracedecay-application/src/lsp_runtime/advisory_source_tests.rs`
- Test: `crates/tracedecay-code-index-runtime/tests/code_index_scheduler/reconcile.rs`
- Test: `crates/tracedecay/tests/runtime_acceptance_suite/advisory_runtime_acceptance.rs`
- Test: `crates/tracedecay-mcp/src/tools/render/tests.rs`
- Test: `crates/tracedecay-mcp/src/handlers/graph_tool.rs`
- Test: `crates/tracedecay-runtime-core/src/storage/tests.rs`
- Test: `crates/tracedecay/src/daemon/invocation_tests/types_tests.rs`
- Test: `crates/tracedecay/src/daemon/maintenance.rs`
- Test: `crates/tracedecay/src/mcp/tools/handlers/dispatch_test_support.rs`
- Test: `crates/tracedecay/benches/coverage/mod.rs`
- Test: `crates/tracedecay-cli/src/tool_command/tests.rs`
- Test: `crates/tracedecay-cli/tests/core_cli_suite/cli_non_interactive_test.rs`
- Test: `crates/tracedecay/tests/mcp_suite/mcp_handler_test/active_project_test.rs`
- Test: `crates/tracedecay/tests/mcp_suite/mcp_handler_test/retrieve_truncation_test.rs`
- Test: `crates/tracedecay/tests/mcp_suite/mcp_server_test/retrieve_behavior_test.rs`
- Test: `crates/tracedecay/tests/mcp_suite/mcp_server_test/protocol_test.rs`
- Test: `crates/tracedecay/tests/mcp_suite/mcp_server_test/support.rs`
- Test: `crates/tracedecay/tests/runtime_acceptance_suite/runtime_surface_acceptance.rs`
- Test: `crates/tracedecay/tests/runtime_acceptance_suite/tool_client_transport.rs`
- Test: `crates/tracedecay/tests/storage_suite/storage_resolver_test.rs`
- Test: `crates/tracedecay/tests/storage_suite/storage_resolver_test/markers.rs`
- Test: `crates/tracedecay/tests/storage_suite/storage_resolver_test/artifact_routing.rs`

**Interfaces:**
- Consumes: Admitted `RegisteredGlobalDbLeaseV1` project/profile session stores and their transactions/snapshots. No filesystem path is accepted as storage authority.
- Produces:
  - `ResponseHandleRecord { handle: String, created_at: i64, expires_at: i64, content: String }`, `ResponseHandlePage { handle: String, created_at: i64, expires_at: i64, original_chars: usize, content: String }`, and `ResponseHandleLookup<T>::{Found(T), Missing, Expired { created_at: i64, expires_at: i64 }}`.
  - `ResponseHandleError::{InvalidHandle, CorruptRecord { handle: String, reason: String }, Store(TraceDecayError)}`; `impl From<ResponseHandleError> for TraceDecayError` maps only invalid handles to `InvalidResponseHandle` and exposes no storage paths.
  - `response_handle_for(content: &str) -> String`, `micros_to_seconds(value: UtcMicros) -> i64`, `is_valid_response_handle(handle: &str) -> bool`, and `RESPONSE_HANDLE_TTL_SECS: i64 = 86_400`.
  - `RegisteredGlobalDb::publish_response_handle(&self, content: &str, now: i64) -> Result<ResponseHandleRecord, ResponseHandleError>`.
  - `RegisteredGlobalDb::retrieve_response_handle(&self, handle: &str, now: i64) -> Result<ResponseHandleLookup<ResponseHandleRecord>, ResponseHandleError>` for bounded cursor payloads.
  - `RegisteredGlobalDb::retrieve_response_handle_page(&self, handle: &str, offset: usize, max_chars: usize, now: i64) -> Result<ResponseHandleLookup<ResponseHandlePage>, ResponseHandleError>`; it reads only the required chunks.
  - `ResponseHandleCleanup { scanned: usize, removed_expired: usize }` and `ResponseHandleInventory { record_count: u64, total_bytes: u64, oldest_expires_at: Option<i64>, newest_expires_at: Option<i64> }`.
  - `RegisteredGlobalDb::cleanup_expired_response_handles(&self, now: i64, limit: usize) -> Result<ResponseHandleCleanup, ResponseHandleError>` and `RegisteredGlobalDb::inventory_response_handles(&self) -> Result<ResponseHandleInventory, ResponseHandleError>`.
  - Owned, cloneable `ResponseHandleStore` in `crates/tracedecay-session-memory/src/response_handles.rs`, constructed only from admitted lease plus layout root: `ResponseHandleStore::admit(database: RegisteredGlobalDbLeaseV1, data_root: impl Into<PathBuf>) -> tracedecay_domain::errors::Result<Self>` is async. Metadata classification uses `spawn_blocking` only for `symlink_metadata(data_root.join("response-handles"))`: absence constructs a serving adapter, an existing legacy entry retains `response_handle_legacy_json_refused` inside the adapter, and any other metadata error returns a typed storage error. No payload is opened or deleted; `data_root` never opens an alternate store.
  - `ResponseHandleStore::database(&self) -> &RegisteredGlobalDb`; async forwarders `store(&self, content: &str, now: i64) -> Result<ResponseHandleRecord, ResponseHandleError>`, `retrieve(&self, handle: &str, now: i64) -> Result<ResponseHandleLookup<ResponseHandleRecord>, ResponseHandleError>`, `page(&self, handle: &str, offset: usize, max_chars: usize, now: i64) -> Result<ResponseHandleLookup<ResponseHandlePage>, ResponseHandleError>`, `inventory(&self) -> Result<ResponseHandleInventory, ResponseHandleError>`, and `cleanup(&self, now: i64, limit: usize) -> Result<ResponseHandleCleanup, ResponseHandleError>`. Each checks the retained feature refusal before forwarding to the admitted lease, so unrelated stores remain usable. There is no production `legacy_refusal_for_test`; tests create real isolated legacy metadata.
  - Async MCP wrappers with the same public names taking `&ResponseHandleStore`; async `render::finalize(store: Option<&ResponseHandleStore>, args: &Value, value: &Value, md: impl FnOnce() -> String + Send) -> String` and `compute_retrieve(store: &ResponseHandleStore, args: &Value)`.
  - `open_feedback_runtime(database: Database, project_root: impl Into<PathBuf>, response_handles: ResponseHandleStore, scope: ResolvedScope, access: ProjectSourceAccessSnapshot)`, `FeedbackRuntime::response_handles(&self) -> &ResponseHandleStore`, and async `store_request_handle`/`load_handle_content` accepting `&ResponseHandleStore`.
  - Async `response_handle_stats_json(store: Option<&ResponseHandleStore>)`, async `sweep_response_handles(stores: Vec<ResponseHandleStore>, max_per_store: usize, cancellation: CancellationToken)`, and async `cli_response_handle_store(profile: &ProfileRoot, project: Option<&Path>) -> Result<Option<ResponseHandleStore>>`. Composition/tests alone construct the adapter with an admitted lease and existing `StoreLayout.data_root`.
  - `ActiveProjectStorageV1` and `StoreLayout` no longer expose `response_handle_root`; `response_handle_project_root` and the response-handle GC/reset families disappear. Legacy roots are classified only by `ResponseHandleStore::admit` to produce the feature-scoped refusal in Global Constraints.

- [ ] **Step 1: Write the failing registered-store behavior tests**

  In the new store and adapter test modules, add `published_replays_renewals_and_utf8_pages_survive_restart`, `truncated_hash_collisions_refuse`, `utf8_chunks_tamper_and_renewal_races_recover_exactly`, `page_reads_fetch_only_selected_chunks`, `cleanup_removes_only_expired_rows_in_the_admitted_namespace`, `foreign_namespace_and_legacy_json_are_refused_without_touching_bytes`, and adapter-specific `legacy_adapter_refusal_is_scoped_and_metadata_failures_stay_typed`. Cover restart, exact replay, half-TTL renewal, collision evidence, 4,096-character boundaries, chunk tampering, concurrent publish/read/cleanup, before/after-commit faults, metadata-only inventory/expiry, cancellation between stores, real isolated legacy metadata, and metadata-access failure. Prove adapter refusal never reads/deletes legacy bytes and unrelated stores continue serving.

- [ ] **Step 2: Register the response-handle test modules**

  Add `response_handles` and its `#[cfg(test)] mod tests;` declaration in `crates/tracedecay-global-db/src/lib.rs`, plus `#[cfg(test)] mod tests;` in `crates/tracedecay-session-memory/src/response_handles.rs`, so global-store and adapter tests compile into their respective `unit_test` targets and cannot disappear from a libtest filter.

- [ ] **Step 3: Run the new store tests to verify failure**

  Run directly `bazel test //crates/tracedecay-global-db:unit_test --test_output=all --test_arg=response_handles::tests::published_replays_renewals_and_utf8_pages_survive_restart --test_arg=--exact` and `bazel test //crates/tracedecay-session-memory:unit_test --test_output=all --test_arg=response_handles::tests::legacy_adapter_refusal_is_scoped_and_metadata_failures_stay_typed --test_arg=--exact`.
  Expected red: each Bazel run exits nonzero before any test runs. The global-db log shows rustc `error[E…]` lines naming `publish_response_handle`, every one in `crates/tracedecay-global-db/src/response_handles/tests.rs`; that compile failure is the red for all six global-store tests. The session-memory log shows rustc `error[E…]` lines naming `ResponseHandleStore`, every one in `crates/tracedecay-session-memory/src/response_handles/tests.rs`. An error in any other file is not this red.

- [ ] **Step 4: Implement the relational owner, metadata, and feature-scoped legacy refusal**

  Install `response_handles` and `response_handle_chunks` with namespace/handle keys, an expiry index, immutable full/chunk digests, and byte/character/expiry metadata. Implement the produced methods with exact replay/collision checks, 4,096-Unicode-scalar chunks, one publication transaction, page-local integrity checks, indexed bounded expiry, and `COUNT`/`SUM`/`MIN`/`MAX` inventory. Derive namespace solely from admitted logical `shard_id`; never accept it from a caller. Implement `ResponseHandleStore::admit` and its refusal-aware forwarders in `crates/tracedecay-session-memory/src/response_handles.rs`, retaining the old module but deleting only its root-lock/file payload implementation. Add `global_db.response_handles.*` tracing spans and classify only legacy root metadata without reading or deleting payload files.

- [ ] **Step 5: Run the store tests to verify passing**

  Run all seven tests through the helper, one invocation each: `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-global-db:unit_test --test_output=all --test_arg=response_handles::tests::<name> --test_arg=--exact` for `published_replays_renewals_and_utf8_pages_survive_restart`, `truncated_hash_collisions_refuse`, `utf8_chunks_tamper_and_renewal_races_recover_exactly`, `page_reads_fetch_only_selected_chunks`, `cleanup_removes_only_expired_rows_in_the_admitted_namespace`, and `foreign_namespace_and_legacy_json_are_refused_without_touching_bytes`, then the session-memory Step 3 command prefixed with the helper.
  Expected: each reports `1 passed; 0 failed`, including pre-commit rollback, post-commit replay without another row, metadata queries that touch no chunks, and unchanged legacy JSON. The adapter also serves a fresh namespace while reporting the legacy feature refusal and typed metadata failures without a production test-only constructor.

- [ ] **Step 6: Write the failing production journeys**

  Add `retrieve_chunk_tamper_returns_corrupt_record_not_content`, `feedback_handles_mint_and_resolve_through_registered_store`, `active_project_hides_legacy_response_handle_root`, `lsp_feedback_expansion_uses_registered_response_handle`, and `cli_truncated_output_retrieves_registered_page`. `active_project_hides_legacy_response_handle_root` reads only the MCP tool's JSON and asserts with the message `active project still reports a response-handle root`. `cli_truncated_output_retrieves_registered_page` drives only the built CLI and asserts with the message `truncated CLI output wrote a legacy response-handles directory`. Both compile on the unmodified tree. Write those two first: Step 8 runs their reds before the tamper test and the journey update, which name `ResponseHandleStore`, are saved into `mcp_suite`. Update the existing large/multibyte retrieve journey to publish through the production lease and reconstruct its 16 KiB, 1 MiB, multibyte, and 4 MiB cases.

- [ ] **Step 7: Register the application feedback test module**

  Add `mod feedback_test;` to `crates/tracedecay-application/tests/application_suite/main.rs`, ensuring `feedback_handles_mint_and_resolve_through_registered_store` is compiled by `//crates/tracedecay-application:application_suite`.

- [ ] **Step 8: Run the production journeys to verify old behavior fails**

  Run each red directly, in this order:
  1. Before the large/multibyte journey update and the tamper test are saved, `bazel test //crates/tracedecay:mcp_suite --test_output=all --test_arg=mcp_handler_test::active_project_test::active_project_hides_legacy_response_handle_root --test_arg=--exact`. Expected red: nonzero exit, `test mcp_handler_test::active_project_test::active_project_hides_legacy_response_handle_root ... FAILED`, the text `active project still reports a response-handle root`, and `test result: FAILED. 0 passed; 1 failed`.
  2. With the journey update and tamper test saved, `bazel test //crates/tracedecay:mcp_suite --test_output=all --test_arg=mcp_handler_test::retrieve_truncation_test::retrieve_pages_reconstruct_large_and_multibyte_handles_with_bounded_frames --test_arg=--exact`. Expected red: nonzero exit before any test runs, with rustc `error[E…]` lines naming `ResponseHandleStore`, every one in `crates/tracedecay/tests/mcp_suite/`.
  3. `bazel test //crates/tracedecay-application:application_suite --test_output=all --test_arg=feedback_test::feedback_handles_mint_and_resolve_through_registered_store --test_arg=--exact` and `bazel test //crates/tracedecay-application:unit_test --test_output=all --test_arg=lsp_runtime::feedback_source::tests::lsp_feedback_expansion_uses_registered_response_handle --test_arg=--exact`. Expected red: each exits nonzero before any test runs, with rustc `error[E…]` lines naming `ResponseHandleStore`, every one in `tests/application_suite/feedback_test.rs` and `src/lsp_runtime/feedback_source.rs` respectively.
  4. `bazel test //crates/tracedecay-cli:core_cli_suite --test_output=all --test_arg=cli_non_interactive_test::cli_truncated_output_retrieves_registered_page --test_arg=--exact`. Expected red: nonzero exit, `test cli_non_interactive_test::cli_truncated_output_retrieves_registered_page ... FAILED`, the text `truncated CLI output wrote a legacy response-handles directory`, and `test result: FAILED. 0 passed; 1 failed`.

- [ ] **Step 9: Cut over every production caller and delete the JSON authority**

  Apply all produced async signatures through MCP, render helpers, retained execution, feedback, advisory/LSP expansion, CLI, daemon composition, maintenance, and stats, taking `&ResponseHandleStore` rather than exposing `&RegisteredGlobalDb` to these surfaces. Open CLI adapters through the canonical session registry and await all CLI rendering; construct adapters only at composition/test admission with an admitted lease and `StoreLayout.data_root`. Migrate every listed test/fixture to adapter-based publication or to a generic artifact-only assertion where no handle behavior is under test; retain unrelated text such as the search-quality source fixture unchanged. Delete only the old root-lock/file implementation inside `crates/tracedecay-session-memory/src/response_handles.rs`, remove `response_handle_root` from layout/status, JSON envelopes, staging/rollback/tombstone helpers, `response_handle_project_root`, and the `response-handles` reset target in `commands/storage.rs`. Leave `ProfileFamilyV1::ResponseHandles` and its footprint mappings in `crates/tracedecay-maintenance/src/retention/footprint.rs` untouched: retention and doctor size findings are L2's, and L2 Task 14 deletes that family after this task merges. No production caller retains a root-based, lease-only, or dual-write path.

- [ ] **Step 10: Regenerate the implicated public contracts**

  Run `pnpm --dir dashboard run contracts:generate` from the repository root to regenerate `dashboard/src/contracts/*`, `sdks/typescript/src/*`, and `crates/tracedecay-sdk/src/operations.rs` for the removed `ActiveProjectStorageV1.response_handle_root`.
  Expected: generated outputs change only through this command; no generated file is hand-edited or conflict-merged.

- [ ] **Step 11: Verify all response-handle production routes and generated output**

  Run `pnpm --dir dashboard run contracts:check`, then each of the six Step 8 tests through the helper (`REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh` before the Step 8 command), then `bazel test //crates/tracedecay-global-db:unit_test //crates/tracedecay-session-memory:unit_test //crates/tracedecay-application:application_suite //crates/tracedecay-mcp:unit_test //crates/tracedecay:storage_suite //crates/tracedecay:runtime_acceptance_suite //crates/tracedecay:mcp_suite //crates/tracedecay-cli:core_cli_suite --test_output=all`.
  Expected: each helper run reports `1 passed; 0 failed`; every target's `test result:` line reports a nonzero passed count and `0 failed`; the active-project contract has no response-handle root, feedback/LSP/CLI continuations resolve exactly, and legacy refusal leaves unrelated source/session routes usable.

- [ ] **Step 12: Measure the large-handle production journey**

  With a coordinator measurement slot, rerun the large/multibyte MCP journey: `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay:mcp_suite --nocache_test_results --test_output=all --test_env=TRACEDECAY_SPAN_TIMINGS=1 --test_arg=mcp_handler_test::retrieve_truncation_test::retrieve_pages_reconstruct_large_and_multibyte_handles_with_bounded_frames --test_arg=--exact`. Expect `1 passed; 0 failed`, and record publication, selected-chunk page retrieval, inventory/expiry, and writer-wait close timings. Report the timing and query evidence only; make no speedup claim.

- [ ] **Step 13: Commit the complete response-handle issue slice**

  Stage only Task 1 paths and commit with `simplify(response-handles): persist pages in registered stores`.

### Task 2: Complete automation run-state cutover (#4427)

**Production files:**
- Create: `crates/tracedecay-global-db/src/automation_run_state.rs`
- Modify: `crates/tracedecay-global-db/src/lib.rs`
- Modify: `crates/tracedecay-global-db/src/schema_stages.rs`
- Create: `crates/tracedecay-automation-runtime/src/automation/exact_run.rs`
- Create: `crates/tracedecay-automation-runtime/src/automation/registered_run_store.rs`
- Create: `crates/tracedecay-automation-runtime/src/automation/registered_run_store/tests.rs`
- Modify: `crates/tracedecay-automation-runtime/src/automation/mod.rs`
- Modify: `crates/tracedecay-automation-runtime/src/automation/run_ledger.rs`
- Modify: `crates/tracedecay-automation-runtime/src/automation/lifecycle.rs`
- Modify: `crates/tracedecay-automation-runtime/src/automation/jobs.rs`
- Modify: `crates/tracedecay-automation-runtime/src/automation/jobs/scheduler_gate.rs`
- Modify: `crates/tracedecay-automation-runtime/src/automation/memory_curator/review.rs`
- Modify: `crates/tracedecay-automation-runtime/src/automation/effect_runtime/settlement.rs`
- Modify: `crates/tracedecay-automation-runtime/src/automation/effect_runtime/recovery_index.rs`
- Delete: `crates/tracedecay-automation-runtime/src/automation/run_ledger/cursor.rs`
- Delete: `crates/tracedecay-automation-runtime/src/automation/run_ledger/exact_lookup.rs`
- Delete: `crates/tracedecay-automation-runtime/src/automation/run_ledger/exact_publication.rs`
- Delete: `crates/tracedecay-automation-runtime/src/automation/run_ledger/lifecycle_index.rs`
- Delete: `crates/tracedecay-automation-runtime/src/automation/run_ledger/scheduler_diagnostic.rs`
- Modify: `crates/tracedecay-dashboard-api/src/state.rs`
- Modify: `crates/tracedecay-dashboard-api/src/automation_run_api.rs`
- Modify: `crates/tracedecay-dashboard-api/src/automation_scheduler_api.rs`
- Modify: `crates/tracedecay-mcp/src/handlers/automation_runs.rs`
- Modify: `crates/tracedecay-mcp/src/handlers/analytics.rs`
- Modify: `crates/tracedecay-cli/src/automation_cli/runs.rs`
- Modify: `crates/tracedecay/src/daemon/scheduler/effect_admission.rs`

**Test, fixture, and module registration files:**
- Create: `crates/tracedecay/tests/automation_runner_test/registered_run_store.rs`
- Modify: `crates/tracedecay/tests/automation_runner_test/main.rs` (register `mod registered_run_store;`)
- Test: `crates/tracedecay-automation-runtime/src/automation/lifecycle/tests.rs`
- Test: `crates/tracedecay-automation-runtime/src/automation/jobs/scheduler_config_tests.rs`
- Test: `crates/tracedecay-automation-runtime/src/automation/effect_runtime/settlement/tests.rs`
- Test: `crates/tracedecay-automation-runtime/src/automation/run_ledger.rs`
- Test only removed-API import/assertions, no artifact behavior change: `crates/tracedecay-automation-runtime/src/automation/run_ledger/publication.rs`
- Test: `crates/tracedecay-daemon-service/src/automation_effect/journal/tests.rs`
- Test: `crates/tracedecay/src/daemon/scheduler/combined_effect.rs`
- Test: `crates/tracedecay/src/daemon/tests/scheduler_config/paused_tick.rs`
- Test: `crates/tracedecay/src/daemon/tests/scheduler_config/reset_blocked_tick.rs`
- Test: `crates/tracedecay/benches/coverage/mod.rs`
- Test: `crates/tracedecay/benches/coverage/admin_fixture.rs`
- Test: `crates/tracedecay-cli/tests/core_cli_suite/cli_non_interactive_test.rs`
- Test: `crates/tracedecay-sdk/tests/sdk_suite/semantic_replay.rs`
- Test: `crates/tracedecay/src/daemon/production_harness/drop_shutdown_journey_test.rs`
- Test: `crates/tracedecay/tests/dashboard_api_test/automation.rs`
- Test: `crates/tracedecay/tests/dashboard_api_test/automation_config.rs`
- Test: `crates/tracedecay/tests/dashboard_api_test/automation_jobs.rs`
- Test: `crates/tracedecay/tests/mcp_suite/analytics_test.rs`
- Test: `crates/tracedecay/tests/mcp_suite/mcp_cli_serve_test.rs`
- Test: `crates/tracedecay/tests/mcp_suite/mcp_handler_test/automation_run_list_behavior_test.rs`
- Test: `crates/tracedecay/tests/mcp_suite/mcp_handler_test/automation_runs_test.rs`
- Test: `crates/tracedecay/tests/mcp_suite/mcp_handler_test/automation_skill_request_test.rs`
- Test: `crates/tracedecay/tests/mcp_suite/mcp_handler_test/fact_store_curate_test.rs`
- Test: `crates/tracedecay/tests/mcp_suite/mcp_handler_test/skills_automation_test.rs`
- Test: `crates/tracedecay/tests/mcp_suite/mcp_server_test/automation_run_view_test.rs`
- Test: `crates/tracedecay/tests/mcp_suite/mcp_server_test/automation_run_artifact_view_test.rs`
- Test: `crates/tracedecay/tests/automation_runner_test/run_ledger.rs`
- Test: `crates/tracedecay/tests/automation_runner_test/jobs.rs`
- Test: `crates/tracedecay/tests/automation_runner_test/runner.rs`
- Test: `crates/tracedecay/tests/automation_runner_test/combined_review.rs`
- Test: `crates/tracedecay/tests/automation_runner_test/memory_curator.rs`
- Test: `crates/tracedecay/tests/automation_runner_test/memory_curator/pagination.rs`
- Test: `crates/tracedecay/tests/automation_runner_test/memory_curator/backend_failures.rs`
- Test: `crates/tracedecay/tests/automation_runner_test/memory_curator/manual_trigger.rs`
- Test: `crates/tracedecay/tests/automation_runner_test/session_reflector.rs`
- Test: `crates/tracedecay/tests/automation_runner_test/skill_writer.rs`
- Test: `crates/tracedecay/tests/automation_runner_test/support.rs`

**Interfaces:**
- Consumes: Admitted project/profile session leases; existing `AutomationRunLedgerRecord`, statuses, triggers, task kinds, effect journals, and artifact readers/writers.
- Produces:
  - `RegisteredAutomationRunStore::admit(database: RegisteredGlobalDbLeaseV1, dashboard_root: PathBuf) -> Result<Self>`; namespace comes only from its admitted logical `shard_id`, and artifact paths use the retained root. Legacy JSONL/spool/append intents produce only `automation_run_legacy_jsonl_refused`.
  - `async fn append_run_record(&self, record: &AutomationRunLedgerRecord) -> tracedecay_domain::errors::Result<()>`.
  - `async fn find_run_record(&self, run_id: &str) -> tracedecay_domain::errors::Result<Option<AutomationRunLedgerRecord>>`.
  - `async fn stage_exact_terminal(&self, record: &AutomationRunLedgerRecord) -> tracedecay_domain::errors::Result<ExactRunPublication>`.
  - `async fn publish_staged_run_record_exact(&self, run_id: &str, publication: &ExactRunPublication) -> tracedecay_domain::errors::Result<ExactRunPublishOutcome>`.
  - `async fn discard_staged_run_record_exact(&self, run_id: &str, expected: &ExactRunPublication) -> tracedecay_domain::errors::Result<()>`.
  - `async fn discard_unbound_staged_run_records_if(&self, run_id: &str, validate_unbound: impl FnOnce() -> tracedecay_domain::errors::Result<bool> + Send + 'static) -> tracedecay_domain::errors::Result<ExactRunUnboundDiscardOutcome>`.
  - `async fn discard_stale_staged_run_record_exact_after_terminal(&self, run_id: &str, expected: &ExactRunPublication, terminal_is_current: impl FnOnce() -> tracedecay_domain::errors::Result<bool> + Send + 'static) -> tracedecay_domain::errors::Result<()>`.
  - `async fn load_run_records(&self, limit: usize) -> tracedecay_domain::errors::Result<Vec<AutomationRunLedgerRecord>>`.
  - `async fn load_run_records_page(&self, limit: usize) -> tracedecay_domain::errors::Result<AutomationRunLedgerPageV1>`.
  - `async fn load_run_records_for_task_key(&self, task_key: &str, limit: usize) -> tracedecay_domain::errors::Result<Vec<AutomationRunLedgerRecord>>`.
  - `async fn load_run_ledger_task_summary(&self, task: AgentTaskKind, task_key: &str) -> tracedecay_domain::errors::Result<AutomationRunLedgerTaskSummary>`; all six selections remain latest logical activity, successful, effectful-any-trigger, scheduler-effectful, scheduler-activity, and session-evidence-budget-exhausted.
  - `AutomationValidationCursorV1::ResumeAfterFactId` and `async fn load_latest_task_validation_cursor(&self, task_key: &str, cursor: AutomationValidationCursorV1) -> tracedecay_domain::errors::Result<Option<Value>>`; the indexed expression is exactly `json_extract(record_json, '$.validation_report.pagination.resume_after_fact_id')`.
  - `async fn append_or_reuse_scheduler_diagnostic(&self, candidate: &AutomationRunLedgerRecord, effectful_anchor_run_id: Option<&str>) -> tracedecay_domain::errors::Result<AutomationRunLedgerRecord>`, validating the exact persistent anchor and candidate in one transaction.
  - `crates/tracedecay-automation-runtime/src/automation/exact_run.rs` is the sole owning module for `ExactRunPublication`, `ExactRunPublishOutcome`, and `ExactRunUnboundDiscardOutcome`; `automation::run_ledger` canonically re-exports those types for existing effect-runtime imports.
  - Exact order is stage row → external journal `Prepared` → SQL current+transition commit → external journal `Terminal`/housekeeping. Exact rows are invisible before publication and replay or conflict by canonical JSON digest/length.
  - `DashboardState::automation_run_store(&self) -> Result<&RegisteredAutomationRunStore>`; MCP run readers take the store; CLI resolves the canonical project store before artifact reads. `RetainedSettlementWaiter<T>::start_future<F>(future: F) -> Self where F: Future<Output = T> + Send + 'static` replaces blocking-wrapper settlement without `block_on`.

- [ ] **Step 1: Write the failing registered run-state tests**

  Add `ordinary_and_exact_publication_replay_conflict_and_restart`, `indexed_history_summary_cursor_and_scheduler_diagnostic`, `publication_commit_faults_leave_retryable_exact_state`, and `legacy_jsonl_is_refused_without_deletion`. Exercise queued/running/terminal transitions, exact replay once, changed same-status bytes refusal, restart history/artifact digests, all six summary selections, the nested validation-report cursor, anchor validation, untouched legacy files, and before/after-commit fault recovery.

- [ ] **Step 2: Register the runtime run-store test module**

  Add `registered_run_store` and its `#[cfg(test)] mod tests;` declaration through `crates/tracedecay-automation-runtime/src/automation/mod.rs`, compiling all four tests into `//crates/tracedecay-automation-runtime:unit_test`.

- [ ] **Step 3: Run the registered run-state tests to verify failure**

  Run directly `bazel test //crates/tracedecay-automation-runtime:unit_test --test_output=all --test_arg=automation::registered_run_store::tests::ordinary_and_exact_publication_replay_conflict_and_restart --test_arg=--exact`.
  Expected red: Bazel exits nonzero before any test runs, and the log shows rustc `error[E…]` lines naming `RegisteredAutomationRunStore`, every one in `crates/tracedecay-automation-runtime/src/automation/registered_run_store/tests.rs`. That compile failure is the red for all four tests.

- [ ] **Step 4: Implement schema, exact type ownership, and the runtime adapter**

  Add current rows keyed by namespace/run ID, immutable transitions keyed by namespace/run ID/status, staged rows, and indexes on sequence, task/status/trigger/canonical completion, and the exact nested cursor expression above. Implement lifecycle validation and atomic current+transition writes, selected-row decoding, exact staging/discard/replay, indexed history and six summary queries, and scheduler diagnostics. Define exact types only in `exact_run.rs`; keep generic artifact content/digest code and helpers, removing only JSONL-specific assertions from its publication test.

- [ ] **Step 5: Run the registered run-state tests to verify passing**

  Run all four Step 3 tests through the helper, one invocation each: `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-automation-runtime:unit_test --test_output=all --test_arg=automation::registered_run_store::tests::<name> --test_arg=--exact` for `ordinary_and_exact_publication_replay_conflict_and_restart`, `indexed_history_summary_cursor_and_scheduler_diagnostic`, `publication_commit_faults_leave_retryable_exact_state`, and `legacy_jsonl_is_refused_without_deletion`.
  Expected: `1 passed; 0 failed` each, including rollback, post-commit replay without another transition, correct cursor lookup beneath `validation_report`, and unchanged legacy JSONL.

- [ ] **Step 6: Write the failing production surface, isolation, and crash tests**

  In the new automation integration module add `runner_publishes_and_reloads_registered_records`, `mcp_cli_and_dashboard_read_the_same_registered_history`, and `wrong_project_and_legacy_jsonl_refuse_without_touching_bytes`. In effect settlement tests add `external_effect_crash_and_cancellation_boundaries_settle_once` and `terminal_conflicting_replay_refuses_without_second_effect`. They use only APIs that exist after Step 4, and each asserts first with its own message: `the runner's terminal record is not in the registered store`, `a reader served history the registered store does not hold`, `legacy JSONL was served instead of refused`, `an external effect settled twice across a crash boundary`, and `a conflicting terminal replay ran a second effect`. Cover one runner write through all readers, wrong project/profile refusal, untouched JSONL, before/after effect, before/after prepared, before/after SQL commit, journal terminal failure, cancellation after acceptance, exact duplicate replay, and conflicting replay.

- [ ] **Step 7: Register the automation integration test module**

  Add `mod registered_run_store;` to `crates/tracedecay/tests/automation_runner_test/main.rs`, ensuring all three production journeys compile into `//crates/tracedecay:automation_runner_test`.

- [ ] **Step 8: Run the production and crash tests to verify failure**

  Run each directly: `bazel test //crates/tracedecay:automation_runner_test --test_output=all --test_arg=registered_run_store::<name> --test_arg=--exact` for `runner_publishes_and_reloads_registered_records`, `mcp_cli_and_dashboard_read_the_same_registered_history`, and `wrong_project_and_legacy_jsonl_refuse_without_touching_bytes`, and `bazel test //crates/tracedecay-automation-runtime:unit_test --test_output=all --test_arg=automation::effect_runtime::settlement::tests::<name> --test_arg=--exact` for `external_effect_crash_and_cancellation_boundaries_settle_once` and `terminal_conflicting_replay_refuses_without_second_effect`.
  Expected red: each Bazel run exits nonzero, and its log shows `test <full module path> ... FAILED`, that test's Step 6 message, and `test result: FAILED. 0 passed; 1 failed`. A compile error instead means a test named an API that Step 4 did not produce; fix the test.

- [ ] **Step 9: Cut over all production callers and delete the JSONL authority**

  Migrate every listed production caller and fixture at once: runner/finalizer/publication methods, job scheduler gates and diagnostics, memory-cursor reads, retained settlement/recovery, dashboards, MCP analytics/readers, and CLI. Mount the store on each admitted project/profile context. Use `start_future` for async settlement. Delete the five JSONL modules plus ordinary appends, spools, intents, file-offset cursors, lifecycle/process indexes, quarantine, and `run_ledger_path`; remove repair/intents from project recovery. Retain only the external-effect journal and artifact code. Convert every listed old test to the store rather than allowing deleted APIs to survive under test-only shims.

- [ ] **Step 10: Run all automation production routes and failure tests to verify passing**

  Run the five Step 8 tests through the helper (`REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh` before each Step 8 command), then `bazel test //crates/tracedecay-automation-runtime:unit_test //crates/tracedecay-daemon-service:unit_test //crates/tracedecay:automation_runner_test //crates/tracedecay:dashboard_api_test //crates/tracedecay:mcp_suite //crates/tracedecay-cli:core_cli_suite --test_output=all`.
  Expected: each exact test reports `1 passed; 0 failed`; every target's `test result:` line reports a nonzero passed count and `0 failed`, identical history/artifact results across routes, and no JSONL API left to satisfy tests.

- [ ] **Step 11: Write the growing-history measurement test**

  Add `growing_history_reports_indexed_read_write_and_lock_spans` to the registered-store tests against isolated 2,000- and 8,000-transition histories for exact lookup, one append, task summary, and writer wait. Assert correct results and bounded selected-row behavior, with tracing enabled only through the existing instrumentation.

- [ ] **Step 12: Run the growing-history measurement**

  Run `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-automation-runtime:unit_test --test_output=all --test_env=TRACEDECAY_SPAN_TIMINGS=1 --test_arg=automation::registered_run_store::tests::growing_history_reports_indexed_read_write_and_lock_spans --test_arg=--exact` under a coordinator measurement slot. Record raw span/query evidence only; require `1 passed; 0 failed`, no full-history selection for exact lookup, and no speedup claim.

- [ ] **Step 13: Commit the complete automation issue slice**

  Stage only Task 2 paths and commit with `simplify(automation): retire JSONL run persistence`.

### Task 3: Complete per-response saved-token cutover (#4447)

**Production files:**
- Modify: `crates/tracedecay-global-db/src/registered_accounting.rs`
- Create: `crates/tracedecay-global-db/src/registered_accounting/tests.rs`
- Modify: `crates/tracedecay-global-db/src/lib.rs`
- Modify: `crates/tracedecay-global-db/src/schema_stages.rs`
- Modify: `crates/tracedecay-global-db/src/tests.rs`
- Modify: `crates/tracedecay/src/mcp/server/ledger.rs`
- Modify: `crates/tracedecay/src/mcp/server/requests.rs`
- Modify: `crates/tracedecay/src/mcp/server/requests/tool_dispatch.rs`
- Modify: `crates/tracedecay/src/mcp/server.rs`
- Modify: `crates/tracedecay/src/mcp/server/connection.rs`
- Delete: `crates/tracedecay-application/src/tracedecay/store_meta.rs`
- Modify: `crates/tracedecay-application/src/tracedecay/mod.rs`
- Modify: `crates/tracedecay-project/src/project/queries/meta.rs`
- Modify: `crates/tracedecay-mcp/src/handlers/admin_project.rs`
- Modify: `crates/tracedecay-mcp/src/handlers/hook_runtime/mod.rs`
- Modify: `crates/tracedecay-mcp/src/handlers/admin_cli.rs`

**Test and fixture files:**
- Test: `crates/tracedecay-global-db/src/registry_maintenance/tests.rs` only if its committed-max behavior changes; retain it unchanged otherwise.
- Test: `crates/tracedecay-project/src/config/tests.rs`
- Test: `crates/tracedecay-project/src/test_support/host_admission/accounting_test_support.rs`
- Test: `crates/tracedecay/src/mcp/server/freshness_tests.rs`
- Test: `crates/tracedecay/src/mcp/server/ledger.rs`
- Test: `crates/tracedecay/tests/mcp_suite/mcp_server_test/protocol_test.rs`
- Test: `crates/tracedecay/tests/mcp_suite/mcp_server_test/analytics_test.rs`
- Test: `crates/tracedecay/tests/dashboard_api_test/runtime.rs`
- Test: `crates/tracedecay/tests/runtime_acceptance_suite/lifecycle_production_authority_test.rs`

**Interfaces:**
- Consumes: Existing released `projects.tokens_saved` and `savings_ledger` rows, `TraceDecay::profile_database()` as the sole canonical profile accounting owner, and `tracedecay_contracts::request_identity::mcp_connection_request_id`.
- Produces:
  - `ProjectTokenSavingsV1 { lifetime_total: u64, epoch: u64, epoch_total: u64 }`, `TokenResponseReservationV1 { project_key: String, response_id: RequestId, epoch: u64 }`, and `TokenSavingsCommitV1 { disposition: TokenSavingsDispositionV1::{Credited, Replayed}, totals: ProjectTokenSavingsV1 }`.
  - `RegisteredGlobalDb::project_token_savings(&self, project_path: &Path) -> tracedecay_domain::errors::Result<ProjectTokenSavingsV1>`.
  - `RegisteredGlobalDb::reserve_project_token_response(&self, project_path: &Path, response_id: &RequestId) -> tracedecay_domain::errors::Result<TokenResponseReservationV1>`.
  - `RegisteredGlobalDb::commit_project_token_response(&self, reservation: &TokenResponseReservationV1, delta: u64) -> tracedecay_domain::errors::Result<TokenSavingsCommitV1>`.
  - `RegisteredGlobalDb::reset_project_token_epoch(&self, project_path: &Path) -> tracedecay_domain::errors::Result<ProjectTokenSavingsV1>`. Conflicting delta, overflow, and unreserved submission are typed nonretryable route errors.
  - Canonical wrappers on `TraceDecay`: `get_tokens_saved(&self) -> tracedecay_domain::errors::Result<u64>`, `get_local_counter(&self) -> tracedecay_domain::errors::Result<u64>`, `reserve_token_response(&self, response_id: &RequestId) -> tracedecay_domain::errors::Result<TokenResponseReservationV1>`, `commit_token_savings(&self, reservation: &TokenResponseReservationV1, delta: u64) -> tracedecay_domain::errors::Result<TokenSavingsCommitV1>`, and `reset_local_counter(&self) -> tracedecay_domain::errors::Result<ProjectTokenSavingsV1>`; each is async and explicitly delegates to `self.profile_database()`, never `configuration_runtime()` or graph metadata.
  - `SavingsObservationOutcomeV1::{Inserted, Replayed}` and `record_savings_observation(&self, project_path: &str, tool_name: &str, before_tokens: u64, after_tokens: u64, timestamp: i64, response_id: Option<&RequestId>) -> Result<SavingsObservationOutcomeV1>`. It replaces `try_record_savings` and its production call; exact observations replay and same-ID changed tool/before/after/timestamp values return `token_savings_observation_conflict`.
  - Additive relational admission adds nullable `response_id TEXT` to `savings_ledger` and a partial unique index on `(project_path, response_id) WHERE response_id IS NOT NULL`. Existing rows and duplicate NULL IDs remain intact. New non-null IDs enforce idempotence; exact replay returns `Replayed` without another row.
  - `DispatchedToolCall.accounting_request_id: Option<RequestId>` carries the already computed application request identity into accounting. `commit_token_savings(project_root, tokens_saved, sink, response_id: Option<&RequestId>, delta, commit_sender) -> bool` reserves then commits without awaiting on the MCP response path.
  - `Option::None` is never replaced with a fallback key: positive savings sends a false commit receipt, logs `token_savings_identity_unavailable`, credits no local total/history/analytics, writes monitor `delta = 0`, and leaves the response nonblocking. Zero-delta behavior is unchanged.
  - `try_upsert_project_tokens` remains only for explicit registry administration/committed-max maintenance, not startup, per-response, or shutdown writes. Shutdown only drains observed writes/checkpoints.

- [ ] **Step 1: Write failing accounting and savings-history tests**

  Add `reverse_completion_commits_each_response_once`, `pre_reset_reservation_conflict_and_overflow_keep_exact_epoch_state`, `accounting_stage_failures_leave_retryable_reservation`, `historical_project_total_survives_additive_schema_convergence`, and `savings_history_repairs_once_without_duplicate_rows`. In the last test, fail the optional observation after a local receipt commits, retry after repair to insert once, replay again after restart to verify no second row, then seed a changed same-ID observation and require refusal. Also assert historical NULL rows survive additive admission unchanged and conflicting local deltas never append history.

- [ ] **Step 2: Register the accounting test module**

  Add `#[cfg(test)] mod tests;` beside the new implementation in `crates/tracedecay-global-db/src/registered_accounting.rs`, ensuring all five new tests compile into the unit suite and fail by assertion/unresolved production API, not by missing registration.

- [ ] **Step 3: Run the accounting tests to verify failure**

  Run directly `bazel test //crates/tracedecay-global-db:unit_test --test_output=all --test_arg=registered_accounting::tests::reverse_completion_commits_each_response_once --test_arg=--exact`.
  Expected red: Bazel exits nonzero before any test runs, and the log shows rustc `error[E…]` lines naming `reserve_project_token_response` and `record_savings_observation`, every one in `crates/tracedecay-global-db/src/registered_accounting/tests.rs`. That compile failure is the red for all five tests.

- [ ] **Step 4: Implement canonical accounting and identity-bearing observations**

  Add epoch and response-reservation tables plus the partial unique observation identity to schema admission. Implement atomic reservation/commit/reset/read, original-receipt replay, checked overflow, exact delta conflicts, and epoch attribution for pre-reset queued responses. Replace `try_record_savings` with `record_savings_observation`: compare the complete observation on duplicate ID, return `Inserted` once or `Replayed` thereafter, and refuse any changed field. Do not import graph metadata or require local totals and optional history to commit in one transaction.

- [ ] **Step 5: Run the accounting tests to verify passing**

  Run each of the five Step 3 tests through the helper, `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-global-db:unit_test --test_output=all --test_arg=registered_accounting::tests::<name> --test_arg=--exact` for `reverse_completion_commits_each_response_once`, `pre_reset_reservation_conflict_and_overflow_keep_exact_epoch_state`, `accounting_stage_failures_leave_retryable_reservation`, `historical_project_total_survives_additive_schema_convergence`, and `savings_history_repairs_once_without_duplicate_rows`, and `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-global-db:unit_test --test_output=all --test_arg=tests::registered_mount_publishes_complete_schema --test_arg=--exact`.
  Expected: each reports `1 passed; 0 failed`; history has one row after local-replay/history-repair/restart-replay, the conflicting observation writes none, and released totals/history remain unchanged.

- [ ] **Step 6: Write failing production identity, scheduling, and enablement tests**

  Add `reverse_scheduled_responses_commit_exactly_once`, `admin_reset_serializes_with_pending_delta`, `disconnect_restart_pending_stages_and_graph_totals_replay_once`, `accounting_failures_keep_response_nonblocking_and_monitor_honest`, `identity_unavailable_never_credits_positive_savings`, and `registered_lifetime_total_survives_restart`. Assert held-writer responses return before storage settles, identical response IDs credit once, changed IDs both count, pre-reset reservation stays in its epoch, disconnect/restart/retry does not duplicate, history repair inserts once, missing identity produces the typed diagnostic/monitor zero with no total/history/analytics credit, and disabled optional savings remains disabled while local registered totals retain their existing behavior. Seed conflicting graph metadata and prove it is ignored. `registered_lifetime_total_survives_restart` drives only the runtime and asserts with the message `the lifetime total did not survive restart from the registered store`, so it compiles on the unmodified tree.

- [ ] **Step 7: Run the production tests to verify failure**

  Run directly `bazel test //crates/tracedecay:unit_test --test_output=all --test_arg=mcp::server::ledger::tests::reverse_scheduled_responses_commit_exactly_once --test_arg=--exact`. Expected red: Bazel exits nonzero before any test runs, and the log shows rustc `error[E…]` lines naming `commit_token_savings` or `accounting_request_id`, every one in `crates/tracedecay/src/mcp/server/ledger.rs`; that compile failure is the red for the five ledger tests. Run directly `bazel test //crates/tracedecay:runtime_acceptance_suite --test_output=all --test_arg=lifecycle_production_authority_test::registered_lifetime_total_survives_restart --test_arg=--exact`. Expected red: nonzero exit, `test lifecycle_production_authority_test::registered_lifetime_total_survives_restart ... FAILED`, the text `the lifetime total did not survive restart from the registered store`, and `test result: FAILED. 0 passed; 1 failed`.

- [ ] **Step 8: Cut over every accounting caller and delete graph counter ownership**

  Thread valid application request identity through dispatch/completion. Make local total reserve/commit use only `TraceDecay::profile_database()` wrappers; make optional history use the observation API after either a new or replayed local receipt so a failed history write can be repaired without double crediting. Remove startup/shutdown absolute upserts, switch admin/hook reads and resets to the wrappers, and delete graph token getters/setters, `store_meta.rs`, their exports, and all listed fixtures. Preserve #4852's nonblocking commit-receipt, analytics gating, and monitor/cost handling while adding the explicit `None` identity refusal. Change helper calls that seeded anonymous savings to `response_id: None`; do not change committed-max registry-maintenance code unnecessarily.

- [ ] **Step 9: Run all accounting production routes and failure tests to verify passing**

  Run each of the six Step 7 tests through the helper, `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay:unit_test --test_output=all --test_arg=mcp::server::ledger::tests::<name> --test_arg=--exact` for `reverse_scheduled_responses_commit_exactly_once`, `admin_reset_serializes_with_pending_delta`, `disconnect_restart_pending_stages_and_graph_totals_replay_once`, `accounting_failures_keep_response_nonblocking_and_monitor_honest`, and `identity_unavailable_never_credits_positive_savings`, and the runtime acceptance Step 7 command prefixed with the helper. Then run `bazel test //crates/tracedecay-global-db:unit_test //crates/tracedecay:unit_test //crates/tracedecay-project:unit_test //crates/tracedecay-mcp:unit_test //crates/tracedecay:mcp_suite //crates/tracedecay:dashboard_api_test --test_output=all`.
  Expected: each named test reports `1 passed; 0 failed`; every target's `test result:` line reports a nonzero passed count and `0 failed`; held writers prove nonblocking responses, pending shutdown drains accepted writes without an absolute rewrite, and no graph setter remains.

- [ ] **Step 10: Verify accounting enablement and historical surfaces**

  Run the default, environment-override, and lifetime-counter cases through the helper by exact name: `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay:mcp_suite --test_output=all --test_arg=mcp_server_test::analytics_test::<name> --test_arg=--exact` for `ledger_records_by_default_without_env_opt_in`, `global_accounting_env_overrides`, and `lifetime_counter_matches_ledger_net_savings`. Expect `1 passed; 0 failed` each. Then run `bazel test //crates/tracedecay:runtime_acceptance_suite --test_output=all` and expect a nonzero passed count with `0 failed`.
  Expected: local totals retain their existing enablement path, optional savings history/analytics remain disabled by the existing environment mode, monitor/cost output and historical savings stay readable, and released relational history never imports graph metadata. Record eliminated per-response graph writes and duplicate owners without claiming less I/O.

- [ ] **Step 11: Commit the complete saved-token issue slice**

  Stage only Task 3 paths and commit with `simplify(accounting): unify response savings ownership`.
