# Dashboard API Layering — Route / Service / Query Design

A concrete module layout for separating the `tracedecay dashboard` HTTP API into
three concerns — **route handlers**, **service/domain logic**, and
**query/repository helpers** — across the three audited domains
(`memory_api`, `lcm_api`, `graph_api`).

This is a **design note + implementation plan**, not a code change. The
acceptance criteria (target files/modules, migration order, compatibility
constraints, explicit non-goals) are the section headings below.

It builds directly on [`DASHBOARD-API-AUDIT.md`](./DASHBOARD-API-AUDIT.md) (the
audit): route inventory, SQL hotspots, fan-in/fan-out, and the 21
"preserve-during-refactor" items (referenced below as **P1–P21**). Every
structural decision here is checked against those items. Read the audit first if
you have not.

---

## 1. Goal, scope, and non-negotiables

**Goal.** Move the three audited route modules from "one file does HTTP + domain
logic + inline SQL + caches" toward a three-layer split, *without changing any
observable behavior*.

**In scope.** `src/dashboard/memory_api.rs`, `lcm_api.rs`, `graph_api.rs`, plus
the shared `util.rs` data-access contract and the curation seam into
`memory_curate.rs`.

**Out of scope** (see §13 Non-goals): `savings_api.rs`, `savings_pricing.rs`,
`token_count.rs`, `assets.rs`, the router/server lifecycle in `mod.rs`, and the
canonical `src/db/` layer.

**Non-negotiable constraints** (each maps to audit items):

| # | Constraint | Source |
|---|---|---|
| C1 | One `router(state)` builder stays the single source of route registration; the route table in `mod.rs` does not change. | P21, §1 |
| C2 | Every route path and JSON key stays byte-identical (standalone UIs + the Hermes reverse-proxy depend on them). | P18, P19, §2 |
| C3 | The fail-soft error contract is preserved: `query_rows → Err(String)` surfaced in the payload `error` field (never a raw 500); `query_i64 → 0` on any error/empty; `JsonPath`/`JsonQuery` rejections stay `{detail}` JSON. | P2, P3 |
| C4 | Deletion stays permanent and goes through `MemoryStore` canonical paths (no raw `DELETE`, no archive/soft-delete). | P4, P5 |
| C5 | The 5 `pub(crate)` curation functions stay a reusable seam for `memory_curate.rs` (the CLI curation path + LLM-review tier). | P20, §5 |
| C6 | Cache keying/invalidation semantics are untouched (per-DB keys, content fingerprints, negative-cache exemption). | P14–P17 |
| C7 | Standalone behavior and Hermes-wrapper compatibility are both preserved — no new auth, no off-loopback exposure. | P1, §11 |

If a migration step would violate any C1–C7, stop and revise the step.

---

## 2. The three-layer model

| Layer | Owns | Does **not** own |
|---|---|---|
| **Route** (`*_api.rs`) | HTTP extractors (`JsonQuery`/`JsonPath`), param deserialization structs, status-code selection (incl. 404 `{detail}`), calling the service, wrapping the result into the final `Json<Value>` payload, the `error`/`detail` envelope. | SQL, business decisions, caches, domain math. |
| **Service** (`*_service.rs`) | Domain logic: assembling overview/search/session/subgraph payloads, FTS→LIKE dispatch, curation planning/applying, degree/PCA/similarity orchestration, cache structs + their `OnceLock` statics, validation gates (`ensure_valid_summary_metadata`). | HTTP status codes, extractor wiring. |
| **Query** (`*_queries.rs`) | SQL strings + column-projection consts + the `query_rows`/`query_i64` call sites that read/write a specific table family. Named, pure functions taking `&Connection` (+ params) → `Result<Vec<Value>, String>` / `i64`. | Payload shaping, HTTP, caches. |

**Why three layers and not two.** The audit shows `format!`-injected SQL is the
dominant code shape (19/24/11 `format!` sites per file) and the duplicated SQL
expressions (degree union ×3, token-estimate ×4, LIKE/FTS dispatch) are the real
maintenance hazard. Giving SQL its own home makes those expressions named,
grep-able, and dedup-able *without* touching domain logic. A two-layer
(route/service) split would leave the SQL interleaved with logic and forfeit the
dedup wins.

