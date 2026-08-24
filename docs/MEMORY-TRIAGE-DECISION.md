# Holographic Memory — Root Triage Decision

Kanban `t_fd962a8a`. This is the **root triage conclusion** for the holographic
(FHRR) memory subsystem. It sits on top of the four audits and the synthesized
plan — it does not repeat their evidence. Its job is three things:

1. **Verify** the upstream audits are sound (independently re-measured, not
   trusted).
2. **Decide** the open questions and lock the build order.
3. **Authorize** the next wave of work as concrete tickets.

Read order: `MEMORY-TRIAGE-PLAN.md` (the plan) → this doc (the decision).

---

## 1. Verification status: all five audits reproduce

Historical note: this section records the triage-time verification snapshot that
authorized Q1/Q2. The current checkout has since adopted Option 1: the former
`trust.rs::temporal_decay` routine and its direct unit test were removed. Only
`retrieval.rs::temporal_decay_factor` remains live.

Every load-bearing number was re-measured against the live checkout DB
(`.tracedecay/tracedecay.db`) and every code anchor re-checked against `master`.

| Claim (from audits) | Re-measured | Match |
|---|---|---|
| `memory_facts` = 129 rows | 129 | ✅ |
| `hrr_vector` blob = 16,392 B, fixed | avg=min=max=16392 | ✅ |
| vector bytes total ≈ 2.11 MiB | 2,114,568 B | ✅ |
| `memory_facts` table ≈ 2.26 MiB | 2,260,992 B (exact) | ✅ |
| entities 686 / banks 6 / dirty-banks 3 / oplog 0 / feedback 0 | 686 / 6 / 3 / 0 / 0 | ✅ |
| never-recalled 129/129 | 129/129 | ✅ |
| below-min-trust facts = 0 | 0 | ✅ |
| orphan entities = 5 | 5 | ✅ |
| `PRAGMA auto_vacuum = 0` | 0 | ✅ |
| historical: former `trust.rs::temporal_decay` (180-day pull-to-0.5) had zero production callers | removed; no `trust.rs::temporal_decay` definition remains | ✅ |
| Live decay = `retrieval.rs::temporal_decay_factor` (365-day half-life) | `retrieval.rs:712`, called at `:109` | ✅ |
| Fusion is a hard gate `relevance · trust · decay` | `retrieval.rs:709` `relevance * trust * temporal_decay.clamp(0,1)` | ✅ |
| `is_non_entity_leading_word` is exact-string (`Prefer` not `Prefers`) | `entities.rs:153-171` — `"Prefer"` present, `"Prefers"` absent | ✅ |

**The audits are accurate and tool-backed.** One factual correction (does not
change any recommendation) is recorded in §5.

---

## 2. Verdict by triage dimension

### 2a. Storage growth — **real but not urgent; one big lever**
- Per-fact cost is **~18 KiB, 93% of it the fixed 16.4 KiB f64 FHRR blob**.
  Growth is linear: ~18 MiB / 1k, ~179 MiB / 10k, ~1.76 GiB / 100k facts.
- No retention/TTL, no `max_facts`, append-only `memory_oplog`/feedback tables,
  no orphan-entity reaper, **`auto_vacuum = 0`** so curation deletes never
  reclaim blob pages.
- **Biggest lever, independent and shippable now:** serialize `Vec<f32>` (−50%,
  8.2 KiB) + enable incremental VACUUM. FHRR similarity is phase-cosine; f32
  phase precision is within the noise margin. → **Ticket Q4.**

### 2b. Retrieval quality — **correctness fine, ranking has a known bias**
- Recall pipeline is sound (FTS5 + entity + trust-baseline → score). Four
  empirical risks, all pinned to binary-driven evidence:
  - **Trust is a hard multiplicative gate** — an off-topic `trust=0.5` fact
    outranked an on-topic `trust=0.3` fact (relevance was actually higher for
    the on-topic one). Freshly-learned facts (start 0.5) lose to entrenched
    medium-trust ones.
  - **Entity extraction is brittle** — `"Prefers Tokio"` becomes the entity, not
    `Tokio`; single-token proper nouns (Postgres/Tokio/Kubernetes) are never
    extracted; `probe`/`reason` are largely blind.
  - **No morphology** — `backup` misses `backups`/`back up`.
  - **The holographic channel is decorative** — 0.5 floor + lexical recall gate
    compress it to a ~0.008 score swing.
