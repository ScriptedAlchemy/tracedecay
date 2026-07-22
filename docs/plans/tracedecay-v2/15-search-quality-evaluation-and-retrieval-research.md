# TraceDecay V2 Search Quality, Evaluation, and Retrieval Research

## Status / Role

Status: active product plan and quality authority. PR9 and PR10 remain
unfinished until their respective callable behavior, direct regressions, and
locked acceptance gates pass; this document does not mark either complete.

PR9 ships the typed federated-retrieval contract, independent exact and lexical
retrievers, adapters for authorities available at that dependency point, deterministic
fusion, source-aware dedupe and diversity, compact-candidate ranking, and the locked
evaluation harness. PR10 ships source-local semantic projections, native semantic
retrieval, and optional bounded reranking. The application service consumes the
accepted ports, the dashboard exposes their controls and state, and the
task/work journey adds the Plan 24 task/session retriever after canonical task
identity exists. Semantic implementation is required in PR10; activation
remains evidence-gated and lexical-only operation remains fully supported.

This plan is the quality and composition authority. It does not replace the canonical
stores, the Plan 23 temporal query kernel, the Plan 24 task/work graph, the Plan 25 code
graph, Plan 13 diagnostic anchors, or their authorization rules.

Plan 15 owns retrieval quality, composition, evaluation, and promotion
semantics. Plan 25 is the current PR9 delivery owner and Plan 31 the later PR10
semantic delivery owner. The application, dashboard, task/work, and public
surface plans are later consumers. They depend on accepted callable behavior
and evidence, not exact historical module, type, fixture, benchmark, command,
or suite-spine names.

## Outcome

Search returns useful, correctly scoped, temporally valid evidence on the first page
across real local projects. Exact technical lookup remains non-demotable. Every result
can explain which retrievers contributed, which source freshness was observed, which
dedupe or diversity decision applied, and why an optional channel or reranker fell back.
The implementation ranks compact authorized candidates before hydrating payloads.

## Non-negotiable decisions

- Retrieval is a federation of independently testable and independently disableable
  exact-literal, lexical, semantic, graph, temporal, task/session, and diagnostic lanes.
  Semantic recall never gates exact or lexical recall, and one lane is never implemented
  as an alias over another lane.
- Exact IDs, diagnostic codes and text, symbols, CLI flags, quoted literals, paths,
  config keys, tool names, commit identifiers, task/session IDs, and protocol fields
  enter a lexicographically higher exact tier. Approximate fusion and reranking cannot
  demote them.
- Approximate candidates use deterministic fixed-point weighted fusion. Every promoted
  weight and calibration belongs to a versioned profile backed by an immutable locked
  evaluation result. Each ranked candidate retains every retriever's raw score domain,
  ordinal rank, calibrated feature, weight, weighted contribution, and exclusion reason.
- Source freshness is source- and retriever-specific. There is no global age-decay
  multiplier over heterogeneous evidence. Temporal validity, index lag, source
  generation, and projection compatibility remain separate facts.
- Duplicate rows from one immutable source occurrence are collapsed before fusion.
  Cross-source copies are collapsed only through an evidence-backed logical-copy
  relation; independent corroboration and contradictions are preserved. Deterministic
  source, repository, session, copy-cluster, and evidence-role caps apply after fusion.
- Retrieval, fusion, dedupe, and diversity operate on compact anchors and metadata.
  After compact ranking, the bounded rerank prefix may receive ephemeral authorized
  rerank views; final context hydration occurs only for the selected result set.
- Reranking is optional and bounded by a promoted profile's candidate, byte/token, work,
  model-call, and deadline budgets. It receives only the approximate prefix after
  source-level dedupe, temporal resolution, fusion, and diversity. Failure returns the
  exact pre-rerank order with a typed visible reason.
- TraceDecay will not create one monolithic embeddings table or a cross-authority vector
  store. Vectors are derived, source-local projections keyed by stable anchor, privacy
  domain, source generation, projection digest, model revision, dimensions,
  normalization, chunking version, and schema version. Federation occurs at query time.
- TraceDecay will not adopt a conventional fixed RRF constant such as `k = 60`, fixed
  fusion weights, similarity cutoffs, abstention margins, graph-hop cutoffs, freshness
  penalties, MMR parameters, diversity quotas, or reranker thresholds without locked
  TraceDecay evaluation. An RRF or threshold profile may be an evaluated candidate, but
  it remains disabled unless its frozen development selection and locked result support
  promotion. Resource-safety ceilings are engineering limits, not quality claims.

## Ownership and module boundaries

The boundaries below are normative; the paths and type spellings record the
original delivery design and are non-normative. Current owners may move,
rename, or consolidate them when direct boundary regressions preserve the same
authority, lane isolation, rank-before-hydrate ordering, and authorization
behavior.

