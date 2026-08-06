# Native semantic code search

## Role

Semantic code search is a fully wired final-V2 capability. It augments exact,
lexical, and graph retrieval with local embeddings while preserving exact scope,
generation, privacy, coverage, ordering, cancellation, and typed availability.
It is not a staged rollout, shadow subsystem, optional future adapter, or
separate search product.

`00-plan-set-index.md` owns roadmap precedence. Plans 05 and 15 own the common
retrieval/fusion contracts, Plan 20 owns configuration, Plan 25 owns canonical
code generations and changed chunks, and Plan 39 owns the shared embedded
Grafeo graph/vector authority.

## Production journey

Every supported surface calls the same daemon application operation:

1. Resolve project, worktree snapshot, authorization, privacy domain, request
   budget, and immutable code generation.
2. Execute exact and lexical lanes from the current code generation and graph
   traversal from the same project GraphDb.
3. Admit the semantic lane only when the selected model, projection key,
   complete vector generation, code generation, authorization epoch, privacy
   domain, and active receipt all match.
4. Embed the bounded sanitized query in memory, run bounded top-k search through
   `tracedecay-graph-db`, and return stable chunk IDs plus distance evidence.
5. Fuse independent lane outcomes under Plan 15. Protected exact identifiers,
   paths, quoted phrases, errors, tool names, flags, and configuration keys
   cannot be demoted by semantic similarity or reranking.
6. Hydrate only the selected page through the owning code/content authority,
   recheck authorization, and return one typed coverage/freshness result.

MCP, CLI, HTTP, dashboard, SDKs, LSP, hooks, and host integrations never open
model, SQLite, Grafeo, generation, or content stores directly. Hooks only wake
daemon convergence and never run embedding or search.

## Canonical authorities

- Plan 25 code generations own sanitized canonical chunks, spans, language,
  extractor/chunker/sanitizer revisions, content identity, sensitivity, and
  exact source provenance.
- The daemon-owned SQLite authority stores relational publication intents,
  batch receipts, activation compare-and-swap state, model configuration, and
  failure/retry evidence. It stores no vector payload or graph topology.
- The one shared project `GraphDb` stores immutable vector-generation
  projections and their graph links to chunks, symbols, files, Git evidence,
  sessions, tasks, tests, diagnostics, and findings.
- `fastembed` is imported by one semantic runtime adapter. Domain, query,
  application, transport, dashboard, and SDK crates depend only on typed ports.
- The owning code/content authority hydrates result payloads. A vector entity,
  ranked candidate, or relational metadata row is never source content.

There is no production flat application scan, SQLite vector table, in-memory
GraphDb fallback, domain-specific graph store, duplicate active pointer,
`Fake*` production state, or test-only production call counter.

## Deterministic projection identity

The embedding projection key covers every vector-affecting input:

- exact model, tokenizer, configuration, instruction, and artifact digests;
- runtime/backend/build revision, deterministic device class, precision, and
  resource profile;
- pooling, normalization, dimension, metric, truncation side/length, and
  quantization when used;
- chunk schema, chunker, sanitizer, sensitivity, privacy-domain, and key epoch;
- exact source code generation and ordered eligible chunk manifest.

Search-index parameters have a separate key so changing only search structure
does not rerun embedding. Execution-only batch/thread settings must reproduce
the same vector digest or fail publication.

Partial, mixed, stale, failed, cancelled, or incompatible generations are never
queryable. A no-op changed set performs zero inference and zero graph writes.
A deletion writes a tombstone without inference. A one-symbol edit embeds and
publishes only changed symbol chunks and affected file-level chunks. A model or
projection-key change replays retained canonical chunks without reparsing
unchanged source.

## Publication and restart

Publication is restart-safe and delta-bounded:

1. Persist an immutable relational intent and ordered bounded batch plan before
   any vector graph write.
2. Embed only the planned changed chunks. Record exact per-batch source/output
   identities and terminal outcomes.
3. Publish only that generation's vector entities in bounded GraphWriteBatch
   chunks. Advance durable chunk progress after each successful graph write.
4. Validate membership, dimension, metric, finite values, output digests,
   source generation, and complete receipt coverage.
5. Atomically activate the complete generation with compare-and-swap. The
   prior compatible generation remains current until this succeeds.
6. Retire an unreferenced generation by projection identity under ordinary
   retention; never rebuild or clone all other vector generations as a side
   effect.

Restart resumes the pending intent from the last committed batch. It neither
hydrates every stored vector nor republishes all staged/current generations.
Interrupted graph-before-receipt and receipt-before-activation windows remain
typed, idempotent, and recoverable.

## Scheduling and performance

The existing daemon semantic scheduler is the sole owner. It consumes Plan 25
changed-chunk receipts, coalesces superseded work by exact project/worktree and
source generation, bounds queued bytes/model sessions/publication concurrency,
and yields to foreground exact/lexical/graph/MCP work.

