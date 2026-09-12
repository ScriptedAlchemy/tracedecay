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
