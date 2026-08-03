# PR10: Native FastEmbed semantic code search

**Status:** active PR10 implementation and product-test authority. The current
checkout contains callable FastEmbed, exact-flat, vector-generation,
calibration, fallback, and runtime-routing artifacts, but PR10 remains
unfinished until the Plan 15 Linux comparison, direct tests, and normal CI
pass. PR9 must still ship and test the exact/lexical/graph fallback and
exact-tier behavior before PR10 can activate semantics. Those versioned PR9
results become immutable runtime prerequisites for semantic composition.

PR9 baseline/profile and generation versions are reproducibility identities,
not evidence that a predecessor wire contract shipped. Pure source-only/internal
PR9/PR10 request helpers, wire-visible V2 request revisions, index/profile
manifests, generations, model bindings, and activation receipts change in
place. Only their exact final persisted shape is accepted; any other database,
store, spool, file, or projection returns typed `ResetRequired` and requires
explicit reset or recreation. No storage reader, migration, backfill, dual
write, or census path exists. Public protocol compatibility is separate and
requires actual independent release evidence.

**Operational qualification (2026-07-27).** The callable semantic machinery
does not mean the live profile is currently serving semantic results. Semantic
search is disabled because the admitted configuration snapshot is invalid.
Plan 20 owns repairing and validating that snapshot; this plan owns mounting a
compatible complete semantic generation and proving successful activation.
Until both occur, fallback-allowed requests preserve exact/lexical/graph
results and strict-semantic requests return typed unavailable. This is truthful
degraded behavior, not PR10 acceptance.

Plan 31 owns the PR10 semantic adapter, projection/runtime, and direct
testing. Plan 15 owns quality evaluation, while Plan 25 owns the PR9 code
generation and lexical/graph prerequisite. Plans 09/10/11/12/14 are later
application and surface consumers. Consumers audit tested callable behavior,
quality fixtures, and direct regressions; they do not rebuild PR9/PR10 by old
module, type, fixture, benchmark, or suite-spine names.

## Outcome

TraceDecay augments exact lexical and graph search with local code embeddings.
Exact results remain authoritative without a model; similarity alone never proves impact, lineage, or equivalence.

## Ownership

- Plan 25 builds deterministic, storage-neutral `CodeSearchDocumentV1`,
  `CodeSearchChunkV1`, `ChangedCodeChunkSetV1`, and
  `CodeIndexCapabilityManifestV1` values from an exact code snapshot.
- Plan 04 defines deterministic changed-input projection, checkpoint, retry,
  and publication semantics. Daemon/service orchestration decides when that
  resumable work is scheduled.
- Plan 02 stores immutable vector generations, manifests, checkpoints, the
  atomic active-generation pointer, and chunk projection receipts through
  daemon-owned writer authority.
- Plans 05/15 own the common `CompactCandidate`, independent lane, deterministic
  fusion, contribution, diversity, rerank, hydration, cursor, fallback,
  explanation, and redundancy contracts to be shipped in PR9. PR10 adds one semantic
  adapter; it does not create a parallel code-search kernel.
- Plan 20 owns versioned model/reranker profile selection, configuration
  precedence, and atomic active/rollback profile pointers. This plan owns the
  root-private versioned artifact manifest and SHA-256 integrity verifier,
  FastEmbed runtime adapter,
  bounded sessions, semantic generation service, and their typed ports. PR11's
  Plan 09 application layer later composes those accepted ports; it does not
  retroactively own PR10's model implementation.
- Daemon/application orchestration schedules changed eligible chunks and
  rebuilds. Plan 04 owns deterministic projection, checkpoint, retry, and
  publication semantics, not scheduling policy.
- Plan 15 exclusively owns retrieval-research design, versioned corpora and
  labels, metrics and strata, candidate profile comparison, thresholds, and
  activation recommendations. PR10 implements measured profiles and emits
  results; it cannot tune or activate itself from aggregate or public benchmark
  rank.
- Plans 10/11/20/21 expose the same application operations through API,
  dashboard, configuration, CLI, and MCP.

Only one root-private adapter depends on `fastembed`. Crates for indexing, store,
query, API, and UI depend on ports and stable domain values, never
FastEmbed runtime types.

## Behavioral contracts and historical layout

The semantic boundary must preserve the ownership and fields below. The listed
paths and Rust names are a historical design sketch, not artifact-name parity
requirements. Current owner-approved names and locations are valid when direct
contract regressions prove the same lane isolation, generation compatibility,
authorization, coverage, fallback, and receipt behavior.

The original PR10 design proposed these files:

- `crates/tracedecay-domain/src/code_intelligence/search.rs`:
  `EmbeddingProjectionKeyV1`, `SemanticSearchIndexKeyV1`,
  `SemanticCapabilityManifestV1` and
  code-specific semantic evidence values. Generic ranked-list, compact-
  candidate, contribution, fusion, rerank, hydration, and cursor values remain
  in PR9's `crates/tracedecay-domain/src/retrieval.rs`.
  Semantic projection uses Plan 25's `CodeChunkProjectionReceiptV1` and
  `ProjectionBatchReceiptV1`; no semantic-specific receipt authority exists.