**The foundation layer stays.** `util.rs` is the existing de-facto data-access
contract (`query_rows`, `query_i64`, `coerce_limit`, `qmarks`, `like_pattern`,
`build_fts_match`, `http_detail`/`json_error`, `JsonPath`/`JsonQuery`,
`i64_field`/`str_field`/`json_object`). It is **not** renamed or rewritten. The
new `*_queries.rs` modules *call* `util.rs`; they do not replace it. This keeps
C3 (the error contract) pinned in one place.

---

## 3. Target module layout

Flat layout, matching the existing `src/dashboard/` convention and the
`src/db/` precedent (flat domain modules + a shared helper). **No existing file
is renamed or moved** — `*_api.rs` files stay exactly where `mod.rs`'s route
table references them (C1). New sibling files are added.

```
src/dashboard/
├── mod.rs                  # unchanged route table; +mod declarations only
├── util.rs                 # unchanged — the shared data-access contract
├── assets.rs               # unchanged
├── curate_preview_store.rs # unchanged
│
├── memory_api.rs           # ROUTE  (11 handlers) — bodies become thin calls
├── memory_service.rs       # NEW — domain logic + curation seam + projection cache
├── memory_queries.rs       # NEW — facts/entities/banks/oplog SQL
├── memory_analysis.rs      # unchanged — similarity/PCA math (SimilarityComputation)
├── memory_curate.rs        # one-line import update only (§5)
│
├── lcm_api.rs              # ROUTE  (6 handlers) — thin calls
├── lcm_service.rs          # NEW — overview/search/session/node/timeline/compression logic
├── lcm_queries.rs          # NEW — MESSAGE/NODE column consts + aggregates + dedup'd token-estimate
│
├── graph_api.rs            # ROUTE  (6 handlers) — thin calls
├── graph_service.rs        # NEW — DegreeSummary cache + default_subgraph + BFS path
└── graph_queries.rs        # NEW — NODE column consts + node/edge/neighbor/subgraph/path SQL
```

**Why flat, not per-domain subdirectories.** (a) The `*_api.rs` route files must
not move (C1 — `mod.rs` references them by path); (b) `db/` and `dashboard/` are
both already flat with a shared helper module, so this matches the established
convention; (c) incremental migration is cheaper when you add files rather than
relocate them. A future consolidation into `dashboard/{memory,lcm,graph}/`
subdirs is compatible with this plan but is itself a separate, purely-mechanical
follow-up — **not** part of this migration (§13).

### 3a. `mod.rs` change surface

The **only** edit to `mod.rs` is adding module declarations:

```rust
mod lcm_queries;
mod lcm_service;
mod graph_queries;
mod graph_service;
mod memory_queries;
mod memory_service;
```

Route table (`router(state)`) and `capabilities`/`plugins_list` are untouched.
`build_state`, `resolve_lcm_store`, `run`, `bind_dashboard` are untouched.

---

## 4. Per-domain function mapping

Each table shows where current code lands. Line numbers are from the audited
`master` tree (≈ `5ad31c4`) so implementers can grep.

### 4a. memory domain

| Current (in `memory_api.rs`) | Destination | Notes |
|---|---|---|
| 11 route fns: `overview`, `fact_detail`, `projection`, `similarity`, `curation_status`, `curation_activity`, `curation_preview`, `curate`, `curate_apply`, `oplog` (+ the `/` overview alias) | **`memory_api.rs`** (stay) | Bodies shrink to: extract params → `memory_service::…` → wrap `Json`. |
| `overview_payload`, `fetch_facts`, `fetch_entities`, `graph_payload`, `providers_stub` | **`memory_service.rs`** | Domain assembly; call `memory_queries`. |
| `vector_facts` (`:595`), `ProjectionComputation` (`:694`), `PROJECTION_CACHE` | **`memory_service.rs`** | Cache + its `OnceLock` move together (C6). |
| **5 curation fns:** `similarity_computation` (`:906`), `build_delete_plan` (`:1144`), `delete_fact` (`:1184`), `apply_delete_op` (`:1316`), `apply_merge_op` (`:1352`) | **`memory_service.rs`** | Stay `pub(crate)` — the curation seam (C5, §5). |
| Inline facts/entities/banks/oplog/trust-histogram/growth SQL | **`memory_queries.rs`** | Named fns, e.g. `fact_rows(conn, fact_id)`, `oplog_rows(conn, limit)`. |
| `memory_analysis.rs` (`SimilarityComputation` `:288`, PCA/phase-cosine) | **unchanged** | Already separated; `memory_service` calls it. |

