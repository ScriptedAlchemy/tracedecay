# Holographic Memory — Prioritized Triage & Implementation Plan

Status: synthesis of four upstream audits on the `master` working tree. This is
the single actionable summary the root triage (`t_fd962a8a`) consumes. Every
recommendation is grounded in a source audit that carries its own `file:line`
evidence; this doc cross-references them and resolves the overlaps, conflicts,
and dependency ordering between them.

Current-status note: this plan is partly historical. Q1/Q2 have since landed by
adopting Option 1 (dynamic-only decay): the former
`trust.rs::temporal_decay` function and its direct unit test were removed. In
the current checkout, only `retrieval.rs::temporal_decay_factor` remains.

## Source audits (read these first for detail)

| Audit | Doc | Scope |
|---|---|---|
| Storage | `docs/MEMORY-STORAGE-GROWTH-AUDIT.md` | per-fact byte cost, capacity math, unbounded-growth paths |
| Retrieval/entity | `docs/RETRIEVAL-QUALITY-EVAL.md` | recall pipeline + 7 empirically-measured quality risks (binary-driven evidence) |
| Trust-decay | `docs/TRUST-DECAY-SEMANTICS.md` | persisted vs. ranking decay, dead code, explainability gaps |
| Visibility | `docs/MEMORY-HEALTH-VISIBILITY-GAPS.md` | dashboard/CLI/doctor/MCP surfacing gaps (G1–G11) |

All four audited the same live checkout (`.tracedecay/tokensave.db`, 129 facts,
memory subsystem 2.43 MiB of a 78.5 MiB DB) and agree on the headline numbers.
Load-bearing anchors were re-verified for this synthesis. The historical
`src/memory/trust.rs:46` anchor named the now-removed persisted-aging routine;
current live anchors are `src/memory/retrieval.rs:698-718`,
`src/memory/entities.rs:153`, and `src/cli.rs:338`.

---

## Cross-cutting findings (what only the synthesis sees)

These are the things invisible from any single audit but decisive for ordering.

### X1 — The trust story is three interlocking pieces, not one
Storage, trust-decay, and retrieval each flag a different symptom of the *same*
trust design. Decided separately they will fight:

- **Storage audit:** `trust_score` never decays on disk, so it wants a
  retention sweep keyed on `trust_score < MIN_TRUST AND updated_at < now − AGE`.
- **Trust-decay audit:** it originally found two decay functions with one name —
  a dead `trust.rs::temporal_decay` (180-day pull-to-0.5) and the live
  `retrieval.rs::temporal_decay_factor` (365-day ranking multiplier). That
  ambiguity is now resolved by Option 1: the former persisted-aging routine was
  deleted, leaving only the retrieval-time ranking multiplier.
- **Retrieval eval:** trust is a **hard multiplicative gate** (`relevance × trust
  × decay`, `retrieval.rs:709`) — an off-topic trust=0.5 fact outranked an
  on-topic trust=0.3 fact in the binary run.

**Implication:** the trust *policy* decision is upstream of the retention
sweep, the explainability API, and the fusion re-balance. If you ship the
retention sweep before deciding whether persisted trust ages, you are
hard-deleting facts against semantics that may change. Decide policy first
(§"Larger design decisions", D-A).

### X2 — Visibility must precede the destructive maintenance actions
Every "suggested maintenance action" in the visibility audit (reap orphans,
prune oplog, retention sweep, FTS rebuild, VACUUM) is a *write*. The cheapest,
highest-value gaps were *read* paths for data that already existed but was
unreachable at triage time — the two highest-value ones (MemoryStatus and the
feedback-history read API) have since landed via Q3/Q6; the rest remain open:

- `MemoryStatus` (capacity 269/bank, missing vectors, below-threshold count,
  repair stats) is **now surfaced** (Q3 landed): `tracedecay memory status` CLI
  (`MemoryAction::Status`), `GET /api/plugins/holographic/status`, and a
  dashboard Memory Health card. (Historical: at triage time it was computed but
  MCP-only — not in the dashboard, not in the CLI; `MemoryAction` had no
  `Status` arm.)