- `src/semantic_code/artifacts.rs`: versioned manifest parsing, SHA-256 member
  integrity verification, import staging, atomic install, inventory, retention,
  and quarantine.
- `src/semantic_code/manifest.rs`: model/runtime/projection and capability
  manifest validation.
- `src/semantic_code/projector.rs`: changed-chunk batching, receipt production,
  resumable checkpoints, and publication handoff.
- `src/semantic_code/fastembed_adapter.rs`: the only `fastembed` import and the
  only model inference implementation.
- `src/semantic_code/session_pool.rs`: bounded sessions keyed by the complete
  projection/privacy identity.
- `src/query/retrieval/semantic.rs`: the code-semantic implementation of the
  PR9 `Retriever` port. It emits generic `CompactCandidate` values carrying a
  typed code anchor and semantic evidence; it consumes neither lexical nor
  graph candidates.
- `src/semantic_code/service.rs`: temporary root-private PR10 orchestration for
  artifact/runtime admission, projection generation, shadow execution, and
  activation/rollback. It exposes typed ports consumed by the PR11 application
  layer and contains no transport binding.
- `src/config/retrieval.rs`: Plan-20-owned versioned profile definitions and atomic
  active/rollback profile pointers.
- `src/store/vector_generations.rs`: Plan-02-owned implementation of the
  semantic projection read/write ports. Query and semantic modules do not
  depend on its physical schema.

The semantic adapter extends the PR9 retrieval contract rather than replacing
it:

```rust
pub struct SemanticRetrievalRequestV1 {
    pub query_digest: QueryDigest,
    pub query_view: EphemeralSanitizedQueryViewV1,
    pub authorized_scope: AuthorizedScopeReceiptV1,
    pub authorization_epoch: AuthorizationEpoch,
    pub scope_digest: ScopeDigest,
    pub code_generation: CodeGenerationId,
    pub projection_key: EmbeddingProjectionKeyV1,
    pub capability_manifest_digest: ManifestDigest,
    pub budget: RetrievalBudget,
    pub continuation: Option<RetrieverContinuation>,
}

pub struct SemanticCodeRetriever {
    // Root-private FastEmbed session and semantic projection read ports.
}

// SemanticCodeRetriever implements
// Retriever<SemanticRetrievalRequestV1, CodeSemanticEvidenceV1>.

pub struct CodeSemanticEvidenceV1 {
    pub projection_key: EmbeddingProjectionKeyV1,
    pub vector_generation: VectorGenerationId,
    pub chunk_id: CodeSearchChunkId,
    pub distance: CanonicalSemanticDistanceV1,
    pub search_kind: SemanticSearchKindV1,
}
```

The adapter implements the one PR9 `Retriever<R, E>` port and receives one
frozen authorized scope, code generation, semantic
projection, and bounded ephemeral sanitized query view. It returns the generic
Plan 15 batch with one `CompactCandidate` and one occurrence-keyed
`CodeSemanticEvidenceV1` per result. It cannot mint exact
admission, read another lane's candidates, fuse, diversify, rerank, hydrate, or
retain raw query text/query vectors. Store conformance uses separate lexical,
graph, vector, receipt, and hydration representations; no table, vector index,
or materialized join becomes authority.
Deadline and cancellation limits are fields of the shared `RetrievalBudget`;
PR10 does not introduce semantic-only budget or continuation types.

## Deterministic documents and generations

Each chunk records repository/project/worktree/ref/snapshot identity, immutable
code generation, file and symbol identity, source span, language/extractor and
chunker versions, sensitivity decision, content digest, stable ordinal, and
bounded sanitized text. Symbol boundaries are preferred; oversized symbols use
versioned structural splits. Generated, vendor, binary, ignored, fixture, and
unsupported content has an explicit classification.

A vector-generation identity includes the ordered eligible-document manifest,
model/tokenizer/runtime manifest, dimension, metric, normalization, chunker,
privacy domain/key epoch, and source watermark. The manifest also pins query
and document instructions/prefixes, pooling, truncation side and length,
precision/quantization, runtime/backend/thread/device identity, and search or
ANN parameters when present. Builds checkpoint in bounded batches. Partial or
mixed generations are never queryable. Publication verifies membership,
dimensions, finite values, digests, and watermark before one atomic pointer
swap; deletion creates a tombstone and unchanged inputs do no embedding work.