- `crates/tracedecay-domain/src/retrieval.rs` owns pure typed contracts:
  `RetrievalRequest`, `RetrieverKind`, `CompactCandidate`, `RetrieverBatch`,
  `RetrieverOutcome`,
  `SourceFreshness`, `CandidateContribution`, `FusionProfile`, `DiversityPolicy`,
  `RerankPolicy`, `FusedCandidate`, `RankedCandidate`, `RetrievalResult`,
  `AuthorizedRerankView`, `HydrationReceipt`, and evaluation decision IDs.
- `src/application/retrieval/{mod.rs,ports.rs,pipeline.rs,types.rs}` owns orchestration,
  budgets, cancellation, query-snapshot pinning, partial-outcome policy, and the
  rank-before-hydrate boundary when PR11 delivers the application layer. It depends on
  PR9/PR10 ports, not storage implementations.
- `src/query/retrieval/{exact.rs,lexical.rs,semantic.rs,graph.rs,temporal.rs,task_session.rs,diagnostic.rs}`
  owns independent adapters; `src/query/retrieval/ports.rs` owns the single
  generic `Retriever<R, E>` port. `fusion.rs`, `dedupe.rs`, `diversity.rs`, `rerank.rs`, and
  `hydrate.rs` own deterministic composition stages.
- `src/query/temporal/` remains the only current/as-of/evolution/forensic temporal
  eligibility and pagination kernel. Plan 23 owns
  `src/query/temporal/ports.rs::TemporalCandidateExport`, which returns authorized
  compact candidates, typed mode/cutoff, source coverage, and freshness before payload
  hydration. `retrieval/temporal.rs` consumes that port; it does not copy temporal
  resolution, temporal fusion/diversity, cursor, or hydration semantics.
- Plan 25's project code graph remains the graph source of truth. `graph.rs` emits stable
  code anchors and bounded relationship evidence without copying graph rows into a
  search corpus.
- Plan 24 owns `TaskId`, task/work topology, attempts, dependencies, and task query
  semantics. `task_session.rs` joins task roots to Plan 23 session evidence by stable
  authorized anchors; it never copies task or session payloads.
- Plan 13 and the diagnostic owning stores retain GitHub, CI, compiler, lint, and runtime
  diagnostic evidence. `diagnostic.rs` resolves their stable anchors and never treats
  LSP projection as canonical storage.
- `src/global_db/retrieval/lexical.rs` owns only global-store lexical projection rows.
  Project graph and other stores expose equivalent projection ports in their owning
  crates. `src/global_db/retrieval/semantic.rs` stores vectors only for global-store
  source namespaces; other authorities keep source-local semantic projections.
- Existing store authorization and privacy-domain resolution are authoritative. Each
  owning source applies authorization, scope, and temporal eligibility before emitting a
  candidate. The application pipeline and every owning-store hydrator recheck eligibility
  as defense in depth.
- `src/config/retrieval.rs` owns versioned activation profiles and the atomic active and
  rollback profile pointers under the configuration-control-plane mutation capability.
  PR14's `src/dashboard/` work renders profile, freshness, fallback, and report state; it
  does not decide promotion.
- The hermetic evaluation harness, direct contract regressions, checked-in
  sanitized manifests, aggregate evidence, and evidence map remain evaluation
  infrastructure. Their current owners do not create a service or evaluation
  database, and their historical paths are not acceptance criteria.
- MCP, CLI, dashboard, and agent surfaces remain thin consumers of the application
  contract. Public operation naming remains with the transport/catalog plans.

## Typed retrieval contract

PR9 must provide an equivalent typed contract with the behavior and information
below. The Rust sketch is explanatory, not an artifact-name or source-layout
requirement; field/type names may change when direct contract tests preserve
the semantics.

