# Dashboard API Audit — Routes & Inline SQL Hotspots

Audit of the standalone `tracedecay dashboard` HTTP backend, focusing on the
three route modules named in the task:

- `src/dashboard/memory_api.rs`  (1,548 lines) — holographic-memory plugin API
- `src/dashboard/lcm_api.rs`     (1,215 lines) — LCM session-store plugin API
- `src/dashboard/graph_api.rs`   (  973 lines) — code-graph explorer plugin API

Route registration, shared state, and the server lifecycle live in
`src/dashboard/mod.rs`. The shared SQL→JSON helper layer is
`src/dashboard/util.rs`. This artifact is for refactor planning: every
"preserve" note is behavior a UI or a sibling module depends on.

---

## 1. Architecture & entry points

**Two entry points, one router.** Both build state and mount the same router:

| Caller | Where | Notes |
|---|---|---|
| CLI `tracedecay dashboard` | `main.rs:1148` → `dashboard::run` | Binds `host:port`, prints parseable `tracedecay dashboard listening on <url>` line for wrappers. |
| MCP tool `tracedecay_dashboard` | `mcp/tools/handlers/dashboard.rs` (start/stop) | Reuses `build_state` + `router` + `bind_dashboard`. **Enforces loopback-only host** (127.0.0.1/localhost/::1) at `handlers/dashboard.rs:41`. |

A single `router(state)` builder (`mod.rs:246`) wires every route, so CLI and
MCP can never diverge. Preserve one router builder.

**Shared state — `DashboardState`** (`mod.rs:69`), cloned per request:

- `mem_conn: libsql::Connection` — project DB. Holds **both** the code graph
  (`nodes`/`edges`/`files`) **and** the holographic memory store
  (`memory_facts`/`memory_entities`/`memory_fact_entities`/`memory_banks`/
  `memory_oplog`).
- `lcm_conn: Option<Connection>` — LCM session store (`lcm_raw_messages`,
  `lcm_summary_nodes`, `lcm_summary_sources` + FTS mirrors). Resolved by
  `resolve_lcm_store` (`mod.rs:117`): project-local `.tracedecay/sessions.db`
  by default; `TRACEDECAY_GLOBAL_DB` override wins (legacy `TRACEDECAY_GLOBAL_DB`
  still accepted); global DB is the fallback. `lcm_scope` ∈
  `{"project_local","global"}` records which.
- `savings_db: Option<Arc<GlobalDb>>` — savings ledger (out of scope here).
- `project_root`, `mem_db_path`, `lcm_db_path`, `lcm_scope` — display/feature-detect fields.
- `curate_preview: Arc<RwLock<Option<CuratePreviewEntry>>>` — last dry-run
  curation preview, also persisted to disk via `curate_preview_store`.
- `token_counts` — BPE token-count cache for the Savings tab.

**Auth / context model — IMPORTANT.** There is **no auth, no middleware, no
per-request user/tenant scoping** in the Rust server. Everything is
project-scoped via `project_root` captured at server start. Security rests on:

1. loopback-only binding (the MCP handler rejects non-loopback hosts; the CLI
   defaults `host` to loopback in practice),
2. filesystem permissions on the SQLite DBs.

The POST write endpoints (`/curate`, `/curate/apply`) are therefore open to any
local process that can reach the port. **Any refactor must not expose these
routes off-loopback without adding auth.** The Hermes wrapper is where auth
actually lives (see §2).

---

## 2. Canonical (standalone) vs Hermes-wrapper compatibility

The module doc-comments and `dashboard/hermes-wrapper/plugin_api.py` make the
split explicit.

**Canonical (this Rust server) — everything in the three audited files:**

- All route handlers, all SQL, all caches, all curation math. This server is the
  source of truth; the wrapper does no data access of its own.
- Curation is **similarity-based deduplication only** (`memory_api` +
  `memory_analysis`). `POST /curate` proposes hard-DELETING the lower-trust fact
  in each `likely_duplicate` pair. No LLM in the server.
