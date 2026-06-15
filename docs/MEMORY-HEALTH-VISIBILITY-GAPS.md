# Holographic Memory — Health Visibility Gap Analysis

Scope: operator visibility into **holographic (FHRR) memory health** across every
user-facing surface — the `tracedecay` CLI, the standalone `tracedecay dashboard`
HTTP backend, the `dashboard/holographic/*` frontend page, the MCP tool surface,
and the doctor/maintenance commands. The LCM session store and the code graph are
referenced only as comparison/contrast.

This is a **gap analysis + recommendations** artifact (task `t_f47ae50b`), not an
implementation. It builds on two prior audits in this repo:

- `docs/DASHBOARD-API-AUDIT.md` — every dashboard route + the data-access contract.
- `docs/MEMORY-STORAGE-GROWTH-AUDIT.md` — per-fact cost, capacity math, unbounded-growth paths.

Real numbers below are measured against the live checkout DB
(`.tracedecay/tokensave.db`, 78.5 MB total; **129 facts**, memory subsystem
**2.43 MiB**) on the `master` working tree at `5ad31c4`.

> **Current-status update (post-Q3/Q6):** two gaps this analysis flagged have
> since **landed in code** and are no longer open:
> - **Q3 — `MemoryStatus` surfacing (was G2):** the rich status object is now
>   reachable by humans via `tracedecay memory status` (CLI,
>   `MemoryAction::Status`), `GET /api/plugins/holographic/status` (dashboard
>   route), and a dashboard Memory Health card; the MCP tool doc now
>   cross-references them. Where this doc still says MemoryStatus is "MCP-only",
>   there is "no `memory status` CLI", or it is "invisible to humans", that is
>   the **gap-analysis-time state**, flagged as historical below.
> - **Q6 — feedback-history read API (trust-decay §5 explainability):**
>   `TraceDecay::fact_trust_history(fact_id)`, the MCP fact `get` `trust_history`
>   field, and `GET /api/plugins/holographic/fact/{fact_id}/trust-history` make
>   the `memory_feedback_events` audit table readable. Where this doc still calls
>   that table "write-only", that is the **gap-analysis-time state**.
>
> Gaps that remain open: the memory **doctor** (G1, M5), the T2 diagnostic cards
> (G3 store-size breakdown, G4 orphan badge, G5 stale-memory card, G6 FTS-health
> strip), the maintenance action menu (G9), and an "effective decayed trust"
> projection view (G7 read side — distinct from the Q6 history read path).

---

## 1. Method

Read, in full:

- CLI: `src/cli.rs` (subcommand enums), `src/main.rs` handlers (`Status`, `Doctor`,
  `Memory`, `Cost`, `Branch`), `src/doctor.rs`, `src/runtime_telemetry.rs`.
- MCP: `src/mcp/tools/definitions.rs`, `src/mcp/tools/handlers/mod.rs`,
  `src/mcp/tools/handlers/memory.rs`, and the LCM doctor at
  `src/sessions/lcm/doctor.rs` (for the contrast baseline).
- Dashboard backend: `src/dashboard/mod.rs`, `src/dashboard/memory_api.rs`
  (`overview_payload`, `trust_histogram`, `hrr_coverage`).
- Dashboard frontend: `dashboard/holographic/src/HolographicMemoryPage.tsx`,
  `CurationPanel.tsx`, `api.ts`.
- Data shapes: `src/memory/types.rs` (`MemoryStatus`, `MemoryRepairStats`),
  `src/tracedecay.rs::memory_status` / `repair_derived_memory`.

Read-only SQL against the live DB to ground each gap in current numbers.

---

## 2. The headline structural gap

**The LCM subsystem has a full doctor; holographic memory does not.**

| Surface | LCM session store | Holographic memory |
|---|---|---|
| Diagnose (schema/FTS/integrity/orphan/retention) | ✅ `tracedecay_lcm_doctor` MCP + `src/sessions/lcm/doctor.rs` | ❌ none |
| Status/telemetry | ✅ `tracedecay_lcm_status` | ⚠️ `tracedecay_memory_status` (counts only — no integrity/health) |
| Plan + apply repairs (dry-run → backup → apply) | ✅ `doctor(mode=repair/clean)` | ❌ none (`memory curate` is dedup-only) |
| Cleanup candidates w/ backup | ✅ `clean_lcm_noise` | ❌ none |