`EmbeddingProjectionKeyV1` pins every vector-affecting input: model artifact,
tokenizer/config and instruction digests, pooling, truncation side/length,
runtime/backend/build revision, deterministic device class, dimension, metric,
normalization, precision/quantization, chunk schema/chunker revision, privacy
domain, and key epoch. Its canonical digest is Plan 25's
`ProjectionKeyV1.profile_digest`. Search/ANN structure parameters use a
separate `SemanticSearchIndexKeyV1`; changing them rebuilds only derived search
structures and query caches. Thread/batch settings remain execution-manifest
evidence and must produce the same vector digest or fail publication. Vectors
and projection rows are replayable derived data; canonical chunks and exact
code-generation evidence remain authority.

For each `ChangedCodeChunkSetV1`, the projector returns one ordered
`CodeChunkProjectionReceiptV1` per added/changed/deleted chunk and an explicit
aggregate reused count in `ProjectionBatchReceiptV1`. A receipt records source
generation and manifest, projection key, chunk ID, prior/current digest,
`embed | tombstone`, completed/skipped/failed outcome, and output digest.
Deleted receipts bind `prior_generation`/`prior_chunk_digest`, the batch
`request_digest`, and have no current digest.
Reused chunks produce no per-chunk receipt. A no-op request with an unchanged
target projection key invokes no inference; a key-only replay expands reused
eligible chunks into explicit embed operations before dispatch. A one-
symbol edit embeds only its changed chunks and affected file-level chunks. A
model, tokenizer, instructions, runtime-compatibility, dimension, metric,
normalization, precision, privacy-domain, or key-epoch change creates a new
projection generation by replaying retained canonical chunks without reparsing
unchanged source. Old retained projections remain immutable and addressable by
their exact key; partial receipt sets never activate.

### Worktree-aware projection scheduling

- Consume Plan 25's reconciled `ChangedCodeChunkSetV1`; do not watch files,
  rescan repositories, invoke Git status, or infer changes inside the semantic
  runtime.
- Batch only added/changed eligible chunks through FastEmbed
  `TextEmbedding::embed`. Construct the model through
  `try_new_from_user_defined` from explicitly installed local ONNX/tokenizer/
  config bytes. Query and projection paths never invoke the hub, discover an
  ambient model cache, or download artifacts.
- A no-op changed set performs zero model calls. Deletions produce tombstones
  without inference. A projection-key change replays retained eligible chunks
  without parser/extractor work; a search-index-key-only change rebuilds only
  the derived exact-flat/search structure.
- Reuse vector bytes across worktrees only when canonical chunk content,
  projection key, privacy domain, and key epoch match. Every projection row,
  receipt, vector generation, and active pointer remains bound to its exact
  source worktree snapshot and code generation.
- Coalesce superseded projection batches by exact worktree and source
  generation. Bound queued bytes, batch size, session count, model memory, and
  publication concurrency. Interactive exact/lexical/graph queries have
  priority; semantic query admission is fail-fast and never enters the
  projection waiter queue.
- Keep a prior semantic generation queryable only when its complete projection
  key and source compatibility still match the frozen code request. Otherwise
  omit semantics until the new complete generation atomically publishes.

## Model and offline lifecycle

Configuration selects a versioned embedding profile and, independently, an
optional reranker profile. With automatic acquisition enabled, the daemon
queues the selected catalog model's immutable Hugging Face repository revision
in the background. Manifests pin actual model/tokenizer/config bytes, licenses,
runtime/build identity, dimensions, normalization, metric, device, threads,
batching, and resource ceilings. Implementation selects maintained
crate/runtime versions during PR10; activated model and reranker profiles still
require a passing Plan 15 evaluation. This plan contains no stale crate or
model-version pin.

Every acquisition/import path verifies artifacts before installation or
activation. Queries never download a model or open an ambient cache. Offline
startup remains healthy and PR9-baseline-complete. Compatible warmed sessions
are pooled under bounded memory, concurrency, idle, and cancellation policy.
Load failure, OOM, corruption, missing bytes, or incompatible pins disables the
affected semantic stage without silently selecting another model.

Artifact handling is explicit:

- A versioned canonical manifest lists profile kind, model/tokenizer/config file digests,
  byte lengths, licenses, upstream source metadata, runtime compatibility,
  projection inputs, and the complete resource ceiling. The manifest and each
  declared member are identified by SHA-256 solely to detect drift, corruption,
  or incomplete installation. These versioned manifests and integrity checks
  are the complete artifact-verification contract.
- Daemon-owned background acquisition resolves only the cataloged repository
  and immutable revision into an explicit cache under the lifecycle root. It
  stages each member, independently checks its declared length and SHA-256,
  and atomically publishes the complete verified install. Query/runtime paths
  perform no download, import, extraction, source discovery, or network
  inference.
- Explicit local-path and configured-HTTPS import APIs remain available as
  manual operations, but no production surface currently wires them. They are
  not the delivered acquisition journey.
- Interrupted imports are resumable only when the source supplies immutable
  length, digest, and range identity; otherwise staging is discarded. Archive
  traversal, symlink/hardlink entries, absolute paths, duplicate members,
  undeclared members, size expansion beyond the manifest, and digest mismatch
  quarantine the import without exposing it to runtime discovery.
