# PR10: Native FastEmbed semantic code search

**Status:** implementation authority for PR10. PR10 delivers configurable native semantic code search end to end; it is not a future experiment bucket.

## Outcome

TraceDecay augments exact lexical and graph search with local code embeddings.
Exact results remain authoritative without a model; similarity alone never proves impact, lineage, or equivalence.

## Ownership

- Plan 25 builds deterministic, storage-neutral `CodeSearchDocumentV1`,
  `CodeSearchChunkV1`, `ChangedCodeChunkSetV1`, and
  `CodeIndexCapabilityManifestV1` values from an exact code snapshot.
- Plan 04 schedules only changed eligible documents and resumable generation
  work.
- Plan 02 stores immutable vector generations, manifests, checkpoints, the
  atomic active-generation pointer, and chunk projection receipts through
  daemon-owned writer authority.
- Plan 05 owns retrieval, deterministic fusion, explanations, and redundancy
  classification.
- Plan 09 owns model artifacts, runtime sessions, model/profile authorization,
  budgets, activation, rebuild, and status. Owning source stores authorize code
  scope and payload reads; the application composes both receipts.
- Plan 15 exclusively owns retrieval-research design, frozen corpora and
  labels, metrics and strata, candidate profile comparison, thresholds, and
  promotion decisions. PR10 implements measured profiles and emits evidence;
  it cannot tune or promote itself from aggregate or public benchmark results.
- Plans 10/11/20/21 expose the same application operations through API,
  dashboard, configuration, CLI, and MCP.

Only one root-private adapter depends on `fastembed`. Crates for indexing, store,
query, API, and UI depend on ports and stable domain values, never
FastEmbed runtime types.

## Required files and typed contracts

PR10 adds these files; ownership follows the plan list above even when the
files land in the current root crate:

- `crates/tracedecay-domain/src/code_intelligence/search.rs`:
  `EmbeddingProjectionKeyV1`, `SemanticCapabilityManifestV1`,
  `RankedChannelListV1`, `RankedCodeCandidateV1`,
  `ChannelContributionV1`, `FusionProfileV1`, `FusedCodeCandidateV1`, and
  `HydratedCodeSearchHitV1`.
- `src/semantic_code/manifest.rs`: model/runtime/projection and capability
  manifest validation.
- `src/semantic_code/projector.rs`: changed-chunk batching, receipt production,
  resumable checkpoints, and publication handoff.
- `src/semantic_code/fastembed_adapter.rs`: the only `fastembed` import and the
  only model inference implementation.
- `src/semantic_code/session_pool.rs`: bounded sessions keyed by the complete
  projection/privacy identity.
- `src/query/code_search/lexical.rs`,
  `src/query/code_search/semantic.rs`, and
  `src/query/code_search/graph.rs`: independent implementations of
  `LexicalCodeRetriever`, `SemanticCodeRetriever`, and `GraphCodeRetriever`.
- `src/query/code_search/fusion.rs`: `DeterministicCodeFusion` implementing the
  frozen `FusionProfileV1`.
- `src/query/code_search/hydrate.rs`: `LateCodeHydrator` for final-page symbol,
  file, and bounded-neighbor evidence.
- `src/application/code_search.rs`: scope/generation resolution,
  authorization, capability admission, fallback, cancellation, and hydration
  reauthorization.
- `src/store/vector_generations.rs`: Plan-02-owned implementation of the
  semantic projection read/write ports. Query and semantic modules do not
  depend on its physical schema.

The retrieval contract is:

```rust
pub enum RetrievalChannelV1 {
    Lexical,
    Semantic,
    Graph,
}

pub struct RankedCodeCandidateV1 {
    pub candidate: CodeSearchCandidateRefV1,
    pub channel: RetrievalChannelV1,
    pub channel_rank: u32,
    pub raw_score: ChannelScoreV1,
    pub evidence: ChannelEvidenceV1,
}

pub struct RankedChannelListV1 {
    pub channel: RetrievalChannelV1,
    pub scope_digest: ScopeDigest,
    pub code_generation: CodeGenerationId,
    pub projection_key: Option<EmbeddingProjectionKeyV1>,
    pub candidates: Vec<RankedCodeCandidateV1>,
    pub coverage: RetrievalCoverageV1,
    pub exhausted: bool,
}

pub struct ChannelContributionV1 {
    pub channel: RetrievalChannelV1,
    pub channel_rank: u32,
    pub raw_score: ChannelScoreV1,
    pub fusion_input: FusionInputV1,
    pub fusion_contribution: FusionContributionV1,
    pub profile_revision: ProfileRevision,
    pub evidence: ChannelEvidenceV1,
}

pub trait LexicalCodeRetriever {
    fn retrieve(&self, request: &AuthorizedCodeSearchRequest)
        -> Result<RankedChannelListV1, RetrievalError>;
}
pub trait SemanticCodeRetriever {
    fn retrieve(&self, request: &AuthorizedCodeSearchRequest)
        -> Result<RankedChannelListV1, RetrievalError>;
}
pub trait GraphCodeRetriever {
    fn retrieve(&self, request: &AuthorizedCodeSearchRequest)
        -> Result<RankedChannelListV1, RetrievalError>;
}

pub struct CodeSearchCandidateRefV1 {
    pub chunk_id: CodeSearchChunkId,
    pub authorization_receipt_id: AuthorizationReceiptId,
    pub authorization_epoch: AuthorizationEpoch,
    pub source_id: CodeSearchSourceIdV1,
    pub file_occurrence_id: FileOccurrenceId,
    pub symbol_occurrence_id: Option<SymbolOccurrenceId>,
    pub source_span: SourceSpan,
    pub chunk_grain: CodeSearchChunkGrainV1,
}

pub struct AuthorizedCodeSearchRequest {
    pub query_digest: QueryDigest,
    pub authorized_scope: AuthorizedScopeReceiptV1,
    pub code_generation: CodeGenerationId,
    pub capability_manifest_digest: ManifestDigest,
    pub fusion_profile: FusionProfileV1,
    pub page: PageRequestV1,
}

pub struct FusionProfileV1 {
    pub revision: ProfileRevision,
    pub channel_weights: ChannelWeightsV1,
    pub rrf_k: u32,
    pub candidate_caps: ChannelCandidateCapsV1,
    pub max_non_exact_per_source: u32,
    pub max_non_exact_per_file: u32,
    pub protected_exact_classes: Vec<ExactTechnicalKindV1>,
    pub rerank_budget: u32,
    pub hydration_budget: HydrationBudgetV1,
}

pub struct HydratedCodeSearchHitV1 {
    pub candidate: FusedCodeCandidateV1,
    pub authorization_receipt: AuthorizedScopeReceiptV1,
    pub symbol: Option<SanitizedSymbolViewV1>,
    pub file_context: Option<SanitizedFileContextV1>,
    pub graph_neighbors: Vec<AuthorizedGraphNeighborV1>,
}
```

Each trait consumes the same frozen authorized scope and code generation, but
none consumes another channel's candidates or scores. Store adapters implement
`LexicalCandidateReadPort`, `SemanticProjectionReadPort`,
`GraphCandidateReadPort`, and `CodeHydrationReadPort` separately. Conformance
tests must pass when all four ports use different in-memory representations;
no single table, vector index, or materialized join is required authority.
`CodeSearchSourceIdV1` is the tuple of project, repository, worktree, selected
ref/snapshot, and privacy domain. `ChannelEvidenceV1` carries channel-local
reason codes and generation provenance; graph evidence additionally carries
the selected ordered edge/path IDs, deterministic equal-path comparator,
coverage, and weakest edge authority. `RetrievalCoverageV1` records examined,
eligible, excluded, capped, and unknown counts. All of these values live in the
domain file named above; ports live in their corresponding query modules.

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

## Model and offline lifecycle

Configuration selects an installed signed embedding profile and, independently,
an optional reranker profile. Manifests pin actual model/tokenizer/config bytes,
licenses, runtime/build identity, dimensions, normalization, metric, device,
threads, batching, and resource ceilings. Implementation selects maintained
crate/runtime versions during PR10; activated model and reranker profiles still
require Plan 15's locked promotion decision. This plan contains no stale crate
or model-version pin.