- `/api/capabilities` advertises `llm_curation: false`.

**Hermes-wrapper compatibility — what exists *only* to keep the ported plugin
bundles and the Hermes host working unmodified:**

1. **Mirrored payload shapes.** Route paths and JSON keys intentionally mirror
   the original Hermes plugin APIs
   (`plugins/memory/holographic_plus/dashboard/plugin_api.py` and the hermes-lcm
   `dashboard/plugin_api.py`). The `providers_stub()`, the `path`/`exists`/
   `storage_scope` fields, the `engine`/`engine_detail` search fields, and the
   `threshold`/`min_similarity` dual keys all exist for shape compatibility.
   **Do not rename keys or change response shapes without coordinating with the
   UI bundles.**
2. **Reverse proxy.** `dashboard/hermes-wrapper/plugin_api.py` is a *thin*
   reverse proxy: it lazily spawns `tracedecay dashboard --port 0` on loopback
   (or uses `TRACEDECAY_DASHBOARD_URL`), forwards `/holographic/* → /api/plugins/
   holographic/*`, `/lcm/* → …/hermes-lcm/*`, `/graph/*`, `/savings/*`, and
   exposes upstream `/api/capabilities` at `/capabilities`. It also adds the
   **Hermes session-token middleware** — the only auth in the stack.
3. **The one Hermes-only extension: LLM curation.** `POST /curation/llm-plan`
   lives in the wrapper and flips `llm_curation` true. The standalone binary
   mirrors that exact contract *outside the dashboard* via
   `src/dashboard/memory_curate.rs` (`tracedecay memory curate --llm` /
   `--llm-ops`), which ports the wrapper's `_CURATION_SYSTEM_PROMPT` verbatim.
   **This is why `memory_api.rs` is also a curation library, not just routes**
   (see §5).

---

## 3. Per-file route inventory

Response shapes are summarized; the `error` field (`""` on success) is omitempty
in several handlers and omitted from the table for brevity.

### 3a. `memory_api.rs` — `/api/plugins/holographic/*` (11 routes)

| # | Method | Path | Handler | Params | Response (top-level) | Notable SQL / behavior |
|---|---|---|---|---|---|---|
| 1 | GET | `/api/plugins/holographic` and `/api/plugins/holographic/` | `overview` | `q`, `limit`(25/100), `graph_limit` | `{providers, query, limit, holographic:{path, exists, overview, facts, entities, graph, error}}` | Aggregates `overview_payload` + `fetch_facts` + `fetch_entities` + `graph_payload`. ~8 queries incl. window-function `growth`, trust histogram, live bank counts. |
| 2 | GET | `/api/plugins/holographic/fact/{fact_id}` | `fact_detail` | path `fact_id:i64` | `{fact, error}` | Fact row + linked entities. **404** `{detail}` if missing. |
| 3 | GET | `/api/plugins/holographic/projection` | `projection` | `q`, `limit`(25/`PROJECTION_POINT_CAP`=2000) | `{exists, dim, limit, method, points, error}` | `vector_facts` decodes HRR blobs → PCA on `spawn_blocking`. **Cached** by `(query,limit,VectorStateFingerprint)`. |
| 4 | GET | `/api/plugins/holographic/similarity` | `similarity` | `min_similarity`, `limit`(25/`SIMILARITY_PAIR_CAP`) | `{exists, dim, count, limit, threshold, min_similarity, total_pairs, score_distribution, pairs, error}` | O(n²·d) pairwise phase-cosine on `spawn_blocking`. **Cached** by fingerprint. Emits `threshold` AND `min_similarity` for shape compat. |
| 5 | GET | `/api/plugins/holographic/curation/status` | `curation_status` | — | curator status stub | Reads `curate_preview`. Mostly static (`paused:false`, `mode:"similarity_dedup"`). |
| 6 | GET | `/api/plugins/holographic/curation/activity` | `curation_activity` | `limit` | `{events, count, limit, error}` | In-memory deterministic curation activity stream capped to the newest events. Preview/apply, agent-plan, and queued automation paths emit phases such as `queued`, `evidence`, `backend`, `validation`, `apply`, `report`, `finish`, `failure`, and `rejection`. |
| 7 | GET | `/api/plugins/holographic/curation/preview` | `curation_preview` | — | `{report, saved_at, stale, stale_reason, error}` | Reads saved dry-run preview; recomputes `memory_facts` fingerprint to flag staleness. |
| 8 | POST | `/api/plugins/holographic/curate` | `curate` | body `{dry_run:bool}` (default true) | `{ran, dry_run, actions, hygiene_candidates, counts, applied_counts, llm_calls, coverage, provider, mode}` | `build_delete_plan` (similarity dedup). dry_run saves preview to state + disk; apply **hard-deletes** losers via `MemoryStore::remove_fact`, records oplog summary. |
| 9 | POST | `/api/plugins/holographic/curate/apply` | `curate_apply` | body `{ops:[{op:"delete"\|"merge", ...}]}` | `{results, counts:{deleted, merged, errors}}` | Generic ops endpoint. `delete`→`MemoryStore::remove_fact`; `merge`→`MemoryStore::merge_facts` (optional content rewrite + hard-delete losers). Per-op failures reported inline (HTTP 200). |
| 10 | GET | `/api/plugins/holographic/oplog` | `oplog` | `limit`(50/300) | `{events, count, limit, error}` | `SELECT … FROM memory_oplog ORDER BY id DESC`. Parses `detail_json`. |