- `memory_feedback_events` is **now readable** (Q6 landed): `fact_trust_history`
  (store + MCP `get` field + dashboard route), so "why did this fact's trust
  change?" is answerable without raw SQL. (Historical: at triage time it was
  write-only — no SELECT path in any handler/dashboard/MCP.)
- **129/129 facts have `last_recalled_at IS NULL`** (never recalled) and **5
  orphan entities** exist in a 129-fact checkout; the never-recalled count is now
  reachable via Q3's status output, though there is still no standing stale-memory
  card or orphan badge (M7).

Ship the read surfaces (T1/T2) **before** exposing any destructive action, so
operators can see what a retention sweep would delete before they click it.

### X3 — Retrieval tuning is gated by an eval harness that does not exist
`tests/memory_eval_test.rs` + `eval/scenarios/*` cover **hygiene contracts only**
(secrets, transient, supersession, dedup). There are **no ranking-quality
scenarios**. Fusing re-balance (F2) and holographic-weight changes (F4) both
re-rank every existing query; without a regression guard they are tuning blind.
The ranking eval (F1) is therefore a prerequisite, not a peer.

### X4 — The single biggest storage lever is independent and shippable now
Each fact stores a 2048-dim `f64` FHRR vector = **16,392 B blob, 93% of every
row** (~18 KiB all-in; 10k facts ≈ 179 MiB, 100k ≈ 1.76 GiB). FHRR similarity is
phase-cosine over [−π,π]; `f32` phase precision is well within the noise margin.
Serializing `Vec<f32>` (8,200 B) halves every estimate and touches one module
(`encoding.rs`) + a backfill mirroring the existing legacy-vector repair path.
It does **not** depend on any policy decision. Pair it with
`PRAGMA auto_vacuum = INCREMENTAL` or the migration's bytes never reclaim.

### X5 — Cross-doc inconsistency — **resolved by Q1/Q2**
The original plan flagged `docs/MEMORY-STORAGE-GROWTH-AUDIT.md:166` for
crediting the dead `trust.rs::temporal_decay` with "affecting scoring only".
Current docs now state the correct behavior: only
`retrieval.rs::temporal_decay_factor` affects ranking; the stored `trust_score`
never decays; the former `trust.rs::temporal_decay` routine has been removed.

---

## Prioritized plan

Each item: **owner surface**, **depends on**, **acceptance test / observability**,
**effort tier** (T1 reuse existing data · T2 small new query/logic · T3 new
subsystem).

### Tier 0 — Quick wins (ship first; low risk, high value, few deps)

#### Q1 — Fix the decay doc inaccuracy — **DONE**
- **What:** `MEMORY-STORAGE-GROWTH-AUDIT.md:166` and the `trust.rs:1`
  module-doc "aging" headline were rewritten to match the trust-decay audit's
  §8-B-1 wording.
- **Depends on:** nothing.
- **Acceptance:** grep for `temporal_decay` across `docs/` should show no
  current-state claim that former `trust.rs::temporal_decay` affects scoring;
  `trust.rs:1` no longer advertises "aging".
- **Tier:** T1. **Done.**

#### Q2 — Resolve the dead-code / name collision — **DONE**
- **What:** Option 1 was adopted. The former `trust.rs::temporal_decay` routine
  and its direct test (`tests/memory_test.rs:262` at the time of the audit) were
  deleted; no unused persisted-aging function sits next to the live decay
  factor.
- **Depends on:** resolved by the D-A dynamic-only decision.
- **Acceptance:** current `temporal_decay` references are the live retrieval
  factor plus historical docs; the remaining live decay is unambiguously
  `retrieval.rs::temporal_decay_factor`.
- **Tier:** T1. **Done.**

#### Q3 — Surface `MemoryStatus` to humans (CLI + dashboard + discoverability) — **DONE**
- **What:** `tracedecay memory status` (human + `--json`) printing
  `memory_status()`; `GET /api/plugins/holographic/status` returning the same
  payload (plus largest-bank utilization); a "Memory Health" dashboard card. The
  existing `tracedecay_memory_status` MCP tool doc now cross-references the CLI
  and dashboard route (M2).
