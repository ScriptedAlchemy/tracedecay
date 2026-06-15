# LCM Compression Decomposition — Triage Report

**Task:** `t_bc5b8cbc` (Triage: decompose LCM compression state machine)
**Scope:** `src/sessions/lcm/compression.rs` and the seams split out of it.
**Status:** Decomposition complete and verified. All five identified seams are extracted, wired, and covered by LCM regression tests.

## Problem

`src/sessions/lcm/compression.rs` was a ~2,376-line module whose
`compress_in_transaction` orchestrator interleaved five distinct concerns:

- preflight / token-budget decisioning,
- summarizer mode selection and invocation,
- boundary / cooldown state machine,
- transaction writes (summary node + lifecycle + maintenance debt + frontier),
- condensation policy (multi-depth summary merging).

This made the orchestrator a complexity hotspot and made each concern hard to
test in isolation.

## Outcome

`compress_in_transaction` (`compression.rs:513`) is now a linear decision/response
pipeline. It loads a `CompressionTransactionContext`, then short-circuits through a
sequence of focused response helpers (`frontier_changed_response`,
`no_backlog_compression_response`, `backlog_below_threshold_response`,
`auxiliary_summary_response`) before falling through to
`persist_and_replay_backlog_compression` for the write path. Every decision and
every write is delegated to a dedicated seam.

Two new modules carry the extracted logic:

- `src/sessions/lcm/compression_decision.rs` (594 lines) — pure decision functions.
- `src/sessions/lcm/summarizer.rs` (256 lines) — summarizer adapter.

`compression.rs` is now 2,276 lines (down ~100) but, more importantly, its
orchestration region is shallow and side-effect-free except at the single
transaction-writer seam.

## The five seams

Line numbers are as verified at triage time.

### 1. Preflight / token-budget decisioning
**Module:** `compression_decision.rs`
**Entry points:** `preflight_decision`, `compression_plan`,
`effective_assembly_token_cap`, `overflow_recovery_assembly_cap`,
`effective_leaf_chunk_tokens`, `bounded_leaf_chunk_len`, `has_eligible_backlog`.
**Call sites in `compression.rs`:** context preparation (`:556`, `:561`),
preflight (`:372`), assembly caps (`:318`, `:398`).
**Regression tests:** `compression_decision_seam_preserves_token_budget_contract`,
`compress_in_transaction_baseline_decision_fixture_preserves_contract`,
`compress_forces_overflow_recovery_with_reserve_derived_cap`,
`compress_noops_for_sub_threshold_backlog_in_threshold_mode`,
`compress_noop_guard_fires_before_auxiliary_summary_request`,
`compress_proceeds_at_exact_leaf_chunk_threshold`.
**Unit tests:** `compression_decision::tests` (token-budget + leaf-chunk helpers).

### 2. Summarizer adapters
**Module:** `summarizer.rs` — `CompressionSummarizerAdapter` enum
(`Noop`, `HermesAuxiliary`, `Persisted(PersistedSummaryInvocation)`) with
`from_mode`, `is_noop`, `persisted_summary_invocation`, `summary_request`.
Hermes-auxiliary `LcmSummaryRequest` construction and `Provided` route-envelope
parsing both live behind this adapter.
**Call sites in `compression.rs`:** `::from_mode` (`:446`), threaded through
`compress_in_transaction` and every response helper.
**Regression tests:** `noop_summarizer_ingests_without_summary_nodes`,
`fake_summarizer_compacts_backlog_and_preserves_fresh_tail`,
`hermes_auxiliary_request_mode_returns_summary_contract`,
`provided_summarizer_advances_frontier_consistently`,
`provided_route_envelope_persists_extraction_metadata`.
**Unit tests:** `summarizer::tests` (all four modes).

