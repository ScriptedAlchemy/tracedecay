# Trust & Temporal-Decay Semantics — Current Behavior

This document describes how a project-memory fact's persisted trust score
changes, and how temporal decay affects search ranking, against the current
`crates/tracedecay-runtime-core` implementation.

An earlier version of this document was written against a single-crate
`src/memory/{retrieval,store,trust}.rs` layout, a `src/db/migrations.rs`
schema, and MCP/dashboard modules under `src/mcp` / `src/dashboard` — none of
which exist anymore. The relevant logic is now split across
`crates/tracedecay-runtime-core/src/memory/trust.rs` (trust constants and
bucketing), `crates/tracedecay-runtime-core/src/store/memory/scoring.rs`
(the ranking-time decay formula), `crates/tracedecay-runtime-core/src/store/
memory/crud/{commands.rs,feedback.rs,lineage.rs}` (the writers of trust),
and the `memory_v2_*` tables in
`crates/tracedecay-runtime-core/src/db/memory_v2/schema/`. The policy
conclusion this document previously reached still holds, and is restated
below against the current code.

## TL;DR

- The persisted `trust_score` (on `memory_v2_current_facts`) **never decays
  on its own**. It only changes through an explicit writer: adding a fact
  with an explicit trust value, updating a fact's trust via an edit, or
  applying helpful/unhelpful feedback. There is no scheduler, no background
  sweep, and no retrieval-time write that ages the stored value.
- Temporal decay of *ranking* is applied **only at retrieval time**, computed
  dynamically from a fact's `updated_at` by `project_memory_temporal_decay`
  in `scoring.rs` — a 365-day half-life, floored at 0.10. It is **never
  persisted** back to the fact.
- Trust changes are event-sourced: every mutation to `trust_score` is
  recorded as a `FactLineageEventKindV1::TrustChanged` event in the fact's
  lineage, and `memory_v2_current_facts.trust_score` is a materialized
  projection replayed from that lineage, not a value written imperatively in
  place.
- Feedback history is append-only and readable, not write-only: every
  helpful/unhelpful action is recorded in `memory_v2_feedback_history` (a
  table whose schema forbids `UPDATE`/`DELETE` outside redaction), and is
  exposed back through the fact-store MCP surface (`fact_store_get` returns
  trust history alongside the current score) and the dashboard API.

---

## 1. The data model

The persisted trust value and its supporting counters live on
`memory_v2_current_facts` (schema in
`crates/tracedecay-runtime-core/src/db/memory_v2/schema/baseline.rs`), keyed
by `(fact_id, owner_kind, project_id)`:

| column | meaning | who writes it |
|---|---|---|
| `trust_score` | persisted per-fact trust, `CHECK`-constrained to `[0, 1]` (or `NULL`) | replayed from lineage events on every commit (see §2) |
| `updated_at` | last mutation time — the input to the ranking decay factor | every commit that touches this fact (create, edit, feedback, curation) |
| `retrieval_count` / `access_count` | scan count vs. returned-count | `project_memory_update_retrieval_projection_tx`, on every search/probe/reason/related hit |
| `last_retrieved_at` / `last_recalled_at` | last scan / last time a search actually returned the fact | the same retrieval-projection update |
| `helpful_count` / `unhelpful_count` / `last_feedback_at` | feedback tallies | `project_memory_update_feedback_projection_tx`, on every feedback event |

`trust_score` is not written directly by an `UPDATE trust_score = …`
statement in the write path. Instead, every fact mutation appends one or more
`FactLineageEventKindV1` events (`AssertionRecorded`, `TrustChanged`,
`Curated`, `PayloadAccessChanged`) to the fact's lineage, and
`publish_current_projection` (in `store/memory/crud/lineage.rs`) replays the
whole lineage into an in-memory `Projection`, then upserts the resulting
`trust_score`/`updated_at`/etc. into `memory_v2_current_facts` in one
`INSERT … ON CONFLICT DO UPDATE`. Only a `TrustChanged` event can move
`trust_score`; the event carries `previous`/`current` and is rejected by
construction if they're equal.

There is still no separate "confidence" concept for memory facts distinct
from trust; trust is represented by the shared `Confidence` domain type
(`[0, 1]`), the same type used for the query filter's `min_trust`.

## 2. What can change `trust_score`

The complete, current set of writers:

1. **Create** (`fact_store_add`) — a fact is always created at
   `DEFAULT_TRUST = 0.5` first; if the caller supplied a different explicit
   trust value, an immediate `TrustChanged { previous: 0.5, current: <requested> }`
   event is appended in the same commit (`store/memory/crud/project.rs`).
2. **Update** (`fact_store_update`) — the update patch
   (`ProjectMemoryFactUpdatePatchV1`) can carry an absolute `trust: Option<Confidence>`
   alongside content/category/tags/entities/metadata; when present, it emits
   a `TrustChanged` event with the new absolute value (not a delta).