- Installed artifacts live in one Plan-02-owned user store keyed by versioned
  manifest digest, never an ambient Hugging Face/ORT/FastEmbed cache. The
  daemon's lifecycle-root Hugging Face cache is an acquisition source only,
  not runtime or installation authority. Inventory records distinguish
  `staged | verified | installed | quarantined | retained_for_rollback`.
  Garbage collection may remove only unreferenced non-active/non-rollback
  artifacts after a daemon lease and append-only receipt.
- Activation is an authenticated Plan 20 compare-and-swap from the expected
  active/rollback profile digests. It verifies artifact inventory, accepted
  Plan 15 report, runtime/platform compatibility, complete projection
  generation, resource ceiling, and executable rollback before publishing.
  Rollback uses the same checks and must pass a cold-start/offline drill.

The mandatory Plan 25 `CodeIndexCapabilityManifestV1` admits lexical/graph
retrieval. Optional `SemanticCapabilityManifestV1` pins the authorized scope
digest, code and vector generations, projection and search-index keys,
supported chunk grains/languages, coverage, partial states, privacy domain/key
epoch, and manifest digest. Fusion, candidate budgets, diversity, reranking,
and hydration are validated separately from the active Plan 15/20 profile. The PR10 service validates the base
manifest before any channel and validates the semantic augmentation only before
semantic/rerank work. Missing semantic capability yields lexical/graph mode; an
explicit strict-semantic request yields the typed unavailable result.

## Query and redundancy

Search resolves exact scope and frozen generation first, reproduces the
versioned PR9 exact+lexical+graph baseline, then adds a compatible semantic
candidate list. Fusion is stable and explainable; Plan 15's exact-admission
authority keeps exact identifiers, paths, quoted phrases, errors, tool names,
and configuration keys in a non-demotable tier. The first production semantic
baseline is deterministic exact flat-vector search unless measured
current/10x evidence shows it violates a reviewed resource budget. Optional
reranking is bounded to a configured top-N candidate set, is admitted only
after candidate-controlled gain with no protected-stratum regression, and
preserves the pre-rerank list byte-for-byte when unavailable. Raw similarity,
logits, margins, or fused scores are not confidence; calibrated abstention
requires a versioned cohort/generation-bound profile and reports invalid or
shifted calibration explicitly. Strict semantic requests return a typed
unavailable result.

Semantic projection and indexing run asynchronously. Existing exact, lexical,
and graph operations remain callable and return without joining, polling, or
waiting for semantic work. The semantic lane is omitted until one complete
compatible immutable vector generation becomes current through a single atomic
publication. Staged, partial, indexing, stale, failed, cancelled, or
incompatible generations contribute no candidates, score, cap pressure,
cursor bytes, or rank effects. A request for strict semantic alone may return
typed unavailable; it may not delay or replace the normal PR9 path.

`code.redundancy` reuses the same active generation. It canonicalizes pairs,
removes self/overlapping chunks, and reports `exact_clone`,
`structural_near_duplicate`, `semantic_analogue`, or `insufficient_evidence`.
Semantic-only matches remain review candidates, never automatic rewrites or CI
violations. Disabled semantics preserves the structural baseline and ordering.

The Plan-05 query pipeline executes these phases without combining them:

1. Resolve and authorize project/repository/worktree/ref scope; freeze code,
   exact, lexical, graph, and compatible semantic generations; validate the
   base and semantic capability manifests, authorization epochs, and budgets.
2. Reuse PR9's separately inspectable `RetrieverOutcome` values for
   `ExactLiteral`, `Lexical`, and `Graph`; produce one independent
   `Semantic` outcome. No semantic code calls, wraps, filters, or mutates
   another lane.
3. Validate/canonicalize the semantic outcome under the generic Plan 15 lane
   contract: finite canonical scores, candidate identity dedupe, generation and
   authorization equality, stable lane-local ordering, complete coverage, and
   a bounded continuation.
4. Pass all compact candidates to the one PR9 fusion implementation. The
   existing exact tier remains lexicographically first and cannot be demoted by
   graph/semantic scores, diversity, or reranking. PR10 evaluates semantic
   weighting/fusion candidates—including exact rational RRF if proposed—on
   the same recorded candidate inputs; no algorithm, constant, or weight
   activates without a passing Plan 15 evaluation.
5. Use the complete comparator and diversity policy from the tested generic
   profile. The cursor binds emitted identities, ordered overflow, every lane
   continuation, profile/generation/projection digests, and authorization
   epoch. Input insertion order, hash order, and completion order cannot affect
   IDs, order, contributions, explanations, coverage, or cursor bytes.
6. Retain every generic `CandidateContribution`, including lane-local raw score
   domain/rank, calibrated fixed-point feature when valid, weighted
   contribution, profile/projection identity, evidence class, and exclusion
   reason. Graph path coverage/authority and semantic projection/distance
   provenance remain typed lane evidence. A fused score is not probability or
   confidence.