### 3. Boundary / cooldown state machine
**Module:** `compression_decision.rs`
**Entry points:** `boundary_transition_decision` (returns `Ignore` / `CarryOver` /
`StartCooldown`), `cooldown_active`.
**Call sites in `compression.rs`:** session-boundary handling (`:133`, `:137-143`),
preflight cooldown gate (`:277`).
**Regression tests:** `boundary_skip_starts_preflight_compression_cooldown`,
`boundary_cooldown_blocks_replay_diff_compression`,
`boundary_cooldown_expires_after_sixty_seconds`,
`boundary_continuation_with_matching_bound_session_records_no_cooldown`.
**Unit tests:** `compression_decision::tests` (boundary transitions + 60s cooldown).

### 4. Transaction writer
**Module:** `compression.rs` — `persist_compression_transaction_writes` (`:857`),
called from `persist_and_replay_backlog_compression` (`:779`). Centralizes summary
insert + lifecycle upsert + maintenance-debt replacement + frontier reload inside
one transaction; the caller only assembles replay and builds the response.
**Regression tests:** `compress_frontier_changed_preserves_existing_transaction_state`
(no-write frontier-change path leaves existing lifecycle/debt untouched),
`compress_persists_summary_frontier_and_remaining_backlog_debt`,
`compress_rolls_back_summary_and_lifecycle_when_debt_write_fails` (rollback on
injected debt-write failure).

### 5. Condensation policy
**Module:** `compression_decision.rs`
**Entry points:** `condensation_policy_decision` (skip-on-backlog /
skip-on-auxiliary / `QueryCandidates(CondensationPolicy)`),
`condensation_candidate_decision`, `incremental_max_depth_limit`.
**Call sites in `compression.rs`:** `condense_summary_nodes_if_ready` (`:1628`,
`:1651`).
**Regression tests:** `condensation_creates_higher_depth_summary_from_existing_leaf_nodes`,
`condensation_waits_for_one_depth_with_enough_unparented_nodes`,
`condensation_orders_same_depth_candidates_by_source_time`,
`condensation_respects_default_incremental_max_depth`,
`condensation_honors_non_default_incremental_max_depth`.
**Unit tests:** `compression_decision::tests` (policy defaults, overrides, negative
depth, backlog/auxiliary skips).

## Mapping to parent cards

| Seam | Implementing parent | Reviewing parent |
|---|---|---|
| Behavior map + baseline fixture | `t_2b3133da` | `t_b6105753` (APPROVED) |
| Preflight / token-budget + condensation policy | `t_9486a895` | `t_c2e1d215` (APPROVED) |
| Summarizer adapter | `t_ccbbc3bd` | (covered by orchestration review) |
| Transaction writer | `t_0afc6d9b` | `t_4381f0bd` (APPROVED) |
| Boundary / cooldown | `t_9486a895` + `t_c2e1d215` | `t_c2e1d215` (APPROVED) |

## Verification (this triage run)

- All five seams confirmed wired into `compression.rs` (delegation call sites
  grepped and read, not just defined in isolation).
- `rustfmt --edition 2021 --check` clean on `compression.rs`,
  `compression_decision.rs`, `summarizer.rs`, `mod.rs`,
  `tests/session_lcm_compression_test.rs`.
- Regression target `session_lcm_compression_test` plus lib unit tests
  `compression_decision::tests` and `summarizer::tests` re-run in an isolated
  cargo target dir (`.tracedecay/target/t_bc5b8cbc`) per the cargo-contention
  policy. Results: `compression_decision::tests` **9/9**, `summarizer::tests`
  **4/4**, `session_lcm_compression_test` **70/70** (0 failed, 0 ignored).
  Isolated target dir reclaimed after the run.

## Non-blocking follow-ups

- **File permissions:** the two new files (`compression_decision.rs`,
  `summarizer.rs`) are mode `0600` on disk while the repo norm is `0664`.
  Git tracks both as blob `100644`, so this is a local-filesystem artifact only
  and normalizes on clone — but `chmod 0664` on the two files would keep the
  working tree consistent. (Not done here; outside triage scope.)
- `summarizer` is declared `mod summarizer;` (private) in `lcm/mod.rs`, while
  `compression_decision` is `pub mod`. This matches current visibility needs
  (the adapter is `pub(crate)`); revisit only if a future caller outside the
  `lcm` module needs it.
