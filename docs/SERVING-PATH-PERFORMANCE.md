# Serving-Path Performance Architecture

Measured baseline (2026-08-01, live daemon, 151K-node store, 8 concurrent
readers): 670% CPU, one search 6+ minutes. perf attribution: ~20% per-access
generation re-validation (canonical JSON + SHA-256 of every chunk), ~17%
unpaced redundancy scanning competing with serving, ~14% canonical serde
churn, ~13% raw SHA-256, ~10% per-record idna/URL parsing, plus O(candidates ×
records) linear snapshot scans in the record-read port.

None of that is I/O. It is CPU doing store-sized work on the request path.
Deadlines are the last resort, not the design: agents saturate the daemon with
hundreds of concurrent calls, and the system must stay fast without dropping
or killing requests.

## Operating assumption: continuous churn

Agents edit the codebase continuously — commits, branch switches, and
transient worktrees arrive at all times. There is no quiescent window to
finish indexing in. Three consequences are load-bearing:

- **Serving never couples to indexing recency.** Reads always serve the last
  complete generation (stale-while-revalidate); a new generation swaps in
  atomically when ready. Blocking a query on an in-progress rebuild is
  disqualifying — under continuous churn that read would block forever.
- **Reindexing is incremental at file granularity.** A commit touching three
  files costs three files, not a generation rebuild. Full rebuilds can never
  keep up with continuous edits; they are reserved for bootstrap and
  corruption recovery.
- **Edits coalesce.** Bursts of commits collapse into batched reindex windows
  (debounced), and transient agent worktrees are indexed lazily on first
  query — never eagerly on registration — and deregistered on deletion.

### Retrieval lanes degrade independently

Serving never couples to indexing recency at the *lane* level either. A query
runs whichever lanes are ready and returns their fused results with an explicit
per-lane coverage marker (`CodeIndexSearchCoverageV1`); a lane that cannot run
is reported `unavailable` with a stable reason, and a lane answering from an
older complete generation is reported `stale` with the generation that
answered. Only when *no* lane can serve does the query fail, and then it fails
immediately with a typed reason rather than waiting on a rebuild.

The coverage marker is additive: a warm response carries the same candidates,
fallback bytes, and cursor it always did, and renders identically. A degraded
response says "partial recall" in its body, because a short result list is
otherwise indistinguishable from a thorough one.

Known gap (owned by the code-index scheduler lane, not the retrieval lane): MCP
search resolves its generation through
`CodeIndexSchedulerRegistryV1::latest_complete_ready_for_scope`, which admits
only an *already-current* generation — `latest_complete_ready_for_query`
abstains whenever freshness is unknown, git metadata moved, or the staleness
threshold elapsed. Every other callable code query resolves through
`latest_complete_fresh_for_scope`, which serves the last complete generation.
That asymmetry is why, after a daemon restart, callers/callees/grep answer from
a published generation within seconds while `search` reports
`GenerationUnavailable` for as long as the rebuild runs. The generation store
makes stale-while-revalidate cheap here — the last complete generation is
already held in the per-worktree `serving_generation` `RwLock` and needs no
re-read — so the fix is for `execute_query_search` to fall back to that
generation and mark the exact/lexical/graph lanes `stale` instead of failing.
Both change points live under `src/daemon/code_index_scheduler/`.

## The invariant

**A serving-path operation performs O(result) work, never O(store).**

Any O(store) computation — integrity hashing, index construction, dedup
analysis, projection rebuilds — happens at write/publish/load time, exactly
once per store change, and is amortized across every read that follows.

## Principles

### 1. Generations are verified once and indexed once

A published code-index generation is content-addressed and immutable.

- Integrity validation (chunk canonical-SHA-256 sweep) runs once per loaded
  generation, memoized on the generation object. Repeat calls are O(1). The
  fail-closed gate is unchanged: a generation that never validated cannot
  serve, and load-time corruption errors exactly as before. Deliberate
  re-verification of on-disk bytes stays available as an explicit
  `validate_fresh` path so the memo can never mask a real re-read.
- Lookup indices (file-occurrence, chunk-id, symbol) are built with the
  generation and live beside the snapshot. Record-read ports resolve
  candidates by hash lookup; `.iter().find()` over snapshot vectors on a
  query path is a defect.

### 2. Serving has priority; maintenance runs on a budget

Interactive requests never queue behind background CPU.

- Every maintenance loop (reconcile, redundancy, retention, projection
  refresh) does bounded work per tick — a work budget plus a fairness cursor
  (the retention round-robin is the reference implementation) — and yields
  between slices.
- Background concurrency is capped (bounded semaphores, sized to cores), and
  long CPU slices run in `spawn_blocking` chunks, never on the request
  runtime's workers for unbounded stretches.
- A cheap serving-pressure signal (in-flight interactive request count)
  lets maintenance defer or shrink its slice while agents are actively
  querying. Results are identical; only pacing changes.

### 3. Hash where data is born, never where it is served

Content digests (payload references, canonical record digests) are computed
once at ingest/publish, persisted, and trusted thereafter under the store's
own integrity gates. Recomputing a digest on a read path is a bug unless it
is an explicit verification request.

### 4. Derived values are cached derivations

URL/host normalization (idna), canonical keys, and similar per-record
derivations are computed once per source record — memoized or persisted —
not re-derived inside per-record loops.

### 5. Reads are paged and ranked in bounded space

Whole-table materialization is forbidden (`collect_rowid_pages` is the
canonical loop; the SQLite row-materialization limiter enforces the floor).
Ranking uses bounded heaps sized to the requested cap, not
sort-everything-then-truncate. Batch lookups (`IN (...)`) replace per-row
round trips.

### 6. Deadlines bound failure, not work

Admission carries the client deadline end-to-end and every handler is wrapped
once, centrally, per dispatch group. A deadline firing in a healthy system
means a principle above was violated; the fix is the violation, not a larger
timeout. Cold-open of a large store under load is the one sanctioned slow
path, and it converges via freshness witnesses (skip redundant re-index)
rather than by serving stale data.

## Status map (2026-08-01)

| Principle | State |
|---|---|
| 1 validation/admission/attribution memoization + generation LRU | merged |
| 1 snapshot hash indices (record port + relation BFS adjacency) | merged |
| 2 redundancy comparison-budget pacing + shared shingle merge | merged |
| 2 reconcile semaphore + retention round-robin | merged |
| 3 lazy output digests + threaded projections + constant-digest memo | merged |
| 4 idna/remote-normalization memoization | merged |
| 5 paging/bounded heaps/batch IN | merged (20-finding wave) |
| 6 carried-deadline central wrap (git, memory) | merged |
| CI perf gate (self-index + 6-worker load, budget verdicts) | merged |

First post-wave measurement (2026-08-01, release build, 96-core host): index
149,226 nodes in 76.7s; 724 calls / 0 errors at 6 workers; warm p50 — search
151ms, callers 65ms, context 197ms, grep 195ms (baseline before the wave: one
search took 6+ minutes at 670% daemon CPU). Open tails: grep p95 4.6s / max
25s; daemon peak RSS 4.4GB.