### 4b. lcm domain

| Current (in `lcm_api.rs`) | Destination | Notes |
|---|---|---|
| 6 route fns: `overview` (`:213`), `search` (`:456`), `session` (`:712`), `node` (`:847`), `timeline` (`:1002`), `compression` (`:1127`) | **`lcm_api.rs`** (stay) | Thin calls; 404 `{detail}` decisions stay here. |
| overview aggregation, search FTS→LIKE dispatch, session assembly, node/source expansion, timeline bucketing, compression ratio math | **`lcm_service.rs`** | `engine`/`engine_detail` "both-FTS" honesty (P12) lives here. |
| `ensure_valid_summary_metadata` (`:163`) + `VALIDATED_METADATA_STORES` | **`lcm_service.rs`** | Negative-cache exemption preserved (P13). |
| `parse_summary_node_ids` (`:113`) | **`lcm_service.rs`** | Post-processing row mapper (P8). |
| `MESSAGE_COLUMNS` (`:52`), `NODE_COLUMNS` (`:71`), all `SELECT {…}` call sites | **`lcm_queries.rs`** | Column consts + named query fns. |
| Token-estimate expr `(LENGTH(COALESCE(content, snippet_text, '')) + 3) / 4` — duplicated at `:57`, `:770`, `:1042`, `:1051` | **`lcm_queries.rs`** as **one named const** | Real dedup win; must stay byte-identical (P7). |

### 4c. graph domain

| Current (in `graph_api.rs`) | Destination | Notes |
|---|---|---|
| 6 route fns: `overview`, `search`, `node`, `neighbors`, `subgraph`, `path` | **`graph_api.rs`** (stay) | Thin calls; 404 `{detail}` stays here. |
| `DegreeSummary` (`:266`), `degree_summary` (`:283`), `degrees_for_ids` (`:221`), `default_subgraph` (`:654`), BFS path reconstruction, `language_for_path` | **`graph_service.rs`** | `DEGREE_CACHE` + its `OnceLock` move together (C6, P16). |
| Param structs (`SearchParams`, `NeighborParams`, `SubgraphParams`, `PathParams`) | **`graph_api.rs`** (stay) | Tied to HTTP extraction. |
| `NODE_COLUMNS`, `NODE_COLUMNS_N`, all node/edge/neighbor/subgraph/path SQL, the degree `UNION ALL` (×3) | **`graph_queries.rs`** | Degree union becomes **one** named fn (dedup; identical behavior). |

---

## 5. The curation seam (C5) — the one cross-file behavioral coupling

`memory_api.rs` is not just routes: `memory_curate.rs` (the dashboard-free CLI
curation core, including the `--llm`/`--llm-ops` review tier) imports 5
`pub(crate)` functions from it:

```rust
// memory_curate.rs:22 — current
use super::memory_api::{
    apply_delete_op, apply_merge_op, build_delete_plan, delete_fact, similarity_computation,
};
```

**Migration rule:** these 5 move to `memory_service.rs` as **one atomic
change**, and `memory_curate.rs`'s import becomes:

```rust
use super::memory_service::{
    apply_delete_op, apply_merge_op, build_delete_plan, delete_fact, similarity_computation,
};
```

Do **not** rename, merge, or inline these functions — they are a versioned seam
(P20). Keep their signatures, `pub(crate)` visibility, and the fact that
`similarity_computation` delegates to `memory_analysis::SimilarityComputation`.
This is the single change most likely to break the CLI curation path if botched,
so it is gated to its own commit (§12, phase 3a).

---

## 6. Visibility conventions

