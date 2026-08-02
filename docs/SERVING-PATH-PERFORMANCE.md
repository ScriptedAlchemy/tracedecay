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

This applies to generation resolution too. MCP search resolves through
`CodeIndexSchedulerRegistryV1::latest_complete_ready_for_scope`, which admits
only an *already-current* generation — `latest_complete_ready_for_query`
abstains whenever freshness is unknown, git metadata moved, or the staleness
threshold elapsed. Every other callable code query resolves through
`latest_complete_fresh_for_scope`, which serves the last complete generation.
That asymmetry used to make `search` report `GenerationUnavailable` for as long
as a rebuild ran, while callers/callees/grep answered from a published
generation within seconds.

`execute_query_search` now closes that gap: when the ready gate abstains it
falls back to `latest_complete_serving_for_scope` and marks the
exact/lexical/graph lanes `stale` against the generation that answered. The
fallback is O(1) and never blocks — the last complete generation is already
held in the per-worktree `serving_generation` `RwLock`, seeded at mount and
rewritten by every publication, so it needs no re-read, no gix status, and no
scheduler lock. Fail-closed behavior is unchanged in both directions: the
fallback still requires an exact scope match, and when no complete generation
exists at all the typed `GenerationUnavailable` fail-fast is preserved rather
than degraded into an empty answer. When a ready generation does exist the
fresh path is untouched, so a warm response is byte-identical.

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

### 2. Serving is protected by a reservation; batch work races to idle

Interactive requests never queue behind background CPU. The mechanism is a
**reserved slice of cores**, not a slower background job.

Maintenance splits into two kinds of work, and they get opposite treatment:

**Batch work with a finish line** — a full index, a worktree reconcile — runs
at full machine width and finishes. Throttling it does not reduce
interference; it stretches the interference window. A reindex that pins 8 of
96 cores for ten minutes is worse for every agent on the box than one that
pins 90 cores for one minute. So:

- One process-wide indexing pool is sized to
  `total_cores - max(2, cores/16)` (90 of 96), and every per-file stage —
  read, sanitize, tree-sitter extract, chunk, digest — fans out across it
  with **no batch barrier**. Barriers are the hidden throttle: re-joining
  every N files means the slowest file in each group gates the group, and
  the pipeline never reaches its nominal width.
- The reserved cores are what keep reads fast during a reindex. They are
  reserved, not merely deprioritized, so an interactive request always has a
  runnable CPU no matter how deep the indexing queue is.
- Width is sizing policy and never semantics. Per-file results are collected
  in input order and the lowest-index failure is the reported one, so a
  sealed generation is byte-identical at width 1 and at width 90. That
  equivalence is a test, not a claim.