`src/sessions/lcm/doctor.rs` gathers schema version, FTS rebuild-needed flags,
payload orphan/missing-file diagnostics, summary-source integrity, lifecycle
frontier checks, retention candidates, and noise cleanup candidates — then plans
safe repairs, backs up the DB, and applies them, all behind `mode`/`apply`
flags. **Nothing equivalent exists for `memory_facts` / `memory_entities` /
`memory_banks` / `memory_oplog`.** That asymmetry is the single biggest
visibility gap and the natural home for most of the metrics below.

The top-level `tracedecay doctor` (`src/doctor.rs`) is an **installation**
health check (binary, project-index existence, global DB, agent integrations,
network) plus a whole-DB `VACUUM`. It reports DB file size + reclaimed bytes but
**zero memory-specific metrics** — no fact count, no vector bytes, no FTS, no
orphans.

---

## 3. Inventory — what exists today, mapped to the seven required visibility areas

Legend: ✅ exposed to operators · ⚠️ computed but not surfaced · ❌ not computed
anywhere.

### 3.1 Dashboard UI (`dashboard/holographic/src/HolographicMemoryPage.tsx`)

Already renders, from `GET /api/plugins/holographic` (`overview_payload`):

- System strip: provider, context engine, curator tools/agents, DB ready/missing, storage **path** (no size).
- **Fact counts**: per-category counts + avg trust (DataBars + CompositionBar).
- **Entity types**: composition (or empty-state "unclassified" explainer).
- **Memory banks**: name, live `fact_count`, `dim`, composition bar, `bundled_fact_count` (staleness vs live count).
- **HRR coverage**: per-category gauge — `hrr_vectors / facts`, status `ready` / `missing_vectors` / `missing_bank` / `stale_bank` (the strongest health signal in the UI today).
- **Trust distribution**: 10-bucket histogram + mean.
- **Growth**: daily + cumulative facts sparkline.
- Facts inspector: per-fact trust, `retrieval_count`, `helpful_count`, HRR flag.
- Curation tab: delete/merge proposals, oplog, activity, history; diagnostic counts incl. `orphan_entities`, `entity_scan_remaining`, `related_clusters`.

### 3.2 Required areas × current surface

| # | Required visibility | Status | Where it is today | Gap |
|---|---|---|---|---|
| A | **Store size** | ❌/⚠️ | `tracedecay doctor` shows whole-DB bytes + VACUUM reclaim; `status --runtime` shows DB/WAL/SHM bytes + db/source ratio. **Neither breaks out the memory subsystem**, and neither is in the dashboard. | No memory-subsystem byte footprint (facts vector bytes, FTS bytes, oplog bytes); no per-table size; no "memory is X% of DB". |
| B | **Vector counts** | ⚠️ partial | Per-category HRR coverage (vectors/facts) in dashboard. Since **Q3 landed**, `MemoryStatus.missing_vector_count` + `hrr_dim` + `estimated_capacity` are surfaced via `tracedecay memory status` (CLI), `GET /api/plugins/holographic/status`, and the Memory Health card (at audit time these were MCP-only). | **Total vector bytes and dimension/precision (f64 vs f32) still unshown** (G3/G10). The capacity ceiling (269 facts/bank) and utilization % are now surfaced by Q3. |
| C | **FTS / entity stats** | ⚠️/❌ | Entity count + entity-type composition in dashboard; `orphan_entities` reported **reactively** inside a curation dry-run plan. | No standing FTS index size/health; **no FTS sync check** (LCM doctor has `rebuild_needed` detection — memory does not); no orphan-entity count except by running curate. **5 orphan entities already exist** in a 129-fact checkout and are invisible without a curation run. |
| D | **Trust-decay status** | ❌ | Trust histogram + mean shown (static `trust_score`). Decay is **ranking-only, recomputed at query time, never written to disk** (`retrieval.rs::temporal_decay_factor`, 365-day half-life, floored at 0.10). The per-result `why` field exposes the dynamic factor per result. | No "effective decayed trust" aggregate view / projection (G7, still open). `below_default_recall_threshold_count` is now surfaced via **Q3** (`memory status` / `/status`). The trust *change history* is now readable via **Q6** (`fact_trust_history` / `/trust-history`). |
| E | **Stale memories** | ❌ | Nothing. `last_recalled_at` is stored per fact but unused for any UI/CLI signal. | No "not recalled in N days" list/count. **In this checkout, 129/129 facts have `last_recalled_at IS NULL` (never recalled)** — a strong staleness signal that is entirely invisible. |
| F | **Index health** | ⚠️/❌ | HRR bank freshness status (`stale_bank`) per category is the only index-health signal. `MemoryStatus.repair` (`missing_vectors_repaired`, `banks_rebuilt`) exists, unsurfaced. | No FTS integrity check; no `memory_facts_fts` row-count sync vs `memory_facts`; no bank-capacity utilization; no dirty-bank queue display (`memory_bank_dirty` = 3 here); no auto_vacuum/VACUUM-status indicator; no "rebuild needed" planner like LCM's. |
| G | **Suggested maintenance actions** | ⚠️ partial | Curation tab proposes delete/merge (similarity dedup) + entity prune/classify. `tracedecay memory curate` (CLI) does the same headless. | No unified "doctor" action list: **reap orphan entities, prune oplog, rebuild FTS, rebuild banks, VACUUM/reclaim, retention sweep** — none are offered as discoverable actions. Several have no code path at all (see §5). |

