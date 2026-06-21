# Holographic Memory — Storage Growth Audit

Scope: the holographic (FHRR) memory subsystem that tracedecay exposes to Hermes
as the `tracedecay` memory provider. This is the `src/memory/` module plus its
SQLite tables in the per-project `tracedecay.db` / `tracedecay.db`. The LCM
session store (`sessions.db`) and the code-graph node store are out of scope but
referenced where they share infrastructure.

All numbers below are measured against the live checkout DB
(`.tracedecay/tracedecay.db`, 129 facts) unless stated otherwise.

---

## 1. Where the data lives

Tables in `tracedecay.db` (schema in `src/db/migrations.rs`, runtime access in
`src/memory/store.rs`, `src/memory/retrieval.rs`):

| Table | Role | Growth | Observed (129 facts) |
|---|---|---|---|
| `memory_facts` | One row per fact; **holds the per-fact FHRR vector blob** | linear in fact count | 129 rows, **2.26 MiB** |
| `memory_banks` | Superposed (averaged) vector per category + `all` | bounded (~6 rows) | 6 rows, 106 KiB |
| `memory_entities` | Unique normalized entities | sub-linear | 686 rows, 57 KiB |
| `memory_fact_entities` | fact↔entity join | linear in fact×entities | 986 rows |
| `memory_facts_fts` | FTS5 shadow of `content`+`tags` | linear in fact count | 49 KiB |
| `memory_feedback_events` | Append-only helpful/unhelpful events | linear in feedback volume | 0 rows |
| `memory_oplog` | Append-only write log (add/reject/remove/merge) | linear in write-ops | 0 rows |
| `memory_bank_dirty` | Dirty-bank rebuild queue | bounded | 3 rows |
| `vectors` | Code-graph node embeddings (NOT memory) | per graph node | 0 rows here |

Schema anchors (`sqlite3 .tracedecay/tracedecay.db ".schema <t>"`):
- `memory_facts(... hrr_vector BLOB, hrr_algebra TEXT DEFAULT 'amari_fhrr', hrr_dim INTEGER DEFAULT 2048, access_count, last_recalled_at)`
- `memory_banks(bank_name UNIQUE, vector BLOB, hrr_dim, fact_count, updated_at)`
- `memory_entities(entity_id PK, name, normalized_name UNIQUE, entity_type, aliases, created_at)`
- `memory_fact_entities(fact_id, entity_id, PK(fact_id,entity_id), FK CASCADE both)`
- `memory_feedback_events(event_id PK, fact_id FK CASCADE, action, trust_delta, old/new_trust, created_at, source, note)`
- `memory_oplog(id PK, ts, op, fact_id, detail_json)` — index `idx_memory_oplog_ts`

FTS5 `memory_facts_fts(content, tags)` is external-content (`content='memory_facts'`)
kept in sync by `AFTER INSERT/DELETE/UPDATE` triggers on `memory_facts`.

---

## 2. The encoding (what a "memory" is, byte-wise)

`src/memory/encoding.rs` — `HolographicEncoder`:
- Algebra: `amari_holographic::FHRRAlgebra<2048>` (crate `amari-holographic` 0.23.0),
  i.e. **Fractional HRR, 2048 dimensions**.
- Each fact is encoded as a 2048-dim vector of `f64` phase coefficients
  (`encode_fact`: binds `role_content ⊗ content` plus one `role_entity ⊗ entity`
  binding per entity, then averages + L2-normalizes).
- Persistence: `HolographicEncoder::serialize` = `bincode::serialize(Vec<f64>)`.

**Measured blob size: `length(hrr_vector) == 16392` bytes for every fact** =
8-byte bincode length prefix + 2048 × 8-byte f64. This is FIXED regardless of
content length and is **~93% of every fact row**.

---

## 3. Per-memory cost (decomposed, observed)

From `SELECT MIN/MAX/AVG(length(...)) FROM memory_facts` (n=129):

| Component | Avg bytes | Notes |
|---|---|---|
| `hrr_vector` blob | **16,392** | fixed; 93% of the row |
| `content` TEXT | 392 (max 926) | variable |
| `metadata` TEXT (JSON) | 81 | |
| `tags` TEXT (JSON) | 47 | |
| fixed int/real/text columns (~15 cols) | ~120 | trust, counts, 5 timestamps, source, algebra, dim |
| row + UNIQUE(content) idx + 4 secondary idx, b-tree overhead | ~250 | |
| **memory_facts row total** | **≈ 17,280** | matches observed 2,260,992 B / 129 = **17,528 B/row** |
| `memory_facts_fts` shadow (data+idx+docsize) | ≈ 381 | 49,152 B / 129 |
| `memory_fact_entities` joins (≈7.6 entities/fact × 16 B) | ≈ 120 | + amortized entity row ~25 B |
| **all-in per fact** | **≈ 18 KiB** | |