```rust
pub enum RetrieverKind {
    ExactLiteral,
    Lexical,
    Semantic,
    Graph,
    Temporal,
    TaskSession,
    Diagnostic,
}

pub struct RetrievalRequest {
    pub query: String,
    pub principal: PrincipalId,
    pub scope: RetrievalScope,
    pub privacy_domain: PrivacyDomainId,
    pub temporal_mode: TemporalQueryMode,
    pub snapshot: RetrievalSnapshot,
    pub profile_id: FusionProfileId,
    pub budget: RetrievalBudget,
}

pub struct CompactCandidate {
    pub anchor_id: RetrievalAnchorId,
    pub logical_evidence_id: LogicalEvidenceId,
    pub source_occurrence_id: SourceOccurrenceId,
    pub source_namespace: SourceNamespace,
    pub repository_id: Option<RepositoryId>,
    pub session_or_thread_id: Option<SessionOrThreadId>,
    pub logical_copy_cluster_id: Option<LogicalCopyClusterId>,
    pub evidence_role: EvidenceRole,
    pub retriever: RetrieverKind,
    pub retriever_revision: ComponentRevision,
    pub score_domain: ScoreDomainId,
    pub raw_score: FixedPointScore,
    pub ordinal_rank: u32,
    pub exact_admission_proof: Option<ExactAdmissionProof>,
    pub retriever_evidence_anchor: RetrievalAnchorId,
    pub freshness: SourceFreshness,
}

pub struct RetrieverBatch<E> {
    pub candidates: Vec<CompactCandidate>,
    pub evidence_by_occurrence: BTreeMap<SourceOccurrenceId, E>,
    pub coverage: RetrieverCoverage,
    pub continuation: Option<RetrieverContinuation>,
}

pub enum RetrieverOutcome<T> {
    Complete(T),
    Partial { value: T, reason: RetrievalFailure },
    Unavailable(RetrievalFailure),
    Denied,
    Stale(SourceFreshness),
    BudgetExceeded(RetrievalBudgetUsage),
    Cancelled,
}

pub trait Retriever<R, E> {
    fn retrieve(
        &self,
        request: &R,
    ) -> Result<RetrieverOutcome<RetrieverBatch<E>>, RetrievalError>;
}

pub struct CandidateContribution {
    pub retriever: RetrieverKind,
    pub retriever_revision: ComponentRevision,
    pub source_occurrence_id: SourceOccurrenceId,
    pub ordinal_rank: u32,
    pub raw_score: FixedPointScore,
    pub score_domain: ScoreDomainId,
    pub calibration_profile_id: CalibrationProfileId,
    pub calibrated_feature_micros: u32,
    pub weight_micros: u32,
    pub weighted_contribution_micros: u64,
}

pub struct OccurrenceProvenance {
    pub source_occurrence_id: SourceOccurrenceId,
    pub retriever_evidence_anchor: RetrievalAnchorId,
    pub source_namespace: SourceNamespace,
    pub repository_id: Option<RepositoryId>,
    pub session_or_thread_id: Option<SessionOrThreadId>,
    pub logical_copy_cluster_id: Option<LogicalCopyClusterId>,
    pub evidence_role: EvidenceRole,
    pub freshness: SourceFreshness,
}

pub struct FusedCandidate {
    pub anchor_id: RetrievalAnchorId,
    pub logical_evidence_id: LogicalEvidenceId,
    pub occurrences: Vec<OccurrenceProvenance>,
    pub exact_class: ExactClass,
    pub utility_micros: u64,
    pub contributions: Vec<CandidateContribution>,
    pub freshness: Vec<SourceFreshness>,
    pub decisions: Vec<RankingDecision>,
}

pub struct RankedCandidate {
    pub candidate: FusedCandidate,
    pub final_ordinal: u32,
}

pub struct FusionProfile {
    pub profile_id: FusionProfileId,
    pub evaluation_result_anchor: RetrievalAnchorId,
    pub calibrations: BTreeMap<RetrieverKind, CalibrationProfileId>,
    pub weights_micros: BTreeMap<RetrieverKind, u32>,
    pub diversity_policy_id: DiversityPolicyId,
    pub rerank_policy_id: Option<RerankPolicyId>,
    pub retrieval_budget: RetrievalBudget,
}

pub struct Pr9FallbackSubpayload {
    pub profile_id: FusionProfileId,
    pub ordered_candidates: Vec<RankedCandidate>,
    pub public_pr9_lane_coverage: BTreeMap<RetrieverKind, PublicRetrieverStatus>,
    pub freshness: Vec<SourceFreshness>,
    pub cursor: Option<RetrievalCursor>,
    pub digest: FallbackSubpayloadDigest,
}

pub enum OptionalStagePublicStatus {
    NotRequested,
    Complete,
    Unavailable(SanitizedStageFailure),
    Rejected(SanitizedStageFailure),
    Cancelled,
    BudgetExceeded(SanitizedBudgetUsage),
}

pub struct SemanticRerankOutcome {
    pub semantic: OptionalStagePublicStatus,
    pub rerank: OptionalStagePublicStatus,
}

pub struct RetrievalResult {
    pub snapshot: RetrievalSnapshot,
    pub profile_id: FusionProfileId,
    pub pr9_fallback: Pr9FallbackSubpayload,
    pub ordered_candidates: Vec<RankedCandidate>,
    pub internal_lane_outcomes: BTreeMap<RetrieverKind, RetrieverOutcome<()>>,
    pub public_lane_coverage: BTreeMap<RetrieverKind, PublicRetrieverStatus>,
    pub freshness: Vec<SourceFreshness>,
    pub semantic_rerank_outcome: SemanticRerankOutcome,
    pub hydration_receipts: Vec<HydrationReceipt>,
    pub cursor: Option<RetrievalCursor>,
}
```

`SourceFreshness` records source namespace and instance, source watermark, projection
watermark, observed timestamp, source generation, generation lag, compatibility status,
and policy revision. Missing, stale, incompatible, and current are distinct states. A
cursor binds the query snapshot, profile ID, authorized source-freshness digest,
authorization revision, ordered authorized candidate-set digest, sanitized public lane
statuses, and checkpoint IDs for admitted authorized lanes only. Sealed denial outcomes
never affect cursor or cache-key bytes. Resume uses the bound candidate set or rejects
the cursor; it never recomputes a differently completed set.