Install/import verifies artifacts before activation. Queries never download a
model or open an ambient cache. Offline startup remains healthy and
lexical-complete. Compatible warmed sessions are pooled under bounded memory,
concurrency, idle, and cancellation policy. Load failure, OOM, corruption,
revocation, or incompatible pins disables the affected semantic stage without
silently selecting another model.

The mandatory Plan 25 `CodeIndexCapabilityManifestV1` admits lexical/graph
retrieval. Optional `SemanticCapabilityManifestV1` pins the authorized scope
digest, code and vector generations, projection and search-index keys,
supported chunk grains/languages, fusion profile revision, candidate/source/
file caps, reranker and hydration support, coverage, partial states, privacy
domain/key epoch, and manifest digest. The application validates the base
manifest before any channel and validates the semantic augmentation only before
semantic/rerank work. Missing semantic capability yields lexical/graph mode; an
explicit strict-semantic request yields the typed unavailable result.

## Query and redundancy

Search resolves exact scope and frozen generation first, runs lexical/graph
channels first, then adds compatible semantic candidates. Fusion is stable and
explainable; exact lexical identifiers, paths, quoted phrases, errors, tool
names, and configuration keys keep a non-demotable tier. The first production
semantic baseline is deterministic exact flat-vector search unless measured
current/10x evidence shows it violates a reviewed resource budget. Optional
reranking is bounded to a configured top-N candidate set, is admitted only
after candidate-controlled gain with no protected-stratum regression, and
preserves the pre-rerank list byte-for-byte when unavailable. Raw similarity,
logits, margins, or fused scores are not confidence; calibrated abstention
requires a versioned cohort/generation-bound profile and reports invalid or
shifted calibration explicitly. Strict semantic requests return a typed
unavailable result.

`code.redundancy` reuses the same active generation. It canonicalizes pairs,
removes self/overlapping chunks, and reports `exact_clone`,
`structural_near_duplicate`, `semantic_analogue`, or `insufficient_evidence`.
Semantic-only matches remain review candidates, never automatic rewrites or CI
violations. Disabled semantics preserves the structural baseline and ordering.

The Plan-05 query pipeline executes these phases without combining them:

1. Resolve and authorize project/repository/worktree/ref scope; freeze code,
   lexical, graph, and compatible semantic generations; validate the capability
   manifest and cost budget.
2. Produce separate `RankedChannelListV1` values for lexical, semantic, and
   graph channels. Exact symbols and qualified names, compiler/runtime error
   codes and text, CLI flags, paths, quoted phrases, tool names, and
   configuration keys are classified as protected lexical hits. Each channel
   rejects non-finite scores, canonicalizes negative zero, deduplicates by
   candidate reference, then assigns dense one-based ranks by channel score
   descending and the candidate identity tuple from step 4 ascending.
3. Fuse lightweight `CodeSearchCandidateRefV1` identities. Protected lexical
   hits form the first tier and cannot be demoted by graph/semantic scores,
   diversity, or reranking. For the remaining tier, the initial implementation
   uses weighted reciprocal-rank fusion:
   `sum(channel_weight / (rrf_k + channel_rank))`. Weights and `rrf_k` are
   unsigned integers; `src/query/code_search/fusion.rs` compares exact rational
   sums by cross multiplication in lexical, graph, semantic enum order, without
   floating-point accumulation.
4. Break equal fused scores by the complete tuple `(protected_tier,
   best_lexical_rank, best_graph_rank, best_semantic_rank, repository_id,
   normalized_path, source_span_start, chunk_grain, chunk_id)`. Missing ranks
   sort after present ranks. Protected tier sorts first; ranks and identity
   fields sort ascending; paths use repository-normalized UTF-8 bytes; chunk
   grain uses declaration order. Input insertion order, hash iteration order,
   task completion order, and raw cross-channel score magnitude cannot affect
   the result.
5. Apply `FusionProfileV1.max_non_exact_per_source` and
   `max_non_exact_per_file` to the non-protected tier before pagination.
   `source` means the complete `CodeSearchSourceIdV1` tuple defined above. Caps
   apply independently to each requested page. Overflow candidates from a
   source/file saturated on the current page are postponed in fused order and
   become eligible on the next page. The cursor binds all emitted identities,
   the ordered overflow queue, channel continuations, and profile/generation
   digests; page-local counters reset only after the cursor seals a page, so
   resume cannot duplicate, omit, or drift. Protected exact hits remain visible
   and are counted separately in diversity explanations.
