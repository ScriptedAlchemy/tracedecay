# Trust & Temporal-Decay Semantics — Current Behavior

Status: factual audit of the `master` branch as of this writing. Every claim is
backed by a `file:line` reference so it can be re-verified.

## TL;DR

- The stored `memory_facts.trust_score` **never decays**. It changes only via
  explicit feedback, an explicit trust set on add/edit, or a manual MCP trust
  bump. There is no scheduler, no background sweep, no maintenance pass, and no
  retrieval-time write that ages trust.
- Temporal decay of *ranking* is applied **only at retrieval time**, computed
  dynamically from `updated_at` by `retrieval.rs::temporal_decay_factor`
  (365-day half-life, floored at 0.10). It is **never persisted**.
- There is now **one** decay function, not two. The former
  `trust.rs::temporal_decay(current_trust, age_days)` dead routine (persisted-score aging
  routine, 180-day exponential pull toward 0.5) was **removed** in favor of
  dynamic-only decay (policy decision in §8 — Option 1, adopted). Its unit test
  (`tests/memory_test.rs:262`) was removed with it. Only
  `retrieval.rs::temporal_decay_factor` remains.
- Operators **can** now see *why* a trust value is what it is. The
  `memory_feedback_events` audit table, which used to be write-only, is now
  readable via `TraceDecay::fact_trust_history(fact_id)`, the MCP fact `get`
  `trust_history` field, and
  `GET /api/plugins/holographic/fact/{fact_id}/trust-history` (landed via Q6;
  see §5). (Historical: at audit time the table was write-only — only `store.rs`
  inserted; no SELECT/read path existed.)
- The storage audit (`docs/MEMORY-STORAGE-GROWTH-AUDIT.md` §7) was corrected:
  it now states that *only* `retrieval.rs::temporal_decay_factor` affects
  ranking, that the stored `trust_score` never decays, and that
  `trust.rs::temporal_decay` has been removed.

---

## 1. The data model (`memory_facts`)

Relevant columns (from `src/db/migrations.rs:1050` / live `.tracedecay/tracedecay.db`):

| column | meaning | who writes it |
|---|---|---|
| `trust_score` REAL default 0.5 | the *persisted* per-fact trust, clamped to [0,1] | feedback, add/edit trust set, manual MCP bump |
| `created_at` | write origin time | INSERT |
| `updated_at` | **last mutation time** — the input to the ranking decay factor | create, edit, feedback (all writes); **not** reads |
| `last_retrieved_at` | last probe/list scan | `record_fact_retrievals` (`store.rs:772`) |
| `last_recalled_at` | last time a recall search *returned* the fact | `record_fact_recalls` (`store.rs:798`) |
| `retrieval_count` / `access_count` | scan count vs. returned-count | the two record_* helpers above |
| `helpful_count` / `unhelpful_count` / `last_feedback_at` | feedback tallies | `record_feedback_event` (`store.rs:870`) |

There is **no "confidence" concept for memory facts**. The word `confidence` in
this codebase belongs exclusively to the *code-graph reference resolver*
(`src/resolution/resolver.rs`, `ResolvedReference.confidence` in
`src/types.rs:612` — values 0.65–0.95). It has no relationship to memory trust.

## 2. What can change `trust_score`

Exhaustive list of writers (verified by grepping every `trust_score =` / clamp):

1. **Create** — `store.rs:175`: `clamp_trust(request.trust.unwrap_or(default_trust))`.
2. **Edit** — `store.rs:425,439`: `request.trust.map_or(existing, clamp_trust)`; re-written on every `update_fact`.
3. **Feedback** — `store.rs:860–888`: `apply_feedback(old, action)` → `+0.05` (helpful) / `−0.10` (unhelpful), clamped (`trust.rs:5,6,16`). Also bumps `updated_at`, `last_feedback_at`, and the helpful/unhelpful tallies, and appends a `memory_feedback_events` row.
4. **Manual MCP trust bump** — `src/mcp/tools/handlers/memory.rs:148`: `(existing.trust_score + delta).clamp(0,1)` computed from a `trust_delta` arg, then written via the edit path.

That is the **complete** set. None of these is time-driven; none runs on a
schedule.

## 3. Where temporal decay is actually applied: retrieval, dynamically

The only live decay is in recall ranking (`src/memory/retrieval.rs`):

```text
score = relevance * trust_score * temporal_decay_factor(updated_at)   # retrieval.rs:709
        \__ fts*0.40 + jaccard*0.30 + holographic*0.30 __/  (weights: retrieval.rs:705)
```

```rust
// retrieval.rs:712 — THE ONLY DECAY THAT RUNS
fn temporal_decay_factor(updated_at: i64) -> f64 {
    if updated_at <= 0 { return 1.0; }                 // unknown age → no penalty
    let age_days = (now - updated_at).max(0) as f64 / 86_400.0;
    0.5_f64.powf(age_days / 365.0).clamp(0.10, 1.0)    // 365-day half-life, floor 0.10
}
```