`Pr9FallbackSubpayload` is canonical-encoded and hashed independently with the
schema/domain separator `tracedecay.pr9-fallback.v1`; the digest field itself
is excluded from those hashed bytes. Its
ranked candidates contain the PR9 contributions/decisions/explanations; its
maps contain only `ExactLiteral`, `Lexical`, and `Graph`. Semantic/rerank
execution may change the enclosing final candidates and
`semantic_rerank_outcome`, but cannot change the subpayload, its digest, or
cursor identity. "Byte-identical fallback" means this typed PR9 subpayload is
identical; it does not forbid the enclosing response from truthfully reporting
semantic unavailability.

Sealed `internal_lane_outcomes` remains only in the enclosing audit result and
is excluded from fallback bytes/digest, cursors, public coverage, and cache
keys. `OptionalStagePublicStatus` deliberately has no denied variant: denied
and absent evidence coalesce through the same sanitized unavailable shape and
cannot differ in counts, timing class, cache effects, or public bytes.

`RankingDecision` records exact-tier admission, same-source duplicate collapse,
logical-copy representative selection, contradiction preservation, each diversity-cap
decision, rerank admission, and fallback. Explanations are rendered from this provenance;
they are not reconstructed from a final scalar score.

Only the central exact-admission validator can mint `ExactAdmissionProof`; retrievers
cannot assign an exact tier. The proof binds rule revision, typed field, original bytes,
canonical bytes, normalization steps, scope, authorization revision, and temporal
snapshot. Fusion derives `ExactClass` only from a validated proof.

Every contribution and hydration receipt keys back to one `OccurrenceProvenance`.
Parallel unassociated provenance vectors are forbidden because they cannot reproduce
dedupe, diversity, freshness, or hydration decisions.
Fusion preserves each exact
`(source_occurrence_id, retriever_evidence_anchor)` pair from the source batch
in `OccurrenceProvenance`; it cannot substitute the candidate's content anchor
or reconstruct evidence after ranking.

Every `RetrieverBatch` contains exactly one typed evidence value for each
returned `source_occurrence_id`; missing, extra, or duplicate evidence rejects
the batch. `retriever_evidence_anchor` addresses that same evidence in the
owning source when it is durably retained. Ephemeral evidence is request-local
but must have the same canonical identity and cannot be reconstructed from the
final fused score.

`internal_lane_outcomes` is sealed server-side audit data. `PublicRetrieverStatus`
coalesces denied and nonexistent evidence and omits unauthorized source freshness,
counts, timing, cap effects, and failure details. Only an independently authorized
operator diagnostic may inspect internal denial state. Public results, cursors, caches,
reports, and timing classes must not distinguish denied evidence from absent evidence.

## Deterministic retrieval pipeline

The authoritative application retrieval operation executes this order,
regardless of its current file or symbol name:

1. Authentication resolves the principal, privacy domain, and maximum scope; public
   callers cannot assert those fields. Resolve authoritative project/worktree/branch,
   typed temporal mode and cutoff, query snapshot, source watermarks, authorization
   revision, active profile, deterministic per-lane work budgets/checkpoints, and global
   resource ceilings once.
2. Parse exact technical literals under a versioned exact-admission specification.
   Exact status permits byte equality and explicitly enumerated canonical equivalences
   for each typed field. Stemming, fuzzy or substring matching, token overlap, and
   semantic similarity cannot confer exact status; phrase status requires explicit
   quoting or parser-recognized phrase syntax. Preserve original bytes and normalization
   provenance.
3. Each owning source applies authorization, scope, and Plan 23 temporal eligibility
   before independently emitting compact candidates against the same snapshot. Snapshot,
   profile, and per-lane work budget select one admissible prefix and commit checkpoint
   before execution. The lane contributes that entire prefix only if the checkpoint
   completes; otherwise it contributes no candidates and returns its typed outcome.
   Scheduler interleaving, timing jitter, cancellation, or a shared deadline cannot
   select a different prefix. A missing optional lane becomes a typed partial outcome; a
   missing exact or lexical lane rejects the request as unavailable.
4. Collapse duplicate rows for the same source occurrence. Never collapse merely by
   content hash, title, timestamp, or embedding similarity.
5. Recheck owning-store authorization and Plan 23 temporal eligibility before fusion.
   A denied candidate leaves no observable rank, count, cap effect, cursor difference,
   explanation, freshness item, timing class, cache entry, or aggregate artifact.
6. Partition candidates lexicographically into exact-message, exact-literal/phrase, and
   approximate tiers. Approximate scoring cannot cross an exact tier.
7. Group contributions by stable anchor plus logical evidence identity while retaining
   structured `OccurrenceProvenance` and occurrence-keyed contribution records in
   `FusedCandidate`. For approximate
   candidates, calibrate only within each declared score domain and
   compute `utility = sum(profile_weight * calibrated_feature)` with checked fixed-point
   arithmetic. Total order is exact class, utility, source validity, stable anchor ID,
   logical evidence ID, then ordered source occurrence IDs. Persist every contribution.