### 3.3 The status surface — now surfaced (was "hidden" at audit time)

`TraceDecay::memory_status()` (`src/tracedecay.rs:3386`) computes a rich health
object — `MemoryStatus` (`src/memory/types.rs:161`):

```
fact_count, entity_count, bank_count, algebra_name, hrr_dim,
estimated_capacity,                 // 2048/ln(2048) ≈ 269 facts/bank ceiling
trust_{0_025,025_050,050_075,075_100}_count,
below_default_recall_threshold_count,// facts below DEFAULT_MIN_TRUST (0.3)
helpful_count, unhelpful_count,
missing_vector_count,               // facts missing/legacy HRR vectors
legacy_backfill_complete,
repair: { missing_vectors_repaired, banks_rebuilt }
```

**Status at audit time (historical):** this object was reachable **only** via
the `tracedecay_memory_status` MCP tool; the dashboard never called it (the only
callers were side-effecting readiness probes in `dashboard/mod.rs:141` and
`memory_curate.rs:117`, whose return value was discarded), and the `tracedecay`
CLI had no `memory status` subcommand (`MemoryAction` had no `Status` variant).
So the richest existing health object was invisible to humans.

**Resolved by Q3 (landed):** a `Status` arm was added to `MemoryAction`
(`src/cli.rs`, `src/main.rs`), giving `tracedecay memory status` (human +
`--json`); the dashboard route `GET /api/plugins/holographic/status`
(`src/dashboard/mod.rs:265`, `src/dashboard/memory_api.rs`) returns the same
payload (plus largest-bank utilization); and a Memory Health card renders it in
the dashboard. The MCP tool doc now cross-references the CLI and dashboard
equivalents (`src/mcp/tools/definitions.rs:1801`). **G2 is closed.**

---

## 4. Gap list (consolidated, severity-ranked)

