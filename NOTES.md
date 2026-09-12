# Lane notes — fable/test-prune

## Lane status 2026-09-12

Branch `fable/test-prune` (40 prune/style commits on top of the
`fable/test-slop` floor 13452d3ea; `origin/fable/test-slop` is that floor
itself, with no prune commits, and no prune commit had landed on the
integration branch) was merged forward onto
`origin/codex/tracedecay-total-redesign-plan-reopened` in three merge
commits: 03427d2b48 (tip 84f29ec656, 117 conflicted files / 157 hunks),
6fd74ec325 (tip 23dde7695d, 11 files / 14 hunks) and e8c8fa36b2 (tip
0ebeedf7dc after #1241, 3 files / 3 hunks). Pushed as PR #1242.

Rule applied to every conflict, per test item: a lane deletion stays only
when the item is identical between the lane floor (95893a92a) and the tip,
or the tip's only change was mechanical (rename, extra argument, import
path, constant/prose update, rustfmt). Tests the tip changed to cover new
behaviour keep the tip's version; tests the tip added stay untouched.

### Kept the tip's version (lane deletion dropped)

| crate / area | test | why |
| --- | --- | --- |
| tracedecay-code-index | `languages.rs::descriptor_lookups_are_canonical_and_deterministic` | tip pins the extractor revision |
| tracedecay-mcp | `application_output/markdown.rs::canonical_markdown_golden_formats_only_the_supplied_view`, `application_output/view.rs::partial_evidence_extracts_the_typed_coverage_fields` | tip covers the new `Block` payload rendering |
| tracedecay-private-fs | `sharing_violation_is_not_lock_contention` | new `windows()` assertion |
| tracedecay-query | `lexical/tests.rs::candidate_sources_admit_rarest_first_within_the_document_budget` | tip adds the typed `CandidateSourcesPruned` partial outcome |
| tracedecay-runtime-core | `branch_meta.rs::add_and_remove_branch` | new `is_query_eligible` assertions |
| tracedecay-domain | `research/id.rs::integrity_digest_types_accept_supported_algorithms` | tip adds `hex_suffix` assertions |
| tracedecay (mcp_suite) | `memory_facts_test::fact_store_reason_requires_an_entity_selection` | tip adds duplicate-entity denial |
| tracedecay-agent-hosts | `kimi.rs::rendered_plugin_uses_kimi_supported_mcp_command` | tip-new test inside a module the lane had emptied |
| scripts | `test-check-pr-dogfood-output.py::test_strict_accepts_graph_ready_bounded_prefix_with_more_symbols`, `test-linux-test-partitions.py::test_complete_disjoint_partition_passes` | tip refactored / extended them for config summaries and macOS groups |

### Pruning re-applied at a moved location

- `daemon/broker_stream_transport.rs` → `daemon/broker_stream_transport_tests.rs`:
  `full_close_wait_ignores_request_half_close` (identical; duplicate of the
  rmcp receive test).
- `doctor/registry_drift.rs` → `tracedecay-maintenance/src/retention/diagnostics.rs`:
  `orphan_finding` + three disposition→kind mapping tests (identical).
- `src/mcp/tools/plugin_conformance_tests.rs` → `tests/product_surface_suite/plugin_conformance.rs`:
  tip only renamed it; stays deleted.

### Pruning dropped because the tip deleted the target itself

`crates/tracedecay-cli/tests/core_cli_suite/host_cli_fixture.rs` (tip
commit 16e2305a6) and 77 individual items the tip had already removed
(`config/tests.rs`, `store_maintenance/mod.rs`, `handlers/info/status.rs`,
`lifecycle/registry.rs`, … blocks).

### Measured lib test counts (`cargo test --workspace --lib`, tip 8df152a7b vs merged)

Crates with no change (capture, host-admission, privacy, sdk,
semantic-contracts, tool-catalog) omitted.