- **Depends on:** nothing — reuses already-computed data; zero new logic.
- **Acceptance / observability:** CLI shows fact/entity/bank counts, capacity
  ceiling 269, largest-bank utilization %, `below_default_recall_threshold_count`,
  missing vectors; dashboard card renders the same. Verified via CLI parse/status
  tests and the dashboard Health card.
- **Tier:** T1. **Done (closed/approved, t_b00e9730).** Closes visibility gaps
  G2, G8.

#### Q4 — f64 → f32 vector serialization + incremental VACUUM
- **What:** `encoding.rs` serializes `Vec<f32>`; add an `hrr_precision` column
  and a one-shot backfill modeled on the existing legacy-vector repair; enable
  `PRAGMA auto_vacuum = INCREMENTAL` (set at creation; `VACUUM` once + flip for
  existing DBs) and schedule `incremental_vacuum`.
- **Depends on:** nothing (independent of all policy decisions).
- **Acceptance:** round-trip phase-cosine similarity preserved within a documented
  tolerance vs f64 baseline; `length(hrr_vector) == 8200`; bank rebuild produces
  equivalent recall ordering on the eval fixture; after a delete, an
  `incremental_vacuum` run reclaims blob pages and the file shrinks.
- **Tier:** T2. **Go — biggest single storage lever.** Closes G3 (read side),
  G10, and gives curation deletes a reclamation path.

#### Q5 — Fix entity extraction coverage + the brittle verb list
- **What:** Capture single high-salience capitalized tokens (or lower the ≥2-word
  threshold for non-sentence-initial capitalized tokens) so Postgres/Tokio/
  Kubernetes become entities; make `is_non_entity_leading_word`
  (`entities.rs:153`) stem/prefix-match (`prefer`/`prefers`/`preferred`/
  `using`/`uses`); extract the head noun of a captured phrase as an additional
  entity.
- **Depends on:** nothing (F5 morphology improves it further, but is not required).
- **Acceptance / observability:** against the retrieval-eval fixture,
  `probe("Tokio")` returns F3 and `reason(["database"])` is non-empty without an
  explicit entity arg; unit tests for each verb form and the head-noun path.
- **Tier:** T2. **Go.** Closes retrieval risks A and the entity half of E.

#### Q6 — Feedback-history read API (trust explainability) — **DONE**
- **What:** `TraceDecay::fact_trust_history(fact_id) → ordered history`, exposed
  as a dashboard endpoint (`GET /api/plugins/holographic/fact/{id}/trust-history`)
  and a `get_fact`/MCP `get` field (`trust_history`). Turns the formerly
  write-only `memory_feedback_events` audit table into an answerable "why is
  trust X?".
- **Depends on:** nothing.
- **Acceptance / observability:** a fact with recorded feedback returns its full
  trust trail via the new API; a fact with no feedback returns empty. Verified via
  `tests/dashboard_api_test.rs` (trust-history route) and memory-handler tests
  (MCP `get` `trust_history`).
- **Tier:** T2. **Done (closed/approved, t_97202453).** Closes trust-decay §5
  and visibility G7 (explainability).

### Tier 1 — Medium follow-ups (after the relevant Tier-0 prerequisites)

#### M1 — Ranking-quality eval scenario family (F1)
- **What:** Extend `tests/memory_eval_test.rs` + `eval/scenarios/*` with ranking
  assertions (`SearchRank { query, top_fact_source, min_rank_gap }`) using the
  same subprocess path the harness already drives. Pin the trust-bias case
  (retrieval Risk C), the supersession case (Risk F), and the morphology case
  (Risk B).
- **Depends on:** nothing technically — but it is the **prerequisite** for M2/M3.
- **Acceptance / observability:** scenarios run in CI and assert on-topic-first
  ordering; the harness has a ranking assertion kind, not just hygiene contracts.
- **Tier:** T2. **Go now — it unblocks safe retrieval tuning.**

