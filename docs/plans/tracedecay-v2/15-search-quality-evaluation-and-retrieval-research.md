# TraceDecay V2 Search Quality, Evaluation, and Retrieval Research

## Status / Role

Status: active product plan and quality authority. It covers federated
retrieval and source-bound shared-code evaluation. Neural dense retrieval is
retired by
[rejected decision 11](00-plan-set-index.md#rejected-approaches).

Federated retrieval uses independent exact, lexical, graph, temporal,
task/session, and diagnostic retrievers. It preserves deterministic fusion,
source-aware deduplication and diversity, compact-candidate ranking, late
hydration, and direct developer evaluation. Source-bound shared-code detection
is a Plan 25 code-generation authority, not a fourth retrieval lane.

This plan is the quality and composition authority. It does not replace the canonical
stores, the Plan 23 temporal query kernel, the Plan 24 task/work graph, the Plan 25 code
graph, Plan 13 diagnostic anchors, or their authorization rules.

Plan 15 owns retrieval quality, composition, evaluation, and profile-selection
rules. Plan 25 owns code retrieval and shared-code facts. The application,
dashboard, task/work, and public surface plans consume tested behavior and
evaluation results. Plan 31 is archival.

## Outcome

Search returns useful, correctly scoped, temporally valid evidence on the first page
across real local projects. Exact technical lookup remains non-demotable. Every result
can explain which retrievers contributed, which source freshness was observed, which
deduplication or diversity decision applied, and why a source was partial or
unavailable. The implementation ranks compact authorized candidates before
hydrating payloads.

## Non-negotiable decisions

- Retrieval is a federation of independently testable and independently
  disableable exact-literal, lexical, graph, temporal, task/session, and
  diagnostic lanes. One lane is never an alias for another.
- Exact IDs, diagnostic codes and text, symbols, CLI flags, quoted literals, paths,
  config keys, tool names, commit identifiers, task/session IDs, and protocol fields
  enter a lexicographically higher exact tier. Approximate fusion cannot demote
  them.
- Approximate candidates use deterministic fixed-point weighted fusion. Every enabled
  weight and calibration belongs to a versioned profile backed by a recorded Linux
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
  Final context hydration occurs only for the selected result set.
- Shared-code facts are sealed with the code generation. Clone backfill and
  verification never gate lexical or graph readiness. Exhausted work, posting,
  or deadline budgets return partial coverage with accounting, not an empty
  complete result.
- A digest narrows shared-code candidates. Token comparison supplies evidence.
  Name similarity, graph proximity, and a scalar score do not prove shared
  implementation.
- TraceDecay will not adopt a conventional fixed RRF constant such as `k = 60`, fixed
  fusion weights, similarity cutoffs, abstention margins, graph-hop cutoffs, freshness
  penalties, or diversity quotas without a direct TraceDecay evaluation. An
  RRF or threshold profile may be an evaluated candidate. Resource-safety
  ceilings are engineering limits, not quality claims.

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
  `FusedCandidate`, `RankedCandidate`, `RetrievalResult`, `HydrationReceipt`,
  and evaluation decision IDs.
- `src/application/retrieval/{mod.rs,ports.rs,pipeline.rs,types.rs}` owns orchestration,
  budgets, cancellation, query-snapshot pinning, partial-outcome policy, and the
  rank-before-hydrate boundary when the application layer lands. It depends on
  the retrieval ports, not storage implementations.
- `src/query/retrieval/{exact.rs,lexical.rs,graph.rs,temporal.rs,task_session.rs,diagnostic.rs}`
  owns independent adapters; `src/query/retrieval/ports.rs` owns the single
  generic `Retriever<R, E>` port. `fusion.rs`, `dedupe.rs`, `diversity.rs`, and
  `hydrate.rs` own deterministic composition stages. Paths are historical
  examples when the current crate layout differs.
- `src/query/temporal/` remains the only current/as-of/evolution/forensic temporal
  eligibility and pagination kernel. Plan 23 owns
  `src/query/temporal/ports.rs::TemporalCandidateExport`, which returns authorized
  compact candidates, typed mode/cutoff, source coverage, and freshness before payload
  hydration. `retrieval/temporal.rs` consumes that port; it does not copy temporal
  resolution, temporal fusion/diversity, cursor, or hydration semantics.
- Plan 25's project code graph remains the graph source of truth. `graph.rs` emits stable
  code anchors and bounded relationship evidence without copying graph rows into a
  search corpus. Plan 25 also owns clone payloads, occurrence bindings, exact
  postings, positional fingerprints, and generation-bound shared-code
  coverage.
- Plan 24 owns `TaskId`, task/work topology, attempts, dependencies, and task query
  semantics. `task_session.rs` joins task roots to Plan 23 session evidence by stable
  authorized anchors; it never copies task or session payloads.
- Plan 13 and the diagnostic owning stores retain GitHub, CI, compiler, lint, and runtime
  diagnostic evidence. `diagnostic.rs` resolves their stable anchors and never treats
  LSP projection as canonical storage.
- `src/global_db/retrieval/lexical.rs` owns only global-store lexical
  projection rows. Project graph and other stores expose equivalent projection
  ports in their owning crates.
- Existing store authorization and privacy-domain resolution are authoritative. Each
  owning source applies authorization, scope, and temporal eligibility before emitting a
  candidate. The application pipeline and every owning-store hydrator recheck eligibility
  as defense in depth.
- Configuration owns versioned retrieval profiles. The dashboard renders
  profile, freshness, coverage, and report state. It does not select ranking
  policy.
- The hermetic developer evaluation and direct contract regressions remain
  evaluation infrastructure. Their current owners do not create a service,
  evaluation database, acceptance packet, or separate evidence authority.
- MCP, CLI, dashboard, and agent surfaces remain thin consumers of the application
  contract. Public operation naming remains with the transport/catalog plans.

## Typed retrieval contract

Federated retrieval must provide an equivalent typed contract with the behavior and information
below. The Rust sketch is explanatory, not an artifact-name or source-layout
requirement; field/type names may change when direct contract tests preserve
the semantics.

```rust
pub enum RetrieverKind {
    ExactLiteral,
    Lexical,
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
    pub retrieval_budget: RetrievalBudget,
}

pub struct QueryFallbackSubpayload {
    pub profile_id: FusionProfileId,
    pub ordered_candidates: Vec<RankedCandidate>,
    pub public_fallback_lane_coverage: BTreeMap<RetrieverKind, PublicRetrieverStatus>,
    pub freshness: Vec<SourceFreshness>,
    pub cursor: Option<RetrievalCursor>,
    pub digest: FallbackSubpayloadDigest,
}

pub struct RetrievalResult {
    pub snapshot: RetrievalSnapshot,
    pub profile_id: FusionProfileId,
    pub query_fallback: QueryFallbackSubpayload,
    pub ordered_candidates: Vec<RankedCandidate>,
    pub internal_lane_outcomes: BTreeMap<RetrieverKind, RetrieverOutcome<()>>,
    pub public_lane_coverage: BTreeMap<RetrieverKind, PublicRetrieverStatus>,
    pub freshness: Vec<SourceFreshness>,
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

`QueryFallbackSubpayload` is canonical-encoded and hashed independently with the
schema/domain separator `tracedecay.query-fallback.v1`; the digest field itself
is excluded from those hashed bytes. Its ranked candidates contain the exact,
lexical, and graph contributions/decisions/explanations; its maps contain only
`ExactLiteral`, `Lexical`, and `Graph`. The subpayload, its digest, and its
cursor identity do not depend on clone indexing or shared-code operation
availability.

Sealed `internal_lane_outcomes` remains only in the enclosing audit result and
is excluded from fallback bytes/digest, cursors, public coverage, and cache
keys. Denied and absent evidence coalesce through the same sanitized
unavailable shape and cannot differ in counts, timing class, cache effects, or
public bytes.

`RankingDecision` records exact-tier admission, same-source duplicate collapse,
logical-copy representative selection, contradiction preservation, each diversity-cap
decision, and fallback. Explanations are rendered from this provenance;
they are not reconstructed from a final scalar score.

Only the central exact-admission validator can mint `ExactAdmissionProof`; retrievers
cannot assign an exact tier. The proof binds rule revision, typed field, original bytes,
canonical bytes, normalization steps, scope, authorization revision, and temporal
snapshot. Fusion derives `ExactClass` only from a validated proof.

Every contribution and hydration receipt keys back to one `OccurrenceProvenance`.
Parallel unassociated provenance lists are forbidden because they cannot
reproduce deduplication, diversity, freshness, or hydration decisions.
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
   inferred similarity cannot confer exact status; phrase status requires explicit
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
4. Collapse duplicate rows for the same source occurrence. Never collapse
   merely by content hash, title, timestamp, or a shared-code candidate.
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
   its evaluated profile revision; unevaluated caps remain disabled except for
   resource-safety ceilings.
10. Recheck authorization and hydrate final context for the selected anchors through each owning
    store under byte/token/deadline budgets. Record a `HydrationReceipt` per anchor.
11. Assemble `RetrievalResult` and compact context with citations, sanitized coverage,
    authorized freshness, ranking decisions, hydration receipts, and a
    lossless source-anchor drill-down path.

The lexical retriever provides fielded BM25 over typed result grains, character-level
typo recovery, query/tool/protocol echo penalties, and exact phrase support. The graph,
temporal, task/session, and diagnostic adapters must expose their own candidate pools and
oracle recall; they do not become lexical fields.

## Source-bound shared-code evaluation

Plan 25 computes shared-code facts from source tokens during the existing
parse. The sealed code generation owns clone payloads, exact occurrence
bindings, digest postings, positional fingerprints, normalization revisions,
budgets, and coverage. Grafeo does not own these facts.

Use this vocabulary in product and evaluation output:

- A **shared implementation** is source-backed overlap between exact source
  occurrences.
- An **exact normalized copy** has equal verified conservative token bytes.
- A **renamed copy** has equal verified rename-normalized token bytes.
- A **verified near-duplicate** has bounded, token-verified overlap with
  separate left and right coverage plus explicit differences.
- A **contained shared block** is a selected source range verified inside
  another occurrence. It is not whole-function equivalence.
- A **review candidate** has enough source-backed evidence to inspect. It is
  not a safe edit, dead code, or guaranteed removable code.

Digest and fingerprint matches only admit candidates. The verifier compares
canonical token bytes before it reports a match. Exact families group
occurrences by verified payload instead of producing quadratic pairs.
Near-duplicate relations are non-transitive. Generated code, unsupported
normalization, too-small bodies, exhausted postings, and exhausted verification
work remain explicit exclusions or partial coverage.

## Developer evaluation and fixtures

Federated retrieval and source-bound shared-code detection use a small
checked-in sanitized corpus and direct production adapters. The corpus covers
exact errors, symbols, flags, paths, IDs, false-exact hard negatives, lexical
variants, graph questions, temporal queries, stale or superseded evidence,
wrong-scope cases, authorization canaries, contradictions, shared-code
positives and hard negatives, and expected no-result cases. Labels are
ordinary reviewable fixture data.

Each Linux developer run records the workload revision, candidate and profile
revision, seed, cache state, command, environment summary, raw measurements,
and a truthful `pass`, `fail`, or `pending` result. Private query text and
source payloads stay in their authorized stores. Checked-in fixtures remain
sanitized.

Task fixtures may pin sanitized initial repository content, verifier/rubric,
agent and tool revisions, budgets, timeout, and seed. Temporal and context
fixtures retain the source generations, watermarks, payload revisions, and
expected eligibility needed to test product behavior. These are product-test
inputs, not PR-specific evidence scaffolding.

## Required comparisons and metrics

Use the same sanitized fixture revision, candidate/context budgets, Linux
environment, cache preparation, and seed for baseline and candidate. Compare
the production baseline, exact and lexical behavior, each independent
retrieval lane, and source-bound shared-code classes. Compare shared-code
results by class, supported language, overlap band, scope, generated-code
policy, and complete or partial accounting.

Report denominators and per-query results for exact/no-answer/wrong-scope,
temporal, privacy, and low-coverage cases. At minimum report first-useful rank,
Recall@10, duplicate and wrong-scope rates, exact-tier preservation, stable
pagination, temporal eligibility, authorization-canary influence, context
precision/recall, cold/warm latency, CPU, peak RSS, index bytes, and incremental
rebuild time. Shared-code evaluation also reports exact and renamed family
precision, near-duplicate precision and recall when that verifier is available,
contained-block outcomes, excluded-too-small counts, examined and omitted
counts, verification work, and continuation behavior. Task evaluations
additionally report completion, failure/timeout, turns, tokens/cost when
authoritative usage data exists, and fallback/abstention behavior.

Linux measurements are descriptive developer evidence. Keep raw samples,
identify the process/resource method, and label unsupported or unexecuted
measurements `pending`; do not manufacture p99, confidence, or cross-platform
equivalence from insufficient samples. Linux/macOS/Windows default-feature
product support is verified by normal CI, not by cloning the developer eval
across operating systems.

Raw similarity, score margins, and fused scores are not probabilities.
Aggregate correlation does not establish causality.

## Decision policy and terminal outcomes

The evaluated workload, revisions, seed, budgets, and pass conditions are
reviewable before a candidate result is used. Zero authorization influence,
exact-tier precedence, temporal eligibility, source-scope correctness, and a
byte-identical exact/lexical result subpayload are hard product invariants. Candidate
quality or resource improvements use practical thresholds justified by the
baseline and product behavior; this plan does not invent universal cutoffs.

The developer summary reports `pass`, `fail`, or `pending`. Invalid fixture or
environment data is `fail`; an unavailable dependency or unexecuted
measurement is `pending`; any hard-invariant regression is `fail`. Only a
passing direct product test suite and passing Linux evaluation support a
profile change. No separate promotion evidence, owner approval record, or
acceptance receipt is created.

## Delivery composition

The exact, lexical, graph, temporal, and diagnostic behavior above ships
through one retrieval profile and one application pipeline. An unavailable
authority remains capability-reported rather than simulated. The dashboard
renders the profile, freshness, coverage, and evaluation state without
selecting retrieval policy.

Shared-code operations consume Plan 25 facts through their own source-range and
occurrence contracts. They do not enter query fusion or become a fourth
code-intelligence lane. Public catalogs expose an operation only when its
production caller, input contract, paging, coverage, and failure states exist.

Task/session retrieval joins Plan 24 task roots to Plan 23 session evidence only
after the canonical task identity and typed application join ship. Until then
that lane is explicitly unavailable and never simulated or copied. Adding it
requires the same lane-disabled comparison and direct tests as every
other retriever.

Evaluation fixtures and result bytes use explicit immutable revisions. A later
change creates a new fixture or profile revision and reruns the direct tests and
Linux evaluation. It cannot reinterpret an earlier result.

## Behavioral tests and evaluation

The delivery must keep direct domain/store retrieval contracts, every lane's
regressions, the hermetic quality suite, normal all-feature CI, and the Linux
developer evaluation green.
Historical binary names, command lines, test-target names, packet schemas, and
artifact paths are not rebuild requirements. Validation fails on fixture
digest drift, private payload inclusion, unresolved profile revisions, invalid
temporal or authorization oracles, or missing measurements required by
the declared result. The checked-in summary reports `pass`, `fail`, or
`pending`.

Contract fixtures cover every retriever independently, exact technical strings, typo
recovery, copies and echoes, contradictions, stale and superseded evidence, wrong
project/worktree/branch/time, authorization canaries, deterministic pagination,
contribution explanations, exact-admission hard negatives, deterministic committed
prefixes under execution-order and timing jitter, partial outcomes, cancellation, no-result behavior,
rank-before-hydrate, and hydration authorization recheck. Shared-code fixtures
cover conservative exact copies, renamed copies, forced digest and fingerprint
collisions, too-small bodies, selected-block containment, changed and deleted
occurrences, restart, branch switching, authorization, generated code,
pagination, cancellation, and partial accounting.

## Failure handling

Running queries and cursors stay pinned to their starting profile, source
manifest, and freshness vector. Authorization leakage, exact-tier demotion,
temporal failure, or scope leakage fails the evaluation. Optional retriever
failure returns a visible partial result. Exact or lexical authority failure
returns unavailable.

Clone backfill and shared-code queries use bounded work, posting, candidate,
verification, result, and deadline budgets. Exhaustion returns a continuation
or partial-with-accounting result. It never resets counters inside one request,
reports absence, or changes lexical or graph readiness.

## Acceptance

- The six retrieval lanes are independently testable, disableable, budgeted,
  and attributable. Exact and lexical remain available without graph, temporal,
  task/session, or diagnostic success.
- The application contract proves compact-candidate retrieval, authorization, temporal
  resolution through Plan 23's export port, deterministic fusion, source-aware
  deduplication and diversity, then final context hydration in that order.
- Every ranked result exposes per-retriever contribution provenance, per-source
  freshness, coverage, cap and dedupe decisions, and typed fallback reasons.
- Exact errors, symbols, flags, paths, IDs, diagnostic codes, config keys, tool names,
  and quoted literals cannot be demoted by approximate fusion.
- The checked-in fixture and run schemas reproduce the baseline, federated
  retrieval, lane ablations, and shared-code classes with immutable evidence.
- Temporal correctness, authorization leakage, context precision/recall, p50/p95/p99
  latency, RSS, tokens, cost, and task completion are measured with the declared methods
  and protected strata. No aggregate score hides a failed invariant or worst stratum.
- Shared-code output uses the approved vocabulary and reports exact source
  identities, generations, normalization revision, separate directional
  coverage where relevant, explicit differences, exclusions, and continuation
  or partial accounting.
- No fixed RRF constant, fusion weight, quality threshold, or diversity quota
  becomes product policy without a passing direct TraceDecay evaluation.
- Hermetic fixture validation, focused direct contracts, the quality suite,
  Linux developer evaluation, and normal all-feature CI pass.
- No public leaderboard, universal rollout count, uncalibrated score, LLM-only judgment,
  or aggregate correlation is treated as profile-selection authority.