| crate | tip | merged | pruned |
| --- | ---: | ---: | ---: |
| tracedecay | 1082 | 994 | 88 |
| tracedecay-agent-hosts | 649 | 490 | 159 |
| tracedecay-api | 63 | 51 | 12 |
| tracedecay-application | 649 | 596 | 53 |
| tracedecay-automation | 64 | 42 | 22 |
| tracedecay-automation-runtime | 507 | 442 | 65 |
| tracedecay-code-extraction | 58 | 52 | 6 |
| tracedecay-code-index | 260 | 176 | 84 |
| tracedecay-code-index-retention | 93 | 92 | 1 |
| tracedecay-code-index-runtime | 494 | 468 | 26 |
| tracedecay-configuration | 70 | 63 | 7 |
| tracedecay-contracts | 442 | 379 | 63 |
| tracedecay-daemon-control | 118 | 86 | 32 |
| tracedecay-daemon-identity | 25 | 24 | 1 |
| tracedecay-daemon-protocol | 69 | 61 | 8 |
| tracedecay-daemon-service | 280 | 260 | 20 |
| tracedecay-dashboard-api | 236 | 169 | 67 |
| tracedecay-domain | 238 | 215 | 23 |
| tracedecay-framing | 9 | 6 | 3 |
| tracedecay-global-db | 395 | 365 | 30 |
| tracedecay-graph-db | 187 | 160 | 27 |
| tracedecay-graph-query | 59 | 45 | 14 |
| tracedecay-hooks | 83 | 82 | 1 |
| tracedecay-host-integration | 8 | 7 | 1 |
| tracedecay-lcm | 180 | 158 | 22 |
| tracedecay-lsp | 217 | 183 | 34 |
| tracedecay-maintenance | 161 | 130 | 31 |
| tracedecay-mcp | 372 | 290 | 82 |
| tracedecay-policy | 16 | 14 | 2 |
| tracedecay-private-fs | 20 | 18 | 2 |
| tracedecay-query | 341 | 310 | 31 |
| tracedecay-runtime-core | 480 | 413 | 67 |
| tracedecay-rusqlite-runtime | 374 | 353 | 21 |
| tracedecay-search-eval | 44 | 43 | 1 |
| tracedecay-semantic | 260 | 224 | 36 |
| tracedecay-session-memory | 361 | 296 | 65 |
| tracedecay-session-runtime | 115 | 107 | 8 |
| tracedecay-session-temporal-store | 150 | 137 | 13 |
| tracedecay-sessions | 701 | 563 | 138 |
| tracedecay-source-edit | 93 | 85 | 8 |
| tracedecay-store | 103 | 83 | 20 |
| tracedecay-store-runtime | 149 | 139 | 10 |
| tracedecay-temporal-query | 224 | 177 | 47 |
| **total** | **10744** | **9293** | **1451** |

Other suites: dashboard vitest 1774 → 1418 (157 files green,
`tsc --noEmit` clean), `sdks/typescript` vitest 36 → 29 green,
`scripts/test-*.py` 117 → 106 green. Integration test binaries
(`crates/*/tests`) were compiled by `cargo check --workspace --all-targets`
but not run or counted here.

### Verification tickets (hauler)

- cc-16731 / cc-16946 — `cargo check --workspace --all-targets`: 0 errors, 0 warnings.
- cc-16734 (tip) / cc-16735 (merged) — `cargo test --workspace --lib --no-fail-fast`.
  Both ran under load 67–103 on 96 cores and both were red; every
  merged-only failure re-ran green in cc-16824, cc-16825, cc-16826,
  cc-16827, cc-16828. Failures shared with the tip run
  (`code_index_scheduler::*` deadline tests, `extract::tests::canonical_rows_digest_matches_pinned_identity`,
  `partitioned_codec::tests::streamed_segment_projection_refuses_an_incomplete_descriptor_set`,
  search-eval `report_tests::*`, `accepted_profile_authority::tests::portable_report_requires_the_current_workload_digest`,
  `lsp_runtime::advisory_source_tests::incomplete_publication_remains_readable_without_consuming_completed_dedupe`,
  `orphan_stores::tests::unregistered_store_sweep_reconciles_interrupted_quarantine`)
  are tip-red, not introduced here.
- cc-16947 — `cargo test --lib` agent-hosts / query / semantic / domain after the second merge: green.
- cc-16952 — `cargo test -p tracedecay --lib daemon::core_doctor daemon::maintenance`: 40 passed.

### Third merge (e8c8fa36b2, tip 0ebeedf7dc)

Three conflicted files. `orphan_stores/tests.rs`: the lane deletion of
`sweep_unregistered_stores_collects_an_exactly_empty_old_directory` stays
(identical on tip); the tip-added
`unregistered_store_with_a_vanished_manifest_root_is_collected_at_once`
is kept. `host_bundle_acceptance.rs`: use-group only, the lane's trimmed
`agents` import group stays because the two tests it served are unchanged
on tip and remain deleted. `NOTES.md` (both added): this section stays on
top, the #1103 text-build lane notes from tip follow.