Banks are constant: ~6–7 rows × 16,392 B vector ≈ **0.1 MiB regardless of N**
(one averaged superposition per category + `all`).

---

## 4. Worked size estimates (1k / 10k / 100k facts)

Formula (memory subsystem only, all-in ≈ 18 KiB/fact + small linear tails):

```
memory_facts  ≈ 17.5 KiB × N          (dominant)
memory_facts_fts ≈ 0.4 KiB × N
entities/joins ≈ 0.15 KiB × N          (sub-linear in entity rows)
oplog ≈ 0.12 KiB × (write-ops)         (~1–1.2 rows per add incl. rejects/merges)
banks ≈ 0.1 MiB                         (constant)
```

| N facts | facts | FTS | banks | entities/joins/oplog | **Total** |
|---|---|---|---|---|---|
| 1,000  | 17.1 MiB | 0.4 MiB | 0.1 MiB | ~0.4 MiB | **≈ 18 MiB** |
| 10,000 | 171 MiB  | 3.7 MiB | 0.1 MiB | ~4 MiB   | **≈ 179 MiB** |
| 100,000| 1.68 GiB | 37 MiB  | 0.1 MiB | ~40 MiB  | **≈ 1.76 GiB** |

Takeaway: growth is **essentially linear and dominated by the 16.4 KiB f64 FHRR
blob** — 93% of every fact, 95%+ of the whole subsystem. Everything else is
rounding error at scale.

---

## 5. Holographic capacity vs. storage capacity (two different limits)

These are NOT the same and the audit needs both:

- **Bank superposition capacity (quality ceiling):**
  `src/tracedecay.rs:3472`
  `estimated_capacity = round(hrr_dim / ln(hrr_dim))` → **2048 / ln(2048) ≈ 269
  facts/bank**. Beyond ~269 facts averaged into one bank vector, the superposition
  becomes noisy and bank-accelerated recall degrades. This is a *recall-quality*
  limit per category bank, not a storage limit.

- **Per-fact storage capacity (disk):** unbounded. Recall does **not** depend on
  banks for correctness — `FactRetriever::search` (`src/memory/retrieval.rs:36`)
  builds candidates from FTS5 BM25 + entity match + `list_facts` (top-N by
  `updated_at DESC`), then scores each candidate's **individually stored** vector
  (phase-cosine, weighted FTS 0.40 / Jaccard 0.30 / holographic 0.30 / trust /
  temporal-decay). So the per-fact vector is mandatory for precise retrieval and
  grows without bound.

Net: storage scales O(N) with no hard cap; bank quality degrades past ~269
facts/category but that only hurts the superposition fast-path, not correctness.

---

## 6. What grows unbounded / has no cleanup path

1. **`memory_facts.hrr_vector` — 16.4 KiB/fact, linear, NO cap.** Dominant term.
   (`src/memory/store.rs` `add_fact` → always INSERTs the vector.)
2. **`memory_facts` rows overall — no retention, no TTL.** No `max_facts` config;
   `src/config.rs` has **zero** memory/trust/recall/retention settings.
3. **`memory_oplog` — append-only, NEVER pruned.** `grep -r "DELETE FROM
   memory_oplog" src/` = 0 matches. One row per write-op (add/reject/remove/merge)
   forever. Stores `detail_json` incl. content hashes.
4. **`memory_feedback_events` — append-only.** No prune path; only CASCADE-deleted
   when a fact is removed.
5. **`memory_entities` — no reaper.** Entity rows persist forever; deleting a fact
   removes the join row but leaves the entity orphaned. **Already 5 orphaned
   entities in a 129-fact checkout.** `grep` finds no `remove_entity` /
   entity-delete path.
6. **No space reclamation: `PRAGMA auto_vacuum = 0`** and no scheduled `VACUUM`.
   Deleted 16.4 KiB blobs go to the freelist (currently 0, no deletes yet) but the
   DB file never shrinks. Curation-driven deletions will accumulate dead pages
   until a manual `VACUUM`.

## 7. Existing controls (what IS present)

- **Exact-dup rejection:** `content` is `UNIQUE`, insert is `INSERT OR IGNORE`
  (`store.rs:166`). Exact re-adds merge entities instead of duplicating.
- **Near-dup report (write-time):** `near_duplicate_diff` (`store.rs:269`) flags
  similarity > 0.9, but **only skips the insert when content is
  `normalized_equivalent`** (case/whitespace). Semantic near-dups with different
  wording are **still stored** — it reports, it does not dedup-store.