8. Resolve evidence-backed logical-copy clusters, preserving independent corroboration
   and every admitted contradiction before choosing representatives.
9. Apply deterministic profile-owned caps per source namespace, source instance,
   repository, session/thread, logical-copy cluster, and evidence role. A cap must carry
   its locked evaluation anchor; absent evidence leaves the cap disabled except for
   resource-safety ceilings.
10. Select only the profile-bounded approximate prefix for optional reranking. Exact
    tiers bypass the reranker. Each owning store may emit an ephemeral
    `AuthorizedRerankView` containing only approved source-local text or token features
    after repeating authorization and temporal checks. Views bind snapshot, privacy
    domain, source compatibility, and budgets; they never enter cross-authority caches or
    persisted artifacts. If any required view is unavailable, skip reranking entirely and
    preserve the canonical pre-rerank sequence.
11. Recheck authorization and hydrate final context for the selected anchors through each owning
    store under byte/token/deadline budgets. Record a `HydrationReceipt` per anchor.
12. Assemble `RetrievalResult` and compact context with citations, sanitized coverage,
    authorized freshness, ranking decisions, rerank outcome, hydration receipts, and a
    lossless source-anchor drill-down path.

The lexical retriever provides fielded BM25 over typed result grains, character-level
typo recovery, query/tool/protocol echo penalties, and exact phrase support. The graph,
temporal, task/session, and diagnostic adapters must expose their own candidate pools and
oracle recall; they do not become lexical fields. Semantic retrieval uses exact flat
vector scan as the quality oracle and a production candidate. Any ANN implementation is
optional and must match the same embeddings, authorization filter, snapshot, and
candidate budget during comparison.

## Semantic projection and reranking constraints

PR10 implements native in-process FastEmbed search with no Python, WASM, llama.cpp,
external inference process, or separate model service. Models load once and reuse
sessions. Document embeddings batch during indexing; unchanged source occurrences reuse
vectors only when every compatibility key matches.

The currently supported Jina code model, one general FastEmbed comparator, and
`BGERerankerV2M3` are reproducible candidates, not predetermined winners. One current
compatible code-specialized challenger may enter only with pinned license, artifact
digest, tokenizer, runtime, and offline-reuse evidence. Public leaderboard rank cannot
promote a model.

Rerank bounds are fields of `RerankPolicy`: admitted candidate count, input bytes,
input tokens, work units, model invocations, deadline, and cancellation checkpoints.
PR10 selects their values from the locked recall/latency/resource frontier and records
them in the promoted profile. Model absence, corruption, incompatibility, refusal,
timeout, cancellation, or budget exhaustion produces the byte-identical pre-rerank
order and a typed reason. No unmeasured substitute model is permitted.

## Evaluation artifacts and fixtures

PR9 creates checked-in sanitized evidence with the following content. Artifact
names and directory layout may evolve; schema validation, immutable
content/revision identity, direct fixture coverage, and traceability are the
acceptance criteria.

- A fixture manifest freezes corpus and label hashes,
  partitions, contamination groups, seeds, baseline revision, exact-admission rules,
  metric definitions, support floors, practical margins, adjudication policy, stopping
  rules, and decision owners before tuning.
- The query corpus contains query identity, partition,
  query family, provider, language, repository-family cluster, snapshot commit, `as_of`,
  principal class, privacy-domain class, allowed-scope IDs, sanitized query or
  authorized private-query locator digest, and contamination-group IDs.
- Snapshot fixtures define each snapshot ID by repository
  commit, branch/worktree identity, canonical-store generations, source and projection
  watermarks, authorization-policy revision, and wall-clock cutoff.
- Development judgments contain development-only
  judged anchor IDs, relevance,
  evidence role, validity interval, supersession relation, logical-copy group,
  forbidden-anchor IDs, abstention oracle, task oracle, labeler provenance, adjudication,
  and correction/supersession lineage.
- Locked-judgment metadata contains only the signed sealed
  holdout digest, authorized-store locator, access policy, and reveal audit contract.
  Locked labels are not checked in or readable during tuning.
- Temporal-event fixtures contain stable event ID, valid
  time, observed/ingest time, arrival sequence, source generation, source and projection
  watermarks, supersession relation, and expected eligibility for each snapshot ID.
- Context-span fixtures bind payload revision and tokenizer
  revision to judged relevant byte and token spans, stale/forbidden spans, citation
  support, and contradiction groups.
- Task fixtures freeze initial repository state, sanitized
  prompt or authorized-store prompt locator plus content digest, verifier or blinded
  rubric, agent/model/tool revisions, decoding parameters, attempt seeds, budgets,
  timeout, workspace reset procedure, and blinded assignment.
- An evidence map links every report claim to the fixture,
  run, aggregate, and immutable TraceDecay result anchors that support it.
- A run manifest is frozen after development-only tuning and
  before access to locked outcomes. It declares resolved candidate, profile, model,
  tokenizer, runtime, and command revisions; the exact profile matrix; candidate and
  context budgets; cache state; randomized execution order; sample-size rationale;
  measurement tools; statistical procedures; output schema; and one executable terminal
  decision expression. Any post-lock change creates a new append-only run revision and
  cannot reuse revealed locked outcomes for selection.
