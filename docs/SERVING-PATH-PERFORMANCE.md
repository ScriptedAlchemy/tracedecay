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
serves `latest_complete_serving_for_scope` and marks the exact/lexical/graph
lanes `stale` against the generation that answered. The fallback is O(1) and
never blocks — the last complete generation is already held in the per-worktree
`serving_generation` `RwLock`, seeded at mount and rewritten by every
publication, so it needs no re-read, no gix status, and no scheduler lock.
Fail-closed behavior is unchanged in both directions: the fallback still
requires an exact scope match, and when no complete generation exists at all the
typed `GenerationUnavailable` fail-fast is preserved rather than degraded into
an empty answer. When a ready generation does exist the fresh path is untouched,
so a warm response is byte-identical.

### Await-new never preempts serve-old

Having a fallback is not enough; the **order** in which resolution reaches it
is itself load-bearing. Asking the ready gate first — as the original fallback
did — meant a query entered the single-flight sealed-generation decode
(`DecodedGenerationCacheV1`) before it could discover it already had a servable
generation. Whenever a new generation was being decoded/activated, every query
parked on that O(store) sweep and the fallback was unreachable for its whole
duration. Measured live: search served 190–220ms right after restart, then
blocked 45s+ during the next full rebuild.

Resolution therefore checks the O(1) `serving_generation` **first**. When it
holds a complete generation, freshness is decided by
`latest_complete_ready_decoded_for_scope` — the same ready gate under
`GenerationDecodeAdmissionV1::AlreadyDecoded`, which serves the active
generation only if it is already decoded and *abstains* rather than claiming a
lease or parking. Abstention means stale, not failure, so the query answers
from the generation it already had. Only a query with nothing servable resolves
under `AwaitDecode`, where joining the in-flight decode is the correct and
still-single-flight behavior, and where absence remains the typed fail-fast.

The same rule governs `latest_complete_fresh` (the grep/context/callers ladder):
a reconcile installs the generation it publishes directly, so the decode-free
read normally hits; when it abstains the path serves the retained generation and
awaits the decode only when nothing is servable. Activation still owns the
decode — queries simply stop queuing on it while something can answer.

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

Per-group wraps are not enough on their own, because a group can simply have no
wrap. `dispatch_deadline_horizon_micros` returns `None` for anything that is
neither an application-surface operation nor a controlled read, so every graph,
info, analysis, health, and session tool — `tracedecay_context` included —
reached its handler carrying no deadline at all. A live Codex `context` call
hung for **900 seconds** against a daemon grinding a failing semantic publish
loop, and the client's own timeout, not the daemon, ended it.

The bound is therefore universal and lives at the single dispatch choke point
in `mcp::tools::handlers`, beneath the per-group wraps rather than beside them:
the carried admission deadline when one is present and shorter, otherwise
`TOOL_DISPATCH_CEILING`. A tool added tomorrow inherits it without opting in,
and no handler can opt out. The ceiling also clamps a carried deadline longer
than itself, so carrying a distant deadline is not an escape hatch. The few
tools whose requested work *is* a long job (running a test suite, an admin
index) carry `LONG_RUNNING_TOOL_DISPATCH_CEILING` instead — still bounded, and
still far below the 900 seconds that motivated this.

That ceiling is only the backstop. The hold it catches was real work on the
request path: `latest_complete_fresh` ran the freshness ladder's *remedy*
(`ensure_fresh_for_query`, a full O(store) reconcile and publish) inline on
whichever request won the scheduler lock. Serve-old-first already protected the
losers of that lock; the winner still paid. Query admission now runs the
ladder's cheap checks inline and hands the rebuild to the background worker,
answering from the retained generation exactly as the busy path always did. The
git authority is still proven inline with an O(1) probe, so a vanished `.git`
still fails closed rather than serving bytes under an identity nothing can
confirm, and a cold open with nothing servable still reconciles inline — the one
sanctioned slow path. The visible contract change is that an out-of-band commit
lands on the next background pass instead of being forced onto the first query
to notice it, which is what "serving never couples to indexing recency"
requires.

### 7. A request never parks on a store-sized hold