#### M2 — Re-balance fusion: gate trust, don't multiply it (F2)
- **What:** Replace `relevance × trust` (`retrieval.rs:709`) with a gate + gentle
  nudge, e.g. `relevance × (0.5 + 0.5·trust) × decay`; keep the
  `DEFAULT_MIN_TRUST` filter for exclusion. Normalize the FTS score (current
  `1/(1+|bm25|)` is collection-size-dependent).
- **Depends on:** M1 (regression guard) — **conditional Go.**
- **Acceptance:** `combined_score` unit tests for the trust-bias case; the M1
  ranking scenario for trust bias passes; no scenario regresses.
- **Tier:** T2. **Conditional Go (after M1).**

#### M3 — Make the holographic signal earn its weight (F4)
- **What:** Either drop the `(sim+1)/2` floor (`retrieval.rs:604`) so raw FHRR
  similarity discriminates, or (if the floor is needed to avoid negatives) drop
  the holographic weight from 0.30 toward 0.10–0.15 until a real embedding model
  replaces the SHA-256 atom keys (`encoding.rs:85`), which currently make the
  channel a deterministic lexical hash, not a semantic signal.
- **Depends on:** M1 (regression guard) — **conditional Go.** The deeper
  question (real embeddings) is design decision D-D.
- **Acceptance:** a scenario asserting two semantically-similar-but-lexically-
  different facts out-rank two lexically-similar-but-unrelated ones — until it
  passes, the channel is decorative.
- **Tier:** T2. **Conditional Go (after M1).**

#### M4 — Add morphology (F5)
- **What:** Configure FTS5 with the `porter` stemmer (`migrations.rs:1136`) and
  apply the same stemming in `tokenize`/`tokenize_text` so `install`/`installing`
  and `backup`/`backups`/`back up` collapse.
- **Depends on:** nothing, but pairs with Q5 (entity extraction) and benefits
  from M1 as a guard.
- **Acceptance:** the morphology eval scenario passes; FTS rebuild after the
  tokenizer change succeeds (see M5 FTS-rebuild action).
- **Tier:** T2. **Go.** Closes retrieval Risk B.

#### M5 — The missing memory doctor (visibility §5.3)
- **What:** A `gather_diagnostics → plan_and_apply_repairs` module modeled on
  `src/sessions/lcm/doctor.rs`, with `mode ∈ {diagnose, repair, clean}` +
  `apply: bool` + pre-apply DB backup (reuse LCM's `backup_database`/
  `checkpoint_wal_for_backup`). Diagnostics are cheap read-only SQL (the full
  list is visibility §5.3: schema version, counts, vector health, orphan-entity
  count, FTS sync, staleness, bank freshness, compaction state). This is the
  home for the four new repair primitives below.
- **Depends on:** Q3 (status surfacing) for the read side; M6/M7/M8 are its
  actions.
- **Acceptance / observability:** `diagnose` returns the diagnostics bundle
  matching the live-DB numbers (129 facts, 5 orphans, 3 dirty banks, 129/129
  never recalled, 2.11 MiB vectors); every `plan` action carries
  `safe: bool`, `description`, `candidate_count`; `apply` with `mode=diagnose`
  mutates nothing.
- **Tier:** T3. **Go — this is what brings memory to parity with LCM's
  operational story** and closes G1, G9.

#### M6 — Safe cleanup primitives the doctor calls
- **What:** `reap_orphan_entities` (one `DELETE ... WHERE NOT EXISTS`),
  `prune_oplog` (cap `memory_oplog` by age/count — no prune path exists today),
  `rebuild_memory_fts` (`INSERT INTO memory_facts_fts(...) VALUES('rebuild')`,
  mirroring LCM `rebuild_summary_fts`), and `vacuum_reclaim` (report freelist;
  apply `VACUUM`/`incremental_vacuum`). All go through `MemoryStore` canonical
  paths (hard-delete + FK-cascade + oplog).