- Cross-worktree admission does **not** multiply throughput, because every
  worktree shares that one pool. Admitting N worktrees only interleaves
  them: the makespan is unchanged, each worktree's index lands N times
  later, and N snapshots sit in RSS at once. Reconcile admission is
  therefore 2 (enough to overlap one worktree's git/store/publication I/O
  with another's extraction), not "half the cores".

Embedding a published generation is batch work with a finish line too, and it
gets the same treatment — but its width has two knobs that must not be
confused:

- **Intra-op threads** are how many CPUs ONNX Runtime uses inside one tensor
  invocation. Changing this changes how a GEMM is partitioned and can change
  floating-point reduction order, so it is *numerics*: pinned by the artifact
  manifest, never inferred from the host, and moved only together with a
  re-embed.
- **Session width** is how many independent batches are in flight. Each batch
  is a separate invocation of the same graph over the same tensor shape, so
  results are bit-identical at any width. This is the knob that scales with
  the host, sized to `indexing_worker_target / intra_threads` so embedding
  lives inside the *same* reservation as extraction rather than stacking a
  second full-machine pool beside it. Chunk grouping is a fixed constant for
  the same reason a batch barrier is forbidden above: regrouping changes the
  padded tensor shape, which is semantics, not sizing.

Before this split the embedder ran one session at four intra-op threads on
every host — roughly 400% CPU on a 96-core box — which is what made post-
dogfood rebuild windows run tens of minutes.

GPU is not enabled. `fastembed`'s `InitOptionsUserDefined` does accept
`with_execution_providers`, so wiring CUDA/DirectML is mechanical, but three
things must land first: the `ort` EP feature must be added to the bundled
runtime build (it changes `FASTEMBED_RUNTIME_BUILD_REVISION_V1`, which is part
of the artifact compatibility pin), device availability must be *detected* and
opt-in by env rather than assumed, and — the blocking one — a GPU EP changes
kernel selection and therefore vector bytes, so `EmbeddingDeviceClassV1` has
to participate in the projection key and force a re-embed rather than silently
mixing CPU- and GPU-produced vectors in one generation.

**Open-ended sweeps** — redundancy scanning, retention, projection refresh —
have no finish line, so they stay paced:

- Bounded work per tick (a work budget plus a fairness cursor; the retention
  round-robin is the reference implementation), yielding between slices.
- Long CPU slices run in `spawn_blocking` chunks, never on the request
  runtime's workers for unbounded stretches.
- A cheap serving-pressure signal (in-flight interactive request count) lets
  them defer or shrink a slice while agents are actively querying. Results
  are identical; only pacing changes.

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
| 2 reserved-width indexing pool (barrier-free per-file fan-out, capture fan-out, admission 2) | merged |
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

Reservation measurement (2026-08-02, release build, 96-core host,
`PERF_REINDEX_WORKTREES=2`, 6 workers × 120s, 149,737 nodes). Interactive p95
with the box idle versus with a full index running continuously beside the
daemon:

| tool | p95 idle | p95 during full reindex |
|---|---:|---:|
| callers | 0.062 s | 0.082 s |
| context | 0.385 s | 0.390 s |
| grep | 0.576 s | 0.493 s |
| search | 1.531 s | 1.375 s |

A full index saturating the machine is within run-to-run noise of an idle
box: the reserved slice, not a slower indexer, is what holds the line.
`search` p95 sits above 1s in BOTH columns, so that tail is a search-path
cost, not indexing interference. Peak daemon RSS on this host is ~13.6GB with
no indexing at all, well over the 6GB gate budget — a live, separate breach
of Principle 5 that this measurement did not introduce.

## Open breach: the vector-generation store violates Principle 5

`DatabaseVectorGenerationStoreV1` persists its entire state as one JSON blob in
a single SQLite row. Every `commit_batch` and `publish_generation` deserializes
that blob, clones the staged generation, mutates, and re-serializes it.

### What has landed

The float payload no longer travels in that blob. Projected vectors are stored
row-per-vector in `semantic_vector_payload_v1`, content-addressed by the
projector's `output_digest`, written in bounded statement groups inside the same
transaction as the state swap, and resolved back through bounded `IN (...)`
reads. `PhysicalVectorBytePoolV1` now sweeps dead weak handles on a fixed intern
cadence and caps its key set, so retiring a generation releases both its bytes
and its keys. Three whole-corpus deep copies are gone: the reuse-index rebuild
no longer clones every published generation, the persistent `commit_batch` no
longer clones the prepared batch to satisfy a retryable closure, and
publish/activate/deactivate mutate the published state in place instead of
cloning it.

Nothing about identity moved. Every digest — `output_digest`, the generation
manifest digest, batch publication digests — is derived by the projector from
domain values, never from the store's encoding, and
`ProjectedChunkVectorV1::validate` re-derives `output_digest` from the hydrated
floats on every load, so a mis-bound payload fails closed rather than serving.
A pre-migration document is still readable and is migrated forward on open
under the existing revision CAS, so a crash leaves the original blob intact.

Measured A/B on identical code with only the encoding differing (2,000 chunks ×
768 dimensions, debug build): peak process RSS 227MB inline versus 125MB
row-per-vector, for the *same* published generation digest. Above the ~66MB
process floor that is 161MB versus 59MB — a 6MB float corpus was costing 155MB
to persist.

### What remains

The state document is still **O(store) in metadata**, and that is now the
binding constraint. Per-vector row metadata, the per-chunk projection receipts
(`ProjectionBatchReceiptV1::receipts`), `plan.expected_chunk_ids`,
`committed_chunk_effects`, and the batch's changed-chunk set all scale with the
corpus and are all still rendered into one JSON value bound as a single SQL
parameter. The runtime caps a request at 64MB (`MAX_REQUEST_BYTES`), so a
whole-corpus commit fails outright past a corpus-size ceiling:

| encoding | 2,000 chunks | 5,000 | 10,000 | 20,000 |
|---|---|---|---|---|
| inline floats | ok | `RequestLimitExceeded` | — | — |
| row-per-vector | ok | ok | ok | `RequestLimitExceeded` |

A 150K-chunk generation therefore still cannot be persisted at all. Two things
close it, and they are independent:

- Move per-vector row metadata and per-chunk receipts into their own tables,
  leaving the state document with generation-level identity only. The serde
  adapters that elide the float payload today are the pattern: serde elides,
  and a separate context-carrying walk does the row I/O.
- Use the incremental commit path production already has available.
  `commit_batch` takes an `expected_checkpoint` and tracks `completed_batches`,
  so bounded incremental commits are supported by the contract — production
  simply performs exactly one whole-corpus commit. Splitting it bounds both the
  document and the live float set per commit.
