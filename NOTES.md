# Lane notes — fable/test-prune

## Lane status 2026-09-12

Branch `fable/test-prune` (40 prune/style commits on top of the
`fable/test-slop` floor 13452d3ea; `origin/fable/test-slop` is that floor
itself, with no prune commits, and no prune commit had landed on the
integration branch) was merged forward onto
`origin/codex/tracedecay-total-redesign-plan-reopened` in two merge
commits: 03427d2b48 (tip 84f29ec656, 117 conflicted files / 157 hunks) and
6fd74ec325 (tip 23dde7695d, 11 files / 14 hunks). Not pushed, no PR.

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

### Undone

- Integration suites (`--tests`) were type-checked only; their pruned
  counts are not measured.
- The integration tip keeps moving; re-merge before pushing.