| Item | Visibility | Rationale |
|---|---|---|
| Route handlers (`*_api::overview`, …) | `pub(crate)` | Referenced by `mod.rs`'s router; already so. |
| Service entry fns called by routes | `pub(crate)` (or `pub(super)`) | Called from the sibling route module. |
| Query fns | `pub(crate)` | Called from the sibling service module. |
| The 5 curation fns | `pub(crate)` | Cross-module seam into `memory_curate`. |
| Cache structs + `OnceLock` statics | private (`static` is module-local); struct is `pub(crate)` only if returned across modules | Move with their service module. |
| Param deserialization structs | `pub(crate)` | Tied to extractors; stay in route module. |
| Column-projection consts (`MESSAGE_COLUMNS`, …) | `pub(crate)` const in `*_queries.rs` | Referenced by sibling service. |

No item becomes `pub` (crate-public) — the whole `dashboard` module is already
gated behind `pub(crate)`/`mod` visibility except `memory_curate` (`pub mod`) and
`assets` (`pub(crate) mod`). Do not widen anything.

---

## 7. Shared types

**Keep response payloads as `serde_json::Value`** — do not introduce typed
response structs for the route bodies. The payloads intentionally mirror the
Python plugin row-dict shapes (P18/P19), and typing them all is high-risk,
low-value churn that violates "minimal behavioral change". This is an explicit
non-goal (§13).

Named types that *do* get extracted/kept:

- **Param structs** (`SearchParams`, `PathParams`, …) — stay in `*_api.rs`,
  `#[derive(Deserialize)]`, `pub(crate)`.
- **Cache structs** (`DegreeSummary`, `ProjectionComputation`,
  `SimilarityComputation`) — live in their service module (or
  `memory_analysis.rs`, unchanged).
- **`DashboardState`** — unchanged in `mod.rs`.
- If a domain needs a small internal typed model (e.g. a parsed oplog entry), it
  is `pub(crate)` and local to that service module. Do **not** create a shared
  `dashboard/types.rs` — there is no cross-domain type sharing today, and
  inventing one is premature.

---

## 8. Shared SQL & the de-facto query contract

`util.rs` is the contract; `*_queries.rs` modules are its per-domain consumers.
Rules for the query layer:

1. **Every query fn calls `util::query_rows` / `util::query_i64`** — never
   `conn.query(...)` directly. (C3: one error contract.) The inline
   `conn.query` sites the audit flagged (`memory_api` 3, `lcm_api` 1,
   `graph_api` 4) are migrated onto the helpers as they move.
2. **Param-binding discipline is mandatory.** User input is always bound
   (`?1`, `qmarks(n)`, `like_pattern`), never string-interpolated. The
   `format!`-injected *column-list consts* and *placeholder lists* are literals,
   so they stay (no injection surface) — but a query fn must not `format!` user
   data into SQL.
3. **Duplicated expressions become named consts/fns in their domain's queries
   module** — degree `UNION ALL` (graph, 3 sites → 1 fn), token-estimate
   (lcm, 4 sites → 1 const), the LIKE/FTS dispatch builders (lcm). Byte-identical
   behavior is required (P7, P10, P11).
4. **No cross-domain shared SQL module yet.** The token-estimate duplication is
   *within* lcm only (within the dashboard); the degree union is graph-only;
   LIKE/FTS helpers already live in `util.rs`. If genuine cross-domain SQL
   duplication appears later, extract a `dashboard/shared_sql.rs` then — not now
   (§13).

---

## 9. Error handling contract (preserve — C3)

The dashboard deliberately does **not** use the canonical `crate::errors::Result`
/ `TraceDecayError` path in its route handlers. It uses a fail-soft contract so
the UIs never see a raw 500 on a bad/missing DB (mirroring the original Python
APIs). The layering must preserve this exactly:

- **Query layer** returns `Result<Vec<Value>, String>` (from `query_rows`) or
  `i64` (from `query_i64`, collapsing errors/empty → 0).
- **Service layer** returns `Result<Value, String>` (or a domain result). It does
  **not** convert `String` errors into `TraceDecayError`. Curation apply paths
  keep their per-op inline result reporting (`{results, counts:{deleted,merged,errors}}`,
  HTTP 200 even on per-op failure — audit §3a route 9/10).
- **Route layer** maps: `Ok(payload)` → `Json(payload)` with `error: ""` (or
  omitempty); `Err(msg)` → same payload shape with `error: msg`; missing-entity
  → `(StatusCode::NOT_FOUND, Json(http_detail(...)))` preserving the FastAPI
  `{detail}` shape (P3).