- Promotion evidence records the accepted profile, prior
  accepted rollback profile, report digest, approvals, activation stage, and atomic
  rollback instruction. It is created only for an `accepted` run.

Private query text and source payloads remain in their authorized stores. Only sanitized
fixtures, immutable digests, aggregate samples, and non-sensitive anchors enter Git.
Corrections append or supersede judgments; they never rewrite a reported run. Near
duplicates, forks, generated copies, repeated issue families, and repository-family
clusters cannot cross development, locked chronological, repository-disjoint, or
forward-confirmation partitions unnoticed.

The harness signs and freezes the run manifest before the locked-label access capability is
granted. The reveal produces an audited access receipt bound to the run digest. Any
pre-freeze access, digest mismatch, unrecorded reveal, or reuse of revealed labels for
selection makes the run `invalid_run`. Forward confirmation uses a separately sealed
label set and access receipt.

The fixture corpus includes exact errors, symbols, flags, paths, IDs, false-exact hard
negatives, paraphrases,
typos, multi-hop graph questions, before/after `as_of` queries, late arrivals,
supersession, equal timestamps, branch/worktree divergence, task-to-session joins,
diagnostic-to-code joins, wrong-scope near matches, authorization canaries, hard
negatives, contradictions, copies/echoes, and expected no-result cases. A stratified
subset is independently double-labeled and adjudicated; agreement and adjudication rate
are reported. Human or deterministic fixture labels are promotion authority. LLM judges
are secondary diagnostics only.

## Required ablations and metrics

Every comparison uses identical fixtures, authorized snapshots, candidate budgets,
context budgets, hardware class, cache-state strata, and randomized profile order. The
locked matrix includes:

1. the pinned production baseline;
2. PR9 exact plus lexical;
3. each PR9 lexical feature removed independently;
4. each graph, temporal, task/session, and diagnostic lane disabled independently;
5. each semantic candidate using exact flat scan;
6. ANN versus exact scan over identical embeddings, when ANN is proposed;
7. deterministic fusion versus every evaluated fusion candidate on saved candidate
   lists;
8. calibration, logical-copy dedupe, each diversity-cap dimension, and context selection
   independently neutralized, with targeted interaction contrasts frozen where those
   mechanisms compose; and
9. reranker off/on over byte-identical saved pre-rerank candidate lists.

Reports include aggregate and per-query-family, language, repository-family,
exact/no-answer/wrong-scope, temporal, privacy, low-coverage, source, and retriever
strata. They report denominators, missingness, and query-level paired deltas. Queries,
not returned documents, are the sampling unit. Repository-family clustered bootstrap
intervals are used for ranking and resource deltas. For task completion, the task is the
paired unit; repeated seeds are aggregated within task, and repository/task-cluster-aware
paired binary intervals prevent attempts from being treated as independent. The run
manifest freezes confidence level, interval algorithm,
resample count, repository-cluster resampling procedure, missing-pair policy,
multiplicity family and correction, and support floors based on independent repository
clusters plus positive and negative event counts. The primary contrast is frozen before
the locked run. Secondary profile comparisons are multiplicity-controlled or explicitly
marked exploratory.

Required measurements are:

- Retrieval quality: Precision@1/3/5, Recall@5/10, MRR, nDCG@10, first-useful rank,
  no-answer precision, duplicate rate, wrong-scope rate, per-retriever oracle Recall@N,
  candidate union Recall@N before reranking, risk/coverage, AURC, and stable-pagination
  agreement.
- Exact protection: exact-tier recall and first-rank agreement for errors, diagnostic
  codes, symbols, flags, paths, config keys, tool names, IDs, and quoted literals. Any
  approximate result above an eligible exact result, or any oracle-ineligible candidate
  assigned exact status, is an invariant failure. Reports include exact-tier precision
  and false-exact promotion rate.
- Temporal correctness: exact eligible- and forbidden-anchor agreement at each `as_of`,
  supersession correctness, late-arrival behavior, source watermark lag, projection lag,
  and freshness classification per source. A stale or unavailable source reported as an
  authoritative no-result is an invariant failure.
- Authorization: forbidden-anchor observations at candidate generation, fused ranking,
  context, explanations, debug output, caches, and persisted artifacts. Paired
  non-interference runs compare an authorized world with the identical world plus denied
  canaries and require byte-identical visible ranking, counts, caps, pages, cursors,
  explanations, cache behavior, timing class, and aggregates. Any observation or
  influence is authorization leakage and rejects the profile.
- Context quality: relevant evidence tokens divided by evidence tokens using
  payload-revision-bound judged spans and the frozen tokenizer. Every assembled output
  token range maps to payload revision and source token span; the run manifest freezes
  truncation, normalization, overlap, and duplicate-attribution rules. Reports also include context recall,
  duplicate-token rate, stale/forbidden-token rate, contradiction coverage, citation
  support, context tokens per useful evidence item, and hydration-byte amplification.
  Structural framing tokens are reported separately and never improve context precision.