- **Depends on:** M5 (doctor entry point).
- **Acceptance / observability:** dry-run reports the 5 orphans / oplog row
  count; apply removes them; a second dry-run shows 0; idempotent. FTS rebuild
  fixes any `memory_facts` vs `memory_facts_fts` row-count drift.
- **Tier:** T2. **Go.** Closes visibility G4, G6, G11 and retrieval's supersession
  half once paired with D-C.

#### M7 — T2 diagnostic surfacing (index-health strip, stale-memory card, orphan badge)
- **What:** D3 (FTS sync + bank freshness + dirty-queue depth + journal/auto_vacuum
  state), D4 (stale-memory card: `last_recalled_at IS NULL` and
  `last_recalled_at < now − 180d` with a drill-in list), D5 (orphan-entity badge
  + reap action wired to M6). Extend `tracedecay doctor` and `status --runtime`
  (`RuntimeSnapshot`: add `fact_count`, `memory_bytes`, `vector_bytes`).
- **Depends on:** Q3 (status route), M5 (doctor diagnostics).
- **Acceptance / observability:** index-health strip shows FTS sync + 3 dirty
  banks; stale-memory card shows 129/129; orphan badge shows 5; `doctor` output
  has a Memory section; `--runtime` carries the new memory counts.
- **Tier:** T2. **Go.** Closes G4, G5, G6, G11 (read side).

#### M8 — Retention sweep (the destructive one)
- **What:** Hard-delete facts with `trust_score < MIN_TRUST AND updated_at <
  now − MAX_AGE`. Add the thresholds to `config.rs` (currently has zero memory
  settings). Surfaced as a doctor action with dry-run-first + backup, mirroring
  LCM.
- **Depends on:** **D-A (trust policy decision)** + Q3 (visibility, so operators
  see what it deletes) + M5 (doctor). **Conditional Go.**
- **Acceptance / observability:** dry-run lists candidate facts; apply hard-deletes
  them via canonical `remove_fact`/`delete_facts` (FK-cascade + oplog entry); a
  `source='retention'` row is logged; re-running the sweep is idempotent; on this
  checkout it deletes 0 (0 facts below threshold) — the sweep is forward-looking.
- **Tier:** T2 → **Conditional Go (D-A now resolved to dynamic-only; still gated
  on Q3 + M5 visibility/doctor prerequisites).**

#### M9 — Persist supersession so entity joins skip stale facts (F6)
- **What:** When `add_fact` classifies `PossibleConflict`, mark the older fact
  superseded (`superseded_by`/`deprecated` flag on `memory_facts`); have
  `probe`/`reason`/`search` exclude or down-rank superseded facts.
- **Depends on:** a schema migration (the only item here that needs one); pairs
  with Q5 (entity extraction) so `probe`/`reason` are actually reachable.
- **Acceptance / observability:** after adding a superseding fact, `probe`/`reason`
  on the shared entity no longer surface the superseded one; a regression
  scenario pins it.
- **Tier:** T2 (logic) + schema. **Go after Q5 + M1.** Closes retrieval Risk F.

### Tier 2 — Larger design decisions (some now resolved)

These are design gates rather than implementation tasks. Some were open at
triage time and have since been resolved; unresolved items still need a product
call before building.

#### D-A — Persisted trust aging — **RESOLVED: dynamic-only (Option 1)**
- **The decision:** Persisted `trust_score` does not age. Only ranking decays,
  dynamically, through `retrieval.rs::temporal_decay_factor`.
- **Current status:** Q2 took Option 1 and deleted the former
  former `trust.rs::temporal_decay` routine; it did not wire a scheduler,
  `trust_decayed_at` watermark migration, or `source='decay'` audit rows.
- **Downstream effect:** M8 (retention sweep) and
  `below_default_recall_threshold_count` should interpret stored trust as raw,
  never-aged state. Re-open only if persisted forgetting becomes an explicit
  product ask.

#### D-B — Trust fusion semantics
- **The decision:** Should trust *exclude* (hard floor) and *nudge* (M2), or stay
  a hard multiplicative gate? The current gate systematically disadvantages
  freshly-learned facts (start at 0.5) against entrenched medium-trust ones.