- `JsonPath`/`JsonQuery` rejection bodies stay `{detail}` JSON (P3). These
  extractors live in `util.rs` and are unchanged.

**Non-goal:** do not unify the dashboard error model onto `TraceDecayError` or
add `?`-propagation that turns SQL errors into 500s. That would break UI error
rendering (C3).

---

## 10. Caches (preserve — C6)

Caches move with their owning service module; their keying and invalidation do
not change:

| Cache | Destination | Preserve |
|---|---|---|
| `PROJECTION_CACHE` / `ProjectionComputation` | `memory_service.rs` | Fingerprint metadata-only, no blob hashing (P14); `spawn_blocking` single-flight (P15). |
| `SIMILARITY_CACHE` / `SimilarityComputation` | (already in `memory_analysis.rs`) | Same fingerprint rule. |
| `DEGREE_CACHE` / `DegreeSummary` | `graph_service.rs` | `(COUNT(*),MAX(id))` of edges; known node-only-edit blind spot (P16). |
| `VALIDATED_METADATA_STORES` | `lcm_service.rs` | One-shot; **failures not cached** (P13). |
| `curate_preview` (`DashboardState`) | unchanged (in `mod.rs`) | Cleared on apply; persisted via `curate_preview_store`. |

All caches stay keyed by `mem_db_path`/`lcm_db_path` because one process can
serve multiple projects via the MCP tool (P17).

---

## 11. Standalone vs Hermes-wrapper compatibility (C7, P1)

**Standalone** (`tracedecay dashboard`, the MCP `tracedecay_dashboard` tool):
unchanged. One `router(state)` builder, one `build_state`, loopback-only
binding. The audit's auth note (P1) holds: **do not expose any route
off-loopback without adding auth.** No new auth is introduced by this layering.

**Hermes wrapper** (`dashboard/hermes-wrapper/plugin_api.py`): a thin reverse
proxy that forwards fixed path prefixes (`/holographic/* → /api/plugins/holographic/*`,
`/lcm/*`, `/graph/*`, `/savings/*`) and adds the session-token middleware.
Because the layering changes **neither route paths nor JSON keys nor the
`/api/capabilities` feature flags** (C2), the wrapper requires **zero changes**.
`POST /curation/llm-plan` stays wrapper-only; the standalone `memory_curate`
mirror (§5) is unaffected.

**Verification gate for compatibility:** after the migration, the reverse-proxy
prefix map in `plugin_api.py` must still resolve every advertised route, and
`/api/capabilities` must still report the same `features`/`dashboards`. Add a
tiny static test asserting the set of registered routes is unchanged (§12).

---

## 12. Migration order

Principle: **leaf domains first, queries before services, every step compiles
and keeps existing tests green, code moves behind re-export shims.**

### Pre-flight (once, before any domain)
- **Add characterization tests.** There is **no integration test coverage** for
  the dashboard in `tests/` today; the only safety net is in-module unit tests
  in `util.rs`, `memory_analysis.rs`, `memory_curate.rs`, `token_count.rs`,
  `savings_*.rs`. Before moving any handler, add golden-payload snapshot tests
  (seed a tiny in-memory libsql DB, hit the service/route fn, assert the exact
  JSON) for at least: one memory overview + `curate` dry_run, one lcm overview +
  search (FTS and LIKE paths), one graph overview + subgraph + path. These tests
  are the regression net for C2/C3. Keep them after migration.
- **Add a registered-routes test** (regex over the `router()` builder, or a
  static `&[&str]` of paths checked in a test) to prove C1.

### Phase 1 — graph domain (lowest coupling)
graph is a leaf module whose only internal coupling is the `DegreeSummary` cache
shared between `overview` and `default_subgraph` (both move to
`graph_service.rs` together).
1. **1a queries:** create `graph_queries.rs`; move `NODE_COLUMNS(_N)`, all
   node/edge/neighbor/subgraph/path SQL, and the degree `UNION ALL` (→ one fn).
   Route/service still call them via `graph_queries::…`. Compile + tests green.