### 3b. `lcm_api.rs` — `/api/plugins/hermes-lcm/*` (6 routes)

Every payload reports `path`, `storage_scope`, `exists` additively (UIs
feature-detect the active store). Handlers return `LcmResult` (`Result<(Status,
Json), _>`); query errors propagate as HTTP 500 via `query_error`.

| # | Method | Path | Handler | Params | Response | Notable SQL / behavior |
|---|---|---|---|---|---|---|
| 1 | GET | `/api/plugins/hermes-lcm/overview` | `overview` | `q`, `limit`(25/200) | `{…, overview:{messages_total, sessions_total, summary_nodes_total, …, role_counts, source_counts, depth_counts, compression}, latest_sessions, latest_summary_nodes, matches:{messages, summary_nodes}}` | ~12 count/aggregate queries; `ensure_valid_summary_metadata` gate. `q` adds LIKE matches. |
| 2 | GET | `/api/plugins/hermes-lcm/search` | `search` | `q`, `limit`, `offset`, `role`, `source`, `session_id`, `since`, `until` | `{…, engine, engine_detail:{messages, summary_nodes}, total:{messages, summary_nodes}, filters, matches}` | **FTS-then-LIKE fallback** for both messages and summary nodes. Reports `engine:"fts"` only if *both* sections used FTS. |
| 3 | GET | `/api/plugins/hermes-lcm/session/{session_id}` | `session` | path `session_id`, `limit`(200/1000), `offset`, `order`(asc\|desc) | `{…, counts, messages, summary_nodes, has_more, has_more_messages, has_more_summary_nodes}` | **404** if both message & summary-node counts are 0. Orders by `ordinal` (ingest order), timestamp as tiebreak. |
| 4 | GET | `/api/plugins/hermes-lcm/node/{node_id}` | `node` | path `node_id` | `{node, sources:{type, ids, messages, nodes}}` | Lossless expand of a summary node; resolves `lcm_summary_sources` rows to raw messages or child nodes. **404** if missing. |
| 5 | GET | `/api/plugins/hermes-lcm/timeline` | `timeline` | `bucket`(hour\|day), `session_id`, `limit`(400/2000) | `{buckets, node_buckets, undated:{count, token_estimate}}` | `strftime` buckets; NULL timestamps excluded from dated buckets and reported via `undated`. |
| 6 | GET | `/api/plugins/hermes-lcm/compression` | `compression` | `by`(node\|session), `limit`(50/500) | `{overall:{source_token_count, token_count, ratio, node_count}, groups}` | Per-group `ratio` computed in Rust. |

