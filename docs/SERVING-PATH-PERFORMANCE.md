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
| 1 validation memoization | in flight |
| 1 snapshot hash indices | in flight |
| 2 redundancy pacing + pressure signal | in flight |
| 2 reconcile semaphore + retention round-robin | merged |
| 3 ingest-side digest audit | open |
| 4 idna/derived-value caching | in flight |
| 5 paging/bounded heaps/batch IN | merged (20-finding wave) |
| 6 carried-deadline central wrap (git, memory) | merged |