Search never waits for projection, acquisition, reconciliation, or model load.
Fallback-allowed requests omit the semantic lane with a typed reason and keep
the exact/lexical/graph subpayload byte-stable. Strict-semantic requests return
typed unavailable, stale, saturated, timed-out, or cancelled as observed.

Status is evidence-backed: queued target, model state/generation, processed and
remaining chunks/batches, last progress, measured throughput, ETA range when
enough samples exist, stalled reason, current generation, and prior compatible
generation. Missing samples remain absent; reads do not mutate counters,
refresh stores, or advance access metadata.

Required measurements use production entry points and separate:

- cold acquisition/load from warm query;
- clean/no-op, one-symbol, deletion, model-key replay, and large catch-up;
- graph publication from relational receipt/activation time;
- exact, lexical, graph, semantic, fusion, rerank, and hydration latency;
- current and production-scale corpora, concurrent queries, cancellation,
  restart-resume, CPU, peak RSS, disk I/O, writer wait, and graph bytes.

Large existing generations plus small deltas must demonstrate work proportional
to the delta. Selected-result hydration uses bounded batch point reads and
never one store call per corpus vector.

## Model and offline lifecycle

Configuration selects exact embedding and optional reranker profiles. The
daemon may acquire a cataloged immutable Hugging Face revision in background.
Each declared member has an exact length and SHA-256 digest; verified bytes are
atomically installed under the lifecycle authority. Query/runtime paths never
download, discover an ambient cache, invoke a network model, or open an
unverified local path.

Offline startup preserves exact/lexical/graph search. Missing, corrupt,
incompatible, oversized, or unavailable model artifacts produce typed semantic
unavailability without silently selecting another model. Bounded warmed
sessions are keyed by complete model/runtime/privacy identity. Cancellation,
OOM, load failure, and worker termination preserve the prior active generation.

Artifact cleanup cannot remove active or rollback members. Explicit manual
imports exist only when a real daemon/application surface owns their validated
journey; unmounted import APIs are deleted.

## Fusion, reranking, and privacy

Semantic retrieval emits an independent candidate batch carrying stable chunk
identity, projection/vector generation, metric/distance, scope, and coverage.
It cannot read other lane candidates, mint exact evidence, fuse, diversify,
rerank, hydrate, or infer impact/lineage/equivalence from similarity.

Plan 15 owns quality profiles and activation evidence. Optional reranking is
bounded to an admitted top-N candidate set and preserves the pre-rerank result
when unavailable. Raw similarity/logits are not confidence. Calibration is
profile- and generation-bound; missing or shifted calibration is explicit.

Raw query text, query vectors, source text, private paths, hydrated
explanations, credentials, and model payloads never enter telemetry, receipts,
cursors, or checked-in reports. The sanitized query view is bounded,
non-serializable, and request-local. Cache identity includes authorization,
scope, privacy domain/key epoch, code/vector generations, model/projection
keys, query digest, profile, and pagination. Any mismatch yields zero reuse.

## Typed outcomes

Every lane reports its own `available`, `partial`, `stale`, `indexing`,
`unavailable`, `unsupported`, `saturated`, `timed_out`, `cancelled`, or
`failed` state with bounded coverage. Semantic failure never becomes successful
zero output and never changes exact/lexical/graph ordering or cursor identity.
Denial invokes no model/vector/content port. Revocation before hydration
returns no payload.

## Direct acceptance

- One production daemon/application search journey is reachable from every
  supported surface with canonical catalog metadata, deadlines, cancellation,
  paging, and typed errors.
- Exact-only, lexical, graph, semantic, fusion, rerank, and hydration tests
  exercise real owners rather than synthetic production counters or adapters.
- A blocked semantic worker cannot delay exact/lexical/graph results; a complete
  compatible activation is the only event that adds semantic candidates.
- No-op, one-symbol, deletion, key replay, cancellation, crash windows, restart,
  rollback, stale generation, wrong scope, privacy rotation, and graph
  unavailability have falsifiable behavioral tests.
- Grafeo owns vector payload/search and cross-domain links; SQLite owns only
  relational intent/receipt/configuration state; no flat or SQL fallback exists.
- Quality evaluation covers protected exact queries, natural-language intent,
  renamed/same-name symbols, no-answer/wrong-scope cases, generated/vendor
  noise, incremental edits, and supported languages. It records per-stratum
  recall/precision/ranking plus resource distributions.
- Isolated-profile workspace/all-feature, dashboard, SDK, host bundle,
  package, offline, and ordinary CI checks pass without dogfooding the
  operator's installed TraceDecay.

The capability is incomplete while any public surface is discovery-only,
semantic publication rebuilds unrelated generations, a read mutates hidden
test state, a model/vector path bypasses the daemon, or active documentation
describes deferred wiring.