### 3c. `graph_api.rs` — `/api/plugins/graph/*` (6 routes)

All bounded: subgraph caps node/edge counts, search paginates, path BFS caps
depth (`max_depth` default 6 / max 10) and visited set (`PATH_VISITED_CAP` =
20,000). All node rows add a `span` object and a `degree` integer.

| # | Method | Path | Handler | Params | Response | Notable SQL / behavior |
|---|---|---|---|---|---|---|
| 1 | GET | `/api/plugins/graph/overview` | `overview` | — | `{path, totals:{nodes,edges,files}, nodes_by_kind, edges_by_kind, files_by_language, top_connected, largest_files}` | `files_by_language` derived in Rust from extension. `top_connected` from cached `DegreeSummary`. |
| 2 | GET | `/api/plugins/graph/search` | `search` | `q`, `limit`(50/200), `offset` | `{query, limit, offset, total, count, results}` | Empty `q` → paged browse; else LIKE over name/qualified_name/signature/file_path with a relevance `CASE`. Attaches per-node `degree`. |
| 3 | GET | `/api/plugins/graph/node/{node_id}` | `node` | path `node_id` | `{node}` | **404** if missing. |
| 4 | GET | `/api/plugins/graph/node/{node_id}/neighbors` | `neighbors` | path `node_id`, `limit`(50/200) | `{node_id, depth:1, limit, callers, callees, edges, edges_by_kind}` | **404** if node missing. Callers/callees filtered to `kind='calls'`. |
| 5 | GET | `/api/plugins/graph/subgraph` | `subgraph` | `node_id`, `q`, `limit_nodes`(80/250), `limit_edges`(120/500) | `{seed_id, mode:"seeded"\|"default", nodes, edges, capped, limits}` | Seeded = 1-hop neighborhood. No seed (and no `q`) = `default_subgraph`: top-degree hubs + edges among them (greedy adjacency selection over cached pool). Explicit `q` with no hit = empty payload. |
| 6 | GET | `/api/plugins/graph/path` | `path` | `from`, `to`, `max_depth` | `{from, to, found, path, nodes, edges, max_depth}` | Undirected BFS over `edges` (chunked IN-clauses of 400). Reconstructs path via parent back-pointers. |

---

## 4. The shared SQL layer (`util.rs`) — the de-facto data-access contract

There is **no repository layer**. Handlers call `mem_conn`/`lcm_conn` directly
through two helpers that every dashboard module depends on:

- `query_rows(conn, sql, params) -> Result<Vec<Value>, String>` — runs the
  query and drains rows into `{column_name: value}` JSON objects. **On SQL error
  it returns `Err(message)`, never panics**; handlers surface that message in
  the payload's `error` field (mirroring the Python APIs, which never 500 on a
  bad/missing DB). Blobs collapse to JSON null (like NULLs).
- `query_i64(conn, sql, params) -> i64` — scalar `COUNT(*)`-style; **all errors
  and empty sets collapse to 0** (overview-card semantics).