- Performance: cold and warm end-to-end and per-stage p50, p95, and p99 latency,
  cancellation latency, and raw samples with quantile uncertainty. The manifest freezes
  enough independent repetitions for the p99 estimator; a sample set where p99 merely
  aliases the maximum is invalid.
- Resources: process-tree index-build peak RSS/PSS, model-load RSS/PSS delta,
  steady-state RSS/PSS, query peak RSS/PSS, CPU time, bytes read,
  index/vector/model bytes, incremental rebuild time, and write amplification. Linux
  runs separately report cgroup v2 `memory.peak` under that name, recreate the cgroup for
  each phase, and freeze page-cache and swap policy; cgroup peak is not labeled RSS.
  Another platform must declare equivalent process-tree and container-boundary methods.
- Tokens and cost: retrieved-context tokens, reranker input tokens, agent input/output
  and cache tokens from authoritative usage receipts, pricing-snapshot digest, cost per
  attempted task, and cost per completed task. Local-only stages report zero provider
  tokens and `not_applicable` provider price rather than an invented estimate.
- Task completion: paired completion rate over all attempts, deterministic verifier pass
  where available, blinded rubric otherwise, timeout/failure rate, turns, tool calls,
  latency, tokens, cost, recovery after fallback, and abstention correctness, all bound
  to the task fixtures' initial state, revisions, seeds, budgets, resets, and assignments.

Raw similarity, logits, score margins, fused scores, and model confidence strings are not
confidence. Every code-quantifier descriptor used by evaluation also carries a metric
card covering construct, formula, units, decision, confounds, supported languages,
coverage, datasets, uncertainty, and prohibited interpretations. Predictive claims use
chronological and project holdouts, LOC/churn/prior-defect baselines, calibration,
abstention, and worst-stratum results. Aggregate correlation does not establish causality.

## Decision policy and terminal outcomes

The fixture manifest freezes every metric direction, denominator, stratum, support
floor, practical margin, and stopping rule before candidate tuning.
A margin must cite a product SLO, correctness invariant, or baseline repeatability study.
The run manifest freezes all resolved revisions, statistical algorithms, and one executable
decision expression before locked outcomes are opened. That expression names one primary
endpoint or ordered objective, its superiority margin, every guardrail and
non-inferiority margin, protected-stratum requirements, missingness treatment, and
multiplicity procedure. Schema validation rejects missing values, unresolved revisions,
unsupported constants, or a threshold without an evidence source. Low-support strata
are `inconclusive`; they are never pooled away. Owners cannot override the expression or
remove metrics, weights, strata, denominators, or ordering after unblinding.

The harness returns exactly one typed outcome:

- `invalid_run`: fixture, revision, environment, authorization, sample-size, or artifact
  validation failed; no quality conclusion is permitted.
- `blocked`: an authoritative dependency, private fixture, usage receipt, model artifact,
  or required measurement is unavailable.
- `rejected`: a correctness invariant or frozen quality/resource gate failed.
- `inconclusive`: invariants passed, but support or uncertainty cannot decide the frozen
  primary contrast.
- `runtime_fallback_observed`: the candidate could not execute as declared and the
  baseline fallback was verified; this is diagnostic evidence, not promotion.
- `accepted`: every invariant passes, every required protected stratum is supported, and
  the frozen executable decision expression evaluates true.

Only `accepted` creates promotion evidence. Zero authorization leakage, exact-tier
precedence, temporal eligibility, source-scope correctness, and byte-identical
PR9 fallback subpayload
are hard invariants. Quality, latency, RSS, tokens, cost, completion, learned weights,
and learned thresholds use frozen evidence-backed margins; this plan deliberately does
not invent universal numeric cutoffs.

## Delivery composition

The exact, lexical, graph, temporal, diagnostic, semantic, and optional bounded
rerank behavior above ships through one accepted retrieval profile and one
application pipeline. An unavailable authority remains capability-reported
rather than simulated. The dashboard renders the accepted profile, freshness,
fallback, and report state without gaining promotion authority.

Task/session retrieval joins Plan 24 task roots to Plan 23 session evidence only
after the canonical task identity and typed application join ship. Until then
that lane is explicitly unavailable and never simulated or copied. Adding it
requires the same lane-disabled ablation and locked acceptance policy as every
other retriever.

Locked comparison inputs and accepted fallback bytes are immutable. A later
change creates a new append-only fixture, run, report, and promotion revision;
it cannot reinterpret an already revealed result or redefine the accepted
lexical fallback.

## Behavioral tests and gates

PR9/PR10 must keep direct domain/store retrieval contracts, every lane's
regressions, the hermetic quality suite, profile activation/rollback
regressions, and the applicable all-feature gate green. The current harness
must expose validation and locked comparison operations, but historical binary
names, command lines, test-target names, and artifact paths are not rebuild
requirements.