- cc-17119 — `cargo check --workspace --all-targets`: 0 errors, 0 warnings.
- cc-17125 — `product_surface_suite host_bundle_acceptance`: 17 passed.
- cc-17124 — `cargo test -p tracedecay-maintenance --lib`: 128 passed,
  3 failed (`portable_inventory_*` timeouts under load 30–50; two of the
  three were already red in the unmodified tip run cc-16734). cc-17153
  re-ran the 6 `portable_inventory` tests single-threaded: 6 passed.

### Undone

- Integration suites (`--tests`) were type-checked only; their pruned
  counts are not measured.

# #1103 text-build lane — Fable lane notes

Worktree: `/fast/tmp/td-text-build-1103` (branch `fable/text-build-1103`).
Target: PR #707 branch `codex/tracedecay-total-redesign-plan-reopened`, merged at `8df152a7b` (measurements) and
again at `9e07045ba0` (clean merge; gate/park/freshness tests, the query freeze test, and clippy re-verified there).
Env: `TRACEDECAY_SKIP_DASHBOARD_BUILD=1 TRACEDECAY_DASHBOARD_BUNDLE_SHA256=806f5649351425b7f80dbb3e088d9032d747dc524922ddc3b823c4ba821f8a1f`.
Not pushed; no PR.

## Lane status 2026-09-12

Journey: `daemon_suite indexing_lifecycle_test::mounted_incremental_lifecycle_preserves_only_complete_compatible_generations`
(`cargo test -p tracedecay --features test-transport --test daemon_suite <name> -- --exact --test-threads=1`),
45 s restart bound untouched. Machine load 40–70 on 96 cores throughout; every number below is under that load
and the before/after pairs are the only fair comparison.

**Before (merged tip `8df152a7b` + `8cb846989`, hauler cc-16667):** red. At the bound: text 417/772 files,
`phase=bulk_commit`, 26.9 files/s, `last_commit_latency` 0.92 s/64-page batch, `code_graph_serving=pending`.

**After (`b564e6e3e3`, cc-16936):** still red, but the owner moved. At the bound: text **772/772**, 196,622 chunks,
`phase=index_build` (finalization), `code_graph_serving=ready`. The graph is no longer on the critical path; the
text build (bulk 29 s + finalization ≈ 8 s + verification ≈ 2 s under this load) after a 13–19 s restart reconcile
is what remains.

Decomposition (Hotpath daemon `target/hotpath/debug/tracedecay` with `test-transport,hotpath`, cold index of the
same 772-file tree = the post-restart phases; report json in `/tmp/td1103/run-hp{2,3}/hotpath.json`):

| phase | before (hp2, load ~70) | after (hp3, load ~50) |
| --- | --- | --- |
| `code_index.reconcile.pass` (extract + assemble 5.9 s serial + publish) | 17.5 s | 20.6 s |
| text build: `query.artifact.batch.scheduler_wake` ×24 | 64.5 s | 34.6 s |
|  of which `batch.sqlite` / `batch.commit` | 27.7 s / 15.3 s | 15.4 s / 4.5 s |
|  finalization `advance_wake` (index build; `term_postings_by_term` 3.6–5.6 s) | 18.4 s | 8.1 s |
| `code_graph.activation.total` (`graph_db.sealed_store.build` 17 s, verify 7 s, catalog 3 s) | 40.1 s, **after** text | 29.9 s, **overlapped** |
| wall to `current/complete/fresh` | text ready 85 s → graph ready 123 s | 83 s, graph ready before text |

Artifacts for the fixture (9.1 MB source, 98,304 one-line fns, 196,620 lexical documents — the `"\n"` window
chunks are already gone): sealed segments 328 MB (772 × 313 KB + 87 MB evidence), lexical artifact **394 MB**
(`term_postings` 91 + `_by_term` 80, `rows` 48, `ngram_postings` 47, dictionary 19, `rows_by_chunk` 17, vocabulary 13+13
MB via `dbstat`), grafeo 262 MB, interactive catalog read-bundle 117 MB; profile total 1.1 GB. The 2.4 GB of the
issue title does not reproduce; the volume is a data-model question across three stores, not lane-sized.