Serve-old/await-new applies to *locks* as well as generations. Three holds
violated it and are now closed:

- **Writer administration was one daemon-wide mutex.** Its own comment conceded
  that "a background refresh or a generation rebuild can hold this gate for
  minutes", and a git-watch sync held it across a full `cg.sync()`. The first
  request for an *unrelated* project parked behind it with no deadline. The gate
  is now per store (`daemon/store_writer_gate.rs`) with three classes —
  `Destructive` (branch-store GC, totally exclusive on its store), `Owner`
  (project open, owner rekey, scheduler transitions) and `Content` (index sync,
  background refresh) — under a daemon-wide `RwLock` that store-scoped writers
  hold shared and all-store sweeps hold exclusively. Exclusivity is preserved
  exactly: two writers of the same class on one store still contend, and
  `Destructive` still excludes everything on its store, which is what lets
  branch GC keep proving no holder before it unlinks a SQLite family. Request-
  side waits (project open) additionally carry a deadline and answer with the
  typed retryable `store_writer_busy` rather than queuing without bound.
- **The lazy stale-sync ran inline.** Edit-shaped tools walked the whole tree
  (`find_stale_files`) and reindexed the entire stale set on the request path.
  The cooldown claim is unchanged; the work is now detached through the same
  single-flighted lane read tools use, and the tool answers on the current
  snapshot.
- **Branch-drift reopen ran inline.** The request that noticed a checkout
  performed the full DB open plus sealed restore, then awaited the daemon owner
  reconcile — through the writer gate — inside a live `tools/call`; the
  branch-tracking-added path was worse still, blocking every caller on the
  reopen mutex. The reopen is now detached and single-flighted, the owner
  reconcile runs behind the swap, and every caller (the one that noticed the
  drift included) serves the last complete snapshot until the swap lands.

## Status map (2026-08-01)

| Principle | State |
|---|---|
| 7 per-store writer gates + request-side gate deadline | merged |
| 7 detached lazy stale-sync + detached branch-drift reopen | merged |
| 1 validation/admission/attribution memoization + generation LRU | merged |
| 1 snapshot hash indices (record port + relation BFS adjacency) | merged |
| 2 redundancy comparison-budget pacing + shared shingle merge | merged |
| 2 reconcile semaphore + retention round-robin | merged |
| 2 reserved-width indexing pool (barrier-free per-file fan-out, capture fan-out, admission 2) | merged |
| 3 lazy output digests + threaded projections + constant-digest memo | merged |
| 4 idna/remote-normalization memoization | merged |
| 5 paging/bounded heaps/batch IN | merged (20-finding wave) |
| 6 carried-deadline central wrap (git, memory) | merged |
| 6 universal dispatch ceiling (every tools/call group) | merged |
| 6 query-admission reconcile moved off the request path | merged |
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
Stores are now born row-per-vector: the inline-payload forward migration that
once rewrote a pre-migration document on open has been removed, so there is no
in-place blob rewrite left on the open path.

Measured A/B on identical code with only the encoding differing (2,000 chunks ×
768 dimensions, debug build): peak process RSS 227MB inline versus 125MB
row-per-vector, for the *same* published generation digest. Above the ~66MB
process floor that is 161MB versus 59MB — a 6MB float corpus was costing 155MB
to persist.

### The metadata split

The state document was still **O(store) in metadata** after the payload split,
and that was the binding constraint. Per-vector row metadata, the per-chunk
projection receipts, `plan.expected_chunk_ids`, `committed_chunk_effects`, the
prepared batches, the tombstone map and the physical-byte bindings all scaled
with the corpus and were all rendered into one JSON value bound as a single SQL
parameter against the runtime's 64MB `MAX_REQUEST_BYTES`:

| encoding | 2,000 chunks | 5,000 | 10,000 | 20,000 |
|---|---|---|---|---|
| inline floats | ok | `RequestLimitExceeded` | — | — |
| row-per-vector | ok | ok | ok | `RequestLimitExceeded` |
| externalized metadata | ok | ok | ok | ok |