Properties:
- **When:** computed per candidate, inside `recall()` scoring (`retrieval.rs:103–128`). Pure read; no DB write.
- **Keyed on `updated_at`** (last *write*), **not** on last access. Reads bump only `last_retrieved_at` / `last_recalled_at` (`store.rs:773,800`), so a frequently-read-but-never-edited fact *still* decays in ranking.
- **Bounds:** any fact loses half its ranking weight every 365 days since its last write, bottoming at 0.10×. Facts never fall fully out of ranking on age alone; the `DEFAULT_MIN_TRUST = 0.3` *recall floor* (`retrieval.rs:245,261,…`) is a separate filter applied to the stored `trust_score`, not to the decay factor.
- **Visible?** Yes, per-result: the `why` string emitted with every `FactSearchResult` includes `temporal_decay=0.xxx` (`retrieval.rs:124–126`). So a caller can see the dynamic factor that ranked a result.
- **Persistent?** No. Nothing writes the decayed value back. `trust_score` on disk is the raw, never-aged value.

## 4. The former aging function `trust.rs::temporal_decay` — removed

The persisted-score aging routine that previously lived in `trust.rs` has been
**deleted** (policy decision: dynamic-only decay — §8 Option 1, adopted). This
resolved the "two decay functions, two policies, two constants, one name"
confusion this audit originally flagged. `src/memory/trust.rs` now contains only
`clamp_trust`, `apply_feedback`, and the bucket/distribution helpers.

For the historical record: the removed `temporal_decay(current_trust, age_days)`
had **zero production callers** — only the now-removed `tests/memory_test.rs:262`
asserted it. Had it been called it would have rewritten the *persisted*
`trust_score` toward `DEFAULT_TRUST = 0.5` (180-day exponential time-constant) —
a fundamentally different policy from the live retrieval factor (365-day
half-life multiplier that never touches the stored value, and only ever
*reduces* ranking weight).

## 5. Visibility / explainability ("can an operator understand why trust changed?")

- **Retrieval-time decay:** visible per result via the `why` field (`retrieval.rs:124`). ✓
- **Feedback-driven trust changes:** an append-only audit row is written to
  `memory_feedback_events` (old_trust → new_trust, delta, action, source, note,
  timestamp) on every feedback event (`store.rs:890–905`). ✓ *recorded* —
- …and **now readable**. Q6 landed a read path across three surfaces:
  `TraceDecay::fact_trust_history(fact_id)` → ordered history
  (`src/tracedecay.rs:3625` / `src/memory/store.rs:858`); the MCP fact `get`
  action returns a `trust_history` field (`src/mcp/tools/handlers/memory.rs:260`);
  and `GET /api/plugins/holographic/fact/{fact_id}/trust-history`
  (`src/dashboard/memory_api.rs:205`, `src/dashboard/mod.rs:271`). ✓ *surfaced*
  (Historical: at audit time there was no read path — no dashboard endpoint, no
  MCP field, no store SELECT — so an operator had to run raw SQL.)
- **Explanation for the *current* trust value:** the MCP fact `get` action now
  returns `trust_history` alongside the scalar `trust_score`, so a caller can
  see how it got there. ✓

## 6. Semantic model (current behavior, condensed)

```
                     ┌──────────────── memory_facts ────────────────┐
  add/edit trust ──► │ trust_score   (clamped [0,1], NEVER decays)  │
  feedback ±0.05/±0.1│ updated_at    (advanced on EVERY write only) │ ──► recall() ──► rank
  manual MCP bump    │ last_*_at     (read-side counters)           │        rank = relevance
                     └──────────────────────────────────────────────┘               × trust_score
                                                                               × decay_factor(updated_at)
   (no scheduler)    trust.rs::temporal_decay = REMOVED (was dead code)   decay_factor = 0.5^(age_days/365) ∈ [0.10,1]
```

Answers to the task's framing ("automatic / maintenance / retrieval / not at all"):
- **Automatic (scheduler):** no.
- **Explicit maintenance pass:** no. `hygiene.rs` is secret/transient *content* detection only; it never touches `trust_score`.
- **At retrieval time:** **yes, ranking only** — `temporal_decay_factor`, dynamic, not persisted.
- **Persisted trust decay:** **no**. The function that would have done it (`trust.rs::temporal_decay`) has been removed; no scheduler ages persisted trust.

## 7. Problems this audit flagged (and their status)

1. **Stated vs. real behavior diverge — RESOLVED.** The module doc (`trust.rs:1`)
   used to advertise "aging" and the storage audit used to credit the former
   former `trust.rs::temporal_decay` with affecting scoring. Both are now corrected and
   the dead function is gone; a reader no longer believes persisted trust ages.