7. Optionally rerank only the profile-bounded non-protected candidate set while
   preserving the pre-rerank list and contributions. Absence/failure returns
   those bytes unchanged.
8. After fusion, diversity, reranking, and page selection, reauthorize and
   hydrate only selected hits through the PR9 hydration port. Hydration may add bounded
   symbol text, file context, declarations, and graph neighbors from the same
   generation; it cannot add candidates or change rank.

Plan 15's generic `FusionProfile` pins the selected algorithm, lane features/
weights, candidate budgets, diversity caps, protected exact classes, rerank
budget, hydration budget, and total comparator. Plan 15 recommends that profile
from direct evaluation; Plan 05 implements it, and Plan 31 supplies
only the semantic lane, artifact/runtime/projection implementation, and
measurements.

## Authorization and local/private boundary

- Owning source stores authorize exact scope, operation, privacy domain, key
  epoch, and code generation. Plan 20's accepted profile and the PR10 service
  separately authorize semantic projection generation and runtime profile. The
  PR10 query composition validates both receipts before invoking the semantic
  lane; source denial invokes zero channels, while
  semantic denial invokes no semantic/rerank port and preserves authorized
  lexical/graph execution.
- Candidate references carry authorization receipt ID and epoch without private
  payload. Immediately before hydration, the shared query pipeline rechecks current
  authorization, scope, privacy domain/key epoch, and frozen generation.
  The owning store performs atomic receipt validation plus bounded payload/
  neighbor read; every neighbor must be inside the authorized scope. Revocation
  before or during a multi-read discards the complete hydrated hit and returns
  a typed denial with no source text or neighbor payload.
- All caches are local and privacy-domain/key-epoch separated. Model artifacts
  key by versioned manifest digest; sessions by embedding projection key; vectors
  and checkpoints by projection key plus source generation/chunk digest.
  Result caches additionally key by authorization receipt/epoch, authorized
  scope and query digests, fusion/rerank/request revisions, and pagination.
  Hydration caches additionally key by candidate identity, requested fields,
  and neighbor budget. Domain/epoch changes produce zero hits in every cache;
  authorization changes produce zero result/hydration hits. No ambient model
  cache, network inference, browser runtime, external process, or cross-domain
  cache is permitted.
- Raw queries, source text, vectors, private paths/symbols, and hydrated
  explanations never enter telemetry or checked-in benchmark artifacts.
  A raw query is sanitized into a bounded ephemeral query view before model
  inference. `EphemeralSanitizedQueryViewV1` contains bounded sanitized bytes
  plus sanitizer and normalization revisions; it is non-serializable,
  non-cacheable, and valid only for the authorized request. `QueryDigest` is a
  privacy-domain/key-epoch keyed MAC over that view, never an unkeyed content
  hash. `QueryDigest` is owned by
  `crates/tracedecay-domain/src/retrieval.rs` and may appear only in in-process
  request state, authenticated cursor identity, and privacy-separated local
  cache keys. `EphemeralSanitizedQueryViewV1` is owned by
  `code_intelligence/search.rs`. Neither value may enter receipts, telemetry,
  durable stores, or checked-in artifacts. Query text and vectors remain in
  memory only for that request.
  Operational receipts contain opaque identities, revisions, outcomes, counts,
  and digests only. Only sanitized fixtures and aggregate Plan 15 reports may
  enter Git.
- Semantic errors, timeout, cancellation, OOM, corruption, or missing artifacts cannot
  broaden scope. When the selected profile permits fallback, the lexical/graph
  lane outcomes and Plan 15's named PR9 fallback subpayload are byte-identical.
  The enclosing response may add only a typed semantic/rerank outcome outside
  that subpayload, its digest, and cursor identity; strict mode fails closed.

## Plan 15 evaluation handoff and fresh-store reset

PR10 ships a versioned sanitized corpus covering exact names, natural-language
intent, mixed queries, renamed symbols, same-name cross-scope cases, no-answer
queries, generated/vendor noise, large symbols, unsupported languages, and
incremental edits. It measures exact-hit retention, precision/recall/MRR/nDCG,
wrong-scope and no-answer error, worst strata, build/update time, p50/p95/p99,
CPU/RSS, model/vector/cache bytes, cancellation, and offline behavior.

Plan 15 owns this corpus's partition/label policy, metrics, thresholds, and
activation recommendation. PR10 owns reproducible Linux execution and truthful
`pass`, `fail`, or `pending` summaries. Activation requires a passing Plan 15
evaluation showing no scope/privacy or protected
exact/no-answer/wrong-scope regression, demonstrated semantic gain, and
declared current/10x Linux resource observations. Sensitive or
ineligible bytes never enter documents, artifacts, metrics, explanations, or
model-assisted routes.