Every one of those collections now lives in `semantic_vector_state_slice_v1`,
content-addressed by the SHA-256 of its encoded bytes, cut into bounded slices,
and verified against that address before it is parsed. The document keeps
generation-level identity only. Content addressing also makes publication free:
a staged collection and the published one it becomes hash alike, so the swap
writes no new slices.

`ExternalV1` serializes *transparently*, so a digest over a value containing one
is byte-identical to a digest over the bare collection — the build-identity
digest still hashes the full expected chunk list. Only the state-document
adapters elide. `DerefMut` clears the address, so a stale address is not
representable. Fresh stores are created at this shape; the forward-migration
path for a pre-externalization document has since been removed along with the
rest of the branch's migration machinery.

### Incremental commits

Production performed exactly one whole-corpus commit, so the entire float corpus
stayed live until a single terminal write and a crash mid-run discarded every
embedding. It now splits the request, commits each batch as it completes, and
resumes from the durable checkpoint.

Splitting is identity-preserving by construction. Boundaries land on multiples
of the projector's encoder group size, so every group holds exactly the changes
a whole-corpus pass would have given it; the tensor shape never changes, so
vector bytes, every `output_digest`, and the generation manifest digest built
from them are byte-identical. The plan is decided from the whole request before
any batch runs, so the generation's watermark and expected membership stay the
corpus's. Only execution lineage differs — one receipt per batch rather than one
for the corpus — which generation identity deliberately ignores.

Resume reads the staged checkpoint once, before any encoder work. The build
identity is a digest of the plan, so reopening the same plan re-adopts the same
staged build and skips the batches already durable rather than re-embedding
them.

Measured (768 dimensions, release build, 96-core host), where
`widest state document` is the value that used to grow with the corpus until it
hit the request limit:

| chunks | commits | widest state document | peak RSS |
|---:|---:|---:|---:|
| 30,000 | 1 | 2,961 B | 0.68 GiB |
| 30,000 | 8 | 2,961 B | 0.66 GiB |
| 75,000 | 19 | 2,962 B | 1.54 GiB |

The document is flat: the curve is per-batch, not per-corpus. The 30,000-chunk
rows publish the *same* generation `sha256:90f0a889…ed28dea8` at one commit and
at eight, which is the digest-equality proof that splitting moves no identity.

### Closed: the whole-corpus publication transaction

*Superseded.* At 150,000 chunks every batch committed and the publication then
failed with `SQLite execute failed: interrupted` — not the document ceiling (the
document is still ~3KB) but the publication transaction running past a runtime
guard: `MIGRATION_SQL_EXECUTION_LIMIT` bounds one guarded execution at 30
seconds, and the batch progress handler also trips on a repeated authority
check.

The dominant writer that pushed publication past that bound — the inline-vector
payload migration, which rewrote the whole corpus inside the same guarded
transaction — no longer exists: stores are born row-per-vector and the migration
was removed with the rest of the branch's migration machinery. The guard itself
(`MIGRATION_SQL_EXECUTION_LIMIT`) is unchanged, so a large enough single
publication could still trip it; the two mitigations below were never landed and
are recorded as options, not as pending work.

Publication is where the remaining O(store) SQL lives: it seals and writes two
collections built fresh at that moment — the concatenated per-chunk receipts and
the physical-byte bindings — and runs reclamation over every payload address.
Two things are worth trying, in order:

- `physical_vector_bindings` is fully derived from the generation's vectors and
  embedding key; `ensure_physical_reuse_index` already rebuilds the pool from
  them at load. Eliding the map entirely removes a corpus-sized collection from
  the publication write, at the cost of making the load-time binding check
  tautological.
- Reclamation is provably a no-op on a first publication, because content
  addressing means the staged collections' addresses are exactly the published
  ones. Skipping the sweep when the loaded reference set is a subset of the new
  one avoids hundreds of statements that delete nothing. Measured alone it did
  not lift the 150K ceiling, so it is a latency win rather than the fix.

Reclamation's anti-join was rewritten from `NOT IN (SELECT …)` to `NOT EXISTS`
against the scratch table's primary key, which is one index probe per row rather
than a scan of the reference set per row.
