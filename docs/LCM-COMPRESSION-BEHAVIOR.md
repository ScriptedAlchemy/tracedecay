# LCM compression behavior map

This note characterizes `src/sessions/lcm/compression.rs` as of the current seam-extraction workstream. It is intentionally implementation-facing: follow-on refactors should preserve the externally visible responses, replay ordering, and database invariants listed here before splitting `compress_in_transaction` into smaller seams.

## Entry points and transaction boundaries

- `preflight(...)` normalizes the effective assembly cap, filters ignored/stateless sessions, ingests active messages in its own immediate transaction, checks compression-boundary cooldown, loads lifecycle state + raw messages, then returns a compression decision. It never writes summary nodes.
- `compress(...)` normalizes the same cap and short-circuits ignored/stateless sessions before opening a transaction.
- Once inside `BEGIN IMMEDIATE`, `compress(...)` ingests active messages first. If the summarizer mode is `Noop`, it commits only those ingest writes and returns `reason = "noop_summarizer"` with no summary nodes.
- All non-`Noop` compression decisions are made by `compress_in_transaction(...)`; its summary-node inserts, lifecycle frontier/debt writes, replay assembly, and condensation writes commit or roll back as one unit.
- `record_session_boundary(...)` uses a transaction only for carry-over. Boundary-skip cooldown writes are a single lifecycle upsert outside `compress_in_transaction`.

## Preflight/token-budget decisions

Effective caps:

- `max_assembly_tokens` is reduced by `context_length - reserve_tokens_floor` when both are positive; if the reserve leaves no positive headroom, the reserve-derived cap is ignored instead of clamping to zero.
- Forced overflow pressure is `current_tokens >= effective max_assembly_tokens`.
- Overflow recovery further tightens replay assembly by subtracting the host-observed prompt overhead (`current_tokens - estimated active message tokens`) from the assembly cap, floored to 1.

Preflight decision order after ingest:

1. ignored/stateless sessions return `should_compress = false` and replay the incoming active messages without persistence.
2. active compression-boundary cooldown returns `should_compress = false`, `reason = "compression_boundary_cooldown"`; ingest remains lossless and committed.
3. forced overflow returns `reason = "forced_overflow_pressure"`.
4. outstanding raw-backlog maintenance debt returns `reason = "maintenance_debt_ready"`.
5. threshold pressure only compresses when the backlog outside the fresh tail has enough total tokens for the effective leaf chunk; `max_source_messages` must not reduce eligibility.
6. active ingest protection can override all non-cooldown decisions with `reason = "ingest_protection_changed_replay"`.

## `compress_in_transaction` decision paths

`compress_in_transaction` starts by loading the lifecycle frontier and building a compression window:

- `pinned_anchors`: every historical `system` or `developer` message before the fresh tail, replayed outside summaries.
- `backlog`: unsummarized non-policy-anchor messages before the fresh tail.
- `fresh_tail`: newest unsummarized messages, defaulting to `LCM_DEFAULT_FRESH_TAIL_COUNT` unless overridden.

Decision paths, in order:

1. `expected_current_frontier_store_id` mismatch: no writes, `reason = "frontier_changed"`, replay only pinned anchors + fresh tail from the current frontier.
2. Empty backlog + forced overflow: assemble overflow-recovery replay from anchors/summaries/tail; return `"overflow_recovery_no_backlog"`, `"irreducible_overflow_no_backlog"`, or over-budget best-effort state.
3. Empty backlog + ready condensation: insert a parent summary node from eligible child summaries and return `"condensed_summary_nodes"` or fallback variant.
4. Empty backlog + no condensation: replay persisted uncondensed summaries plus anchors/tail; return `"no_backlog_to_compress"`.
5. Non-empty backlog below leaf threshold: unless forced overflow or maintenance debt applies, return `"backlog_below_leaf_chunk_threshold"` with no writes and replay the deferred backlog + fresh tail.
6. `HermesAuxiliary` summarizer: return `status = "needs_summary"`, `reason = "hermes_auxiliary_not_available"`, source range/messages/extraction request, and fresh-tail replay. It does not insert a summary.
7. `Fake` / `Provided` summarizer: insert one or more leaf summaries, update lifecycle frontier/debt, then assemble replay from anchors, uncondensed summaries, deferred backlog, and fresh tail.

## Summary policy and summarizer modes

- `Fake` and `Provided` modes write summary nodes. `Provided.route` can carry a JSON envelope; `summary_route` and `pre_compaction_extraction` are persisted in summary metadata.
- `HermesAuxiliary` is request mode, not write mode: the response includes the exact `LcmSummaryRequest` contract follow-on code must feed back through `Provided`.
- `Noop` is handled by `compress(...)` before `compress_in_transaction`; it persists active ingest only.
- If a supplied summary does not compress enough and the source has at least `MIN_SUMMARY_RESCUE_SOURCE_TOKENS`, deterministic fallback summary text is substituted and attempt state reports `fallback_used = true` / `retry_status = "fallback_summary"`.
- Forced overflow may run up to `MAX_FORCED_CATCHUP_PASSES`; non-forced compression inserts at most one leaf node per call.

## Boundary/cooldown handling

- `record_session_boundary` only handles `boundary_reason = "compression"`, non-empty old session ids, and actual session rotation.
- If `bound_session_id == old_session_id`, carry-over finalizes the old session and reassigns raw messages, summary nodes, payloads, lifecycle frontier, and maintenance debt to the new session. The target session must have no raw rows; rejection rolls the whole carry-over back.
- If the bound session does not match, `boundary_skip_at` is written and preflight suppresses all compression triggers for 60 seconds with `reason = "compression_boundary_cooldown"`. Ingest still happens during the cooldown.

## Replay and condensation policy

- Replay order is by source/store position, with summary blocks sorted before raw messages at the same source position.
- Policy anchors (`system`, `developer`) are never summarized when they appear before the fresh tail; historical `tool` messages are not anchors and can be summarized.
- Budgeted replay keeps leading anchors, selects newest contiguous raw tail under the cap, may evict over-budget assistant/tool turns, and stops at over-budget prompt-bearing user/system turns.
- The latest user objective can be reinjected as a preserved system scaffold when it would otherwise be lost from a tool-heavy tail.
- Uncondensed summaries are loaded into replay even when there is no new raw backlog.
- Condensation chooses unparented summaries from one eligible depth only. Default `incremental_max_depth = 1` means only depth-0 leaves can condense; higher depths require an explicit `incremental_max_depth`.

## Regression fixtures and tests

Primary reusable fixture:

- `tests/session_lcm_compression_test.rs::compress_in_transaction_baseline_decision_fixture_preserves_contract` uses the `CompressBaselineCase` fixture to cover the main seam contracts in one place: stale frontier no-op, below-leaf-threshold guard, auxiliary summary request contract, and fake-summary DB writes/frontier/replay.

Existing focused tests also cover:

- preflight cap derivation, threshold eligibility, maintenance debt, ingest-protection replay changes, and cooldown behavior.
- boundary skip, cooldown expiry, matching carry-over, payload/debt reassignment, and rollback on failed carry-over.
- exact leaf threshold, zero leaf/fresh-tail knobs, dynamic chunking, forced overflow catch-up/budget states, no-backlog overflow recovery, and preserved objective scaffolds.
- condensation readiness, ordering, default/non-default max depth, and replay after condensation.

Follow-on seam extractions should run the baseline fixture first, then the full `session_lcm_compression_test` target before changing behavior intentionally.