- **What waits on it:** M2, and indirectly the "below recall threshold" metric.
- **Recommendation:** **Go with M2's gate+nudge** — but only after M1 lands so
  re-ranking is guarded. This is the highest-leverage retrieval-quality change.

#### D-C — Supersession policy
- **The decision:** On `PossibleConflict`, auto-mark-old-as-superseded,
  prompt, or keep advisory-only (today)? Affects how aggressively stale facts are
  pruned from `probe`/`reason`.
- **What waits on it:** M9.
- **Recommendation:** **Prompt-first** (match the existing curation dry-run →
  confirm shape), auto-supersede only behind a config flag. Avoids silently
  hiding a fact a user may still want.

#### D-D — Real embeddings vs. decorative holographic channel
- **The decision:** The holographic channel is currently a deterministic SHA-256
  lexical hash (`encoding.rs:85`) — "install" is as unrelated to "installing" as
  to "xylophone". The 0.30 weight buys almost no ordering (measured swing ≈0.008
  after the 0.5 floor + lexical gate). Invest in a real embedding model, or
  demote the channel until then?
- **What waits on it:** M3, and the long-term value of the per-fact 16 KB
  (→8 KB f32) vector blob.
- **Recommendation:** **Demote to 0.10–0.15 now (M3), defer real embeddings.**
  Real embeddings are a large, separate effort (model selection, batching,
  storage) — track as its own epic, not folded into this triage.

#### D-E — Hard `max_facts` / byte budget with LRU/trust eviction
- **The decision:** Should the store have a hard ceiling regardless of ingestion?
- **What waits on it:** nothing, but premature until Q4 (f32) + M8 (retention)
  land and storage is still a problem.
- **Recommendation:** **Defer.** f32 + retention + incremental_vacuum remove
  the near-term pressure; revisit if 100k-fact (1.76 GiB f64 → ~880 MiB f32)
  scale is a real target.

---

## Dependency graph (build order)

```
Q1 (doc fix) ─────────────────────────────────────── DONE
Q2 (dead code) ─── D-A resolved to Option 1 ───────── DONE
Q3 (MemoryStatus surfacing: CLI + dashboard + MCP doc) ── DONE
Q4 (f32 + incremental_vacuum) ────────────────────── standalone
Q5 (entity extraction fix) ───────────────────────── standalone (pairs w/ M4)
Q6 (feedback-history read API) ────────────────────── DONE

M1 (ranking eval) ────────────────────────────────── prerequisite for M2, M3
M2 (fusion re-balance) ─── depends on M1 ─────────── gated on D-B
M3 (holographic weight) ─── depends on M1 ────────── gated on D-D
M4 (morphology / porter) ── pairs with Q5 ────────── standalone-ish
M5 (memory doctor) ──────── depends on Q3 (read side)
M6 (cleanup primitives: reap/prune/fts-rebuild/vacuum) ── depends on M5
M7 (T2 diagnostics surfacing) ── depends on Q3 + M5
M8 (retention sweep) ────── D-A resolved + depends on Q3 + M5 ── conditional
M9 (persist supersession) ── depends on Q5 + M1 (+ schema migration)
```

Three critical paths:
1. **Visibility path:** Q3 → M5 → M6/M7 → M8. (Visibility must precede the
   destructive retention sweep.)
2. **Retrieval-quality path:** M1 → M2/M3, with Q5/M4 parallel.
3. **Storage path:** Q4 (f32 + vacuum) is independent and can ship immediately.

---

## Go / no-go recommendations (the executive summary)

### Maintenance / doctor actions

| Action | Verdict | Condition |
|---|---|---|
| Doc fix (Q1) + dead-code resolution (Q2) | **DONE** | Option 1 was adopted; former `trust.rs::temporal_decay` was deleted |
| `MemoryStatus` surfacing — CLI/dashboard/MCP (Q3) | **DONE** | landed (t_b00e9730); reuses existing data |
| f32 vector migration + incremental_vacuum (Q4) | **GO** | biggest storage lever, independent |
| Memory doctor module (M5) + cleanup primitives (M6) | **GO** | depends on Q3 |
| Retention sweep (M8) | **CONDITIONAL GO** | gated on D-A + Q3 + M5 |
| Persisted trust aging scheduler (Option 2) | **NO-GO now** | only if forgetting is a product ask (D-A) |
| `max_facts` hard ceiling (D-E) | **DEFER** | revisit after Q4 + M8 |