| ID | Gap | Severity | Root cause |
|---|---|---|---|
| G1 | **No memory doctor** (vs LCM's full diagnose/repair/clean flow) | High | No `memory` doctor module; only `curate`. |
| ~~G2~~ | ~~**`MemoryStatus` not surfaced** to dashboard or CLI~~ | ~~High~~ | **Resolved by Q3.** `memory status` CLI (`MemoryAction::Status`), `GET /api/plugins/holographic/status`, and a dashboard Memory Health card now surface it. |
| G3 | **Store size** never broken out by memory table; not in dashboard | High | `doctor`/`--runtime` report whole-DB bytes only; no `dbstat`/per-table rollup. |
| G4 | **Orphan entities invisible** without a curation run (5 now) | High | No standing diagnostic; only reported inside a curate plan. |
| G5 | **Stale / never-recalled memories invisible** (129/129 here) | High | `last_recalled_at` stored but unused for any signal. |
| G6 | **FTS index health not checked** (no `rebuild_needed` like LCM) | Med | No FTS row-sync/`rebuild` probe for `memory_facts_fts`. |
| G7 | **Trust-decay status not surfaced** (decay is query-time only) | Med | No "effective trust" view; `below_default_recall_threshold_count` hidden. |
| G8 | **Bank capacity/utilization (269 ceiling) not shown** | Med | `estimated_capacity` computed in `MemoryStatus`, never displayed. |
| G9 | **No maintenance action menu** (reap/prune/rebuild/VACUUM/retention) | Med | Several actions have no code path at all (oplog prune, entity reap, FTS rebuild). |
| G10 | **Vector precision (f64 vs f32) + total vector bytes not shown** | Low | Precision is implicit in the blob; audit recommends f32 migration but nothing displays it. |
| G11 | **Dirty-bank queue & oplog growth not surfaced** | Low | `memory_bank_dirty` (3) and unbounded `memory_oplog` have no visibility. |

---

## 5. Recommendations

Grouped by surface. Each names the metric/action, the proposed location, and the
backend endpoint/data needed. Tiers: **T1** = reuses existing data, low effort;
**T2** = small new query/logic; **T3** = new subsystem (the doctor).

### 5.1 Dashboard cards/actions (frontend `dashboard/holographic/*`)

| # | Recommendation | Tier | Backend needed |
|---|---|---|---|
| D1 | **"Memory Health" card** at top of the Inspector view: fact count, entity count, bank count, HRR dim, **estimated capacity (269) + largest-bank utilization %**, total vector bytes, missing-vector count, below-recall-threshold count. Backed by exposing `MemoryStatus` (D2). | T1 | New `GET /api/plugins/holographic/status` returning `memory_status()`. |
| D2 | **Store-size breakdown card**: per-table bytes (`memory_facts`, `memory_facts_fts*`, `memory_entities`, `memory_fact_entities`, `memory_banks`, `memory_oplog`), memory-subsystem total, and "% of whole DB". A donut/composition. | T2 | New endpoint running `dbstat` rollup (or cached `SUM(length(hrr_vector))` + table-size probes). |
| D3 | **Index-health strip**: FTS sync status (`memory_facts` rows vs `memory_facts_fts` doc count), bank-freshness aggregate (reuse existing `hrr_coverage` `stale_bank`), dirty-bank queue depth (`memory_bank_dirty`), `journal_mode` + `auto_vacuum` state. | T2 | New endpoint; FTS sync = two `COUNT(*)`s; the rest are one-shot PRAGMAs/counts. |
| D4 | **Stale-memory card**: counts of `last_recalled_at IS NULL` and `last_recalled_at < now − N days` (default 180, matching the decay half-life), with a drill-in list. | T2 | New endpoint; one `GROUP BY`/`CASE` query over `memory_facts`. |
| D5 | **Orphan-entities badge + action**: surface the orphan count (5 now) as a standing badge on the Entities card with a one-click "Reap N orphans" button calling the doctor's reap action (§5.3). | T2 | Doctor diagnostics + reap action (M3). |
| D6 | **Maintenance action menu** (Curation tab or new "Health" tab): Reap orphan entities · Prune oplog · Rebuild FTS · Rebuild banks · Vacuum/reclaim · Run retention sweep. Each dry-run-first with a confirm step, mirroring LCM doctor's `mode`/`apply` shape. | T3 | The doctor endpoint set (§5.3). |

### 5.2 CLI (`src/cli.rs` / `src/main.rs`)

| # | Recommendation | Tier | Backend needed |
|---|---|---|---|
| C1 | ~~**`tracedecay memory status`** — print `MemoryStatus` (human + `--json`).~~ | T1 | **Done (Q3).** `MemoryAction::Status` in `src/cli.rs`/`src/main.rs` surfaces `cg.memory_status()` (human + `--json`). |
| C2 | **`tracedecay memory doctor`** — the CLI twin of the dashboard doctor: diagnose → plan → (`--apply`) repair/clean. Mirrors `tracedecay doctor` ergonomics and the LCM `doctor` mode/apply shape. | T3 | The doctor module (§5.3). |
| C3 | **Extend `tracedecay doctor`** with a "Memory" section: fact/entity/bank counts, missing vectors, orphan entities, FTS sync, oplog rows, memory-subsystem bytes. Reuse the doctor diagnostics so the single command is the one-stop health check. | T2 | Doctor diagnostics (read-only subset). |
| C4 | **Extend `status --runtime`** (`RuntimeSnapshot`) with memory counts: `fact_count`, `memory_bytes` (subsystem), `vector_bytes`. Currently it only reports graph `node_count`/`edge_count` + whole-DB bytes. | T2 | Add fields to `DatabaseSnapshot`; 2–3 more scalar queries in `sample_database`. |

### 5.3 Backend — the missing memory doctor (highest-leverage new work)

Model it on `src/sessions/lcm/doctor.rs`: a `gather_diagnostics` →
`plan_and_apply_repairs` pipeline with `mode ∈ {diagnose, repair, clean}` and
`apply: bool`, plus a pre-apply DB backup (LCM already has
`backup_database`/`checkpoint_wal_for_backup` to reuse).

**Diagnostics to gather** (all cheap read-only SQL; current values in parens):

- Schema: migration present + current vs expected (mirrors LCM).
- Counts: facts (129), entities (686), fact↔entity joins (986), banks (6),
  **oplog rows (0, unbounded)**, feedback events (0), dirty-bank queue (3).
- Vector health: `missing_vector_count` (0), `hrr_dim`/`hrr_algebra` uniformity,
  **total vector bytes (2.11 MiB)**, per-category count vs **estimated_capacity
  (269)** → utilization % + "noisy bank" warning when a category exceeds ~269.
- Entity health: **orphan-entity count (5)** —
  `SELECT COUNT(*) FROM memory_entities e WHERE NOT EXISTS (SELECT 1 FROM memory_fact_entities fe WHERE fe.entity_id=e.entity_id)`.
- FTS health: `memory_facts` row count vs `memory_facts_fts` doc count →
  `rebuild_needed` bool (mirrors LCM's FTS detection); FTS table bytes (49 KiB).
- Staleness: `last_recalled_at IS NULL` (129/129) and
  `last_recalled_at < now − 180d`; facts with `trust_score < DEFAULT_MIN_TRUST`
  (0 now); facts `updated_at < now − 180d`.
- Bank freshness: aggregate of existing `hrr_coverage` `stale_bank`/`missing_bank`;
  dirty-queue depth.
- Storage/compaction: `journal_mode`, `auto_vacuum`, freelist page count, and the
  memory-subsystem `dbstat` rollup (2.43 MiB) vs whole-DB size (78.5 MiB).

**Planned/applied actions** (each `safe: bool`, description, candidate_count):

| Action | Exists today? | Notes |
|---|---|---|
| `rebuild_memory_fts` | ❌ new | `INSERT INTO memory_facts_fts(memory_facts_fts) VALUES('rebuild')`. Mirrors LCM `rebuild_summary_fts`. |
| `reap_orphan_entities` | ❌ new | One `DELETE ... WHERE NOT EXISTS (...)`. Removes the 5 orphans. |
| `rebuild_dirty_banks` | ✅ `rebuild_dirty_banks` exists | Just needs a doctor entry point. |
| `prune_oplog` | ❌ new | Cap `memory_oplog` by age/count. No prune path exists today (confirmed). |
| `vacuum_reclaim` | ✅ `TraceDecay::optimize` | Report freelist/reclaimable bytes; apply `VACUUM` (or `incremental_vacuum` after enabling it). |
| `retention_sweep` | ❌ new | Hard-delete facts with `trust_score < MIN_TRUST AND updated_at < now − MAX_AGE`. Surfaces the existing decay math as a real cleanup path (audit rec #2). |

### 5.4 MCP

| # | Recommendation | Tier |
|---|---|---|
| M1 | **`tracedecay_memory_doctor`** tool — the agent-callable twin of LCM's `tracedecay_lcm_doctor`, wrapping §5.3. Restores parity between the two subsystems. | T3 |
| M2 | ~~Document/promote **`tracedecay_memory_status`** (already exists) — make it discoverable (it is not referenced by any UI/CLI).~~ | T1 | **Done (Q3).** The MCP tool description now points humans to `tracedecay memory status` and `GET /api/plugins/holographic/status` (`src/mcp/tools/definitions.rs:1801`). |
| M3 | Add the **entity-reap** and **oplog-prune** as discrete safe actions the doctor (and thus agents) can call. | T2 |

---

## 6. Backend endpoints / data needed (consolidated)

To support §5.1–§5.4 the backend needs:

1. ~~**`GET /api/plugins/holographic/status`** — wraps `cg.memory_status()`.~~
   **Done (Q3).** Route wired at `src/dashboard/mod.rs:265` returning
   `memory_status()` + largest-bank utilization; unblocks D1/C1. (Additive field
   — preserves the shape-compat rules in `DASHBOARD-API-AUDIT.md` §6 P18/P19.)
2. **`GET /api/plugins/holographic/health`** (or `/doctor?mode=diagnose`) — the
   diagnostics bundle from §5.3. New read-only queries; no schema change.
3. **`POST /api/plugins/holographic/doctor`** `{mode, apply}` — plan+apply
   repairs with pre-apply backup. The memory twin of LCM's doctor endpoint.
4. **`RuntimeSnapshot` extension** — add `fact_count`, `memory_bytes`,
   `vector_bytes` to `DatabaseSnapshot` (C4).
5. **New repair primitives** (backend functions the doctor calls):
   `rebuild_memory_fts`, `reap_orphan_entities`, `prune_oplog`,
   `retention_sweep`. None exist today; all are small and go through
   `MemoryStore` canonical paths (respect `DASHBOARD-API-AUDIT.md` P4/P5 —
   deletion stays hard-delete + FK-cascade + oplog).
6. **No schema migration required** for any of the above — every signal is
   derivable from existing columns/tables/PRAGMAs.

---

## 7. Suggested sequencing

1. **T1 surfacing (partly done):** C1 `memory status` CLI, D1 Health card, and
   M2 MCP discoverability **landed via Q3**, closing G2 and G8 (read side) with
   almost no new logic. D2 store-size card (G3 — needs a `dbstat`/per-table
   rollup) is still open.
2. **T2 diagnostics:** D3 index-health strip, D4 stale-memory card, D5
   orphan-entity badge, C3 extend `doctor`, C4 extend `--runtime`. Closes G4,
   G5, G6, G7, G8 read side.
3. **T3 the doctor:** §5.3 module → C2 `memory doctor` CLI + M1
   `tracedecay_memory_doctor` + D6 maintenance menu + the four new repair
   primitives. Closes G1, G9 and gives every read-side gap a matching *action*.

The Q3 portion of step 1 has landed — `MemoryStatus` is no longer unreachable
by humans. The store-size breakdown (G3, D2) and step 3 (the memory doctor)
remain open; step 3 is what brings memory to parity with LCM's operational
story.

---

## 8. Artifacts referenced

- CLI: `src/cli.rs` (`Commands`, `MemoryAction`, `BranchAction`), `src/main.rs`
  (`Status:520`, `Doctor:1281`, `Memory:1496`), `src/doctor.rs`,
  `src/runtime_telemetry.rs` (`RuntimeSnapshot:27`, `DatabaseSnapshot:55`).
- MCP: `src/mcp/tools/definitions.rs:1789` (`def_memory_status`),
  `src/mcp/tools/handlers/memory.rs:333` (`handle_memory_status`),
  `src/sessions/lcm/doctor.rs` (parity baseline).
- Dashboard backend: `src/dashboard/memory_api.rs:184` (`overview_payload`),
  `:145` (`trust_histogram`), `:225` (`hrr_coverage`).
- Dashboard frontend: `dashboard/holographic/src/HolographicMemoryPage.tsx`,
  `CurationPanel.tsx:32` (`DIAGNOSTIC_COUNT_KEYS`), `api.ts`.
- Data: `src/memory/types.rs:161` (`MemoryStatus`), `:155` (`MemoryRepairStats`),
  `src/tracedecay.rs:3386` (`memory_status`), `:3367` (`repair_derived_memory`).
- Prior audits: `docs/DASHBOARD-API-AUDIT.md`, `docs/MEMORY-STORAGE-GROWTH-AUDIT.md`,
  `docs/HOLOGRAPHIC-DASHBOARD-SEAMS.md`.
- Live DB: `.tracedecay/tokensave.db` (129 facts; memory subsystem 2.43 MiB of
  78.5 MB total; 5 orphan entities; 3 dirty banks; 129/129 facts never recalled).

*Generated for Kanban task t_f47ae50b. Source audited at the `master` working
tree (commit range around `5ad31c4`). All numbers measured read-only against the
live checkout DB.*