Late interaction, quantization, and specialized ANN remain measured candidate
profiles, not PR10 defaults. ANN is admitted only when it beats exact search's
reviewed resource budget while meeting exact-oracle average, tail, minimum,
and zero-recall-query gates under immutable-generation compatibility. No HNSW,
DiskANN, ScaNN, vector database, precision, or quantization choice is
mandatory. Public benchmark rank cannot select a production profile.

Non-final vectors are never trusted, republished, or converted. They return
`ResetRequired`; an operator explicitly resets or recreates the generation from
current canonical documents without using the old vector as input.

## Current delivery audit

The audit below separates callable delivery from pending direct execution. A
present source file or type name is not completion by itself; the cited direct
regressions invoke the relevant boundary. Static fixture validation is useful,
but the current Linux evaluation remains pending.

**Production lifecycle correction (updated 2026-07-26).** Commit `dd4adbe2a`
ships daemon-owned immutable `hf-hub` background acquisition.
`shared_lifecycle_owner()` opens a lifecycle-root-scoped
`HfHubModelMemberSourceV1`; startup queues acquisition when configured, and the
source resolves only the cataloged repository/revision before the lifecycle
checks every member's declared length and SHA-256 and atomically installs the
package. Status and Doctor expose downloading, verifying, installed, and
failed states. A clean temporary profile downloaded and digest-verified the
full default Jina model end to end, and packaged offline acceptance installs
the same verified model from the daemon-owned cache. The explicit local and
configured-HTTPS import APIs remain manual and production-unwired; they are not
being certified by this correction. Installed model bytes remain semantically
omitted until compatible vector indexing publishes readiness, preserving the
asynchronous non-blocking contract.

**Why the old green gate was misleading.** Before `dd4adbe2a`,
`HfHubModelMemberSourceV1::fetch_member` unconditionally returned
`semantic model lifecycle operation rejected`, so no shipped binary could
acquire a model. The distribution gate's packaged-semantic half tested only
typed fallback/strict-unavailable behavior—the exact terminal absence produced
by that rejection—and therefore encoded the bug as its passing specification.
The corrected gate now runs packaged background acquisition and verifies the
installed Jina members; restoring the unconditional rejection makes that gate
fail, and restoring the source makes it pass.

- **Library-first FastEmbed:** delivered. The root manifest keeps
  `fastembed` optional with upstream defaults disabled, the
  `semantic-fastembed` feature selects the native runtime, and
  `FastEmbedEmbeddingRuntime::open_session` uses FastEmbed's user-defined local
  byte constructor. Native model execution and resource evidence remain
  pending.
- **Default equals all features:** delivered in the root feature manifest.
  Normal Linux/macOS/Windows CI builds and tests that default-feature product
  posture.
- **Daemon-owned immutable acquisition:** delivered. Startup queues the
  selected catalog revision without blocking startup or query paths; the
  lifecycle-root source cache is explicit, and verification plus atomic install
  remain independent of `hf-hub`.
- **Local verified model bytes only:** delivered at the runtime boundary.
  Model, tokenizer, and config bytes come from the installed manifest members;
  runtime construction has no hub, ambient-cache, download, external-process,
  or network-inference path. Background acquisition now creates the verified
  installed manifest and members; explicit local/HTTPS imports remain
  production-unwired. Direct tests validate constructor and integrity reads,
  while cross-platform runtime coverage remains normal CI work.
- **Exact-flat semantic baseline:** delivered by
  `SemanticCodeRetriever`/`SemanticVectorReadPort::scan_exact_flat`, with direct
  deterministic ordering, provenance, generation, and coverage regression in
  `exact_flat_scan_is_deterministic_and_emits_generic_semantic_evidence`.
- **Immutable generations and atomic publication:** delivered by the staged
  vector-generation store. Direct regressions
  `indexing_and_cancellation_leave_only_the_compatible_prior_generation_queryable`
  and `checkpoint_and_active_pointer_publish_atomically` prove staged work is
  not queryable, cancellation preserves the current generation, and a failed
  publication cannot expose half of a swap.
- **Asynchronous, non-blocking indexing:** delivered at the projection,
  generation-selection, runtime-routing, and bounded-session boundaries.
  `only_a_current_receipt_routes_to_semantic_search` routes indexing, degraded,
  unavailable, and rollback states to the frozen PR9 fallback, while
  `saturated_runtime_omits_semantics_without_entering_the_waiter_queue` proves
  query work does not wait for semantic capacity.
- **Calibrated abstention:** delivered. Missing or shifted calibration invokes
  no semantic authority and preserves fallback; distance and margin rejection
  are versioned and generation-bound. Strict semantic returns typed unavailable.
  Linux threshold evaluation remains pending.
- **Byte-stable PR9 fallback:** delivered at the semantic service boundary.
  Direct regressions retain the caller-owned validated fallback object through
  augmentation and every tested abstention path. The versioned PR9 fallback
  bytes and digest remain pending direct PR9 verification.
