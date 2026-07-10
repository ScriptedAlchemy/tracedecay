# TraceDecay V2 Query Crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build one bounded, explainable, cancellation-safe query engine over federated TraceDecay V2 shards, with stable snapshot pagination and identical semantics for CLI, MCP, HTTP, SSE, dashboard, labs, and exports.

**Architecture:** `tracedecay-domain` owns the transport-neutral `TraceQueryV1` AST; `tracedecay-query` validates and plans it, prunes catalog-described shards, delegates typed fragments through store/projector ports, and deterministically merges bounded pages. The crate returns typed rows, ranking evidence, vector watermarks, coverage, cursors, export chunks, and live-read-model deltas without importing SQLite, Axum, MCP, CLI, or dashboard code.

**Tech Stack:** Rust 2024 workspace; `serde`; `thiserror`; `uuid`; `blake3`; `base64`; `hmac`/`sha2` for opaque cursor authentication; `futures` boxed futures/streams; `tokio` test runtime; `proptest`; Criterion; V2 SQLite/FTS5 and representation indexes behind `tracedecay-store` ports.

---

## 1. Contract Lock

This plan refines master-plan PRs 11–16 and supplies the query-side contracts consumed by PRs 24A–24E, 27–31, and 34–37.

Plan [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md) extends this same `TraceQueryV1` algebra with initiative/plan/work-item/dependency/lease/attempt/executor/packet/artifact/outcome sources, typed traversal, critical path, agent-relevant slices, and saved task projections. It cannot introduce a task-only query engine, board filter language, cursor, ranking path, or context assembler.

- Canonical AST ownership remains `crates/tracedecay-domain/src/query/`; its public name is `TraceQueryV1`. `crates/tracedecay-query/src/ast.rs` is a parser/re-export façade. This reconciles the master plan's PR 11 file name with its dependency rule that `TraceQueryV1` is a domain type.
- Canonical entity, scope, time, evidence, sensitivity, shard, watermark, and schema identifiers come from `tracedecay-domain`; this crate does not introduce parallel string IDs.
- Catalog/projector/store implementations remain in `tracedecay-store` and `tracedecay-projectors`. This crate defines read ports and logical fragments, not SQL migrations or projector write paths.
- `tracedecay-application` owns authorization decisions, saved-view mutations, annotations, export job lifecycle, and use-case composition. It passes an already-authorized `QueryAccess` to this crate.
- `tracedecay-api` owns HTTP/OpenAPI/SSE framing, `Last-Event-ID`, heartbeat bytes, and bearer/CSRF/CSP enforcement. It maps query snapshot/delta/gap types without changing semantics.
- Exact public replay mode names shared with capture and policy are `ReplayMode::ExactDeterministic`, `ReplayMode::RecordedResult`, and `ReplayMode::CurrentBestEffort`. Query Lab uses the query engine's own versioned plan/index/ranker references; policy evaluators use the policy crate.
- A frozen query never observes rows above its captured per-shard high-watermarks. A live query starts with the same frozen snapshot and then emits ordered deltas.
- A missing, corrupt, stale, incompatible, locked, or redacted shard never disappears silently. Its disposition is present in `CoverageReport`.
- Read-only execution never updates usage, retrieval, hint, ranking, or memory counters. Adoption/feedback is recorded later as an explicit application/domain event.
- [`23-session-lcm-temporal-retrieval-and-evaluation.md`](23-session-lcm-temporal-retrieval-and-evaluation.md) is the normative session/LCM specialization: this crate owns its occurrence/logical-copy/summary-DAG candidate channels, temporal current/as-of/evolution/forensic resolver, authority/supersession features, federated fusion, context assembly, explanations, and evaluation execution. No legacy `message_search` or LCM binding retains independent rank semantics.

## 2. Goals

- Express All/profile, project set, project, repository, checkout, worktree, ref, snapshot/commit, session, agent, workflow, and saved-collection scopes in one typed AST.
- Use domain `ScopeSelectorV2` unchanged everywhere; never silently infer current project/worktree/ref/generation when a selector is absent, stale, or ambiguous.
- Query nearby active agents/claims across the same or parallel worktrees by repo/ref/PR/file/symbol/query overlap, TTL/status, read/write intent, parent/goal, and declared redundancy without exposing prompt text.
- Support typed predicates; occurred/ingested/valid/as-of time; lexical search; semantic similarity; graph/path/impact; facets; aggregates; projections; sorting; comparison; and declared sampling/level of detail.
- Prune shards from catalog metadata before opening them and cap concurrent opens at 32.
- Reject unbounded graph, timeline, aggregate, hydration, and export work before execution.
- Push eligible filters, FTS, vector, traversal, aggregation, and top-k work into shards.
- Merge shard results deterministically with a declared ranking profile and stable entity-ID tie-breaking.
- Authenticate opaque cursors and bind them to query fingerprint, access digest, schema/ranking/index versions, expiry, watermarks, shard positions, and the global merge cutoff.
- Return searched/skipped/stale/unavailable/incompatible/locked/redacted coverage and exact truncation reasons on every response.
- Provide explain plans that expose selection, pushdown, cost, ranking, timing, coverage, and safe fingerprints without query literals or payloads.
- Provide bounded JSONL and Parquet export streams with manifests, hashes, redaction reports, and snapshot completeness.
- Provide snapshot/delta/gap/resync read-model contracts for SSE without depending on Axum.
- Match V1 behavior where compatibility requires it and make every intentional rank/order difference measurable.

## 3. Non-Goals

- No GraphQL execution layer.
- No remote libSQL, hosted coordinator, multi-tenant authorization server, or required network service in the first V2 default.
- No raw SQL, SQLite connection, migration, WAL, blob file, HTTP, MCP, CLI, or React type in this crate.
- No implicit network embedding call. Representation generation is a separate, consent-gated policy/projector concern; query consumes only declared local representations.
- No unbounded all-entity graph, match-all payload hydration, or open-ended export.
- No mutation of facts, retrieval counters, hints, saved views, annotations, automation, policies, or source data.
- No hidden score formula. Every enabled rank component has an ID, version, normalization, weight, and optional per-result explanation.
- No attempt to reconstruct hidden model chain-of-thought.

### 3.1 Convergence boundary

Query is the sole federated read planning/execution owner inside [`19-system-defragmentation-convergence-and-extensibility.md`](19-system-defragmentation-convergence-and-extensibility.md). It consumes canonical scope resolution from the application and [`16-cross-project-repository-worktree-scope.md`](16-cross-project-repository-worktree-scope.md), sink-eligible projections from Plans [`01`](01-domain-crate.md)/[`04`](04-projectors-crate.md), follows the retrieval evaluation program in [`15`](15-search-quality-evaluation-and-retrieval-research.md), and enforces containment from [`18`](18-secret-detection-redaction-and-private-data-safety.md).

| Boundary | Contract |
|---|---|
| Enters | Validated `TraceQueryV1`, application-produced `ScopeResolutionV2`, access/privacy context, ranking/representation manifests, registered shard capabilities, deadlines/budgets, and captured watermarks. |
| Exits | Deterministic rows/edges/facets/aggregates, signed cursors, exports/change streams, explain plans, rank evidence, coverage/freshness/redaction/retention, and query receipts. |
| Upstream owners | Application resolves/authorizes scope; projectors own semantic read models; store owns physical registered fragments/snapshots; domain owns AST/types. |
| Downstream owners | Policy consumes immutable candidates; application composes use cases; API/CLI/MCP/SDK/UI render one shared envelope. None performs hidden search/ranking/graph expansion. |
| Extension seam | A new operator/channel requires a registered AST/capability, store lowering, cost/cancellation/privacy rules, explain shape, deterministic merge, qrel/ablation gates, and transport generation—never a handler-local SQL/search path. |
| Scale/concurrency | Catalog pruning precedes opens; bounded shard/vector concurrency, cancellation checkpoints, frozen vector snapshots, stable top-k merge/cursors, and explicit partial coverage. |
| Migration/retirement | V1 query/search handlers remain internal differential adapters. Retire each only after semantic/precision/latency/coverage parity and current-client cutover; no live fallback or duplicate ranker remains. |

Query errors cover validation/planning/execution (`invalid_query`, `resolution_mismatch`, `budget_exceeded`, `deadline_exceeded`, `cancelled`, cursor restart reasons, shard/payload/representation failures). Scope locator ambiguity/not-found and public remediation belong to application/API Plans 09/17.

## 4. V1 Seams to Preserve, Replace, or Retire