Fix in this lane (`b564e6e3e3`): the publication worker drove the whole text projection inline and only then gated
graph prepare/activation on a *ready* text owner, serialising two builds that both consume only the sealed
generation. The projection now runs on its own task, the gate prepares once the owner has *reopened*, and the seat
joins the projection (an unfinished or latched-failed owner still seats nothing; activation is idempotent). Pinned by
`convergence_park_tests::graph_activation_starts_while_the_published_text_owner_is_parked` (times out on the old
ordering — verified by flipping the gate back). `tracedecay-code-index-runtime --lib`: 492 passed, 1 failed —
`verified_empty_source_remains_observable_while_scheduler_is_busy` fails identically on the unmodified tip
(`Elapsed` waiting for `reconciled_without_generation`), not this lane's. Clippy `-D warnings` clean.

WIP disposition: the four dirty edits were a duplicate of `eda2011eb` (running field-length totals, named
`field_stats_staging` on tip), the parallel-decode test adjustment `683d87d5d` already carries, a tightened
`disk_artifact_defers_statistics_and_serving_indexes_until_freeze` (kept, `8cb846989`, staging-table name fixed), and a
println measurement probe example (checkpointed in `420d7ca9d`, removed in `f2c8d6e597`; `code_lexical_catchup` is
the maintained bench).

### What remains for #1103 item 4
1. Text build rate is the wall: 25 batches at 0.6–0.9 s of `batch.sqlite` each (single-row `INSERT` CPU on
   `term_postings`/`ngram_postings`, `journal_mode=DELETE synchronous=NORMAL` commit 0.18–0.6 s per batch under load),
   plus ~10 s of per-wake sealed-source decode (`restore.file_admit` 6.5 s, `segment_decode` 5.7 s aggregate) that
   is not pipelined with the SQLite append, plus 8 s finalization. Under an idle machine the earlier lane measured
   27 s; the restart bound leaves ~25 s for it after a 16–19 s reconcile.
2. Restart reconcile re-seals the recovered batch from scratch: `build.assemble` 5.9 s serial and
   `sealed_encode.evidence` 4.4 s serial are the two remaining serial phases in `build.and_publish`.
3. The interactive-catalog read bundle (117 MB) and `graph_db.sealed_store.build` (17 s) are off the critical path now
   but still define the graph-ready time when the text build gets faster.
4. Item 5 (hooked saves reconcile only at the 30 s window, `hook_hint_count: 0`) is untouched by this lane.
5. CI: the validated `root-daemon-suite` partition from the issue thread should land with the item-4 fix.

## Lane status 2026-09-12 — fable/wave2-root (root crate cleanup, ponytail wave 2)