6. Attach every `ChannelContributionV1`, including channel rank, raw
   channel-local score, RRF input/contribution, profile revision, generation/
   projection identity, evidence class, graph path coverage, and weakest graph
   edge authority. A fused score is not probability or confidence.
7. Optionally rerank only the profile-bounded non-protected candidate set while
   preserving the pre-rerank list and contributions. Absence/failure returns
   those bytes unchanged.
8. After fusion, caps, reranking, and page selection, reauthorize and hydrate
   only selected hits through `LateCodeHydrator`. Hydration may add bounded
   symbol text, file context, declarations, and graph neighbors from the same
   generation; it cannot add candidates or change rank.

`FusionProfileV1` pins algorithm revision, channel weights, `rrf_k`, per-
channel candidate budgets, source/file caps, protected exact classes, rerank
budget, hydration budget, and the total tie-break comparator. Plan 15 selects
and promotes this profile from locked evidence; Plan 05 implements it, and
Plan 31 supplies the semantic channel and measurements.

## Authorization and local/private boundary

- Owning source stores authorize exact scope, operation, privacy domain, key
  epoch, and code generation. Plan 09 separately authorizes semantic projection
  generation and model profile. The application composes the required receipts
  before invoking each channel; source denial invokes zero channels, while
  semantic denial invokes no semantic/rerank port and preserves authorized
  lexical/graph execution.
- Candidate references carry authorization receipt ID and epoch without private
  payload. Immediately before hydration, the application rechecks current
  authorization, scope, privacy domain/key epoch, and frozen generation.
  The owning store performs atomic receipt validation plus bounded payload/
  neighbor read; every neighbor must be inside the authorized scope. Revocation
  before or during a multi-read discards the complete hydrated hit and returns
  a typed denial with no source text or neighbor payload.
- All caches are local and privacy-domain/key-epoch separated. Model artifacts
  key by signed artifact digest; sessions by embedding projection key; vectors
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
  Operational receipts contain opaque identities, revisions, outcomes, counts,
  and digests only. Only sanitized fixtures and aggregate Plan 15 reports may
  enter Git.
- Semantic errors, timeout, cancellation, OOM, corruption, or revocation cannot
  broaden scope. When the selected profile permits fallback, the lexical/graph
  ranked lists and pre-semantic fused order are byte-identical and the response
  carries a typed visible reason; strict mode fails closed.

## Plan 15 evaluation handoff and migration

PR10 ships a frozen sanitized corpus covering exact names, natural-language
intent, mixed queries, renamed symbols, same-name cross-scope cases, no-answer
queries, generated/vendor noise, large symbols, unsupported languages, and
incremental edits. It measures exact-hit retention, precision/recall/MRR/nDCG,
wrong-scope and no-answer error, worst strata, build/update time, p50/p95/p99,
CPU/RSS, model/vector/cache bytes, cancellation, and offline behavior.

Plan 15 owns this corpus's partition/label policy, metrics, uncertainty method,
protected strata, practical margins, stopping rule, thresholds, and promotion
decision. PR10 owns reproducible execution and immutable result anchors.
Activation requires Plan 15's signed locked report showing no scope/privacy or
protected exact/no-answer/wrong-scope/worst-stratum regression, demonstrated
semantic gain, and declared current/10x resource budgets. Sensitive or
ineligible bytes never enter documents, artifacts, metrics, explanations, or
model-assisted routes.

Late interaction, quantization, and specialized ANN remain measured candidate
profiles, not PR10 defaults. ANN is admitted only when it beats exact search's
reviewed resource budget while meeting exact-oracle average, tail, minimum,
and zero-recall-query gates under immutable-generation compatibility. No HNSW,
DiskANN, ScaNN, vector database, precision, or quantization choice is
mandatory. Public benchmark rank cannot select a production profile.

Legacy vectors are never trusted or republished. Migration records
`rebuild_from_retained_eligible_code | drop_with_receipt | quarantine_unreadable`
and proves every active generation was rebuilt from canonical documents.