3. **Feedback** (`fact_feedback`) — `project_memory_feedback_delta` maps
   `Helpful → +0.05` / `Unhelpful → −0.10`; the new trust is
   `(old_trust + delta).clamp(0.0, 1.0)`, recorded as a `TrustChanged` event,
   and also bumps `helpful_count`/`unhelpful_count`/`last_feedback_at` via
   `project_memory_update_feedback_projection_tx` and appends a row to
   `memory_v2_feedback_history`. These deltas match the constants in
   `crates/tracedecay-runtime-core/src/memory/trust.rs`
   (`HELPFUL_DELTA = 0.05`, `UNHELPFUL_DELTA = -0.10`).

That is the complete set — there is no separate "manual trust bump" tool with
its own delta argument anymore; an absolute trust set now goes through the
same update patch as any other field edit. None of the above is time-driven;
none runs on a schedule.

## 3. Where temporal decay is actually applied: retrieval, dynamically

The only live decay is in recall ranking, in
`crates/tracedecay-runtime-core/src/store/memory/scoring.rs`:

```rust
pub(super) fn project_memory_temporal_decay(updated_at: UtcMicros, now: UtcMicros) -> f64 {
    if updated_at.0 <= 0 {
        return 1.0;
    }
    let age_micros = now.0.saturating_sub(updated_at.0).max(0) as f64;
    let age_days = age_micros / 86_400_000_000.0;
    0.5_f64.powf(age_days / 365.0).clamp(0.10, 1.0)
}
```

Properties (unchanged from the pre-rewrite formula):

- **When:** computed per candidate, during ranking in
  `project_memory_search_scores` (`store/memory/search.rs`). Pure function of
  `updated_at` and the current time; no DB write.
- **Keyed on `updated_at`** (last *write*), not on last access. Reads only
  bump `last_retrieved_at` / `last_recalled_at`, so a frequently-searched but
  never-edited fact still decays in ranking exactly as fast as an
  never-searched one.
- **Bounds:** a fact loses half its ranking weight every 365 days since its
  last write, bottoming out at `0.10×`. Age alone never fully excludes a
  fact from ranking; the `DEFAULT_MIN_TRUST = 0.3` recall floor
  (`memory/trust.rs`) is a separate filter applied to the *stored*
  `trust_score`, unrelated to the decay factor.
- **Visible?** Yes, per result: every hit's `why` string includes
  `temporal_decay=0.xxx` alongside the other raw scoring components (see
  `RETRIEVAL-QUALITY-EVAL.md` §1).
- **Persistent?** No. Nothing writes the decayed value back; `trust_score` on
  disk is always the raw, never-aged value.

There is only one decay function in the current codebase — no second,
persisted-trust-aging routine exists alongside it.

## 4. Semantic model (condensed)

```text
                   ┌───────────── memory_v2_current_facts ─────────────┐
 add (explicit) ──►│ trust_score    (Confidence [0,1], never decays)   │
 update (absolute) │ updated_at     (advanced on every commit only)    │──► search/probe/reason
 feedback ±0.05/.10│ retrieval/access/helpful/unhelpful counters       │        ranking:
                   └────────────────────────────────────────────────────┘   relevance × trust_score
                                                                              × temporal_decay(updated_at)
   (no scheduler ages trust_score)                    temporal_decay = 0.5^(age_days/365), floor 0.10
```

Answers to the recurring framing question ("automatic / maintenance /
retrieval / not at all"):

- **Automatic (scheduler):** no. Nothing periodically touches `trust_score`.
- **Explicit maintenance pass:** the memory-curation review/apply flow
  (`store/memory/curation/{review.rs,apply.rs}`) can record `Curated` lineage
  events (contradiction/merge dispositions) but does not itself age or reset
  trust; only add/update/feedback move `trust_score`.
- **At retrieval time:** yes, ranking only — `project_memory_temporal_decay`,
  dynamic, never persisted.
- **Persisted trust decay:** no. No writer in the current codebase ages a
  fact's stored `trust_score` based on elapsed time.

## 5. Visibility / explainability

- **Retrieval-time decay** is visible per result via the `why` field on every
  search hit.
- **Feedback-driven trust changes** are recorded append-only in
  `memory_v2_feedback_history` (old trust → new trust, delta, action, source
  label, note, timestamp). The table's schema forbids `DELETE` and restricts
  `UPDATE` to detail redaction only (`memory_v2_feedback_history_no_delete`,
  `memory_v2_feedback_history_only_redaction` triggers in
  `db/memory_v2/schema/final_authority.rs`), so the audit trail cannot be
  rewritten or dropped through normal writes.
- **That history is readable, not write-only.** It is exposed through the
  fact-store query surface (the `fact_store_get` fact-store MCP tool returns
  trust history alongside the current score) and through the dashboard API
  (`crates/tracedecay-dashboard-api/src/memory_api.rs`, documented there as
  `GET /api/plugins/holographic/fact/{fact_id}` for full fact detail and
  `GET /api/plugins/holographic/fact/{fact_id}/trust-history` for the
  append-only history specifically).