- **Secret gate:** `detect_secret_like` (`hygiene.rs`) rejects credential-like
  content; only a content hash hits the oplog. (Hard reject, not storage bloat.)
- **Transient flag:** `detect_transient` marks ephemeral-looking facts but only as
  a *curation proposal* — never auto-deletes.
- **Trust + recall floor:** feedback adjusts `trust_score` (helpful +0.05 /
  unhelpful −0.10, clamped [0,1]); `DEFAULT_MIN_TRUST = 0.3` floors recall. Low-
  trust facts sink out of results but **are not deleted** — they still occupy
  their 16.4 KiB.
- **Temporal decay (ranking only):** only `retrieval.rs::temporal_decay_factor`
  (365-day half-life, floored at 0.10) affects scoring. The stored `trust_score`
  **never decays on disk**; decay is recomputed at query time. (Former `trust.rs::temporal_decay`
  was dead code with zero production callers and has been removed.)
- **Manual/curation deletion:** `remove_fact` / `delete_facts(loser_ids)`
  (`store.rs:547–587`) is the **only** removal path — driven by the curation
  panel / explicit MCP call, never automatic.
- **Bank rebuild:** `rebuild_dirty_banks` re-averages superposition vectors;
  bounded by category count.
- **Missing-vector repair:** status `repair` / `legacy_backfill_complete`
  **adds** vectors to facts missing them — increases storage, never compacts.

---

## 8. Recommendations (ranked by impact)

1. **Halve the vector width: f64 → f32 (8.2 KiB/fact, −50%).**
   `encoding.rs` serializes `Vec<f64>`. FHRR similarity is phase-cosine over
   `[-π,π]`; f32 phase precision is well within the noise margin of a 2048-dim
   binding algebra. Store `Vec<f32>` (8,192 + 8 = 8,200 B). Transparent
   migration via the existing `hrr_dim`/`hrr_algebra` columns (add
   `hrr_precision`) + a one-shot backfill like the existing legacy-vector repair.
   Biggest single lever; cuts every estimate above in half.

2. **Add a retention sweep (TTL + trust floor).** A periodic job that hard-deletes
   facts with `trust_score < MIN_TRUST` AND `updated_at < now − MAX_AGE` (e.g.
   180 d, matching the decay half-life). Surfaces the existing decay math as a
   real cleanup path instead of ranking-only. Wire thresholds into `config.rs`
   (currently has none).

3. **Prune the oplog.** Cap `memory_oplog` by row count or age
   (`DELETE FROM memory_oplog WHERE ts < ?`); it currently grows forever.

4. **Reap orphaned entities.** After fact deletes, run
   `DELETE FROM memory_entities WHERE NOT EXISTS (SELECT 1 FROM
   memory_fact_entities WHERE entity_id = memory_entities.entity_id)`. Already
   5 orphans in a tiny checkout.

5. **Enable `PRAGMA auto_vacuum = INCREMENTAL`** (set at DB creation; for existing
   DBs, `VACUUM` once then flip) and schedule periodic `PRAGMA
   incremental_vacuum`. Otherwise curation deletes never reclaim the 16.4 KiB
   blob pages.

6. **Promote semantic near-dup to optional merge.** `near_duplicate_diff` already
   computes >0.9 similarity; offer a config to auto-merge (or auto-replace on
   supersession) instead of storing both. Today two paraphrases of the same fact
   cost 2 × 16.4 KiB.

7. **Add a `max_facts` / byte budget** with an LRU/trust-based eviction policy so
   the store has a hard ceiling regardless of ingestion rate.

8. **Minor: stale `memory_banks.fact_count`.** Observed `all` bank `fact_count =
   130` vs 129 actual facts — the dirty-bank rebuild can leave a stale count.
   Recompute `fact_count` from `COUNT(*)` on rebuild rather than incrementally.

---

## 9. Artifacts referenced

- Encoding: `src/memory/encoding.rs` (`HolographicEncoder`, DIMENSIONS=2048, bincode)
- Store / writes: `src/memory/store.rs` (`add_fact:70`, `near_duplicate_diff:269`,
  `remove_fact:561`, `rebuild_bank:992`, `log_oplog:813`)
- Retrieval / scoring: `src/memory/retrieval.rs:36` (search), `:698` (combined_score),
  `:712` (temporal_decay_factor)
- Capacity formula: `src/tracedecay.rs:3472`
- Trust/decay: `src/memory/trust.rs`
- Hygiene gates: `src/memory/hygiene.rs`
- Schema: `src/db/migrations.rs`; live DB `.tracedecay/tracedecay.db`
- Crate: `amari-holographic = "0.23.0"` (`Cargo.toml:144`)