2. **Two functions, one name, incompatible policies — RESOLVED.** The 180-day
   pull-to-0.5 routine was deleted; only the 365-day multiplicative ranking
   penalty (`temporal_decay_factor`) remains, so intent is unambiguous.
3. **Unobservable trust history — RESOLVED (Q6 landed).** The audit data is now
   reachable through `fact_trust_history` (store + MCP `get` field + dashboard
   route), so "why did this fact's trust change?" is answerable without SQL.
4. **Untestable end-to-end — PARTIALLY RESOLVED.** With persisted aging gone by
   design, the remaining invariant to assert is "stored `trust_score` is
   unchanged by a `now` advance, while `temporal_decay_factor` moves ranking" —
   the integration test proposed in §8 item 6 still has no home.

## 8. Recommendations (API / scheduler / migration / docs)

Pick **one** of two coherent policies and make it the single source of truth; do
not leave both half-implemented.

### A. Decide the policy — DECIDED: Option 1 (dynamic-only), adopted

- **Option 1 — keep decay dynamic (ranking-only) — ADOPTED.** The dead
  `trust.rs::temporal_decay` and its unit test were removed; the misleading doc
  claims were corrected; `updated_at` stays the clock. No migration.
- **Option 2 — also age persisted trust on a schedule — NOT taken.** If genuine
  "forgetting" becomes a product ask later, wire a renamed
  `apply_persisted_trust_aging` into a real job and persist the result
  (scheduler + migration below). Until then it is explicitly out of scope.

### B. Concrete changes (ranked; do regardless of A/Option)

1. **Docs — fix the inaccuracy (DONE).** The storage audit
   (`docs/MEMORY-STORAGE-GROWTH-AUDIT.md` §7) now reads: only
   `retrieval.rs::temporal_decay_factor` (365-day half-life, floor 0.10, keyed
   on `updated_at`) affects ranking; the stored `trust_score` never decays;
   `trust.rs::temporal_decay` has been removed. The `trust.rs:1` module doc no
   longer mentions "aging".
2. **Eliminate the name collision / dead code (DONE).** `trust.rs::temporal_decay`
   and its unit test were deleted (Option 1). No unused aging function sits next
   to the live decay factor anymore.
3. **Explainability API — DONE (Q6).** A read path for `memory_feedback_events`
   landed: `TraceDecay::fact_trust_history(fact_id)` → ordered history, exposed
   as a dashboard endpoint
   (`GET /api/plugins/holographic/fact/{fact_id}/trust-history`) and an MCP field
   (`trust_history`) on fact `get`. The existing audit table now answers "why is
   trust X?".
4. **Scheduler (only if Option 2).** A periodic `tokio` task (e.g. daily,
   alongside the existing startup catch-up spawn in `mcp/server.rs`) that, in a
   single transaction, applies `apply_persisted_trust_aging` to facts whose
   `updated_at` is older than the period, stamping a new `updated_at`/
   `last_feedback_at`-style watermark to avoid double-application. Must be
   idempotent and log every change to `memory_feedback_events` with
   `source='decay'` so the audit trail (now readable via item 3) explains it.
5. **Migration (only if Option 2).** Add a watermark column, e.g.
   `trust_decayed_at INTEGER` (default = `created_at`/`updated_at`), seeded in a
   one-shot migration in `src/db/migrations.rs`, so the scheduler can compute
   "age since last aging" deterministically and survive restarts. No schema
   change needed for Option 1.
6. **Make it testable (OPEN — Option 1 branch).** Add an integration test in
   `tests/memory_test.rs` asserting that `trust_score` is byte-identical
   before/after a simulated `now` advance and that `temporal_decay_factor` moves
   the ranking as expected (the Option 1(a) branch). The Option 2 branch is
   moot since Option 2 was not taken. A test that the explainability API
   (item 3) returns the expected history is covered by the Q6 read-API tests
   (`tests/dashboard_api_test.rs` exercises `/trust-history`; MCP `get`
   `trust_history` is exercised in the memory-handler tests).

### Acceptance-criteria check

- **When is decay applied today?** Ranking only, at recall time, dynamically via
  `retrieval.rs::temporal_decay_factor(updated_at)`; never on a schedule, never
  to persisted `trust_score`. (§3, §6)
- **Is it visible/persistent?** The ranking factor is visible per result (`why`
  field) and **not** persisted. The persisted `trust_score` never decays. The
  feedback audit trail is recorded and **now readable** via the Q6 read API
  (store method, MCP `get` field, dashboard route). (§3, §5)
- **What changes are required to make it explicit & testable?** The doc
  inaccuracy, the dead-code/name-collision, and the feedback-history read API
  (Q6) are DONE; remaining work is the Option-1 integration test
  (§8 item 6, OPEN). The scheduler + watermark migration apply only under
  Option 2, which was not adopted. (§8)