Supporting helpers: `coerce_limit` (clamp to `[1,max]`, port of `_coerce_limit`),
`qmarks(n)` (`,`-joined `?` list for `IN(?)`), `like_pattern` (escapes
`%`_`\` and wraps in `%…%`), `build_fts_match` (safe FTS5 MATCH expression),
`http_detail`/`json_error` (FastAPI `{detail}` shape), and **`JsonPath`/`JsonQuery`
extractor wrappers** that turn Axum's default text/plain rejection into the same
`{detail}` JSON so the UIs' error paths work.

> **Preserve.** Changing the error contract of `query_rows`/`query_i64` ripples
> into every handler's error handling. Changing `JsonPath`/`JsonQuery` rejection
> bodies breaks UI error rendering.

### Inline SQL hotspot counts (line-level occurrences)

| File | `query_rows` | `query_i64` | inline `conn.query` | `format!` SQL | `LIKE ?1 ESCAPE` | `spawn_blocking` |
|---|---:|---:|---:|---:|---:|---:|
| `memory_api.rs` | 16 | 4 | 3 | 19 | 2 | 2 |
| `lcm_api.rs`    | 4  | 21 | 1 | 24 | 5 | 0 |
| `graph_api.rs`  | 16 | 10 | 4 | 11 | 11 | 0 |

Observations:

- **`lcm_api` is `query_i64`-heavy** (21) — the overview/compression handlers do
  many scalar `COUNT/SUM` aggregates, each its own round-trip, all sequential.
- **`memory_api` and `graph_api` are `query_rows`-heavy** — they build rich row
  payloads.
- **`format!` is the dominant code shape everywhere** — it injects compile-time
  column-list constants (`NODE_COLUMNS`, `NODE_COLUMNS_N`, `MESSAGE_COLUMNS`)
  and `qmarks` placeholder lists into SQL strings. There is **no SQL-injection
  surface** (placeholders are always bound; the consts are literals), but any
  refactor that "cleans up" the string assembly must keep the param-binding
  discipline.

### Repeated inline SQL patterns (refactor candidates, but must stay identical)

1. **The degree UNION-ALL subquery** — `SELECT source AS node_id FROM edges
   UNION ALL SELECT target AS node_id FROM edges` — appears **3× in
   `graph_api`** (`degrees_for_ids`, and twice inside `degree_summary`: the pool
   query and the `top_connected` query). The `DegreeSummary` cache exists
   *because* these were per-request full `edges` scans.
2. **The token-estimate expression** `(LENGTH(COALESCE(content, snippet_text,
   '')) + 3) / 4` — the chars/4 heuristic. It is inlined in `MESSAGE_COLUMNS`
   *and* repeated in the `session` handler's `token_estimate_total` SUM *and*
   the `timeline` `msg_sql`/`undated_sql`. Duplicated 3–4×; must stay byte-identical.
3. **The LIKE substring fallback** (`LIKE ?1 ESCAPE '\'` + `like_pattern`) —
   graph_api 11×, lcm_api 5×, memory_api 2×. The canonical "no FTS / FTS-empty"
   path; externalized LCM rows (content=NULL) are only findable this way.
4. **FTS-then-LIKE dispatch** (`lcm_api::search`) — builds FTS count+select for
   messages and nodes, falls back to LIKE on any failure/empty MATCH. The FTS5
   column filter `{summary_text expand_hint} : (...)` qualifies the node MATCH so
   `metadata_json` text cannot over-match.
5. **`json_valid`/`json_extract` over `metadata_json`** (`lcm_api` `NODE_COLUMNS`
   + `node` handler) — derives `category`/`tags`/`entities`. `ensure_valid_summary_metadata`
   pre-validates the whole store once (cached per-DB) and returns **422** if any
   row has malformed JSON.
6. **`json_group_array` subquery** for `summary_node_ids` in `MESSAGE_COLUMNS`,
   re-parsed to a real array by `parse_summary_node_ids` before rows are returned.
7. **Window-function cumulative growth** (`memory_api::overview_payload`) — daily
   buckets + one-time prior-window count. Already optimized from a correlated
   COUNT that did ~181 full scans/overview.

### Process-wide caches (keyed by `mem_db_path`/`lcm_db_path`, content-fingerprint invalidation)

| Cache | Where | Key | Invalidation |
|---|---|---|---|
| `DEGREE_CACHE` | `graph_api` | `mem_db_path` → `Arc<DegreeSummary>` | `(COUNT(*),MAX(id))` of `edges` |
| `PROJECTION_CACHE` | `memory_api` | `mem_db_path` → `Arc<ProjectionComputation>` | `(query, limit, VectorStateFingerprint)` |
| `SIMILARITY_CACHE` | `memory_api` | `mem_db_path` → `Arc<SimilarityComputation>` | `VectorStateFingerprint` |
| `VALIDATED_METADATA_STORES` | `lcm_api` | `HashSet<lcm_db_path>` | one-shot; **failures NOT cached** |
| `curate_preview` | `DashboardState` | per-state `RwLock` | cleared on any apply; persisted to disk |

`VectorStateFingerprint` = `(count, max_updated_at, sum_fact_id, hash)` — it
**deliberately does not hash the HRR blobs** (at the 2000-fact cap that was ~32 MB
pulled out of SQLite per request). All projection/similarity computation runs on
`spawn_blocking` with the cache mutex held across the work (single-flight).

---

## 5. Cross-module dependencies — fan-in / fan-out

```
                       ┌─────────────────────────────┐
  HTTP (mod.rs routes) │                             │
  ────────────────────▶│  memory_api.rs              │
                       │  (routes + curation library)│
  ┌───────────────────▶│                             │
  │                    └──────────┬──────────────────┘
  │                               │ fan-out (heavy)
  │                               ▼
  │   memory_curate.rs ──fan-in──▶ memory_analysis.rs (similarity/PCA/proposals)
  │   (CLI curation + LLM tier,   │
  │    reuses 5 pub(crate) fns)   ├──▶ crate::memory::store::MemoryStore
  │                               │      (remove_fact / merge_facts / record_oplog)
  │                               ├──▶ crate::memory::encoding::HolographicEncoder
  │                               ├──▶ super::curate_preview_store (save/clear/load)
  │                               └──▶ super::util  (query_rows/query_i64/...)
  │
  ├── HTTP ──▶ lcm_api.rs   ──▶ super::util, DashboardState   (self-contained)
  └── HTTP ──▶ graph_api.rs ──▶ super::util, DashboardState   (self-contained;
                                                              DegreeSummary cache
                                                              couples overview↔subgraph)
```

**`util.rs` has the widest fan-in** — every dashboard module. Its `query_rows`/
`query_i64` error contract is effectively the data-access API.

**`memory_api.rs` is the coupling hotspot.** It is fan-in from *two* sides:

1. `mod.rs` routes (the 11 HTTP handlers).
2. **`memory_curate.rs` imports 5 `pub(crate)` functions** —
   `build_delete_plan`, `delete_fact`, `apply_delete_op`, `apply_merge_op`,
   `similarity_computation` (`memory_curate.rs:22`). `memory_curate` is the
   dashboard-free curation core (`tracedecay memory curate`, including the
   `--llm`/`--llm-ops` review tier that mirrors the Hermes wrapper's
   `/curation/llm-plan`).

   → **memory_api is not just a route module; it is a curation library.** Moving,
   renaming, or inlining these 5 functions breaks the CLI curation path and the
   LLM-review tier. Keep them as reusable seams (or update `memory_curate` in the
   same change).

`lcm_api.rs` and `graph_api.rs` are leaf route modules (only `mod.rs` calls
them); each depends only on `util` + `DashboardState`. The one internal coupling
in `graph_api` is the `DegreeSummary` cache shared between `overview` and
`default_subgraph`.

`mod.rs` itself fans out to `global_db::GlobalDb`, `tracedecay::TraceDecay`,
`sessions::cursor`, and `sessions::ingest_global_sources` (the detached
catch-up ingest that runs when serving project-local LCM).

---

## 6. Risky behavior that MUST be preserved during refactor

Grouped so a refactorer can grep them. Each is load-bearing for a UI, a sibling
module, or a documented invariant.

**Security / boundary**
- P1. **No auth, loopback-only.** Do not expose routes off-loopback without
  adding auth. Preserve the MCP loopback check (`handlers/dashboard.rs:41`).

**Error contract (UIs depend on "never 500 on a bad/missing DB")**
- P2. `query_rows` returns `Err(message)`; handlers surface it in the payload
  `error` field, never a raw 500. `query_i64` collapses all errors/empty → 0.
- P3. `JsonPath`/`JsonQuery` rejections must stay `{detail: ...}` JSON
  (FastAPI shape), not Axum's text/plain.

**Data invariants**
- P4. **Deletion is permanent** (`/curate`, `/curate/apply`,
  `MemoryStore::remove_fact`/`merge_facts`). No archive, no soft-delete. This is
  intentional (see project memory facts). Do not "add an archive" without
  explicit intent.
- P5. Curation **must** go through `MemoryStore` canonical paths (transactional
  delete + FK-cascade entity links + FTS trigger + bank dirty-marking + oplog).
  Raw `DELETE`s break invariants.
- P6. `MESSAGE_COLUMNS` `content` fallback `COALESCE(content, snippet_text)` —
  externalized LCM rows (`storage_kind='external'`, content=NULL) are only
  readable/findable via `snippet_text`/`index_text`. Preserve or they vanish.
- P7. The token-estimate expression `(LENGTH(COALESCE(content, snippet_text,
  '')) + 3) / 4` must stay byte-identical across all duplicated sites (§4.2) or
  counts desync from the canonical reader.
- P8. `summary_node_ids`: keep the `json_group_array` subquery **and**
  `parse_summary_node_ids` re-parse together.
- P9. Banks are named after their category directly (**no `cat:` prefix**);
  differs from Hermes by design. Preserve the mapping in `overview_payload`/
  `graph_payload`/HRR coverage.

**Search / validation semantics**
- P10. `lcm search` FTS→LIKE fallback ordering; externalized rows must stay
  searchable via the LIKE path over `index_text`/`snippet_text`.
- P11. The FTS5 column filter `{summary_text expand_hint} : (...)` prevents
  `metadata_json` over-match; preserve.
- P12. `engine`/`engine_detail`: report `fts` only when **both** sections used
  FTS (worst-case honesty).
- P13. `ensure_valid_summary_metadata`: per-store one-shot validation, **failures
  NOT cached** (422 until repaired). Preserve the negative-cache exemption.

**Cache correctness**
- P14. `VectorStateFingerprint` must stay metadata-only (no blob hashing) or
  reintroduce the ~32 MB/request regression.
- P15. Projection/similarity must stay on `spawn_blocking` with single-flight
  (mutex held across computation).
- P16. `DegreeSummary` fingerprint `(COUNT(*), MAX(id))` of edges does **not**
  detect node-only edits until the next sync rewrites edges. Known limitation;
  coordinate before "fixing".
- P17. All caches are keyed by `mem_db_path`/`lcm_db_path` because one process
  can serve multiple projects via the MCP tool. Preserve per-DB keying.

**Compatibility / additive fields**
- P18. LCM payloads report `path` + `storage_scope` additively; UIs
  feature-detect the active store. Preserve.
- P19. `similarity` emits `threshold` **and** `min_similarity`; `overview`
  emits `bundled_fact_count` alongside the live `fact_count`; `providers_stub`
  advertises `memory_provider`/`context_engine`. Keep all additive keys.

**Refactor seams (do not break these without updating callers)**
- P20. The 5 `pub(crate)` functions in `memory_api` reused by `memory_curate`
  (`build_delete_plan`, `delete_fact`, `apply_delete_op`, `apply_merge_op`,
  `similarity_computation`).
- P21. One `router()` builder shared by CLI + MCP entry points.

---

## 7. Suggested (non-blocking) follow-ups

These are improvements a future refactor *could* make; none are requested here
and each touches behavior flagged above:

- Introduce a thin read-repository trait so `query_rows`/`query_i64` usage can be
  mocked and the duplicated token-estimate / degree-union expressions become
  named queries.
- Batch the sequential scalar aggregates in `lcm overview`/`compression` (21
  `query_i64` round-trips) via CTEs or a single UNION-ALL aggregate.
- Extract the `MESSAGE_COLUMNS`/`NODE_COLUMNS` projection + post-processing
  (`parse_summary_node_ids`) into a shared row-mapper so `lcm_api` stops having
  `format!`-injected column lists duplicated across handlers.
- Consider an explicit `Auth` layer long before any non-loopback deployment is
  contemplated (today the only auth is the Hermes wrapper's session middleware).

---

*Generated for Kanban task t_2ccceb97. Source files audited at the `master`
working tree (commit range around `5ad31c4`).*