## PR10 phases, tests, and benchmarks

`tests/semantic_search_suite/main.rs` is the Cargo integration-test entrypoint
and declares every `tests/semantic_search_suite/*.rs` module named below.

1. **Contracts and capability admission:** add domain values, semantic ports,
   manifest validators, and split in-memory adapters. Tests:
   `tests/semantic_search_suite/contracts.rs`,
   `capabilities.rs`, and `storage_independence.rs`.
2. **Incremental projection:** add the root-private FastEmbed adapter and
   projector against Plan 04 scheduling, Plan 02 checkpoint/receipt/publication,
   and Plan 09 runtime-session ports; do not duplicate those authorities.
   Tests: `tests/semantic_search_suite/projection.rs`,
   `model_replay.rs`, `atomic_publication.rs`, and `offline_lifecycle.rs`.
3. **Independent retrieval and fusion:** add three channel retrievers,
   deterministic fusion, protected exact tier, contribution provenance,
   diversity spillback, fallback, and cursor binding. Tests:
   `tests/semantic_search_suite/channel_isolation.rs`,
   `fusion_provenance.rs`, `protected_exact.rs`,
   `diversity_pagination.rs`, and `fallback.rs`.
4. **Late hydration and privacy:** add hydration budgets, generation checks,
   authorization recheck, revocation handling, domain-keyed caches, and
   payload-safe receipts. Tests:
   `tests/semantic_search_suite/hydration.rs`,
   `authorization.rs`, and `privacy_domains.rs`.
5. **Evaluation handoff and surfaces:** add
   `benchmarks/pr10-semantic/{workload-v1.json,expected-v1.json,README.md}`,
   rebuild-only migration, and immutable Plan 15 report inputs. Add typed
   application conformance in
   `tests/semantic_search_suite/application_conformance.rs`, status/Doctor
   conformance in `tests/semantic_search_suite/status_doctor.rs`, and shared
   API/CLI/MCP/dashboard fixture expectations in
   `tests/semantic_search_suite/surface_contract.rs` for Plans 10/11/20/21 to
   consume. Those plans own their adapters; Plan 15 alone interprets and
   promotes the profile.

`benches/semantic_code_projection.rs` measures clean, warm one-symbol,
deletion, no-op, model-key replay, cancellation, and incompatible rebuild.
`benches/semantic_code_query.rs` reports lexical, graph, semantic, fusion,
rerank, and hydration time separately at current and 10x corpus sizes,
including p50/p95/p99, CPU, peak RSS, model/vector/cache bytes, candidates per
channel, chunks embedded/reused/deleted, hydration fetch count, and fallback.
Channel ablations use equal candidate budgets; exact flat-vector search is the
semantic oracle. PR20 owns later end-to-end performance tuning, while Plan 15
owns quality/resource trade-off and promotion policy.

Each workload manifest pins corpus/query digests, exact file/chunk/query counts,
language/source strata, seed, model/projection/fusion revisions, hardware and
runtime manifest, cache state, and concurrency. The 10x workload contains
exactly ten times the eligible chunks of the current workload without copying
quality labels across partitions. Query benchmarks run 10 untimed warmups then
1,000 measured queries at concurrency 1 and the declared saturation
concurrency; projection cases run 5 warmups and 30 measured repetitions.
Reports retain all samples needed to recompute percentiles.

## Acceptance

PR10 is complete when indexing, atomic publication, lexical-preserving search, bounded
fusion/reranking/redundancy, artifact/offline lifecycle, configuration,
status/Doctor, API/CLI/MCP/dashboard parity, corpus/resource/privacy gates,
fault recovery, and rebuild-only migration pass direct tests. No separate
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
  pointer unchanged and no partial projection queryable.
- Each retriever emits a separately inspectable ranked list. Disabling or
  failing semantic retrieval leaves lexical and graph list bytes unchanged.
  Fused explanations reproduce the declared RRF score and complete comparator
  for every result.
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
- The checked-in benchmark corpus contains only sanitized fixtures and expected
  opaque anchors; raw private queries/source are absent. Promotion requires the
  Plan 15 locked report and cannot be inferred from public rank or aggregate
  gain.