- **Critical dependency:** tuning the fusion (M2) or holographic weight (M3)
  re-ranks every existing query and must be guarded by a **ranking eval harness
  that does not exist yet** (`tests/memory_eval_test.rs` covers hygiene contracts
  only). → **Ticket M1 (prerequisite), then Q5 (entities) + M4 (morphology).**

### 2c. Trust-decay semantics — **decision adopted: Option 1**
- The stored `trust_score` **never decays**. Only ranking decays, dynamically at
  recall (`temporal_decay_factor`, 365-day half-life, never persisted).
- The former `trust.rs::temporal_decay` dead-code/name collision was resolved by
  deleting the persisted-aging routine and its direct unit test. Current code has
  no persisted-aging function; only the retrieval-time ranking factor remains.
- **D-A decision (keystone):** dynamic-only decay (Option 1) is now adopted.
  Re-open Option 2 only if persisted "forgetting" becomes an explicit product
  ask. Destructive retention work should continue to treat stored trust as a raw,
  never-aged value.

### 2d. Dashboard / CLI / doctor visibility — Q3/Q6 landed; doctor still missing
- `MemoryStatus` (capacity ceiling 269/bank, missing vectors, below-threshold
  count, repair stats) is **now surfaced** to humans via Q3: `tracedecay memory
  status` CLI (`MemoryAction::Status`), `GET /api/plugins/holographic/status`,
  and a dashboard Memory Health card. (Historical: at triage time it was computed
  but unreachable — no `memory status` CLI, no `/status` route; the only
  consumers discarded the return value.)
- `memory_feedback_events` is **now readable** via Q6 — "why is this fact's trust
  X?" is answerable through `fact_trust_history` (store + MCP `get` field +
  dashboard route). (Historical: at triage time it was write-only, answerable
  only with raw SQL.)
- **129/129 facts never recalled** and **5 orphan entities** exist; the
  never-recalled/below-threshold counts are now reachable via Q3's status output,
  though there is still no standing stale-memory card or orphan badge (M7).
- **No memory doctor** — LCM has a full diagnose/repair/clean doctor; holographic
  memory has none (only `curate` dedup). This remains open (M5).
- **Q3 and Q6 are DONE** — both were the highest value-to-effort read surfaces,
  reusing already-computed data with zero new logic.

### 2e. Maintenance / doctor actions — **defer destructive ones until visibility lands**
- Safe, missing cleanup primitives: reap orphan entities, prune oplog, rebuild
  FTS, VACUUM/reclaim — several have **no code path today**.
- **Ordering rule (locked): visibility must precede destructive maintenance.**
  Ship the read surfaces (Q3/Q6) and the doctor module (M5) before exposing any
  retention sweep (M8), so operators see what a sweep would delete before they
  click it. → Tier-1 tickets deferred until Tier-0 lands (§4).

---

## 3. The keystone decision (one human input needed)

**D-A — Persisted trust aging: dynamic-only (Option 1) or scheduled (Option 2)?**

- **Today:** persisted `trust_score` never ages; only ranking decays.
- **Recommendation: Option 1 (dynamic-only).** It is the current behavior,
  removes the dead-code ambiguity, and unblocks the retention sweep with zero
  new machinery. Re-open only if/when persisted "forgetting" is an explicit
  product ask.
- **Current status:** Q2 took the dynamic-only path and deleted the dead code.
  M8 (retention sweep) and `below_default_recall_threshold_count` semantics now
  inherit that decision: stored trust is not aged before deletion logic.

This is the **only** decision the plan needs from a human. Everything else is
"go, independent" or "go after its prerequisite."

---

## 4. Authorized next wave (Tier-0 tickets, created as children of this task)

Standalone, non-destructive, ready-to-ship. Distinct file ownership so they run
concurrently without collision:

| Ticket | What | Files | Route |
|---|---|---|---|
| **Q1+Q2** | DONE: fixed `temporal_decay` doc inaccuracy and deleted the former `trust.rs::temporal_decay` name collision; only `retrieval.rs::temporal_decay_factor` remains | `docs/MEMORY-STORAGE-GROWTH-AUDIT.md`, `docs/TRUST-DECAY-SEMANTICS.md`, `src/memory/trust.rs`, `tests/memory_test.rs` | codex-gpt-5-3 |
| **Q3** | DONE: surfaced `MemoryStatus` — `tracedecay memory status` CLI (`MemoryAction::Status`) + `GET /api/plugins/holographic/status` + dashboard Health card + MCP doc cross-reference | `src/cli.rs`, `src/main.rs`, `src/dashboard/memory_api.rs`, `dashboard/holographic/*` | codex-gpt-5-4 |
| **Q4** | f64 → f32 vector serialization + incremental VACUUM (one-way door; backfill + tolerance test) | `src/memory/encoding.rs`, `src/db/migrations.rs`, `src/memory/store.rs` | codex-gpt-5-5 |
| **Q5** | Fix entity extraction coverage + brittle verb list (stem/prefix + head noun) | `src/memory/entities.rs` | codex-gpt-5-3 |
| **Q6** | DONE: feedback-history read API (trust explainability) — `TraceDecay::fact_trust_history`, MCP fact `get` `trust_history`, `GET /api/plugins/holographic/fact/{id}/trust-history` | `src/memory/store.rs`, `src/mcp/tools/handlers/memory.rs`, `src/dashboard/memory_api.rs` | codex-gpt-5-4 |
| **M1** | Ranking-quality eval scenario family (prereq for M2/M3) | `tests/memory_eval_test.rs`, `eval/scenarios/*` | codex-gpt-5-4 |
| **D-A** | DONE: dynamic-only persisted-trust policy adopted; do not build an aging scheduler unless product asks for persisted forgetting | decision only | inherit |

**Dependency links:** Q6 → after Q3 (shared dashboard/MCP surface). Everything
else parallel. Q4/M1 touch disjoint files from the others.

**Deferred (Tier-1, decompose after Tier-0 lands):** M2 (fusion re-balance,
after M1), M3 (holographic weight, after M1), M4 (porter morphology, pairs Q5),
M5 (memory doctor, after Q3), M6 (cleanup primitives, after M5), M7 (T2
diagnostics surfacing, after Q3+M5), M8 (retention sweep, **gated on D-A + Q3 +
M5**), M9 (persist supersession, after Q5+M1).

---

## 5. Correction to the upstream record

Two audits state `MemoryAction` is **"Curate-only"** (`MEMORY-HEALTH-VISIBILITY-
GAPS.md` §3.3; `MEMORY-TRIAGE-PLAN.md` X2/Q3). Re-checked against `src/cli.rs:338`,
the enum actually has **six variants: `Curate`, `List`, `Add`, `Remove`,
`Removeall`, `Gc`** — all curation/CRUD, none of which surface `MemoryStatus`.

This does **not** change any recommendation: at triage time there was no
`memory status` subcommand and no `Status`/`Doctor` variant, so Q3 ("surface
`MemoryStatus` to operators") stood as-is. Q3 has since **landed** — it added the
`Status` arm to the existing enum (now 7 variants), wired the CLI handler, and
registered `GET /api/plugins/holographic/status`. Recorded so nobody builds
against the wrong shape.

---

## 6. Summary

The memory subsystem is **healthy and self-consistent** — no correctness bugs,
no runaway growth today (129 facts, 2.4 MiB). The work is **operational parity
and quality-tuning**, not firefighting:

- **Ship now (independent):** f32 + VACUUM (storage), MemoryStatus surfacing
  (visibility), entity-extraction + eval-harness (retrieval quality
  prerequisites).
- **Decide once:** D-A trust-aging policy (recommend dynamic-only).
- **Sequence:** visibility → doctor → destructive maintenance; eval harness →
  fusion tuning.

Every recommendation here is grounded in a verified `file:line` or a
re-measured DB number. Full detail lives in `MEMORY-TRIAGE-PLAN.md` and the four
audits it cites.