Validation is hermetic and fails on fixture/hash drift, private payload inclusion,
unresolved profile or model revisions, absent support floors or margins, unsupported
fixed constants, invalid temporal or authorization oracles, insufficient p99 sampling,
or missing decision owners. Locked comparison uses distinct outcomes for every terminal
outcome and fails the promotion gate unless the required outcome is `accepted`; it also
fails unless it emits raw samples, aggregates, decision JSON, fallback observations, and
an evidence map whose digests validate.

Contract fixtures cover every retriever independently, exact technical strings, typo
recovery, copies and echoes, contradictions, stale and superseded evidence, wrong
project/worktree/branch/time, authorization canaries, deterministic pagination,
contribution explanations, exact-admission hard negatives, deterministic committed
prefixes under execution-order and timing jitter, partial outcomes, cancellation, no-result behavior,
rank-before-hydrate, and hydration authorization recheck. PR10 additionally covers model
installation and offline reuse, batching, incremental vector reuse, incompatibility and
rebuild, privacy isolation, bounded reranking, model corruption/refusal/timeout,
configuration pinning, and byte-identical fallback.

## Rollout, rollback, and failure handling

Activation and rollback require the configuration-control-plane mutation capability; an
evidence-file path grants no authority. The transaction verifies artifact digest,
approvals, revisions, current-profile precondition, source/projection compatibility, and
that the rollback profile remains executable under the target schema. It then
compare-and-swaps active and rollback pointers and records the authenticated actor.
Rollout proceeds through lexical default, optional-channel shadow,
the report's predeclared staged cohort, and default eligibility. The cohort unit and
stopping rule come from the locked report; there is no universal rollout count.
Running queries and cursors stay pinned to their starting profile and freshness vector.
Runtime safety ceilings may equal or exceed the promoted profile budgets but may not bind
below them; otherwise activation fails because the evaluated profile cannot execute.

Authorization leakage, exact-tier demotion, temporal-invariant failure, or scope leakage
prevents activation and immediately disables the candidate profile. A frozen operational
budget breach triggers the report's rollback rule. Optional retriever failure produces a
visible partial result using the accepted lexical order; exact/lexical authority failure
returns unavailable rather than silently substituting semantic evidence. Reranker failure
returns the byte-identical pre-rerank order.

Promotion requires a successful rollback drill. Unauthorized callers, tampered evidence,
stale and concurrent updates, crash atomicity, incompatible rollback targets, pinned
cursors, and audit completeness are integration-tested. An incompatible rollback fails
closed instead of activating an unvalidated profile. Activation, status verification,
and rollback must be callable production operations that consume validated
promotion evidence. Their historical CLI spelling and evidence path are not
acceptance requirements.

Rollback writes an audit event containing the failed profile, restored profile, trigger,
freshness vector, and report anchor. It does not delete vectors, rewrite fixtures, or
alter canonical evidence. Re-enablement requires a new accepted append-only report.
Integration fixtures inject each staged stop and automatic trigger: authorization
influence, exact-tier demotion, temporal error, scope leakage, and operational-budget
breach. Each test asserts atomic disablement or rollback, unchanged pinned in-flight
queries, complete authenticated audit data, and rejection of a runtime ceiling below the
evaluated profile budget.

## Acceptance

- The seven retrieval lanes are independently testable, disableable, budgeted, and
  attributable; exact and lexical remain available without semantic, graph, temporal,
  task/session, or diagnostic success.
- The application contract proves compact-candidate retrieval, authorization, temporal
  resolution through Plan 23's export port, deterministic fusion, source-aware
  dedupe/diversity, optional bounded authorized rerank views, then final context
  hydration in that order.
- Every ranked result exposes per-retriever contribution provenance, per-source
  freshness, coverage, cap and dedupe decisions, and typed fallback reasons.
- Exact errors, symbols, flags, paths, IDs, diagnostic codes, config keys, tool names,
  and quoted literals cannot be demoted by approximate fusion or reranking.
- The checked-in fixture and run schemas reproduce the baseline, PR9, PR10, channel
  ablations, exact-scan/ANN comparison, and reranker comparison with immutable evidence.
- Temporal correctness, authorization leakage, context precision/recall, p50/p95/p99
  latency, RSS, tokens, cost, and task completion are measured with the declared methods
  and protected strata. No aggregate score hides a failed invariant or worst stratum.
- Semantic vectors are source-local derived projections; no monolithic embeddings table,
  second corpus database, or cross-privacy-domain vector authority exists.
- No fixed RRF constant, fusion weight, quality threshold, diversity quota, ANN choice,
  model, or reranker is promoted without locked TraceDecay evidence.
- Hermetic validation, locked comparison requiring `accepted`, focused direct
  contracts, activation tests, the quality suite, and the applicable
  all-feature gate pass; the evidence map validates; and the authorized
  rollback drill restores the prior accepted profile atomically.
- No public leaderboard, universal rollout count, uncalibrated score, LLM-only judgment,
  or aggregate correlation is treated as promotion authority.