- **Evaluation before activation:** still pending. Current runtime
  routing requires an observed current activation receipt and rejects an
  indexing receipt, but the Plan 15 Linux evaluation, current/10x measurements,
  normal cross-platform CI, and rollback drill are still incomplete.
  `result-pending.json` therefore keeps activation false and truthfully reports
  `pending`.

## Planned behavioral delivery and direct verification

PR10 remains unfinished. The checkpoints below are required product behavior.
Paths, type spellings, fixture filenames, benchmark entrypoints, and test-suite
registration are non-normative historical suggestions. Completion follows the
callable semantic operation, direct regressions, the Plan 15 Linux evaluation,
and normal CI.

1. **Contracts and capability admission:** add semantic-only domain values,
   ports, manifest validators, ephemeral query-view rules, and split in-memory
   adapters. Reuse PR9's generic retrieval/fusion/hydration behavior. Direct
   regressions cover contract validation, capability admission, and storage
   independence.
2. **Artifact and runtime foundation:** implement one root-private artifact
   verifier, manifest validator, FastEmbed adapter, and bounded session pool.
   Direct regressions cover versioned manifest and SHA-256 member verification,
   local and explicit HTTPS import,
   traversal/expansion rejection, interrupted staging, atomic install,
   quarantine/GC, cold and warm sessions, OOM, cancellation,
   offline startup, no ambient cache, and Linux/Windows native-runtime
   compatibility.
3. **Incremental vector projection:** implement the projector and vector-generation
   store against Plan 04 projection/checkpoint semantics, Plan 02
   receipt/publication authority, and PR10 runtime ports. Daemon/service
   orchestration owns asynchronous worktree-fair bounded scheduling; do not
   assign it to Plan 04. Direct regressions cover changed-chunk batching,
   deletion without inference, no-op zero inference, cross-worktree physical
   reuse without identity reuse, superseded-batch cancellation, model-key
   replay, search while indexing without waiting, omission of incomplete/
   incompatible generations, atomic publication, cancellation/failure
   isolation, and offline lifecycle.
4. **Exact-flat semantic retrieval and shadow composition:** implement one
   independent semantic outcome and compose it with frozen PR9 lane outputs
   through the shared fusion/diversity/cursor behavior. Direct regressions
   cover lane isolation, contribution provenance, protected exact results,
   diversity/pagination, and byte-identical fallback.
5. **Late hydration, privacy, and rollback:** reuse PR9 hydration with semantic
   profile admission, generation checks, authorization recheck,
   domain-keyed caches, payload-safe receipts, active/rollback pointer CAS, and
   cold offline rollback. Direct regressions cover hydration, authorization,
   privacy-domain isolation, activation, and rollback.
6. **Developer evaluation:** use reproducible checked-in Linux workloads,
   explicit reset/recreation checks, exact-flat oracle comparisons, focused
   channel/fusion/calibration/rerank ablations, privacy/non-interference checks,
   and current/10x resource observations. Optional ANN and reranker branches
   begin only after the exact-flat shadow checkpoint and cannot enter the
   critical path without a passing comparison.
7. **Activation and verification:** only a passing Plan 15 result may make
   semantics eligible for the existing configuration activation. Run staged
   shadow/cohort behavior, rollback, fresh-store reset, privacy, architecture, direct
   tests, Linux evaluation, and normal all-feature cross-platform CI. Status
   and Doctor behavior are exercised through the production lifecycle states;
   activation still requires the remaining evaluation and indexing evidence.
   PR10 does not create a temporary public semantic endpoint or reserve later
   surface contracts.

A reproducible projection workload measures clean, warm one-symbol, deletion,
no-op, model-key replay, cancellation, and incompatible rebuild. A query
workload reports lexical, graph, semantic, fusion, rerank, and hydration time
separately at current and 10x corpus sizes,
including p50/p95/p99, CPU, peak RSS, model/vector/cache bytes, candidates per
channel, chunks embedded/reused/deleted, hydration fetch count, and fallback.
Channel ablations use equal candidate budgets; exact flat-vector search is the
semantic oracle. End-to-end performance work consumes these production
measurements, while Plan 15 owns quality/resource trade-off and activation
recommendations.

The query workload also overlaps queries with a blocked projection worker and
with staged, partial, failed, cancelled, stale, and incompatible generations.
It records exact/lexical/graph completion independently, requires zero wait on
the semantic worker, compares the visible PR9 fallback bytes and rank before
and during indexing, and observes semantic candidates only after the complete
compatible generation and active pointer become visible in one atomic step.

Each workload manifest pins corpus/query digests, exact file/chunk/query counts,
language/source strata, seed, model/projection/fusion revisions, hardware and
runtime summary, cache state, and concurrency. The 10x workload contains
exactly ten times the eligible chunks of the current workload without copying
quality labels across partitions. Reports retain the raw Linux samples needed
for every statistic and label unsupported p99 or uncertainty claims `pending`.

### Hard activation barrier