Worktree `/fast/tmp/td-wave2-root-fable`, base `5292a39d2c` (tip after #1245), target dir
`/fast/tmp/td-target-wave2-root`. Five commits, one per audit item; each compiles under
`cargo check -p tracedecay --lib --tests --features test-helpers` and
`cargo check --workspace --all-targets` (cc-18039) at the branch head.

| item | outcome |
| --- | --- |
| 1 alias re-exports | `tracedecay::query` had zero consumers; `tracedecay::code_index` had four (workflow handler digest, CLI blocking-thread sizing, one product-surface test, and the scheduler flight test the root compiled via `#[path]`). All retargeted to the sibling crate; CLI gains the edge with `default-features = false`. Both aliases deleted. |
| 2 dispatch table | Forwarding arms for graph (18), info (10), analysis (18), git (8), health (7) moved beside their handlers as one `dispatch_tool` per family in `tracedecay-mcp`; the root lends its admission funnel as `VerifiedGraphOpen`. Root `dispatch_groups.rs` 1478→1104 lines. `LegacyToolCompatibilityOwner` kept (see below); its dead `OWNER` label deleted. |
| 3 in-src tests | 73 `#[cfg(test)]` roots probed by compiling them as an integration target: 66 use private items or sit under private modules, 6 import private `super::` items, 1 (`remote_protocol_tests`, 289 lines) is clean but is the fixture for two blocked siblings. **0 lines movable as pure moves.** Two files the root compiled out of `tracedecay-code-index-runtime/src` via `#[path]` were relocated: the scheduler journeys (2,110 lines) into `tests/daemon_suite/code_index_ignored_dependencies_test`, the census journey (144) into `src/daemon/`. `production_harness` gate moved to the `mod` declaration, deleting 36 per-item repeats and narrowing 3 to `cfg(unix)`. |
| 4 hook runtime | **False claim.** Root `hook_runtime/` (4.9k, 1.4k tests) is the `tracedecay_hook_runtime` action handler composing `tracedecay-hooks` (ledger, config snapshot, envelope types), host-admission, sessions, and `TraceDecay`; `tracedecay-mcp::hook_runtime` is 62 lines of error mapping the root already calls. Zero shared function names; ripwire clone scan across the three trees found no production clone (one 0.82 near-miss: `hook_v2_family_label` vs `HookEventKind::as_key`, different enums). Nothing folded. |
| 5 serve stubs, module name | `ensure_initialized*` had no caller beyond three tombstone assertions: stubs, in-src test, and the two integration assertions deleted; `serve` is now crate-private around the URI decoder. `src/tracedecay.rs` → `src/project.rs`; 236 `crate::tracedecay::` and 71 `tracedecay::tracedecay::` paths retargeted (root, suites, benches, CLI). |

### Tip-side reds observed (not this lane's)

- Clippy `-D warnings`: four `clippy::large_futures` errors in `daemon/hook_v2_replay_consumer.rs`
  (147, 197, 311) and `project_open_owners/advisory_runtime.rs:1520`; identical bytes in the tip's
  CI Clippy job (run 34696973432). Verified this lane with `-A clippy::large_futures`.
- Root lib: `mcp::tools::handlers::search_graph_independence_tests::{tracedecay_search_preserves_lexical_results_when_graph_admission_is_missing, tracedecay_search_refuses_foreign_generation_graph_evidence_without_erasing_results}`
  (`node_id` not null) — FAILED in the tip's `Test Linux root-lib` job.
- `code_index_ignored_dependencies_test::flight_tests::aborted_flight_owner_wakes_follower_and_allows_a_fresh_owner`
  is a pre-existing intra-test race (fails alone 1/6, module 3/4 in the tip-equivalent in-lib binary of
  `/fast/projects/tracedecay` at 5be9a952e7; FAILED in the tip's root-lib CI). Mechanism: after
  `owner.abort()` + `hold.release()` the orphaned blocking build still publishes, so the fresh owner's
  `expected_generation` is stale (`IgnoredDependency(StaleGeneration)`). Moving the file did not change it.
- `daemon::production_harness::lcm_preserved_profile_journey_test::preserved_profile_lcm_discovery_converges_without_blocking_retrieval`
  went `outcome=stale` once under load 43 alongside 25 harness journeys; passes alone 2/2 (46 s each).

### `LegacyToolCompatibilityOwner` — why it stays

`admits(name)` is advertised-name membership (`get_tool_definitions()`), consulted after the typed
daemon-surface groups return and before group dispatch. It is not redundant with the binding table:
`MCP_TOOL_BINDING_SPECS` is a static list, so a bound-but-unadvertised name (host-gated
`tracedecay_ast_grep_search`/`_rewrite` when ast-grep is absent) is rejected as unknown only by this
gate. Tools it guards that the root still serves (P0-1 leftovers): `tracedecay_retrieve`,
`tracedecay_remote_status`, `tracedecay_status`, `tracedecay_active_project`,
`tracedecay_project_{list,search,context}`, `tracedecay_admin_sync`, `tracedecay_runtime`, admin
(`hook_runtime`, `admin_cli`, `admin_project`), edit (10 `tracedecay_*_replace/insert/move/rename/rollback/reconcile`),
memory (`automation_run_*`, `analytics`, `skill_*`, `hermes_skill_bridge`), session-workflow
(`diagnose`, `run_affected_tests`, `dashboard`), and every retained-application operation.

## P0-1/P0-2 collapse plan

### Dependency direction (cargo tree, normal edges)

`tracedecay-mcp` ← `tracedecay-agent-hosts` ← `tracedecay-daemon-service` ← `tracedecay` ← `tracedecay-cli`.
`tracedecay-mcp` depends on 25 crates and on none of agent-hosts, daemon-service, daemon-control,
daemon-identity, host-admission, lsp, automation-runtime. The only agent-hosts → mcp edge is
`ports/mcp_tools.rs` (`get_tool_definitions`, `format_capable_tool_names`).

Two facts block every "move the rest into tracedecay-mcp":

1. **`TraceDecay` lives in the root** (`src/project/`, 2.8k lines) and is read at 161 sites in
   `src/mcp/tools`, 52 in `src/mcp/server`, and throughout `src/daemon`. It depends on agent-hosts
   (context-scout owner lookup), configuration, store-runtime, application, graph-query, and the root's
   `config`, `runtime_ports`, `project_store_runtime`, `test_support`.
2. **Cycle** if `tracedecay-mcp` took daemon-service or agent-hosts:
   `tracedecay-mcp → tracedecay-daemon-service → tracedecay-agent-hosts → tracedecay-mcp`.

Break it first: replace the two `tracedecay_mcp::` calls in `tracedecay-agent-hosts/src/ports/mcp_tools.rs`
with a tool-name list passed in by the composition root (or sourced from `tracedecay-tool-catalog`). That
removes agent-hosts → mcp and lets `tracedecay-mcp` depend on agent-hosts and host-admission. Then give
`TraceDecay` a home below both consumers: new crate `tracedecay-project` = root `project/` + `config.rs`
+ `project_store_runtime.rs` + `runtime_ports.rs` (test fixtures behind `test-helpers`), depending on
agent-hosts for the scout owner. After that every (c) row below is movable.

### Root `src/mcp/` — 42,256 lines, 12,390 in test files

(a) duplicate of an extracted owner → delete: none remain (this lane removed `effective_path`, four
`unknown_tool_error` copies, and the forwarding tables). Checked by name overlap and ripwire `--clones`.

(b) movable as-is (target crate):

| module | lines | target | note |
| --- | ---: | --- | --- |
| `tools/binding.rs` + `binding/` | 1,630 | tracedecay-mcp | catalog: contracts + tool-catalog; one `resolve_catalog_tool_binding` call to relocate |
| `tools/handlers/dashboard_lcm.rs` | 1,067 | tracedecay-mcp | needs `tracedecay-lcm`, `tracedecay-session-runtime` edges |
| `tools/catalog_discovery.rs` | 673 | tracedecay-mcp | two daemon-service calls to lift |
| `server/routing.rs` + `serve.rs` | 612+59 | tracedecay-mcp | root-URI decoding + scope routing |
| `tools/handlers/dashboard_delivery.rs` | 564 | tracedecay-daemon-service | already daemon-service shaped |
| `tool_analytics.rs` | 563 | tracedecay-mcp | one agent-hosts type to check |
| `scope.rs` | 443 | tracedecay-mcp | memory/storage scope selection |
| `server/session_refresh.rs` | 393 | tracedecay-mcp | |
| `server/project_host_admission_replay.rs` | 312 | tracedecay-daemon-service | host-admission + sessions |
| `tools/dispatch.rs` | 255 | tracedecay-daemon-service | daemon-protocol + daemon-service |
| `tools/handlers/hook_runtime/envelope.rs` | 222 | tracedecay-hooks | identity minting policy; swap `config_error` for a hooks error |
| `tools/handlers/session_authorities.rs` | 99 | tracedecay-mcp | |
| `tools/handlers/dashboard_git_correlation.rs`, `server/status_resource.rs` | 99 | tracedecay-mcp | |

(c) composition-root wiring, stays until `TraceDecay` moves (then → tracedecay-mcp, or a new
`tracedecay-mcp-daemon` if daemon-service must stay below mcp): `server.rs` (1,546),
`server/{requests,connection,construction,ledger,rmcp,lifecycle,hook_dispatch,hook_writes}.rs` (6,569),
`tools/handlers/mod.rs` (`ToolCallRegistryOptions`, 50 daemon-owned authorities), `dispatch_groups.rs`
(admission funnel, `McpToolContext` binding, retained protocol), `handlers/{edit,workflow,admin_cli,
admin_project,analytics,skills,automation_runs,dashboard,application_surface,retained_catalog,
tool_call_support,dispatch_controls}.rs`, `handlers/hook_runtime/` minus `envelope.rs`,
`handlers/info/` (admin sync), `project_route.rs` (975).

(d) blocked by dependency direction: every (c) module that names `TraceDecay` or `tracedecay_daemon_service`
(`dispatch_groups.rs` 27/3, `dashboard.rs` 4/11, `mod.rs` 7/4, `construction.rs` 8/2, `edit.rs` 17,
`hook_runtime/` 38/5) — the cycle above.

### Root `src/daemon/` — 97,528 lines, 46,370 in test files (51,158 production)

(a) duplicate → delete: none at module level. Name overlap with daemon-service is zero for all 23
production modules checked; ripwire `--clones` over both trees (481 groups) finds 27 cross-tree type-3
near-misses, all ≤160-token observability/receipt helpers (`observe_remote_deletion_receipt` ~
`record_query_admission_refusal`, `admission_state` ~ `workflow_topology_problem`) — a shared receipt
helper in daemon-service, not deletions.

(b) movable as-is → tracedecay-daemon-service (no `TraceDecay`, no `crate::mcp`, no root `config`):
`shutdown_orchestration.rs` (1,362), `shutdown_coordination.rs` (588), `shutdown_watchdog.rs` (338),
`invocation_executor.rs` (800; daemon-protocol), `invocation_dispatch.rs` (931), `context_scout_lifecycle/`
(566 + tests), `remote_deletion.rs` (341), `database_owner_registry.rs` (370), `lsp_sessions.rs` (208),
`github_credential_lifecycle.rs` (237), `bootstrap_route.rs` (175), `automation_observation.rs` (66),
`project_delivery_mount.rs` (46), `core_doctor_schema.rs` (22), `http_application_router.rs` (125) —
≈6.2k lines. `core_lifecycle.rs` (399) and `wire_io.rs` (448) go to daemon-protocol/daemon-control once
their two `crate::mcp` uses are typed.

(c) composition wiring (stays until `TraceDecay` moves): `branch_admin/` (5,571; TraceDecay 12, mcp 11),
`scheduler/` (5,016; TraceDecay 44), `project_open_owners/` (4,441), `project_composition/` (3,458;
`crate::mcp` 64 — it constructs the MCP server), `connection_serving.rs` (2,211), `maintenance.rs`
(2,040), `engine.rs` (1,480), `pr_autotrack/` (1,379), `project_open_admission/` (1,294),
`core_{doctor,proxy,admission,client,logging,hooks,handshake}.rs`, `bootstrap.rs`, `dashboard_automation/`,
`http_application.rs`, `retained_owner/`, `projectless.rs`, `project_open_{orchestration,handshake}.rs`,
`project_server_lifecycle.rs`, `project_routing.rs`, `branch_add.rs`, `hook_v2_replay_consumer.rs`,
`graph_resolution.rs`, `automation_effect/`, `adoption_observation.rs`, `store_maintenance/`,
`invocation_state.rs` (1,662; root `config` ×2), `doctor_kernel/` (1,341; root `config` ×2).

(d) blocked by dependency direction: `project_composition/` and `connection_serving.rs` build the root
`McpServer`, so they can only move after P0-1; everything naming `TraceDecay` waits on `tracedecay-project`;
`invocation_state.rs`, `doctor_kernel/`, `core_logging.rs`, `bootstrap.rs` wait on root `config.rs`
(786 lines: `PinnedUserDataDir`, `user_data_dir`, `DaemonRuntimeConfiguration`) moving with it.

### Order

1. `tracedecay-agent-hosts/src/ports/mcp_tools.rs`: drop the two `tracedecay_mcp::` calls (cycle break).
2. New `tracedecay-project` (root `project/`, `config.rs`, `project_store_runtime.rs`, `runtime_ports.rs`).
3. `src/mcp` (b) rows, one commit per target crate; then (c) into tracedecay-mcp behind `McpToolContext`.
4. `src/daemon` (b) rows into daemon-service; then (c) as `tracedecay-daemon-service::composition`.
5. Delete `src/mcp/` and `src/daemon/`; the root keeps `lib.rs` re-exports, `product_runtime`, `doctor`,
   `dashboard`, `version`, and the `test_support` fixture surface.

### Test placement facts (item 3 probe)

`crates/tracedecay/tests/zz_relocation_probe.rs` (temporary, deleted) mounted each of the 73 `#[cfg(test)]`
module roots under a `pub use tracedecay::<parent>::*` shim and compiled with `test-helpers,test-transport`:
507 errors — private modules (`daemon::{automation_effect,context_scout_lifecycle,core_doctor,doctor_kernel,
invocation_executor,production_harness,project_composition,…}` are `pub(crate)`/private), private fields
(`ToolCallRegistryOptions`, server internals), private fns, and `cfg(test)`-only fixtures
(`TraceDecay::init_test_fixture_with_registered_runtime`). A test that reaches those must stay in-src; the
sanctioned fixture surface is `test_support` behind `test-helpers`.