| V1 seam | Current responsibility | V2 action and parity evidence |
|---|---|---|
| `src/sessions/lcm/query.rs` (`load_session`, `recent_sessions`, `session_replay_slice`, `grep`, `expand`, `expand_query`, `describe`, `status`) | LCM enumeration, FTS/LIKE fallback, reranking, replay slices, status, payload health, SQL assembly | Replace reads with `TraceQueryV1` profiles and timeline/session services. Port punctuation/CJK/emoji, phrase, provider, Git-scope, raw/summary provenance, content slicing, and stable-order golden cases into PRs 11–15. Keep the V1 adapter only inside shadow/rollback until parity is explained. |
| `src/sessions/lcm/query.rs` (`GrepQueryPlan`, `grep_query_plan`, `sanitize_fts5_query`, `requires_like_fallback`, `grep_order_by`, `sort_hits`) | Query parsing, fallback, candidate widening, ordering inside one 3k-line module | Move parser/lexical semantics to `ast.rs` and `operators/fts.rs`; move ordering to versioned rank profiles. Differential fixtures assert exact inclusion and declared score/order differences. |
| `src/sessions/git_correlation.rs` (`GitScopeFilter`, `session_ids_for_scope`, `git_scope_exists_predicate`) | Converts branch/worktree/commit filters into session filtering | Consume projected evidence relations through `RelationPredicate`; never generate transport-specific SQL. Preserve direct-vs-inferred evidence and health/partial-state reporting. |
| `src/memory/retrieval.rs` (`FactRetriever`, `build_fts_query`, `combined_score`, `temporal_decay_factor`) | Memory FTS, token overlap, holographic score, trust and time weighting | Replace read orchestration with retrieval query/ranking profiles. Preserve V1 as a named compatibility profile; current V2 hybrid ranking is a separate version and cannot silently reorder compatibility output. |
| `src/hooks/memory_inject.rs` (`select_digest_facts`, `select_prompt_recall_facts`) | Additional memory selection and dedupe after retrieval | Selection becomes a policy evaluation over query-produced candidates. Query emits immutable candidates/features and never records injection or usage. |
| `src/dashboard/graph_api.rs::search` and graph DB search/context/path handlers | Dashboard-specific graph query orchestration | Route through typed text/graph operators and application use cases. No dashboard SQL remains after the Code workspace cutover. |
| `src/mcp/tools/handlers/{analysis,graph,grep,memory,session,workflow_query}.rs` | MCP-specific scope resolution, filtering, rendering, truncation, and routing | Retain only argument mapping, application call, and renderer. PR 24E parity tests compare typed JSON before markdown rendering. |
| V1 Git discovery/context tools `branch_list`, `branch_search`, `branch_diff`, `pr_context`, `changelog`, `commit_context`, `sessions_for`, `workflows` | Separate local-graph, Git/session-correlation, and live-delivery entry points whose freshness can be mistaken for one truth | Catalog every tool as a typed query profile with required source/capability/freshness. Preserve local semantic graph and live GitHub truth as separate read models; joined results require revision reconciliation and carry both watermarks. Routing policy is owned by `06-policy-crate.md`. |
| `src/cli/parse_tests.rs::parses_sessions_ingest_and_search_commands` and CLI session handlers | CLI-specific search grammar and output | Use current cases as differential fixtures and map the accepted current surface to one AST. Do not publish retired flags/tool names as runtime aliases. |
| `src/mcp/response_handles.rs` (`store_response_handle`, `retrieve_response_handle`) | Renderer-level truncation recovery using expiring files | Structured query pages paginate before rendering. Compatibility handlers may wrap a V2 cursor/export ID; new APIs never use response handles as pagination. |
| LCM/session exports and automation artifact-specific exports | Separate payload/export conventions | Replace with one manifest-bearing export stream whose sink is supplied by application/store. |
| `tests/session_suite/{lcm_query,message_search_eval_test}.rs`, `tests/mcp_suite/{mcp_handler_test,workflow_query_test}.rs`, `tests/memory_suite/{memory_test,memory_eval_test}.rs` | Existing search, filtering, rank, scope, and rendering behavior | Copy cases into a redacted V1/V2 differential corpus; retain V1 tests through the internal data rollback window only. |

The V1 policy-specific seams (`src/hooks/tool_hints*`, correlation scoring, scheduler, and memory injection) are detailed in `06-policy-crate.md`; this crate supplies their read-only candidate and evidence inputs.

### 4.1 Base and incoming-master prerequisites refreshed on 2026-07-10

- The inspected base `99ad19bc` contains merged PR #405 (`fix(storage): adopt legacy identity stores safely`) and #412 (`fix(runtime): drain daemon safely during upgrades`). Query inventory consumes the adopted canonical `ShardRef` once. Snapshot acquisition during update/maintenance must observe the lifecycle drain/checkpoint receipt or return named stale/unavailable coverage, never race an old WAL writer.
- PR #407 (`fix(hermes): use the user TraceDecay profile`) consolidates Hermes into the user profile and removes Hermes-local bridge/config/inventory paths. `All` and every cursor bind to the canonical user `ProfileId`; query planning must not open or federate an implicit Hermes profile. Duplicate imported rows are reconciled by the migration manifest before query exposure.
- PR #410 (`fix(sessions): collapse copied subagent prompts`) is the V1 semantic baseline for query-time parent representative dedupe and `direct_user`/`subagent`/`tool_result` filters. V2 adds raw/native, representative, human/direct-user, subagent, tool-result, and protocol modes with hidden-copy counts, classifier version, and provenance; it never deletes copied native rows.
- PR #411 (`fix(doctor): report foreign-installation skill packages as info, not update-nag`) makes ownership and remediation agreement query-visible: Observatory/skills queries return owner class, severity, actionable capability, and `no_action_for_this_installation`; they cannot recommend a mutation the current installation refuses.
- Publication master `9f7a1108` later merged #410/#411/#413/#414/#415/#416/#417/#419/#420/#422. Open #407/#418/#423 are refreshed before PR 11; #417's split-identity visibility is a required scope/coverage fixture, #414/#419's edit capability is catalog/application behavior rather than a query operator, and #423's FTS direction/counter behavior is a future regression input until merge. PR #409 remains historical.
- Rebase PR 11 onto then-current master, regenerate store/profile/tool inventories, and rerun V1 golden queries. Deleted transition paths are not V2 extension points.

## 5. Exact File and Module Tree

```text
crates/tracedecay-query/
├── Cargo.toml
├── src/
│   ├── lib.rs                    # curated public API and crate invariants
│   ├── ast.rs                    # domain AST re-exports plus parse/canonicalize entry points
│   ├── error.rs                  # QueryError and stable error/restart codes
│   ├── ports.rs                  # catalog, shard, payload, representation, change-feed ports
│   ├── request.rs                # QueryRequest, QueryContext, QueryAccess, QueryMode
│   ├── validate.rs               # bounds, operator compatibility, sensitivity validation
│   ├── cost.rs                   # CostBudget, CostEstimate, per-operator accounting
│   ├── cursor.rs                 # signed cursor codec, expiry, compatibility, resume state
│   ├── coverage.rs               # shard dispositions and completeness summaries
│   ├── privacy.rs                # Plan 18 receipt/eligibility/containment checks; no detector/redactor
│   ├── explain.rs                # safe plan/result/ranking explanation view models
│   ├── planner/
│   │   ├── mod.rs                # QueryPlanner orchestration
│   │   ├── scope.rs              # validate application-resolved scope and bind it to plan/cursor
│   │   ├── shards.rs             # capability/time/kind/statistics pruning
│   │   ├── pushdown.rs           # logical operator to shard-fragment lowering
│   │   └── hydrate.rs            # batched entity/provenance/payload hydration plan
│   ├── execute/
│   │   ├── mod.rs                # QueryEngine orchestration
│   │   ├── coordinator.rs        # bounded parallel shard execution and cancellation
│   │   ├── merge.rs              # global sort/top-k/facet/aggregate merge
│   │   └── resume.rs             # per-shard resume and unavailable-shard preservation
│   ├── operators/
│   │   ├── mod.rs                # typed OperatorPlan/ShardOperator vocabulary
│   │   ├── filter.rs             # typed scalar/entity/evidence filters
│   │   ├── fts.rs                # phrase/prefix/tokenizer/fallback contract
│   │   ├── fuzzy.rs              # bounded typo/alias candidate generation after exact/token channels
│   │   ├── vector.rs             # metric/model/version/exact-fallback contract
│   │   ├── graph.rs              # bounded neighborhood/path/impact/affected-tests contract
│   │   ├── coordination.rs       # nearby agents, claim overlap, TTL/redundancy contract
│   │   ├── time.rs               # event windows and bitemporal/as-of contract
│   │   ├── aggregate.rs          # facets/grouping/unknown denominators
│   │   └── hydrate.rs            # fields, provenance, authorized payload slices
│   ├── rank/
│   │   ├── mod.rs                # RankingProfile and Ranker
│   │   ├── lexical.rs            # shard BM25 normalization
│   │   ├── vector.rs             # distance-to-score normalization
│   │   ├── rrf.rs                # deterministic reciprocal-rank fusion
│   │   ├── features.rs           # recency/trust/graph/usage feature application
│   │   ├── diversity.rs          # deterministic MMR and explicit multi-repository diversity
│   │   ├── rerank.rs             # optional bounded local reranker and exact fallback
│   │   └── explain.rs            # per-result component explanation
│   ├── export/
│   │   ├── mod.rs                # ExportRequest and bounded stream orchestration
│   │   ├── jsonl.rs              # canonical JSONL row/manifest encoding
│   │   ├── parquet.rs            # stable typed Parquet schema encoding
│   │   └── manifest.rs           # counts, hashes, coverage, redaction, watermark
│   ├── live/
│   │   ├── mod.rs                # live query subscription entry point
│   │   ├── delta.rs              # snapshot/delta/progress/gap/resync types
│   │   └── coalesce.rs           # bounded idempotent delta coalescing
│   └── labs/
│       ├── mod.rs                # read-only lab gateway
│       └── query.rs              # Query Lab plan/version/corpus comparison
├── tests/
│   ├── support/mod.rs            # deterministic catalog/shard/change-feed fixtures
│   ├── ast_validation.rs
│   ├── planner_pruning.rs
│   ├── budgets_cancellation.rs
│   ├── cursor_resume.rs
│   ├── partial_coverage.rs
│   ├── lexical_parity.rs
│   ├── search_quality_eval.rs
│   ├── hybrid_ranking.rs
│   ├── graph_time_as_of.rs
│   ├── coordination_proximity.rs
│   ├── export_manifest.rs
│   ├── live_read_model.rs
│   ├── query_lab.rs
│   ├── security_privacy.rs
│   └── v1_differential.rs
└── benches/
    ├── planner.rs
    ├── federated_topk.rs
    ├── timeline.rs
    └── graph.rs
```

Required companion files, owned by other plans/PRs:

```text
crates/tracedecay-domain/src/query/{mod.rs,predicate.rs,scope.rs,text.rs,semantic.rs,relation.rs,time.rs,aggregate.rs,sort.rs}.rs
src/v2_adapters/query_store/{mod.rs,catalog.rs,sqlite_shard.rs,fts.rs,vector.rs,graph.rs,time.rs,coordination.rs}
crates/tracedecay-projectors/src/read_models/{facets.rs,timeline.rs,observatory.rs}.rs
crates/tracedecay-application/src/use_cases/{query.rs,search.rs,graph.rs,timeline.rs,export.rs,subscribe.rs}.rs
crates/tracedecay-api/src/http/{query.rs,search.rs,graph.rs,timeline.rs,exports.rs}.rs
crates/tracedecay-api/src/sse/{mod.rs,resume.rs}.rs
```

The root composition crate owns `src/v2_adapters/query_store/**`; application owns only the use-case/query ports. Query and application never import `rusqlite`, graph files, or another concrete store.

## 6. Canonical AST Consumed from `tracedecay-domain`

PR 4 must land these names before PR 11. `tracedecay-query::ast` re-exports `TraceQueryV1`, `ScopeSelectorV2`, `MessageView`, `TimePredicate`, `AttributePredicate`, `TextPredicate`, `SemanticPredicate`, `TraversalPredicate`, `ProvenancePredicate`, `SensitivityFilter`, `FacetRequest`, `AggregateRequest`, `FieldProjection`, `SortKey`, `PageSize`, `SnapshotMode`, `ExplainMode`, `QueryBudget`, `CursorClaimsV1`, `FrozenSnapshot`, and `VectorWatermark` unchanged.

```rust
pub struct TraceQueryV1 {
    pub query_id: QueryId,
    pub scope: ScopeSelectorV2,
    pub entity_kinds: Vec<EntityKind>,
    pub message_view: Option<MessageView>,
    pub time: Option<TimePredicate>,
    pub attributes: Vec<AttributePredicate>,
    pub text: Option<TextPredicate>,
    pub semantic: Option<SemanticPredicate>,
    pub traversal: Option<TraversalPredicate>,
    pub provenance: Option<ProvenancePredicate>,
    pub sensitivity: SensitivityFilter,
    pub facets: Vec<FacetRequest>,
    pub aggregates: Vec<AggregateRequest>,
    pub projection: FieldProjection,
    pub sort: Vec<SortKey>,
    pub page_size: PageSize,
    pub snapshot: SnapshotMode,
    pub explain: ExplainMode,
    pub budget: QueryBudget,
}
```

Grouping, comparison intervals, relation evidence filters, code predicates, sampling/downsampling, and saved-collection expansion are encoded through registered attributes/predicates and query profiles until a versioned domain schema adds fields; the query crate must not fork `TraceQueryV1` to add them.

Validation limits are explicit constants in `validate.rs`: page size `1..=1000`; graph depth `0..=5`; path alternatives `1..=20`; facet buckets `1..=1000`; projected scalar fields `1..=128`; payload slice `0..=1 MiB` per page; export rows `1..=5_000_000`; export bytes `1..=10 GiB`; timeline raw-event page `1..=10_000`. Application may impose lower limits but cannot raise them.

## 7. Public Query API and Ports

`src/lib.rs` exposes only these families: AST re-exports, request/context, engine/planner, response/coverage/cursor, explain, export, live, errors, and port traits.

```rust
pub struct QueryEngine<C, S, P, R, F> {
    catalog: C,
    shards: S,
    payloads: P,
    representations: R,
    changes: F,
    cursor_codec: CursorCodec,
    limits: EngineLimits,
}

impl<C, S, P, R, F> QueryEngine<C, S, P, R, F>
where
    C: CatalogQueryPort,
    S: ShardQueryPort,
    P: PayloadReadPort,
    R: RepresentationQueryPort,
    F: ChangeFeedPort,
{
    pub async fn execute(
        &self,
        request: QueryRequest,
        context: QueryContext,
    ) -> Result<QueryResponse, QueryError>;

    pub async fn export(
        &self,
        request: ExportRequest,
        context: QueryContext,
    ) -> Result<ExportStream, QueryError>;

    pub async fn subscribe(
        &self,
        request: LiveQueryRequest,
        context: QueryContext,
    ) -> Result<LiveQueryStream, QueryError>;
}

#[derive(Clone, Debug)]
pub struct QueryRequest {
    pub query: TraceQueryV1,
    pub resolution: ScopeResolutionV2,
    pub ranking: RankingProfileRef,
    pub deadline: Instant,
    pub budget: CostBudget,
}

#[derive(Clone)]
pub struct QueryContext {
    pub request_id: QueryId,
    pub access: QueryAccess,
    pub cancellation: Arc<dyn QueryCancellation>,
    pub now: SystemTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryAccess {
    pub profile_id: ProfileId,
    pub allowed_privacy_domains: BTreeSet<PrivacyDomainId>,
    pub allowed_sensitivity: BTreeSet<DataSensitivity>,
    pub payload_access: PayloadAccess,
    pub access_digest: Digest,
}

pub trait QueryCancellation: Send + Sync {
    fn is_cancelled(&self) -> bool;
    fn cancelled(&self) -> BoxFuture<'_, ()>;
}
```

The clock is captured once by application. Planning/execution does not call the ambient clock, which makes cursor expiry, relative time, and tests deterministic.

```rust
pub trait CatalogQueryPort: Send + Sync {
    fn shard_inventory<'a>(
        &'a self,
        scope: &'a ScopeResolutionV2,
    ) -> BoxFuture<'a, Result<Vec<ShardDescriptor>, QueryError>>;
}

pub trait ShardQueryPort: Send + Sync {
    fn capture_watermark<'a>(
        &'a self,
        shard: &'a ShardDescriptor,
    ) -> BoxFuture<'a, Result<ShardWatermark, ShardError>>;

    fn execute_fragment<'a>(
        &'a self,
        request: ShardRequest,
        cancellation: &'a dyn QueryCancellation,
    ) -> BoxFuture<'a, Result<ShardPage, ShardError>>;

    fn explain_fragment<'a>(
        &'a self,
        request: &'a ShardRequest,
    ) -> BoxFuture<'a, Result<ShardExplain, ShardError>>;
}

pub trait PayloadReadPort: Send + Sync {
    fn read_slices<'a>(
        &'a self,
        requests: &'a [AuthorizedPayloadSlice],
        cancellation: &'a dyn QueryCancellation,
    ) -> BoxFuture<'a, Result<Vec<SanitizedPayloadSlice>, PayloadError>>;
}

pub trait RepresentationQueryPort: Send + Sync {
    fn query_vector<'a>(
        &'a self,
        request: &'a SemanticQuery,
        access: &'a QueryAccess,
    ) -> BoxFuture<'a, Result<QueryVector, RepresentationError>>;
}

pub trait ChangeFeedPort: Send + Sync {
    fn subscribe<'a>(
        &'a self,
        request: ChangeFeedRequest,
    ) -> BoxFuture<'a, Result<BoxStream<'a, Result<ShardChange, ChangeFeedError>>, ChangeFeedError>>;
}
```

Errors have stable machine codes: `invalid_query`, `resolution_mismatch`, `resolution_expired`, `budget_exceeded`, `deadline_exceeded`, `cancelled`, cursor mismatch/restart codes, `scope_denied`, `scope_stale`, `payload_denied`, `privacy_receipt_invalid`, `export_limit_exceeded`, `all_shards_unavailable`, and `internal_invariant`. Scope parsing/ambiguity/not-found errors are application-owned. Individual shard failures normally become coverage, not a top-level error.