### Dashboard visibility

| Surface | Verdict | Tier |
|---|---|---|
| Memory Health card + `GET /api/plugins/holographic/status` (D1) | **DONE** | T1 — landed via Q3 |
| Store-size breakdown card (D2) | **GO** | T2 — needs a `dbstat`/table-size rollup |
| Index-health strip (D3) + stale-memory card (D4) + orphan badge (D5) | **GO** | T2 — from M5/M7 |
| Maintenance action menu (D6) | **GO** | T3 — from M5/M6 |
| `tracedecay memory status` CLI (C1) | **DONE** | T1 — landed via Q3 |
| Extend `tracedecay doctor` + `status --runtime` (C3/C4) | **GO** | T2 — from M7 |

### Retrieval quality

| Change | Verdict | Condition |
|---|---|---|
| Entity extraction fix (Q5) | **GO** | standalone |
| Morphology / porter stemmer (M4) | **GO** | pairs with Q5 |
| Ranking eval harness (M1) | **GO now** | prerequisite |
| Fusion re-balance gate+nudge (M2) | **CONDITIONAL GO** | after M1; gated on D-B |
| Demote holographic weight (M3) | **CONDITIONAL GO** | after M1; gated on D-D |
| Persist supersession (M9) | **GO after Q5+M1** | needs schema migration |
| Real embedding model (D-D) | **DEFER** | separate epic |

---

## Risks & open questions (require product/design input)

1. **D-A was the keystone decision; Option 1 is now adopted.** Persisted trust
   does not age. The former dead-code fix is complete; retention and
   explainability work should build on dynamic-only ranking decay. Confirm only
   before anyone revives the Option-2 scheduler.
2. **Fusion re-ranking affects every user's existing queries.** M2/M3 must ship
   behind the M1 eval guard and ideally a config flag, not as a silent change.
3. **f32 is a one-way door on disk format.** It needs the backfill path (mirror
   the existing legacy-vector repair) and a rollback story. FHRR phase-cosine
   tolerance must be measured and documented, not assumed.
4. **Retention sweep hard-deletes.** It must reuse the LCM doctor's
   dry-run → backup → apply shape. On this checkout it deletes 0 (no facts below
   threshold), so it is forward-looking — but the operator affordance must make
   the candidate list visible *before* apply (hence Q3/M5 before M8).
5. **"confidence" naming collision** (code-graph resolver score vs. memory trust)
   is a documentation/disambiguation issue, not a code change — but worth a doc
   note so nobody wires the two together by accident.
6. **Supersession UX (D-C).** Auto-superseding a fact a user still wants is a
   silent data-loss footgun. Prefer prompt-first; auto only behind a flag.
7. **Bank quality ceiling (269 facts/bank) is recall-quality only, not storage.**
   Do not confuse it with a storage limit — per-fact vectors are mandatory and
   grow unbounded; the ceiling only degrades the bank-accelerated fast-path.

---

## Verification posture for this plan

- Every Tier-0/Tier-1 item carries an acceptance test or observability check
  (see each item). The two with the most risk (Q4 f32, M8 retention) require
  measured, documented tolerances and a rollback/backfill path.
- The retrieval changes (M2/M3) are pinned to M1 scenarios; they cannot ship
  before the guard exists.
- The destructive actions (M6 reap/prune, M8 retention) are gated behind the
  doctor's dry-run → backup → apply flow and are idempotent by construction.
- Source-audit anchors re-checked for this synthesis: the former
  `src/memory/trust.rs:46` persisted-aging anchor is historical and removed;
  current live anchors are `src/memory/retrieval.rs:698-718`,
  `src/memory/entities.rs:153`, and `src/cli.rs:338`.