2. **1b service:** create `graph_service.rs`; move `degree_summary`,
   `degrees_for_ids`, `default_subgraph`, BFS path, `language_for_path`, plus
   `DegreeSummary` + `DEGREE_CACHE`. Route handlers now call
   `graph_service::…`. Compile + characterization tests green.

### Phase 2 — lcm domain (leaf module)
1. **2a queries:** create `lcm_queries.rs`; move `MESSAGE_COLUMNS`/`NODE_COLUMNS`
   and the `SELECT {…}` call sites; **dedup the 4 token-estimate sites into one
   const** (assert byte-identical). Green.
2. **2b service:** create `lcm_service.rs`; move overview/search/session/node/
   timeline/compression logic, `ensure_valid_summary_metadata` +
   `VALIDATED_METADATA_STORES`, `parse_summary_node_ids`. Green.

### Phase 3 — memory domain (highest coupling; do last)
1. **3a queries:** create `memory_queries.rs`; move facts/entities/banks/oplog/
   trust-histogram/growth SQL. Green.
2. **3b service + seam (atomic):** create `memory_service.rs`; move
   `overview_payload`/`fetch_facts`/`fetch_entities`/`graph_payload`/
   `providers_stub`, `vector_facts`/`ProjectionComputation`/`PROJECTION_CACHE`,
   **and the 5 curation fns** (§5). In the **same commit**, update
   `memory_curate.rs:22` import to `super::memory_service::{…}`. This commit
   must compile and pass `memory_curate`'s unit tests + the curate
   characterization test. Green.

### Phase 4 — closeout
- Confirm `mod.rs` route table byte-identical to pre-migration (diff).
- Confirm `/api/capabilities` payload unchanged.
- Run the full dashboard via the smoke harness / MCP tool once against a real
  project DB for a manual eyeball of all four tabs.
- Optional: remove now-dead `use` imports flagged by `cargo`/clippy.

### Re-export shim technique (keeps each step independently green)
When a function moves from `*_api.rs` to `*_service.rs`, but some other code still
imports it from the old spot, leave a one-line re-export in the old module until
all call sites are updated, then delete it:

```rust
// in memory_api.rs, transiently
pub(crate) use super::memory_service::{build_delete_plan, delete_fact, /* … */};
```

This lets phase 3b move the 5 fns and fix `memory_curate.rs` in one commit while
keeping `memory_api.rs`'s own internal references compiling during the move.
Delete the shim in the same commit once `memory_curate` points at
`memory_service` directly.

---

## 13. Non-goals

- **No change to any response shape or JSON key** (C2). Additive keys
  (`threshold`/`min_similarity`, `bundled_fact_count`, `path`/`storage_scope`,
  `providers_stub`) stay exactly as-is (P18, P19).
- **No typing of response payloads.** They stay `serde_json::Value` (§7).
- **No new auth / no off-loopback exposure** (P1).
- **No archive/soft-delete**, no bypassing `MemoryStore` canonical delete/merge
  paths (P4, P5).
- **No perf changes.** Do not batch the 21 `query_i64` aggregates in lcm, do not
  rewrite the window-function growth query, do not "fix" the `DegreeSummary`
  node-edit blind spot. Those are audit §7 follow-ups, separate from layering.
- **No repository trait / mock layer.** Audit §7 floats this; it is out of scope
  here. The query modules are concrete fns over `&Connection`, not a trait, to
  avoid a speculative abstraction layer.
- **No moving `util.rs`, `mod.rs` lifecycle, `savings_*`, `token_count`,
  `assets`, or `curate_preview_store`.**
- **No submodule-directory reorg** (`dashboard/{memory,lcm,graph}/`) — that is a
  later mechanical follow-up, not this migration.
- **No behavior change to the `format!`-injected column-list assembly** beyond
  relocating it. The param-binding discipline is preserved verbatim.

---

## 14. Compatibility constraints (cross-reference)

Every audited preserve-item, mapped to where this design honors it:

- **P1** (loopback-only, no auth) → §11; no route exposure changes.
- **P2/P3** (error contract, `{detail}` rejections) → §9; query/service/route
  each keep their slice of the contract; `util.rs` unchanged.