## 8. Plan, Fragment, Budget, and Cancellation Contracts

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryPlan {
    pub plan_id: QueryPlanId,
    pub fingerprint: QueryFingerprint,
    pub registry_version: RegistryVersion,
    pub ranking: RankingProfileRef,
    pub mode: SnapshotMode,
    pub vector_watermark: VectorWatermark,
    pub shards: Vec<ShardPlan>,
    pub merge: MergePlan,
    pub hydration: HydrationPlan,
    pub estimate: CostEstimate,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShardPlan {
    pub shard_id: ShardId,
    pub kind: ShardKind,
    pub watermark: ShardWatermark,
    pub capabilities: CapabilitySet,
    pub operators: Vec<ShardOperator>,
    pub local_limit: NonZeroU32,
    pub resume: Option<ShardResume>,
    pub estimated_cost: CostEstimate,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CostUnits {
    pub shard_opens: u32,
    pub rows_scanned: u64,
    pub fts_candidates: u64,
    pub vector_candidates: u64,
    pub graph_edges: u64,
    pub hydrated_bytes: u64,
    pub export_rows: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CostBudget {
    pub maximum: CostUnits,
    pub max_wall_time: Duration,
    pub max_peak_bytes: u64,
}
```

Planning rules:

1. Verify application-produced `ScopeResolutionV2` matches `TraceQueryV1.scope`, access digest, catalog generation, canonical selector digest, and freshness policy before catalog inventory. Unresolved/ambiguous/unauthorized roots never enter the engine; stale/unavailable/quarantined selected rows become coverage according to the selector. Query never resolves locators or uses CWD/current project/first Claude CWD/active base checkout/current branch graph fallback.
2. Canonicalize the AST and compute a keyed fingerprint that omits literal plaintext.
3. Filter shards by scope, privacy domain, kind, time range, schema, capability, health, and source watermark.
4. Capture high-watermarks for all selected shards before executing any fragment.
5. Lower only capability-supported predicates. Residual filters execute in the coordinator and are costed.
6. Reject when conservative cost exceeds any caller budget or crate hard limit. Do not “try and truncate” an invalid unbounded query.
7. Execute with at most 32 shard futures and at most four vector-heavy fragments concurrently on the reference configuration.
8. Check cancellation before open, after every shard page, between merge batches, before hydration, every 4,096 graph-edge visits, and every 1 MiB exported.
9. On deadline/cancellation, drop in-flight futures, call the store cancellation hook, and return no cursor for uncommitted merge state.
10. Metrics record fingerprint, operator IDs, counts, durations, coverage, and error codes; never AST literals, payloads, paths classified sensitive, or vector values.

## 9. Stable Cursor and High-Watermark Contract

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CursorV1 {
    version: u16,
    issued_at: UtcMicros,
    expires_at: UtcMicros,
    query_digest: ContentDigest,
    access_digest: ContentDigest,
    schema_version: QuerySchemaVersion,
    ranking: RankingProfileRef,
    index_versions: BTreeMap<ShardId, IndexVersionSet>,
    snapshot: FrozenSnapshot,
    per_shard_positions: BTreeMap<ShardId, ShardCursorPosition>,
    sort_cutoff: Vec<SortValue>,
    last_entity_id: Option<EntityId>,
    emitted_ids_digest: ContentDigest,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct StableSortKey {
    pub components: Vec<SortValue>,
    pub entity_id: EntityRef,
}
```

`CursorV1` is the query crate's private signing/codec representation of domain `CursorClaimsV1`; every field maps one-to-one and schema tests compare canonical encodings before authentication. `StableSortKey` is the in-memory merge key and lowers to `sort_cutoff` plus `last_entity_id` in the claim.

- Encode canonical CBOR, authenticate with per-profile HMAC key, then base64url without padding. A cursor contains no raw query literal, payload, secret alias, or filesystem path.
- Default expiry is 24 hours. An application may request a shorter duration.
- Resume validates MAC, expiry, query fingerprint, access digest, schema, ranking, index generations, retention horizon, and each shard identity before opening a shard.
- Retention crossing a frozen watermark, an incompatible shard replacement, or changed ranking/schema returns a typed restart reason.
- A newly unavailable shard is marked unavailable while other stored positions resume. A shard newly registered after the snapshot is excluded.
- Frozen mode filters every shard at `row.sequence <= captured_watermark.sequence` (or its equivalent immutable generation).
- Equal user-visible sort values are broken by canonical `EntityRef`; ranks never depend on arrival order, SQLite row ID, hash-map iteration, or shard-open order.
- Live delta cursors are distinct from page cursors and carry last per-shard outbox sequence plus duplicate-suppression digest.

## 10. Coverage and Response Contract

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryResponse {
    pub snapshot: QuerySnapshot,
    pub plan_id: QueryPlanId,
    pub rows: Vec<QueryRow>,
    pub edges: Vec<QueryEdge>,
    pub facets: Vec<FacetResult>,
    pub aggregates: Vec<AggregateResult>,
    pub next_cursor: Option<OpaqueCursor>,
    pub truncation: Option<TruncationReason>,
    pub coverage: CoverageReport,
    pub timing: QueryTiming,
    pub explain: Option<QueryExplain>,
    pub level_of_detail: LevelOfDetailReport,
    pub retention_watermark: RetentionWatermark,
    pub message_view: Option<MessageViewReport>,
}

pub struct MessageViewReport {
    pub requested: MessageView,
    pub native_rows_in_scope: u64,
    pub returned_rows: u64,
    pub hidden_copy_rows: u64,
    pub unknown_origin_rows: u64,
    pub classifier: Option<ProducerRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoverageReport {
    pub complete: bool,
    pub searched: Vec<ShardCoverage>,
    pub skipped: Vec<ShardCoverage>,
    pub stale: Vec<ShardCoverage>,
    pub unavailable: Vec<ShardCoverage>,
    pub incompatible: Vec<ShardCoverage>,
    pub locked: Vec<ShardCoverage>,
    pub redacted: Vec<ShardCoverage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TruncationReason {
    PageSize { requested: u16, returned: u16 },
    CostBudget { operator: OperatorId, consumed: CostUnits },
    Deadline,
    PayloadBytes { limit: u64 },
    Sampling { method: SamplingMethod, original_estimate: u64 },
    LevelOfDetail { contract: LevelOfDetail },
}
```

`complete` is true only when no selected shard is stale, unavailable, incompatible, locked, or redacted and no result-level truncation/sampling occurred. `skipped` may still be present for correctly pruned out-of-scope/capability shards and does not make an in-scope result partial. Every `ShardCoverage` includes shard ID/kind, safe project/profile label, requested/captured watermark, schema/capability versions, disposition reason, last successful freshness time, and rows scanned/returned when known.

Unknown denominators serialize as `value: null, state: "unknown", reason`, never numeric zero. Partial aggregate results include source-watermark vectors.

## 11. FTS, Vector, Ranking, Graph, and Time Contracts

### 11.1 FTS

- `TextQuery` distinguishes phrase, terms, prefix, fields, tokenizer profile, language hint, and compatibility profile. It never accepts raw SQL/FTS syntax.
- V1 compatibility fixtures cover quotes, punctuation, parentheses, operators, path separators, CJK, emoji, empty tokens, prefixes, raw-vs-summary provenance, and LIKE fallback inclusion.
- Store adapters return raw BM25, tokenizer/index version, match fields, token/phrase spans, and fallback mode. Query normalizes but preserves raw values in explain output.
- Sensitive, secret-like/quarantined, locked, and reasoning content excluded by projection policy cannot be recovered through fallback search.
- Field boosts and tokenizer versions belong to `RankingProfile`, not HTTP/MCP handlers.

### 11.2 Vector

- `SemanticQuery` requires representation kind, model ID/version constraint, metric, top-k, exact-fallback policy, and permitted sensitivity classes.
- Query vectors come through `RepresentationQueryPort`; consent and model loading occur outside this crate.
- Shards declare `Exact`, `Approximate { algorithm, build_version }`, or `Unavailable`. Exact fallback is mandatory for the versioned eval corpus; production may return named partial semantic coverage when the budget cannot exact-scan.
- Distance normalization is metric-specific and versioned. Mixed model/dimension/normalization results are never merged.
- Vector values and secret-bearing source text never enter logs, cursors, manifests, or explain output.

### 11.3 Ranking

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RankingProfile {
    pub id: RankingProfileId,
    pub version: SemVer,
    pub rrf_k: NonZeroU16,
    pub components: Vec<RankComponent>,
    pub tie_break: TieBreakPolicy,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RankExplanation {
    pub profile: RankingProfileRef,
    pub final_score: FiniteF64,
    pub components: Vec<ComponentScore>,
    pub stable_sort_key: StableSortKey,
}
```

- Initial V2 fusion is reciprocal-rank fusion over lexical and semantic lists, followed by declared finite weights for recency, trust, graph proximity, and usage only when those features are present and authorized.
- Missing features contribute `Absent`, not zero; profiles declare whether to renormalize or preserve fixed weights.
- NaN/infinite values are rejected at shard boundaries.
- Compatibility profile `v1-memory-2026-07` reproduces `FactRetriever` scoring/order for eligible V1 facts. V2 default is separately versioned.
- nDCG@10 must be at least 0.85, recall@20 at least 0.90, and release regression no worse than 0.02 on the versioned intent corpus.

### 11.4 Graph and time

- Neighborhood requires explicit edge kinds, direction, evidence classes, confidence floor, depth `<= 5`, node limit, edge limit, and level of detail.
- Path requires source/target sets, edge kinds, maximum depth/cost, maximum 20 alternatives, cycle policy, and stable ordering by path cost then entity IDs.
- Impact/affected-tests is a named profile over evidence-bearing code/delivery relations, not an unrestricted traversal.
- Git/code result roles are disjoint and explicit: `directly_changed`, `structurally_impacted`, `candidate_test`, and `context_only`. Each row carries the producing edge/path/profile, evidence/confidence, truncation, and source/index watermark; transitive/file-level fan-out cannot increment the direct-change count or render as “modified.”
- Traversal stops at privacy-domain/sensitivity boundaries and reports redacted frontier counts without leaking hidden IDs.
- Time predicates distinguish occurred, ingested, valid, observed, and comparison intervals.
- As-of state uses `valid_from <= t < valid_to` and `observed_from <= knowledge_time < observed_to`; callers must provide both when asking “what was known then.”
- Timeline density uses server-side buckets. Sanitized native/canonical events are never returned unbounded for wide intervals.
- In-memory CSR acceleration is permitted only behind the same bounded operator contract and must produce the same entity/edge set as the store reference implementation.

### 11.5 Agent proximity and coordination

```rust
pub struct NearbyAgentsRequest {
    pub scope: ScopeSelectorV2,
    pub source_agent: EntityRef,
    pub source_session: EntityRef,
    pub at: UtcMicros,
    pub include_same_worktree: bool,
    pub include_parallel_worktrees: bool,
    pub scope_kinds: BTreeSet<CoordinationScopeKind>,
    pub intents: BTreeSet<WorkIntent>,
    pub limit: u16,
}

pub enum ClaimOverlapKind {
    SameWorktree,
    ParallelWorktree,
    SameRef,
    SamePullRequest,
    SameFile,
    SameSymbol,
    SameQueryScope,
    SharedRetrievalAnchor,
}

pub struct ClaimOverlapEvidence {
    pub kind: ClaimOverlapKind,
    pub left_anchor: RetrievalAnchorId,
    pub right_anchor: RetrievalAnchorId,
    pub evidence: EvidenceClass,
    pub observed_at: UtcMicros,
}

pub struct CoordinationCoverage {
    pub watermark: VectorWatermark,
    pub searched_shards: Vec<ShardId>,
    pub stale_shards: Vec<ShardId>,
    pub unavailable_shards: Vec<ShardId>,
    pub redacted_candidates: u64,
    pub truncated: bool,
}

pub struct NearbyAgentHit {
    pub presence: AgentPresenceV1,
    pub claim: WorkClaimV1,
    pub proximity: WorktreeProximity,
    pub overlap: Vec<ClaimOverlapEvidence>,
    pub materiality: FiniteF64,
    pub declared_redundancy: RedundancyMode,
    pub coverage: CoordinationCoverage,
}
```

`limit` is `1..=100`; `scope` is the exact caller selector and cannot be narrowed to the source agent's current project/worktree. Only nonexpired current presence/claims appear by default, while historical replay explicitly selects as-of time. Overlap uses typed canonical scopes and retrieval-anchor digests, never summary text alone. Results distinguish same worktree, parallel worktree, same PR/ref, file, symbol, and query scope; carry evidence/watermark/partial state; and do not claim duplication or authority. Coordination Lab reuses this frozen candidate query through application/policy.

## 12. Export Contract

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportRequest {
    pub query: TraceQueryV1,
    pub format: ExportFormat,
    pub ranking: RankingProfileRef,
    pub max_rows: NonZeroU64,
    pub max_bytes: NonZeroU64,
    pub include_payloads: bool,
    pub redaction: RedactionProfileRef,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ExportFrame {
    ManifestStart(ExportManifestStart),
    Schema(ExportSchema),
    Rows(ExportRowBatch),
    ManifestEnd(ExportManifestEnd),
}
```

- Formats are canonical JSONL and typed Parquet. JSONL orders object keys and uses RFC 3339 UTC timestamps; Parquet schema metadata records domain/query schema versions.
- The engine captures one frozen vector watermark, streams bounded row batches, hashes uncompressed canonical row bytes, and emits final counts only after successful completion.
- Manifest includes export/query/schema/ranking/index/redaction versions; created time; query fingerprint; scope; source watermarks; searched/skipped/stale/unavailable/incompatible/locked/redacted coverage; rows/bytes; payload inclusion; redaction counts/reasons; per-part BLAKE3 hashes; and completeness.
- Human-authored versus provider-protocol messages is an explicit exported field. No text predicate is required for session/message enumeration.
- Provider-exposed reasoning is excluded unless policy, retention, authorization, and explicit request all permit it; exclusion is counted in the redaction report.
- Export sink path containment, private permissions, atomic publication, encryption, and job IDs are application/store responsibilities. Query emits bytes only through `ExportStream`.
- Cancellation or sink failure yields no completed manifest and no published artifact; application removes staged parts.

## 13. SSE Read-Model Contract

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum QueryStreamEvent {
    Snapshot { sequence: StreamSequence, response: QueryResponse },
    Delta { sequence: StreamSequence, watermark: VectorWatermark, changes: Vec<RowDelta> },
    Progress { sequence: StreamSequence, progress: ProjectionProgress },
    Gap { sequence: StreamSequence, expected: VectorWatermark, available_from: VectorWatermark },
    ResyncRequired { sequence: StreamSequence, reason: ResyncReason },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RowDelta {
    Upsert { row: QueryRow, stable_sort_key: StableSortKey },
    Remove { entity: EntityRef, cause: RemovalCause },
    CoverageChanged(CoverageReport),
}
```

- Subscription starts with a frozen snapshot, then consumes per-shard ordered outbox sequences above that snapshot.
- Duplicate/out-of-order changes are idempotently suppressed by `(shard_id, sequence, entity_id, projector_version)`.
- Coalescing may replace repeated upserts for one entity but cannot reorder across a remove/upsert boundary or hide coverage changes.
- Finite replay retention and missing sequences produce `Gap` then `ResyncRequired`; the browser must fetch a new snapshot.
- Query sets bounded channel capacity and returns a slow-consumer error when coalescing cannot preserve correctness. API maps this to explicit SSE termination/reconnect behavior.
- API owns heartbeat events and `Last-Event-ID`; it encodes `StreamSequence` without exposing query literals.

## 14. Query and Cross-Domain Lab Contracts

`labs/query.rs` implements Query Lab directly:

- Inputs: immutable `TraceQueryV1`, query/index/planner/ranking versions, access fixture, vector watermark, budget, and recorded shard fixture manifest.
- Outputs: canonical AST, validation, cost estimate, selected/pruned shards, pushed/residual filters, FTS/vector/graph/time operators, merge/rank explanations, cursor state with secrets removed, timing, and coverage.
- Comparison: planner/index/ranking A/B over the same input/watermark; output changed entities/order/scores/facets/coverage/cost and stable digest.
- Request export: equivalent CLI args, MCP arguments, HTTP JSON, and raw AST generated from one canonical query.
- Read-only proof: fixture ports panic on write; no usage/retrieval/ranking counters exist in query ports.

Other labs consume query contracts without moving policy into this crate:

| Lab | Query crate contribution | Owning evaluator/orchestrator |
|---|---|---|
| Hint | Candidate memory/tool/skill rows, evidence, score explanations, immutable watermark | `tracedecay-policy` + application |
| Retrieval | FTS/vector/entity/recent candidate sets, exclusions, rank features, no counter writes | `tracedecay-policy` |
| Ingest | Query projected observations/events/rows by manifest and compare output watermarks | `tracedecay-capture`/projectors + application |
| Correlation | Bounded candidate/evidence relation query and graph/time explanation | `tracedecay-policy` |
| Coordination | Frozen nearby-presence/claim candidates, material-overlap evidence, TTL/status, worktree proximity, declared redundancy, and coverage | `tracedecay-policy` + application |
| Scheduler | As-of activity/run/lock/config read model with source watermarks | `tracedecay-policy` |
| Memory | Fact/version/trust/conflict/retrieval/deletion-impact read models | `tracedecay-policy` |
| Policy Diff | Enumerate saved corpus inputs and hydrate recorded/executed decisions | `tracedecay-policy` |

All labs use domain `ReplayMode::{ExactDeterministic, RecordedResult, CurrentBestEffort}`. Query Lab exactness is stated independently as `ExactQueryReplay` only when all shard fixtures, index generations, ranking profile, planner version, schema, and watermarks are present; otherwise it returns recorded inspection or current best-effort with named substitutions.

## 15. Consumes and Produces

| Boundary | Consumes | Produces |
|---|---|---|
| `tracedecay-domain` | IDs, `TraceQueryV1`, entity/relation/evidence/time/sensitivity/schema types | No domain writes; canonical query fingerprints and read-only result references |
| Store-backed query ports | Catalog inventory, shard capabilities/statistics/health, captured watermarks, bounded fragment pages, payload slices, representation results | Typed `ShardRequest`, resume positions, cancellation, safe explain requests; no `tracedecay-store` import |
| Projected read models through query ports | Facets, aggregates, timeline density, search docs, rank features, outbox deltas and source watermarks | Read-model requirements and version/capability contracts; no `tracedecay-projectors` import |
| Policy-selected domain refs supplied by application | Ranking profile refs and immutable policy candidate requirements | Candidate sets, rank explanations, query snapshots, no feedback mutation and no `tracedecay-policy` import |
| `tracedecay-application` | Authorized request, clock, deadlines, budgets, saved-query content | `QueryResponse`, `ExportStream`, `LiveQueryStream`, typed errors/restart reasons |
| `tracedecay-api`/CLI/MCP | No transport types imported | Stable schemas mapped without semantic changes |
| Dashboard/Explorer/Loom/labs | No frontend state imported | Coverage, explain, facets, LOD, cursors, deterministic rows/edges/deltas |

Dependency direction remains `tracedecay-domain <- tracedecay-query <- tracedecay-application <- adapters`; store/projector implementations satisfy query-owned ports without causing domain/query to import concrete persistence.

## 16. PR and TDD Execution Plan

Each PR is independently reviewable. Every red test must fail for the named reason before production changes; if it passes, repair the fixture or assertion before proceeding. Commands run from the repository root with the checkout-local `target/` and no `CARGO_TARGET_DIR` or `TRACEDECAY_DATA_DIR` override unless Cargo reports target-lock contention.

Program numbering is authoritative: PR 12 is implemented in dependency order as internal planner slices 12.1–12.3, master-plan PR 12A first end-to-end vertical slice, PR 12B federated global routing, then PR 12C privacy containment. Master PR 13 and 14 series use 13A–13C and 14A–14C exactly. PR 15 is reviewed as 15A–15B without changing its master-program ownership.

### PR 11A: Domain AST, parser, canonicalization, and validation

**Files:** domain query files listed in Section 5; query `Cargo.toml`, `src/{lib,ast,error,request,validate}.rs`; `tests/ast_validation.rs`.

- [ ] Add AST cases `round_trips_every_scope`, `multi_repo_worktree_selector_is_order_stable`, `empty_explicit_scope_is_not_current_project`, `ambiguity_policy_is_explicit`, plus bounds/fingerprint/message-view cases. Assertions require stable errors/canonical bytes and no literal in safe fingerprints.
- [ ] Run `cargo test -p tracedecay-query --test ast_validation -- --nocapture`. Expected: compilation fails because `tracedecay_query::validate` and the domain query types do not exist.
- [ ] Add the AST and public definitions from Sections 6–7, exhaustive validation, canonical ordering, and keyed BLAKE3 fingerprinting. `ast.rs` must re-export domain types rather than duplicate them.
- [ ] Re-run the command. Expected: every named AST/scope case passes; no ignored tests.
- [ ] Run `cargo test -p tracedecay-domain query -- --nocapture`. Expected: all domain query serialization/schema-registry tests pass.
- [ ] Commit `feat(query): add bounded TraceQueryV1 contracts`.

### PR 11B: Cost model and safe explain plan

**Files:** `src/{cost,explain,ports}.rs`, `src/operators/*.rs`, `tests/budgets_cancellation.rs` cost cases.

- [ ] Add tests `rejects_before_opening_shards_when_estimate_exceeds_budget`, `unknown_statistics_use_conservative_upper_bound`, and `explain_redacts_literals_and_vectors`. The fake shard open counter must stay zero on rejection; serialized explain JSON must omit a supplied secret literal and vector values.
- [ ] Run `cargo test -p tracedecay-query --test budgets_cancellation cost -- --nocapture`. Expected: tests fail because no estimator/explain implementation exists.
- [ ] Implement `CostUnits`, `CostBudget`, operator estimates, hard limits, safe operator fingerprints, and `QueryExplain` views. Unknown cardinality uses shard-size upper bounds, never zero.
- [ ] Re-run the command. Expected: 3 tests pass and fake shard opens remain zero.
- [ ] Commit `feat(query): enforce query costs and safe explain plans`.

### PR 12.1: Resolved-scope validation, shard pruning, pushdown, and captured watermarks

**Files:** `src/planner/*.rs`, `tests/planner_pruning.rs`, `tests/support/mod.rs`.

- [ ] Add fixture catalog with activity, multiple repositories/projects/checkouts/worktrees/refs/generations, locked/stale/quarantined/polluted/incompatible/wrong-range/capability shards. Add tests `prunes_without_opening_irrelevant_shards`, `multi_repo_opens_all_exact_shards`, `sessions_project_key_never_overrides_selector`, `claude_first_cwd_is_candidate_only`, `active_base_checkout_never_supplants_pr_worktree_generation`, `ignored_dependency_hint_cannot_drop_scope`, `stale_registry_store_is_named_not_selected`, `captures_all_selected_watermarks_before_execution`, `keeps_partial_coverage`, and `leaves_unsupported_predicate_as_residual`.
- [ ] Run `cargo test -p tracedecay-query --test planner_pruning -- --nocapture`. Expected: compilation fails because `QueryPlanner` and `ShardPlan` do not exist.
- [ ] Implement application-resolution digest/generation/access validation plus the planner sequence in Section 8, deterministic shard ordering by `ShardId`, typed pushdown, residual costing, and `QueryPlan` serialization. Query does not perform locator matching or emit the public ambiguity/not-found problem.
- [ ] Re-run the command. Expected: all scope/pruning tests pass; every exact selected shard opens, irrelevant/stale polluted shards do not, and captured watermarks precede execution.
- [ ] Commit `feat(query): plan and prune federated shards`.

### PR 12.2: Coordinator, cancellation, partial coverage, and deterministic merge

**Files:** `src/execute/*.rs`, `src/coverage.rs`, remaining `tests/budgets_cancellation.rs`, `tests/partial_coverage.rs`.

- [ ] Add tests `limits_concurrent_shard_opens_to_32`, `propagates_cancel_to_store_and_stops_merge`, `returns_healthy_rows_when_one_shard_fails`, `all_unavailable_is_typed_error`, and `equal_scores_order_by_entity_id_not_arrival`.
- [ ] Run `cargo test -p tracedecay-query --test budgets_cancellation --test partial_coverage -- --nocapture`. Expected: tests fail because coordinator/coverage types are absent.
- [ ] Implement bounded futures, cancellation checkpoints, shard-error classification, merge batches, `CoverageReport`, unknown-denominator semantics, and deterministic tie breaks.
- [ ] Re-run the command. Expected: all tests pass; fixture peak concurrency equals 32; cancelled run produces no cursor.
- [ ] Commit `feat(query): coordinate shards with explicit partial coverage`.

### PR 12.3: Authenticated stable cursors and frozen resume

**Files:** `src/cursor.rs`, `src/execute/resume.rs`, `tests/cursor_resume.rs`.

- [ ] Add tests `round_trips_cursor_without_plaintext`, `rejects_tampering`, `rejects_query_or_access_mismatch`, `expires_after_24_hours`, `invalidates_replaced_index_and_retention_crossing`, `resume_preserves_missing_shard_position`, and `live_ingest_does_not_change_frozen_pages`.
- [ ] Run `cargo test -p tracedecay-query --test cursor_resume -- --nocapture`. Expected: compilation fails because `CursorCodec` is absent.
- [ ] Implement canonical CBOR + HMAC-SHA256 + base64url codec and all validations in Section 9. Fixture clocks and keys are explicit bytes; no ambient time or global key lookup.
- [ ] Re-run the command. Expected: 7 tests pass; concatenated cursor bytes do not contain the query literal or sensitive path; page union has no duplicates/gaps under interleaved ingest.
- [ ] Commit `feat(query): add stable snapshot cursors`.

### PR 12A: First end-to-end V2 vertical slice

**Files:** query/store/projector/application/API integration files from Section 5; `tests/v1_differential.rs`; the program's redacted Codex/TraceDecay fixture manifest.

- [ ] Add one copied/redacted Codex session with tools/subagents, one unavailable project shard, known watermarks, and expected table/timeline/inspector rows. Add differential test `codex_session_vertical_slice_matches_manifest`.
- [ ] Run `cargo test -p tracedecay-query --test v1_differential codex_session_vertical_slice_matches_manifest -- --exact --nocapture`. Expected: test fails because store ports do not yet expose the V2 slice.
- [ ] Wire capture/backfill, identity/evidence, session/tool/subagent projectors, store adapters, minimal application/HTTP query, and prototype read model. Do not add transport logic to this crate.
- [ ] Re-run the command and the slice's HTTP contract test. Expected: exact row IDs/order/watermarks/coverage/export hash match the manifest; unavailable shard is named.
- [ ] Commit each owning-crate slice separately, ending with `feat(query): prove first federated vertical slice`.

### PR 12B: Federated scope planning and globally routed retrieval

**Files:** extend `src/planner/{scope,shards,hydrate}.rs`, `src/execute/{coordinator,resume}.rs`, `src/cursor.rs`, `src/coverage.rs`, `tests/{planner_pruning,cursor_resume,partial_coverage}.rs`; add globally routed retrieval fixtures shared with the scope plan.

- [ ] Add failing tests `search_hit_routes_to_exact_cross_project_turn`, `adjacent_context_needs_no_cwd_or_store_switch`, `cursor_binds_catalog_scope_set_generation`, `saved_system_opens_exact_member_generations`, `all_reports_locked_corrupt_migrating_stale_unauthorized_incompatible`, `globally_routed_ref_never_uses_current_project`, and `related_scope_is_proposed_not_silently_added`.
- [ ] Bind the full `ScopeSelectorV2`, catalog/saved-set generation, exact repository/checkout/worktree/ref/snapshot/graph-generation tuple, per-shard snapshots/watermarks, authorization digest, and partial/stale policy into `QueryPlan` and `CursorClaimsV1`.
- [ ] Implement opaque globally routable entity/retrieval refs so a result can hydrate exact session/message/Turn/entity, adjacent context, source observation, and export row through query-owned ports without exposing a store path or requiring caller-side project switching.
- [ ] Preserve per-domain capability: unavailable code graph cannot suppress healthy profile activity, memory, Git, automation, or catalog results. Missing/stale/incompatible tuple members remain coverage and cursor state; no alternate current/base generation is opened.
- [ ] Run `cargo test -p tracedecay-query --test planner_pruning --test cursor_resume --test partial_coverage federated_scope`; expected: one-project, saved-system, and explicit-All fixtures pass with deterministic results and complete coverage dispositions.
- [ ] Run the shared Rspack/Rsbuild/React Router and search-to-exact-LCM conformance fixtures; expected: result -> exact object -> context/export succeeds in one request chain.
- [ ] Commit `feat(query): route federated scopes and retrieval refs`.

### PR 12C: Privacy-aware query and global containment

**Ordering:** execute after Plan 18 PR 4B domain taint contracts, store PR 6B, capture PR 7A, projector PR 10A, and query PR 12B. This is the query-owned slice of [`18-secret-detection-redaction-and-private-data-safety.md`](18-secret-detection-redaction-and-private-data-safety.md).

**Files:** extend `src/{privacy,coverage,cursor,explain}.rs`, all search/graph/aggregate/hydration/export/cache operators, application query-store adapters, and `tests/{security_privacy,partial_coverage,cursor_resume,export_manifest}.rs`; add synthetic forbidden-sink canaries only.

- [ ] Add failing tests `unsafe_projection_never_enters_candidate_pool`, `incomplete_or_revoked_receipt_blocks_hydration`, `exact_load_cannot_bypass_redaction`, `graph_expansion_stops_at_redacted_frontier`, `aggregate_threshold_prevents_existence_leak`, `rank_explain_contains_no_candidate_or_fingerprint`, `cursor_and_cache_contain_no_secret_equality`, `export_requires_export_eligible_text`, `finding_invalidates_descendant_cache_and_generation`, and `one_unsafe_shard_cannot_leak_through_all_scope`.
- [ ] Require sink-eligible result/hydration types, receipt/descendant validation, authorization, safe markers, redacted/blocked/unknown coverage, minimum aggregate thresholds, and generation/cache invalidation. No query operator scans, redacts, stores a candidate fingerprint, or falls back from an unsafe representation to raw source text.
- [ ] Run the complete Plan 18 sink-canary matrix through lexical/vector/graph/time/aggregate/exact-load/cursor/cache/export paths. Expected: zero plaintext or candidate digest bytes; every blocked descendant is visible as coverage without hidden-ID leakage.
- [ ] Run `cargo test -p tracedecay-query --test security_privacy --test partial_coverage --test cursor_resume --test export_manifest`; expected: all containment and ordinary query cases pass.
- [ ] Commit `feat(query): enforce privacy containment across federated reads`.

### PR 13A: Time-safe search evaluation corpus and baseline

**Files:** `tests/search_quality_eval.rs`, versioned redacted corpus/qrel/pool/split manifests owned by the search-quality plan, benchmark/report adapters, and CI aggregate thresholds; no production query algorithm changes.

- [ ] Build private real-prompt-derived and synthetic hard-case pools with chronological cutoff, train/dev/test and cross-project/provider/time holdouts, origin/audience/kind labels, hidden representative membership, exact technical identifiers, typo/alias cases, and secret/reasoning exclusions.
- [ ] Require blinded labels, adjudication, per-intent query IDs, agreement metrics, pool depth/source, candidate-generation recall, and immutable corpus/query/qrel digests. Copied workflows and representative prompt clusters cannot count as independent judgments.
- [ ] Run the current V1 and empty/new V2 baselines; record Recall@20, nDCG@10, MRR, exact-identifier success, zero-result rate, latency, memory, per-intent/per-provider/per-project slices, confidence intervals, and failure examples without private literal leakage.
- [ ] Add leakage tests proving no post-cutoff data, same representative cluster, or test qrel informs training/tuning; missing judgments are unjudged, not negative.
- [ ] Run `cargo test -p tracedecay-query --test search_quality_eval baseline`; expected: manifest/label/leakage contracts pass and the baseline artifact is reproducible from pinned inputs.
- [ ] Commit `test(query): freeze time-safe retrieval evaluation`.

### PR 13B: Precision-first lexical search, facets, and V1 parity

**Files:** `src/operators/{fts,aggregate}.rs`, `src/rank/lexical.rs`, application-owned query/store FTS adapter files, `tests/{lexical_parity,v1_differential}.rs`.

- [ ] Port redacted V1 cases for phrases, punctuation, parentheses, FTS operators as text, prefixes, paths, CJK, emoji, raw/summary source, providers, Git scopes, LIKE fallback, facets, equal-rank ordering, and PR #410 native/representative/human/direct-user/subagent/tool-result/protocol views. Name the corpus/tokenizer/ranking/origin-classifier versions in expected output.
- [ ] Run `cargo test -p tracedecay-query --test lexical_parity --test v1_differential lexical -- --nocapture`. Expected: inclusion/order assertions fail before FTS lowering and normalization exist.
- [ ] Implement typed FTS lowering, tokenizer profile, safe fallback, BM25 normalization, field boosts, facets, match spans, and explanations. Preserve excluded sensitivity classes in every fallback path.
- [ ] Re-run the command. Expected: all inclusion sets match; every order difference is either zero or listed in the checked-in parity manifest with component scores; every message view reports native/returned/hidden-copy/unknown-origin counts and preserves raw-row locators.
- [ ] Run `cargo test --test session_suite lcm_query -- --nocapture`. Expected: V1 tests remain green.
- [ ] Add exact/phrase/fielded BM25, origin/audience/kind fields, query/tool self-echo penalty, representative clusters, hidden counts, and component explanations. Release gates use the untouched PR 13A test split; no aggregate gain may hide exact-identifier or high-severity intent regression.
- [ ] Commit `feat(query): unify lexical search and facets`.

### PR 13C: Bounded fuzzy recall and result diversity

**Files:** create `src/operators/fuzzy.rs`, `src/rank/diversity.rs`; extend `tests/{lexical_parity,search_quality_eval}.rs` and `benches/federated_topk.rs`.

- [ ] Add failing tests for character typo distance bounds, token-aware aliases, quoted technical literals, Unicode/CJK/emoji, path/symbol exact priority, fuzzy suppression above exact/token threshold, MMR session/project/provider diversity, and deterministic tie/order.
- [ ] Implement bounded character fuzzy/alias candidate generation only after exact/token channels, with explicit channel/raw score and cap. Fuzzy cannot rewrite quoted/exact technical identifiers or cross privacy domains.
- [ ] Implement versioned MMR diversity over bounded candidates; expose relevance/diversity contributions and preserve a minimum per-repository share for explicit multi-repo scope without forcing irrelevant results.
- [ ] Run `cargo test -p tracedecay-query --test lexical_parity --test search_quality_eval fuzzy_diversity`; expected: exact parity remains, recall/diversity gates pass, and no protected content crosses a domain.
- [ ] Benchmark enabled/disabled fuzzy and MMR at current/10x corpora; record candidate amplification, p50/p95, allocations, RSS, and quality delta.
- [ ] Commit `feat(query): add bounded fuzzy recall and diversity`.

### PR 14A: Representation candidates and privacy/resource benchmark

**Files:** `src/operators/vector.rs`, representation manifest/port contracts, `tests/{hybrid_ranking,security_privacy,search_quality_eval}.rs`, `benches/federated_topk.rs`.

- [ ] Compare lexical-only against privacy-domain local dense and learned-sparse candidate generation on the untouched PR 13A test split. Pin model/tokenizer/dimension/metric/normalization/index builder/hardware/runtime manifests and separate cold/warm measurements.
- [ ] Add tests `rejects_mixed_representation_dimensions`, `exact_fallback_matches_reference_topk`, `secret_fixture_never_reaches_vector_port`, `representation_never_crosses_privacy_or_key_domain`, `missing_model_is_explicit_coverage`, and `disabled_profile_never_opens_vector_port`.
- [ ] Measure Recall@20/nDCG@10/MRR, per-intent regressions, build/update time, index bytes, p50/p95, peak RSS/VRAM, energy/runtime when available, and deterministic exact fallback. Both candidate types remain disabled until quality/privacy/resource gates pass.
- [ ] Run `cargo test -p tracedecay-query --test hybrid_ranking --test security_privacy --test search_quality_eval representation`; expected: privacy/fallback/manifests pass whether optional representation profiles are accepted or rejected.
- [ ] Commit `feat(query): add gated representation candidates` only for profiles that pass; otherwise commit the benchmark and explicit disabled disposition without dormant production routing.

### PR 14B: Hybrid fusion, bounded graph expansion, and hard negatives

**Files:** `src/operators/vector.rs`, `src/rank/{mod,vector,rrf,features,explain}.rs`, `tests/{hybrid_ranking,security_privacy}.rs`, `benches/federated_topk.rs`.

- [ ] Add tests `rrf_is_deterministic_across_shard_arrival`, `rejects_mixed_representation_dimensions`, `exact_fallback_matches_reference_topk`, `missing_features_are_absent_not_zero`, `explain_reconstructs_final_score`, and `secret_fixture_never_reaches_vector_port`.
- [ ] Run `cargo test -p tracedecay-query --test hybrid_ranking --test security_privacy -- --nocapture`. Expected: tests fail because vector/rank modules are absent.
- [ ] Implement representation constraints, metric normalization, exact/approximate capability reporting, RRF, finite feature weights, stable ties, and explanation arithmetic.
- [ ] Re-run the command. Expected: all tests pass; reconstructed scores match within `1e-9`; secret port call count is zero.
- [ ] Run `cargo bench -p tracedecay-query --bench federated_topk -- --save-baseline pr14`. Expected: report current and 10x corpus N, watermark, candidates, p50/p95, allocations, peak RSS; current-N top-k p95 is at most 800 ms.
- [ ] Add bounded typed graph expansion, hard-negative mining from labeled false positives, cross-project/provider/time holdouts, and lexical/vector/graph/recency/trust/usage per-component ablations. Graph expansion is off unless requested/profiled and never escapes scope/privacy/depth/node budgets.
- [ ] Commit `feat(query): add explainable hybrid fusion`.

### PR 14C: Optional bounded reranking

**Files:** create `src/rank/rerank.rs`; extend `tests/{hybrid_ranking,search_quality_eval,security_privacy}.rs` and `benches/federated_topk.rs`.

- [ ] Compare no rerank, late-interaction, and local cross-encoder rerank on identical bounded candidate pools. Pin executable/model/tokenizer/runtime/device/batch/determinism manifests and retain lexical fallback.
- [ ] Add failing tests for candidate-pool cap, deadline/cancellation, model unavailable/OOM, deterministic stable ties, input redaction, domain isolation, explanation provenance, and fallback preserving the pre-rerank list exactly.
- [ ] Measure cold/warm p50/p95, throughput, peak RSS/VRAM, build/download size, and quality with confidence intervals plus per-intent regressions. A reranker is disabled unless its lower-bound quality gain exceeds the declared minimum without violating latency/resource/privacy gates.
- [ ] Run `cargo test -p tracedecay-query --test hybrid_ranking --test search_quality_eval --test security_privacy rerank`; expected: every accepted or disabled disposition is fixture-locked and fallback is byte-stable.
- [ ] Commit `feat(query): add gated local reranking` only when gates pass; otherwise preserve the benchmark/disabled decision outside production routing.

### PR 15A: Graph, impact, timeline, coordination, and bitemporal/as-of operators

**Files:** `src/operators/{graph,time,coordination}.rs`, application-owned query/store graph/time/coordination adapters, `tests/{graph_time_as_of,coordination_proximity}.rs`, `benches/{timeline,graph}.rs`.

- [ ] Add graph/time tests plus `nearby_agents_caps_at_100`, `expired_claim_is_historical_only`, `parallel_worktree_overlap_is_evidenced`, and `planned_redundancy_is_not_duplicate`. Freeze parent prefix `019f4906`, PR #359 child agents `agent-ac3ce9b1ebf998cfb`, `agent-a245d2442cefc621d`, `agent-a96d21dc6391ceba8`, `agent-a6661fd133491631c`, and Cursor session `ebc96a27-b046-4c88-865f-b38d76da9d2d` in the coordination fixture manifest.
- [ ] Run `cargo test -p tracedecay-query --test graph_time_as_of --test coordination_proximity -- --nocapture`. Expected: tests fail because graph/time/coordination lowering is absent.
- [ ] Implement Sections 11.4–11.5, reference store traversal, optional CSR adapter, stable paths, density buckets, bounded claim-overlap queries, and evidence/provenance hydration.
- [ ] Re-run the command. Expected: graph/time and coordination tests pass with deterministic result/overlap order and byte-identical store/CSR entity/edge sets.
- [ ] Run both Criterion benches. Expected: current neighborhood p95 <=100 ms; 10x bounded two-hop p95 <=500 ms; timeline first page <=200 ms current and <=700 ms 10x; output records corpus/watermark/reference machine.
- [ ] Commit `feat(query): add graph and bitemporal operators`.

### PR 15B: Deterministic export and live read models

**Files:** `src/export/*.rs`, `src/live/*.rs`, `tests/{export_manifest,live_read_model,security_privacy}.rs`.

- [ ] Add tests `jsonl_and_parquet_have_equivalent_rows`, `manifest_hashes_complete_frozen_export`, `cancelled_export_has_no_end_manifest`, `reasoning_is_excluded_and_counted`, `snapshot_precedes_deltas`, `dedupes_replayed_delta`, `gap_requires_resync`, and `slow_consumer_terminates_without_silent_loss`.
- [ ] Run `cargo test -p tracedecay-query --test export_manifest --test live_read_model --test security_privacy -- --nocapture`. Expected: tests fail because export/live modules are absent.
- [ ] Implement Sections 12–13 with bounded batches/channels and canonical encoders. Use fixture sinks/change feeds; filesystem and SSE bytes remain outside this crate.
- [ ] Re-run the command. Expected: all tests pass; JSONL/Parquet logical row digests match; reasoning text is absent; gap sequence is explicit.
- [ ] Commit `feat(query): stream complete exports and live deltas`.

### PR 16: Aggregate projections and Query Lab

**Files:** `src/labs/*.rs`, projector read-model files, `tests/query_lab.rs`, `benches/planner.rs`.

- [ ] Add tests `all_scope_rollups_preserve_source_watermarks`, `unknown_denominator_is_null`, `lab_compare_reports_rank_and_plan_changes`, `lab_emits_equivalent_transport_requests`, and `lab_ports_cannot_mutate`.
- [ ] Run `cargo test -p tracedecay-query --test query_lab -- --nocapture`. Expected: tests fail because lab/rollup ports do not exist.
- [ ] Add project/day/kind/provider/model/tool/hint/automation/health/cost read-model capabilities and the Query Lab contract from Section 14. Mutation methods must not appear on any lab trait.
- [ ] Re-run the command. Expected: all tests pass; A/B digest is stable; unknown denominator serializes as null plus reason.
- [ ] Run `cargo bench -p tracedecay-query --bench planner -- --save-baseline pr16`. Expected: planner avoids irrelevant shard opens and reports current/10x p95 plus pruning ratio.
- [ ] Commit `feat(query): add aggregate read models and Query Lab`.

### PR 24C/24E: Application, API/SSE, CLI, MCP, and dashboard adapters

**Files:** application/API adapter files listed in Section 5; existing CLI/MCP handlers; generated TypeScript client; adapter parity tests.

- [ ] Add contract tests that submit one canonical query through in-process application, CLI JSON, MCP JSON, HTTP JSON, export, and dashboard client and compare rows/order/facets/coverage/watermarks/restart codes.
- [ ] Add SSE tests for snapshot, reconnect, duplicate/out-of-order delta, gap/resync, stale-but-visible, partial/offline, backpressure, and authorization filtering.
- [ ] Run the focused application/API/adapter suites. Expected: tests fail while adapters still call V1 query/store functions directly.
- [ ] Move one domain adapter per reviewable PR to application/query contracts; retain V1 flag/schema/render compatibility. Generated clients are drift-checked.
- [ ] Re-run focused suites after each adapter. Expected: semantic JSON matches; only renderer whitespace may differ; no SQL/ranking/policy imports remain in handlers.
- [ ] Commit per domain using `refactor(<adapter>): route <domain> through V2 query services`.

## 17. Evaluation, Performance, Privacy, and Security Gates

- Differential corpus: exact V1/V2 inclusion for compatibility profiles; every score/order divergence has query, corpus, old/new versions, feature explanation, and disposition.
- Pagination property test: arbitrary equal scores, shard completion order, page sizes, and interleaved live ingest produce the exact frozen reference set once, in stable order.
- Fault matrix: absent, corrupt, locked, stale, incompatible, replaced, retention-crossed, and mid-page-failing shards return required coverage/restart behavior.
- Budget matrix: page, graph, path, facet, projection, hydration, timeline, export, wall-time, RSS, and shard-concurrency limits reject or truncate only with typed reasons.
- Performance: current-scale FTS p95 <=150 ms; current registry-N top-k p95 <=800 ms without irrelevant opens; 10x hot facets <=400 ms; 10x text <=750 ms; peak query RSS <=1.5 GiB; at most 32 concurrently open shards.
- Ranking: nDCG@10 >=0.85; recall@20 >=0.90; regression <=0.02; exact-fallback and approximate recall are reported separately.
- Privacy: secret corpus yields zero FTS/vector/fact/export/log/cursor hits; locked stores expose metadata coverage only; redacted graph frontiers leak no hidden IDs/counts below approved aggregate thresholds.
- Security: cursor tamper/fuzz tests, query parser fuzzing, decompression/size limits, malicious field/path strings, export schema injection, NaN/inf scores, and access-digest mismatch all fail closed.
- Compatibility: current CLI/MCP/HTTP/dashboard/export results share typed semantic fixtures. V1 tools exist only in internal differential/shadow harnesses; stale live clients and retired names fail protocol/catalog checks.
- Quality: every new production file targets <=800 lines; `cargo fmt --check`, `cargo clippy -p tracedecay-query --all-targets -- -D warnings`, and `cargo test -p tracedecay-query` pass.

## 18. Cutover and Rollback

1. Land query contracts behind `v2_query_shadow`; no default behavior changes.
2. Shadow canonical V1 searches with literal-safe fingerprints and compare inclusion/order/coverage/latency. Never run shadow exports or payload hydration without authorization.
3. Gate each domain on zero unexplained parity gaps, privacy corpus pass, target latency/RSS, and projection lag below two seconds for 24 hours.
4. Cut over product reads by bounded context: sessions, graph, knowledge, policy/automation, accounting, then All/product shell. Each receipt records V1/V2 source watermarks, schema/ranking/index versions, feature flag, and rollback command.
5. Rollback flips the domain read flag to V1 and preserves V2 shards/cursors for diagnosis. V2 cursors return `cursor_backend_rolled_back` with restart guidance; they are never interpreted by V1.
6. Keep V1 stores and differential query code/tests through the data rollback window, but do not expose V1 adapters to live clients after cutover. Exact protocol/catalog mismatch returns restart/update/current-capability guidance.
7. Archive export manifests, parity reports, benchmark records, cursor-key rotation receipt, and migration receipts before retirement.

## 19. Final Verification

- [ ] Run `cargo fmt --check`. Expected: exit 0.
- [ ] Run `cargo clippy -p tracedecay-domain -p tracedecay-query --all-targets -- -D warnings`. Expected: exit 0, no warnings.
- [ ] Run `cargo test -p tracedecay-query --all-features`. Expected: all unit/integration/property tests pass, none ignored.
- [ ] Run the V1 session, memory, graph, MCP, CLI, dashboard, and storage suites named in Section 4. Expected: all compatibility tests pass.
- [ ] Run all four query benchmarks on the recorded reference machine at current and 10x corpora. Expected: every Section 17 gate passes and output includes corpus manifest, N, watermarks, versions, p50/p95, allocations, RSS, and pruning ratio.
- [ ] Run `rg -n 'rusqlite|libsql|axum|rmcp|clap|dashboard' crates/tracedecay-query/src`. Expected: no matches.
- [ ] Run `rg -n 'TB[D]|TO[D]O|\bimplement lat[e]r\b|\bfill i[n]\b|\bappropriate erro[r]\b|\bsimilar to Tas[k]\b' docs/superpowers/plans/tracedecay-v2/05-query-crate.md`. Expected: no matches.
- [ ] Inspect the final dependency graph. Expected: no `tracedecay-query -> tracedecay-store/projectors/application/api/root` edge; adapters depend inward through application contracts.
- [ ] Record parity, privacy, fault, benchmark, and rollback-drill artifacts in the PR 34/35 manifests before enabling V2 query by default.

## 20. Definition of Done

- The exact module tree, public AST/ports/results/cursor/coverage/export/live/lab contracts, consumes/produces boundaries, and PR/TDD sequence exist with no storage/transport/application dependency.
- One canonical `TraceQueryV1` drives All/profile/project/ref/session/agent/time/text/semantic/graph/aggregate/message-view queries with bounded cost, cancellation, deterministic merge, and explicit partial coverage.
- Multi-repo/project/checkout/worktree/ref/snapshot/generation scope is preserved end-to-end; graph queries open only the resolved tuple and surface ambiguity/stale/quarantine coverage instead of using current project/base checkout/current graph.
- PR 12B globally routable refs prove cross-project search hit -> exact object -> adjacent context/source/export without CWD/store switching, and cursors bind scope-set/catalog generations plus every partial disposition.
- PR 12C proves every result/hydration/search/graph/aggregate/cursor/cache/export path consumes the one domain sink-eligibility contract; query never rescans/redacts content or bypasses a missing/incomplete/revoked receipt.
- PR 13A–13C and 14A–14C use time-safe held-out judgments, exact lexical priority, bounded fuzzy/diversity, privacy-domain representation isolation, per-component ablations, deterministic fallback, and accept/disable gates for optional hybrid/rerank features.
- Nearby-agent queries are bounded to 100, require the same explicit scope contract, expose typed overlap/TTL/redundancy/coverage evidence, and never infer duplicate work or coordination authority.
- Frozen cursors bind query/access/schema/ranking/index/watermark/positions/sort/emitted-ID state; resume never observes above the snapshot or silently drops an unavailable shard.
- PR #410 native rows remain queryable while representative/human/direct/subagent/tool-result/protocol views expose native/returned/hidden/unknown counts and classifier provenance.
- V1 lexical/rank/scope/filter/export behavior has a named parity disposition; every transport consumes the same typed result before rendering.
- Shadow/cutover/rollback receipts pass per bounded context. V1 query code/tests are deleted only after the data rollback window, archived parity/rollback artifacts, no internal fixture dependency, and explicit retirement approval; they never keep stale live clients working.
- All correctness, fault, privacy, security, performance, full-crate, compatibility, benchmark, and final-verification commands pass on the recorded corpus/reference machine.