No semantic comparison starts until callable PR9 exact/lexical/graph behavior
passes direct regressions and its versioned profile, exact-tier contract,
fallback-subpayload bytes, and quality fixtures are identified by real
content/revision digests. Historical artifact-name parity is not part of this
barrier. No activation occurs unless Plan 15 reports `pass`,
authorization/scope leakage is zero, protected exact results are
unchanged, the PR9 fallback subpayload is byte-identical, generation
compatibility holds, search-during-indexing leaves PR9 bytes and rank
unchanged, incomplete or stale generations contribute nothing, all
resource ceilings pass, and cold offline rollback succeeds.

A `fail` or `pending` result leaves semantics disabled and no semantic profile
eligible.

## Acceptance

PR10 is complete when semantic projection, atomic publication, PR9-preserving
search, bounded generic fusion/reranking/redundancy, artifact/offline lifecycle,
configuration, production status/Doctor behavior, corpus/resource/privacy
tests, fault recovery, rollback, explicit reset/recreation, the Linux developer
evaluation, and normal CI pass. PR11/PR12/PR14 still own application, public
transport, and dashboard adapters. No separate
semantic endpoint, vector database, browser inference runtime, or model-specific
transport is introduced. Queries never silently substitute a model/revision,
download at query time, cascade to an unmeasured representation, or treat
semantic similarity as identity, impact, lineage, or equivalence.

- A no-op chunk manifest performs zero embedding calls. A one-symbol edit
  embeds exactly its changed symbol chunks and affected file-level chunks and
  tombstones explicit deletions. Changing only vector-affecting model profile
  fields with unchanged canonical chunk inputs replays all eligible chunks with
  zero parser/extractor calls; chunker/sanitizer/sensitivity changes follow
  Plan 25's canonical rebuild path.
- Receipt fixtures reject missing, duplicate, extra, wrong-generation, wrong-
  digest, and wrong-key entries; crash/cancellation leaves the previous active
  pointer unchanged and no partial projection queryable. Queries issued while
  projection is blocked complete through exact/lexical/graph without waiting
  and match the versioned PR9 fallback bytes and rank. Only a complete compatible
  generation published with its active pointer in one atomic step may add the
  semantic lane.
- The semantic adapter emits a separately inspectable generic
  `RetrieverOutcome<RetrieverBatch<CodeSemanticEvidenceV1>>`; PR9 exact,
  lexical, and graph outcomes are unchanged. Disabling or failing semantic
  retrieval preserves Plan 15's named PR9 fallback subpayload bytes. Fused
  explanations reproduce every declared generic profile
  contribution and the complete comparator for every result.
- Exact-hit retention and first-relevant-protected-hit Recall@10 are 100% over
  Plan 15's versioned protected-query set, with numerator, denominator, and
  support reported separately for symbols, qualified names, compiler/runtime
  errors, CLI flags, paths, quoted phrases, tool names, and configuration keys.
  For multi-target queries, every relevant protected hit admitted by the
  requested page size precedes every fuzzy, graph-only, or semantic-only hit.
- One hundred runs with shuffled candidate insertion and channel completion
  order produce byte-identical channel lists, fused IDs/order/contributions,
  diversity spillback, explanations, and cursors.
- Fixture profiles with `max_non_exact_per_source = 3` and
  `max_non_exact_per_file = 2` enforce those caps before pagination, preserve
  protected exact hits, and deterministically refill from overflow candidates.
  A three-page fixture proves no duplicate, omission, or cap drift after cursor
  resume.
- A page requesting five hydrated hits reads payloads and neighbors for at most
  those five hits and the profile's declared neighbor budget. Hydration cannot
  change rank; every neighbor matches the frozen generation and preserves graph
  ordered path identity, coverage, and weakest edge authority.
- Denied authorization invokes zero retrieval ports and zero payload reads.
  Revocation before hydration returns no payload. Privacy-domain/key-epoch
  changes yield zero session/vector/result/hydration cache hits, and cross-
  domain candidates, vectors, receipts, metrics, or explanations are zero.
- Split-adapter conformance produces identical results when lexical postings,
  graph evidence, vectors, receipts, and hydration payloads use separate stores,
  proving embeddings and no single physical table are authority.
- Artifact fixtures reject malformed versioned manifests, undeclared or
  duplicate members, path traversal, links, size expansion,
  digest/length mismatch, incompatible runtime/platform pins, interrupted
  publication, and deletion of active/rollback artifacts. Query execution
  performs zero network/import/cache-discovery operations.
- Distribution acceptance starts from an isolated profile, queues the
  background worker, installs every digest-verified default Jina member, and
  proves semantics remain omitted until indexing readiness. Its offline mode
  consumes only the lifecycle-root cache and must fail if production
  acquisition regresses to unconditional rejection.
- The checked-in benchmark corpus contains only sanitized fixtures and expected
  opaque anchors; raw private queries/source are absent. Activation requires a
  passing Plan 15 evaluation and cannot be inferred from public rank or
  aggregate gain.