- **P4/P5** (permanent delete via `MemoryStore`) → curation fns move verbatim to
  `memory_service.rs`; no new delete paths.
- **P6** (`content`→`snippet_text` fallback) → preserved in `lcm_queries.rs`
  `MESSAGE_COLUMNS` const.
- **P7** (token-estimate byte-identity) → one named const in `lcm_queries.rs`.
- **P8** (`summary_node_ids` subquery + re-parse) → both move to
  `lcm_service.rs`/`lcm_queries.rs` together.
- **P9** (bank = category, no `cat:` prefix) → preserved in `memory_service.rs`.
- **P10/P11/P12** (FTS→LIKE, column filter, both-FTS honesty) → `lcm_service.rs`.
- **P13** (negative-cache exemption) → `lcm_service.rs`.
- **P14/P15** (fingerprint, single-flight) → `memory_service.rs`.
- **P16/P17** (degree fingerprint, per-DB keys) → `graph_service.rs` / all
  service modules.
- **P18/P19** (additive fields) → route layer unchanged shapes.
- **P20** (5-fn curation seam) → §5, atomic move.
- **P21** (one router builder) → C1; `mod.rs` route table untouched.

---

## 15. Post-split conventions and regression gates

The implementation now follows the route/service/query split described above.
Keep these conventions when adding or moving dashboard endpoints:

- `*_api.rs` is the only place for Axum extractors, `coerce_limit` calls, HTTP
  status selection, and `{detail}`/`Json` wrapping. Do not make service/query
  modules depend on HTTP types.
- `*_service.rs` owns payload assembly, cache fingerprints, fallback decisions
  (for example LCM FTS→LIKE and graph default-subgraph selection), and all
  cross-query/domain math.
- `*_queries.rs` owns SQL text and should expose named functions over
  `&libsql::Connection` plus already-normalized parameters. User-controlled
  values stay bound as SQL parameters; only literal column lists and placeholder
  fragments may be assembled with `format!`.
- `src/dashboard/mod.rs` remains the single route table for the standalone
  server. The Hermes dashboard wrapper remains a thin reverse proxy from
  `/api/plugins/tracedecay/{holographic,lcm,graph,savings}/*` to the native
  standalone routes, preserving query strings verbatim.

Regression coverage that should stay in the verification set:

- `tests/dashboard_api_test.rs` covers holographic memory route shapes,
  curation, JSON error bodies, and LCM empty/global/project-store behavior.
- `tests/dashboard_lcm_api_test.rs` covers the standalone LCM route surface and
  source expansion against a seeded session store.
- `tests/dashboard_graph_api_test.rs` covers graph overview/search/detail,
  neighbors, subgraphs, and paths against a seeded code graph.
- `tests/hermes_dashboard_test.rs::deployed_wrapper_preserves_canonical_api_proxy_surface`
  pins the Hermes wrapper route rewrite/query-preservation contract so the
  wrapper cannot silently drift from the canonical standalone server.

---

## 16. Open decisions (for future implementation workers)

1. **`pub(crate)` vs `pub(super)` for service/query fns.** `pub(crate)` is the
   existing style in these modules and is recommended for consistency;
   `pub(super)` would be stricter. Decide once and apply uniformly.
2. **Whether `*_queries.rs` fns take `&Connection` or `&DashboardState`.**
   Recommend `&Connection` (and pass `mem_db_path` only where a cache needs it) —
   it keeps the query layer state-free and testable with a bare libsql
   connection, matching the existing `util::query_rows` signature.
3. **Where param-struct clamp/normalize logic lives** (e.g. `coerce_limit`).
   Recommend: route layer clamps (it owns param parsing); service/query receive
   already-clamped values. This keeps the "overview-card semantics" (P2) at the
   edge.
4. **Characterization-test fixture shape.** Recommend a single shared helper
   that builds an in-memory libsql DB with the minimal schema for one domain and
   returns a `DashboardState`-like handle, so each domain's snapshot tests share
   scaffolding. Implementer to choose exact shape.

---

*Design note for Kanban task t_166b3cf9. Inputs: `docs/DASHBOARD-API-AUDIT.md`
(parent task t_2ccceb97). Source audited at the `master` working tree (≈ `5ad31c4`).
Consumed by the per-domain implementation tasks.*
