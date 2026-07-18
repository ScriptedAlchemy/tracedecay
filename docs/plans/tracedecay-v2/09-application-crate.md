# TraceDecay V2 Application Crate

## Status / Role

Normative product plan. `tracedecay-application` is the transport-neutral use-case layer between product adapters and the domain/query/store ports. It participates in every vertical product PR from PR5 onward; PR11 completes the shared application core needed by the public adapters.

## Outcome

Every user-visible operation has one direct typed application entry point. CLI, MCP, HTTP, hooks, automations, and the dashboard invoke the same behavior without duplicating policy, authorization, consistency, or error handling.

## Owns

- Typed request, response, and error contracts for product use cases.
- `RequestContext`: actor, project/repository/worktree scope, capabilities,
  immutable capability-grant ID/revision/digest, issuer, expiry and exact scope
  constraints, request ID, deadline, and cancellation.
- Read orchestration across query and store ports.
- Command orchestration, validation, authorization, idempotency, and transaction boundaries.
- Freshness, coverage, provenance, pagination, and partial-result semantics.
- Stable progress and event contracts consumed by streaming adapters.
- Typed transport-neutral operations and state contracts for LSP session
  admission, current diagnostics, analyzer engine and coverage state, and code
  navigation as required by
  [35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- The one typed, transport-neutral semantic-evidence/provider contract that
  ships in PR11 with the application core. Every analyzer-backed capability
  implements this contract. Plan 35 implements analyzer-backed providers behind
  it; this crate owns the contract's type, evolution, and canonical
  provider-result identity/compatibility semantics—not a copy scoped to LSP.
  Every provider result identity tuple is complete from PR11 onward: PR11 ships
  explicit current-project/single-root scope/project/worktree identity available
  then; PR15 upgrades and composes that scope identity with Plan 16 canonical
  multi-root/cross-project scope identity. Plan 16 is not a PR11 prerequisite.
  The tuple also includes clean-generation or node/client/session overlay
  identity; file/content digest; document version where applicable;
  producer/analyzer identity and revision; requested capability; freshness;
  coverage/completeness; provenance; Plan 25 language-descriptor
  identity/revision; Plan 20 configuration revision/digest; and Plan 06 policy
  decision/revision/digest.
- Translation from provider results into Plan-05-owned explicit query-evidence
  inputs for diagnostics, navigation, impact, and affected-test reads.
- The one advisory, transport-neutral branch-aware feedback-cycle
  request/result, orchestration, and finding lifecycle, shipping in PR11 as
  part of the first PR11–PR13 milestone defined by
  [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md).
  Plan 37 defines the architecture; this crate owns the concrete contract.
  Producers are composed, not reimplemented: post-edit diagnostics plus Plan 05
  impact evidence, CI-failure-localization input, ingested GitHub review
  threads, and concurrent-agent proximity warnings. Each request is one-shot
  only — no automatic follow-up, fix application, or effect execution.
  Results carry branch/worktree/commit/generation/content identity, stable
  finding IDs, [Plan 13](13-research-provenance-and-context-anchors.md)
  `RetrievalAnchorId`s where durable evidence exists, coverage/state,
  safe bounded previews, pagination/continuation metadata, and source
  provenance. Findings translate into Plan 05 explicit query-evidence inputs;
  this crate creates no second diagnostic or finding store.
- Direct product operations for capture, search, context, sessions, memory, code, delivery, automation, Doctor, configuration, and workflows.
- Canonical structural-search, source-outline, and source-rewrite operations
  backed by the PR9 in-process code-intelligence kernel.
- One source-edit `EditTransaction` for preview and apply across exact, symbol,
  insert, move, and structural rewrites.
- One daemon-owned `GitIndexTransaction` for typed `stage_hunks`,
  `unstage_hunks`, and `commit_index` execution against a real locked Git
  index. It owns immutable previews, CAS revalidation, idempotency, receipts,
  and explicit effect classes without exposing arbitrary Git execution.
- PR17 typed [Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md)
  initiative/work-plan/work-item commands and queries: versioned graph
  transactions, dependency/readiness and acceptance evaluation, evidence
  relations, saved projection inputs, assignment and route-review decisions,
  context assembly, transport-neutral auxiliary-attempt submission/inspection,
  and the exact admission/revalidation bridge to Plan 32.

## Does not own

- HTTP, SSE, MCP, CLI, hook, or frontend transport details.
- LSP JSON-RPC framing, stdio or socket bridging, upstream process
  supervision, or per-connection protocol buffers.
- SQL, libSQL connections, filesystem layout, indexing, or migration mechanics.
- Domain entity definitions or domain invariants.
- A generic command bus, query bus, plugin framework, service locator, or runtime registry.
- A generic LSP or JSON-RPC pass-through operation.
- Analyzer-provider cache storage, admission, reuse, eviction, invalidation
  execution, or lifecycle; those remain owned by
  [35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- GitHub REST/GraphQL identity, comment posting, or adapter packaging;
  [Plan 27](27-cross-host-agent-plugin-bundles.md) owns read-only GitHub
  ingestion mechanics. PR17 workflow composition is optional and does not gate
  the PR11–PR13 advisory cycle.
- A second diagnostic or finding store, transport bindings, LSP field
  projection, or host delivery adapters; those remain owned by Plans
  05/13/21/27/35/37 respectively.
- Developer plan parsing, Markdown execution, rewrite completion tracking,
  generated inventories, or compatibility ledgers. Product task/work graph
  operations are explicit PR17 typed use cases, never inferred from roadmap
  files.
- JavaScript workflow execution. PR17 workflows are real typed product operations, not developer-plan machinery.
- Runtime clocks/timers, scheduling, queues, leases, attempts, effects, retries,
  artifacts, or cancellation execution; Plan 32 owns that one shared kernel.
  This layer authorizes and maps Plan 24 task steps to it without duplicating
  runtime state.
- Merge, rebase, cherry-pick, branch/tag/ref mutation, history rewriting, or an
  autonomous Git workflow engine.

## Required behavior

- Define one explicit service method or use-case type per product operation; prefer ordinary Rust calls over indirection.
- Depend only on domain types and narrow port traits. No adapter or root-crate imports.
- Validate scope and capability before reads or writes; never infer authority from transport origin.
- Revalidate capability grant, policy, configuration, evidence, expected
  versions, and operation/sink/disclosure subset immediately before every
  command, page expansion, hydration, or effect.
- Preserve repository, worktree, branch, project, and user scope through every call.
- LSP-facing operations preserve authorized workspace scope, deadline,
  cancellation, document version, source generation, freshness, and coverage
  without accepting transport-native arbitrary payloads.
- Navigation, type-hierarchy, context, impact, affected-test, diagnostics, and
  refactoring-preview operations are enriched internally with the
  semantic-evidence provider's source/producer identity, provenance,
  coverage, freshness, and conflicts rather than exposed through a duplicate
  public `lsp_*` tool family. Active-document type semantics may come from an
  admitted analyzer provider; the code graph remains authoritative for stable
  symbol identity, generations, bounded traversal, history, cross-project
  evidence, and test attribution. Unsupported, absent, indexing, stale,
  cancelled, timed-out, failed, and partial provider states are reported
  explicitly; none may collapse to a clean empty result. Empty output is valid
  only for a supported, successfully completed request with complete coverage
  and zero matches. Impact and affected-test operations may incorporate provider
  reference/dispatch evidence translated into Plan-05-owned explicit typed
  inputs alongside graph, Git, and test evidence; a provider never proves that
  a test executed or that a change was delivered.
- Catalog, dashboard, and observability surfaces consume typed application
  results and state, never the provider port directly.
- Return structured freshness, coverage, provenance, warnings, and continuation data where relevant.
- Make mutation retries safe through operation-specific idempotency keys and daemon-owned transactions.
- Source edits use one journaled all-or-recoverable `EditTransaction`. Preview
  pins the file set and digests; apply revalidates every digest/CAS guard, stages
  sibling files, journals recovery data, and commits in deterministic order.
  A single file publishes by atomic rename; portable multi-file atomicity is not
  claimed. Success is reported only after every file commits. After a crash,
  reconciliation completes or rolls back the journal before new edits or
  reindexing. CLI `--dry-run` and tool `dry_run` mean this same preview.
- PR11 Git index mutations use `GitIndexTransaction`. Preview pins repository,
  worktree, HEAD/index identity, selected hunks, path/content digests, intended
  effect class, and canonical transaction digest. Apply acquires the real Git
  index lock, revalidates every CAS guard, executes only the previewed
  `stage_hunks`, `unstage_hunks`, or `commit_index` steps, and releases the lock
  on every outcome. A reused idempotency key returns the same durable receipt;
  mismatched input fails closed. Concurrent index change, stale HEAD/content,
  lock contention, and patch conflict remain distinct typed states. No partial
  success is reported as committed.
- `str_replace` is a compatibility binding to one-operation
  `multi_str_replace`; `insert_at_symbol` binds typed `insert_at`. Keep
  `replace_symbol`, in-process structural rewrite, and `move_symbol` as typed
  views over the same transaction; do not add split/import mutation tools.
- Check cancellation and deadlines around expensive or multi-stage work.
- Map domain and port failures into a small stable application error taxonomy without erasing actionable detail.
- Keep streaming events bounded, ordered, resumable where the product contract requires it, and independent of SSE framing.
- Expose workflow create, validate, run, inspect, cancel, and history operations in PR17 as typed domain/application contracts only.
- Expose Plan 24 initiative, work-plan/version, work-item/dependency,
  evidence/history, projection, assignment/review, runtime-admission, and
  routing-review operations in PR17. The same transport-neutral family covers
  task-shape assessment, decomposition proposal/review/decision,
  routing recommendation/fallback explanation, live split/merge/resize/re-route
  proposal review, independent review-grade recording, outcome attachment, and
  calibration reports. These are application concepts, not frozen PR18 SDK
  names.
- Proposal commands pin expected work-plan/work-item/proposal versions,
  evidence watermarks, code/Git generation, scope, and
  policy/config/catalog/privacy revisions. The application revalidates
  authorization, freshness, graph legality, acceptance, and Plan 32 runtime
  state immediately before an explicit accept/reject/supersede action. Reading
  or generating a proposal has no graph or runtime effect.
- Every executable admission pins the exact
  graph/readiness/scope/acceptance/route/grant/budget revisions and returns the
  mapped Plan 32 run/node identity; stale evidence fails before dispatch.
- PR17 auxiliary-attempt submission accepts only the Plan 24-owned typed
  request identity plus expected graph/evidence/policy/config/catalog/privacy
  revisions. The application resolves and authorizes exact
  project/repository/worktree/branch and parent task/attempt/Session identity,
  bounded retrieval anchors, requested provider/model/reasoning, sandbox and
  approval class, capabilities, budgets, deadline/cancellation, and opaque
  secret references before asking Plan 32 to admit. It never accepts a shell
  string, raw environment, arbitrary executable, unbounded prompt/context, or
  adapter-local fallback flag.
- Plan 32 returns the negotiated provider descriptor, lease/attempt identity,
  progress/event frontier, artifacts, and typed terminal receipt through this
  application contract. `Unsupported`, `Absent`, `Stale`, `Cancelled`,
  `TimedOut`, `Failed`, and `Partial` remain distinct. A requested-versus-actual
  provider/backend/model mismatch without the pinned explicit fallback
  decision fails closed.
- Add each use case in the same product PR as its domain/store/query behavior; do not create speculative APIs ahead of executable behavior.
- PR11 ships the transport-neutral semantic-evidence/provider contract with
  the completed application core and explicit current-project/single-root
  scope/project/worktree identity in every provider result. PR9 query work does
  not import this crate or depend on live providers.
- PR15 upgrades provider-result scope identity by composing PR11's
  single-root identity with Plan 16 canonical multi-root/cross-project scope
  identity. Plan 16 is not a PR11 prerequisite.
- PR11 removes remaining root-level business orchestration by routing adapters
  through the completed application core.
- PR17 keeps graph semantics in Plan 24 application use cases and runtime
  semantics in Plan 32; no handler, projector, board, or host adapter decides
  readiness, route quality, proposal acceptance, scheduling, or completion
  locally. Policy recommendations never lower directly to runtime effects.
- PR11 feedback-cycle requests bind project/repository/worktree/branch/ref/HEAD
  SHA, clean source-generation identity or an explicitly tagged ephemeral
  overlay, file digest and document version, agent/session/turn identity,
  changed files/ranges/symbols, the exact trigger, policy/config digests, and
  deadline/cancellation/budget inputs. Overlay-triggered requests may return
  immediate session-only findings to the authorized overlay owner; those
  findings are never durable — they cannot enter capsules, envelopes,
  checkpoints, receipts, feedback-history records, observations, facts, memory,
  telemetry payloads, spools, caches, replicas, exports, or ingested GitHub
  evidence. Durable findings require exact saved-content/clean-generation
  identity.
- PR11 feedback-cycle results name exactly one termination reason from Plan
  37's taxonomy, including `duplicate_noop` when the exact trigger/address/
  content/branch/generation/evidence identity was already evaluated with no
  new evidence. It is neither `clean` nor adapter silence. Results distinguish
  new versus pre-existing diagnostics, preserve
  complete provider-state sets without collapsing unavailable/partial coverage
  to clean empty results, and expose finding lifecycle state
  (active/superseded/resolved/cleared) keyed by stable finding IDs plus Plan 13
  anchors when present.
- Feedback findings keep score kind (`ordinal_rank`, `heuristic_score`,
  `calibrated_probability`, or `calibrated_interval`), calibration validity,
  deterministic rank components, source lifecycle, delivery disposition,
  human outcome, and total/returned/omitted counts as orthogonal fields.
  Ranking or suppression never deletes canonical evidence or adjudicates it
  false.
- Branch-relative impact optionally binds exact origin and destination
  `RepositorySnapshot`s plus merge base, each impact set and coverage, and
  added/removed/changed delta-impact relations. Missing destination evidence
  is partial or stale and can never produce `clean`.
- PR17 resolves an authorized `TaskId`/`WorkItemId` into bounded task context,
  dependencies, attempts, independent reviews, temporal outcomes, sessions,
  Threads, Turns, messages, agents, tool calls, artifacts, receipts, handoffs,
  and other-agent work, plus Plan 13-anchored code, Git, CI, diagnostic,
  generation, impact, and affected-test evidence. Plan 23 supplies the sole
  current/as-of/evolution/forensic session narrative kernel; summaries
  accelerate retrieval but never replace exact source anchors or owning-store
  evidence.
- Promotion or calibration results may reference compact immutable Plan 15/26
  evaluation records and anchors. Statistical formulas, benchmark
  orchestration, and a second evaluation platform do not enter application
  contracts.
- Keep the application's direct dependency graph narrow and feature-minimal.
  Concrete stores, transports, providers, model runtimes, dashboard assets, and
  their build scripts must not enter its normal check or test graph.
- Treat PR11 as a compilation-boundary migration as well as an ownership
  migration: record same-host warm incremental check and representative
  application-test compilation before and after root orchestration moves.
  Regressions require an identified cause and explicit disposition.

## Common result and evidence contracts

PR11 seals all transport-neutral outcomes behind these application-owned
contracts. Payload types are concrete per use case; no field accepts arbitrary
`serde_json::Value`.

```rust
pub type ApplicationResult<T> =
    Result<ApplicationEnvelope<T>, ApplicationProblemEnvelope>;

pub struct ApplicationEnvelope<T> {
    pub contract: ResultContractRef,
    pub request_id: RequestId,
    pub scope: ResolvedScope,
    pub outcome: ApplicationOutcome<T>,
}

pub enum ApplicationOutcome<T> {
    Evidence(EvidencePacket<T>),
    Preview(PreviewResult<T>),
    Effect(EffectResult<T>),
}

pub struct ApplicationProblemEnvelope {
    pub contract: ResultContractRef,
    pub request_id: RequestId,
    pub problem: ApplicationProblem,
}

pub struct EvidencePacket<T> {
    pub temporal: TemporalState,
    pub authority: AuthorityReceipt,
    pub evidence_authorities: Vec<EvidenceAuthority>,
    pub coverage: Coverage,
    pub omissions: Vec<Omission>,
    pub scores: Vec<EvidenceScore>,
    pub contributions: Vec<RetrieverContribution>,
    pub page: PageState,
    pub execution: OperationReceipt,
    pub payload: Option<T>,
}
```

`ResultContractRef` carries stable `schema_id` and `schema_revision`; a field
cannot be removed, repurposed, or change enum/number/null semantics without a
revision change.

`TemporalState` records the requested mode (`Current`, `AsOf`, `Evolution`, or
`Forensic`), requested and resolved horizons, source snapshot/generation,
watermark, observed-at time, and freshness classification. `AuthorityReceipt`
exists only after successful authorization and records decision/grant identity,
revision/digest, authorized scope digest, disclosure class, and revalidation
time. It never carries credentials or policy inputs.

`Coverage` records requested evidence domains, visited/eligible/returned counts,
completeness (`Complete`, `Partial`, or `Unknown`), bounded search horizon, and
per-domain state. `Omission` records only an authorized requested domain, count,
and one reason (`Budget`, `Redacted`, `Unavailable`, `Unsupported`, `Stale`,
`Failed`, `Cancelled`, or `TimedOut`); it cannot identify a hidden resource.
`EvidenceScore` preserves `ScoreKind` (`OrdinalRank`, `HeuristicScore`,
`CalibratedProbability`, or `CalibratedInterval`), value or interval,
calibration revision/validity, and deterministic components. Scores are
optional evidence metadata, never authority or truth.

`AuthorityReceipt` proves the caller's request authorization.
`EvidenceAuthority` is separate claim/source authority keyed by
`EvidenceIdentity` (or an explicit identity group) and records source kind,
owner/producer, scope, revision, and authority horizon. It cannot grant access
or runtime authority. Every evidence-bearing payload identity has exactly one
matching authority record or a typed unknown-authority omission.

Each `RetrieverContribution` records `RetrieverId`, contract and producer
revision, requested domain, terminal state, coverage delta, returned and
omitted counts, score references, provenance/anchor references, and elapsed
budget class, plus claim-specific evidence-authority references. A contribution
cannot claim broader coverage or authority than its source port reported. The
packet-level coverage is a deterministic fold over these contributions;
adapters and planners cannot recompute it.

`PageState` carries stable order revision, total when known, returned count, and
an optional authenticated opaque cursor bound to capability and use-case IDs,
request digest, scope/grant digest, temporal horizon, source snapshot/generation
and watermark, result-schema and sort revisions, last sort key, and expiry.
Every resume reauthorizes before decoding or hydrating the next page.
Scope/grant mismatch returns `NotFoundOrNotAuthorized` before any cursor-state
detail. Concatenated pages must equal the same-snapshot bounded full result.

Every returned item has a stable `EvidenceIdentity` within the pinned source
snapshot, and every score has a stable `ScoreId` referencing one or more
evidence identities. Contribution folding sorts by `(domain, retriever_id,
evidence_identity)`, deduplicates identical evidence identities, and retains
all distinct provenance/anchor refs. `returned` counts unique authorized
evidence. Every contribution carries `CoveragePartitionId` plus its exact
visited/eligible membership as a bounded exact `EvidenceIdentity` set, an
explicit `DisjointFrom(Vec<CoveragePartitionId>)` proof with cardinality, or a
typed unknown-overlap/cardinality state. Identity-set digests authenticate
membership but never prove overlap by themselves. Packet `visited` and
`eligible` are union cardinalities only when exact membership or disjointness
proofs establish the union: disjoint partitions add, exact overlapping sets
deduplicate by evidence identity, and any unproved overlap makes packet
completeness/counts `Unknown` rather than using sum or maximum. Omission counts
sum only disjoint omission partitions keyed by `(domain, reason,
partition_digest)`. Conflicting payloads for one identity produce `Partial`
coverage plus a conflict omission; they are never silently selected. Packet
scores preserve all referenced score records in stable `ScoreId` order and do
not average unlike score kinds.

`OperationReceipt` records start/end, effective deadline, cancellation
observation and stage, budget consumed, and one termination state. Commands use
the stronger contract:

```rust
pub struct PreviewResult<T> {
    pub preview_id: PreviewId,
    pub preview_digest: PreviewDigest,
    pub effect_class: EffectClass,
    pub authority: AuthorityReceipt,
    pub expected_state: ExpectedStateDigest,
    pub execution: OperationReceipt,
    pub payload: Option<T>,
}

pub struct EffectResult<T> {
    pub effect_id: EffectId,
    pub effect_class: EffectClass,
    pub idempotency_key: IdempotencyKey,
    pub authority: AuthorityReceipt,
    pub expected_state: ExpectedStateDigest,
    pub execution: OperationReceipt,
    pub reconciliation: ReconciliationState,
    pub receipt: EffectReceipt,
    pub payload: Option<T>,
}
```

`EffectReceipt` binds operation identity, request/actor/scope, effect class,
idempotency identity, input and expected-state digests, policy/config/catalog/
privacy revisions, committed state or external proof, and exactly one outcome:
`Completed`, `Cancelled`, `TimedOut`, `Failed`, `Partial`, or `EffectUnknown`.
Only `Completed` can claim success. Cancellation and deadline expiry report the
last proven stage (`BeforeAdmission`, `BeforeRead`, `DuringRead`,
`BeforeEffect`, `EffectInFlight`, `Reconciling`, or `AfterCommit`) and never
rewrite a committed effect.

Phase determines where terminal state is represented. Syntax, admission,
authentication/authorization, scope, and precondition failures before an
operation is admitted return `ApplicationProblemEnvelope`. Once a read is
admitted, `Cancelled`, `TimedOut`, `Failed`, and `Partial` return an
`EvidencePacket` with the true operation receipt, coverage, omissions, and
contributions. Once a preview or effect is admitted, every terminal state
returns `PreviewResult` or `EffectResult` with its canonical receipt;
`EffectUnknown` is never a pre-admission problem. This rule prevents adapters
from replacing admitted execution evidence with a transport error.
`Completed` evidence with zero matches carries `Some(empty_collection)` plus
complete coverage and no omissions. An admitted non-completed operation may
carry `None` when no payload was produced or `Some(partial_payload)` when the
receipt and coverage describe exactly what was produced. `None` is never
rendered as a clean empty result.

Resource-addressed authorization failure returns
`ApplicationProblem::NotFoundOrNotAuthorized` before an evidence packet,
cursor, count, timing detail, provider state, anchor state, or legal action is
disclosed. The same problem code, terminality, retry class, safe message, and
CLI/MCP/HTTP mapping apply whether the resource is absent, outside scope, or
hidden by policy. Internal audit records may retain the denied decision under
Plan 18, but public results cannot distinguish those causes.

## Retrieval primitive ports and planner boundary

PR11 defines no universal `RetrievalPrimitive`, generic evidence retriever,
query bus, or capability-by-name call. It defines concrete use-case types:
`SymbolSearch`, `SymbolExactLookup`, `QualifiedNameLookup`, `SignatureLookup`,
`ImplementationLookup`, `TypeHierarchyRead`, `SourceLinesRead`,
`SourceBodyRead`, `SourceOutlineRead`, `ModuleApiRead`, `FileMetadataRead`,
`CallersRead`, `CalleesRead`, `CallChainRead`, `FileDependentsRead`,
`ImpactRead`, `DependencyDepthRead`, `TestMapRead`, `AffectedTestsRead`,
`SessionLookup`, `MessageSearch`, `SessionNarrativeRead`, `AnchorExpand`,
`CatalogRead`, `ConfigurationRead`, `ProjectRead`, `HealthRead`, and
`StorageRuntimeRead`. Each exposes one ordinary `execute` method whose concrete
request and result append `Request` and `Result` to the use-case type name; for
example, `SymbolSearch::execute(&RequestContext, SymbolSearchRequest) ->
ApplicationResult<SymbolSearchResult>`. No method accepts an untyped operation
name or payload.

Concrete ports are `SymbolRetrievalPort`, `SourceRetrievalPort`,
`GraphRetrievalPort`, `TestRetrievalPort`, `TemporalRetrievalPort`,
`AnchorHydrationPort`, and `OperationalRetrievalPort`. Each method accepts a
typed request with explicit scope, bound, temporal mode, order, cursor, and
projection and returns one typed provider contribution. The application service
validates context, calls the named port, and produces the common packet. A
primitive never accepts a prompt, planner state, workflow definition,
capability name, or nested operation list.

The immutable `EvidencePacket<T>` is the planner-facing consumer boundary; PR11
defines no planner trait, request-proposal callback, or planning extension.
Plan 24's PR17 `work/context.rs` consumes packets and owns task decomposition,
retrieval-plan intent, and fan-out shape. Plan 32 alone admits and executes
parallel branches, enforces concurrency/failure budgets, and returns branch
receipts. This crate serves each admitted primitive request and may
deterministically fold an explicitly supplied list of already-completed
packets; it owns no hidden parallelism, model call, scheduler, lease, retry
loop, recursive retrieval, or request proposal. Non-planner callers invoke the
same primitives directly.

Pre-admission application error mapping is exhaustive and transport-neutral:
`InvalidRequest`, `NotFoundOrNotAuthorized`, `Conflict`, `Stale`,
`Unsupported`, `Unavailable`, `Saturated`,
`Cancelled { stage: BeforeAdmission }`, and
`TimedOut { stage: BeforeAdmission }`. Admitted cancellation, deadline,
failure, partial, and unknown-effect states use receipts as defined above.
Port-specific detail is retained as bounded safe diagnostics and retriever
state without changing the stable problem code. Empty payload is valid only for
`Some(empty_collection)`, `Complete` coverage, no omissions, and a completed
operation receipt.
Every problem carries one `RetryDirective`: `Never`, `SameRequest`,
`AfterDelay`, `AfterRevalidate`, or `AfterReconcile`. Adapters preserve it and
cannot infer retry safety from the problem code.

## Files and dependency order

PR11 creates these Plan-09-owned files:

- `Cargo.toml` — workspace registration for `tracedecay-application`;
- `crates/tracedecay-application/Cargo.toml` — domain/port/catalog-contract
  dependencies only;
- `crates/tracedecay-application/src/lib.rs` — narrow module declarations and
  public contract re-exports;
- `crates/tracedecay-application/src/context.rs` — `RequestContext`,
  deadline/cancellation references, and immutable grant inputs;
- `crates/tracedecay-application/src/result/mod.rs` — sealed result exports;
- `crates/tracedecay-application/src/result/envelope.rs` —
  `ApplicationEnvelope`, `ApplicationProblemEnvelope`, `ApplicationOutcome`,
  and contract revision;
- `crates/tracedecay-application/src/result/evidence.rs` — `EvidencePacket`,
  temporal, authority, coverage, omission, score, contribution, and page types;
- `crates/tracedecay-application/src/result/receipt.rs` —
  `OperationReceipt`, `PreviewResult`, `EffectResult`, `EffectReceipt`,
  cancellation stages, and reconciliation states;
- `crates/tracedecay-application/src/result/stream.rs` — bounded ordered
  `StreamEvent`, `StreamFrontier`, `StreamGap`, `ResumeToken`,
  `StreamTermination`, drop/truncation coverage, and terminal-event contract;
- `crates/tracedecay-application/src/result/problem.rs` — stable problem
  taxonomy, safe diagnostics, retry class, and legal actions;
- `crates/tracedecay-application/src/handlers.rs` — closed validation-only
  `ApplicationHandlerDescriptors`; it has no function pointers, invocation,
  runtime registration, or dispatch;
- `crates/tracedecay-application/src/retrieval/ports.rs` — the seven narrow
  retrieval port traits;
- `crates/tracedecay-application/src/retrieval/requests.rs` — bounded typed
  primitive request/projection/order contracts;
- `crates/tracedecay-application/src/retrieval/service.rs` — authorization,
  deadline/cancellation checks, one-port execution, contribution folding, and
  packet construction;
- `crates/tracedecay-application/src/retrieval/catalog.rs` — Plan 08
  contributions for shipped primitive handlers.

Plan 24's PR17 contributions live in
`crates/tracedecay-application/src/work/catalog.rs` and consume packets through
`crates/tracedecay-application/src/work/context.rs`. Plan 32
admission/control contributions live in
`crates/tracedecay-application/src/workflow/catalog.rs`; their runtime
implementation remains outside this crate. Plan 21 imports only the public
application contracts. Plans 05/13/23 implement the query, anchor, and temporal
ports without importing application or transport crates.

Implementation order is mandatory:

1. Plan 08 lands inert catalog IDs/record types without importing application;
2. add workspace/crate manifests, `lib.rs`, result/context types, and
   serialization fixtures;
3. land port traits and fake-port tests without concrete stores;
4. land the retrieval service, non-disclosure, cursor, deadline, cancellation,
   score, contribution, and omission folds;
5. register Plan 08 contributions only for executable handlers;
6. root-owned `src/catalog_composition.rs` assembles contributions and validates
   their UseCaseIds/schema refs against `ApplicationHandlerDescriptors`;
7. migrate one primitive family at a time from root/handler orchestration and
   run old-versus-new semantic fixtures;
8. switch Plan 21 adapters only after each family passes CLI/MCP canonical
   result parity; and
9. delete the migrated handler-local query/auth/error path before admitting the
   next family, so no shadow application layer survives.

PR17 adds work/task consumers only after the PR11 packet contract is stable.
Its CLI/MCP names follow Plan 21 compatibility policy and do not constrain or
require approval from PR18. PR18 independently chooses SDK method names and
adds SDK BindingIds only with SDK conformance fixtures.

## Test matrix and migration gates

Focused tests are fixed at:

- `crates/tracedecay-application/tests/evidence_contract.rs` — stable
  result/problem serialization, temporal modes, evidence-identity deduplication,
  partition-union contribution/omission/count folding including unknown
  overlap, request-versus-evidence authority, score identity and calibration,
  preview identity, optional payload terminal invariants, and clean-empty rules;
- `crates/tracedecay-application/tests/retrieval_primitives.rs` — every narrow
  port, bound/order/projection enforcement, one-port execution, pagination,
  resume equivalence, and no nested dispatch;
- `crates/tracedecay-application/tests/authorization_non_disclosure.rs` —
  absent/out-of-scope/policy-hidden equivalence for lookup, cursor, anchor,
  and shipped operational requests, including equal public problem envelopes
  and no count/timing/existence leakage; PR17 extends this file with task,
  WorkItem, and provider requests when those handlers ship;
- `crates/tracedecay-application/tests/deadline_cancellation.rs` — cancellation
  pre-admission problem mapping, cancellation at every admitted stage, deadline
  precedence, no new evidence/effect admission after cancellation, suppression
  of late uncommitted data, after-commit reconciliation/receipt publication,
  and `EffectUnknown` reconciliation;
- `crates/tracedecay-application/tests/stream_contract.rs` — monotonic sequence
  and frontier, gap/drop/truncation coverage, bounded resume, expiry,
  cancellation/deadline ordering, exactly one terminal event, and no events
  after terminal publication;
- `crates/tracedecay-application/tests/effect_receipts.rs` — effect class,
  expected-state digest, idempotent replay, mismatched-key rejection, partial
  and unknown-effect outcomes;
- `crates/tracedecay-application/tests/planner_boundary.rs` — dependency and
  fake-port assertions proving no universal retrieval trait/query bus exists
  and concrete primitives cannot import/invoke a planner, model, catalog
  dispatcher, Plan 32 runtime, or parallel executor; and
- `tests/architecture_boundaries.rs` — no transport, concrete store, provider,
  dashboard, scheduler, or runtime dependency.
- `tests/catalog_composition_contract.rs` — root snapshot validates every
  contribution against the closed handler descriptors without creating an
  invocation registry.

The PR11 gate runs:

```bash
cargo test -p tracedecay-application --test evidence_contract
cargo test -p tracedecay-application --test retrieval_primitives
cargo test -p tracedecay-application --test authorization_non_disclosure
cargo test -p tracedecay-application --test deadline_cancellation
cargo test -p tracedecay-application --test stream_contract
cargo test -p tracedecay-application --test effect_receipts
cargo test -p tracedecay-application --test planner_boundary
cargo check -p tracedecay-application --all-features
cargo test --test catalog_composition_contract
cargo test --test architecture_boundaries
```

Migration is blocked if canonical JSON changes without a result-contract
revision, a primitive lacks exact bounds/ordering/temporal behavior, an
unauthorized request differs publicly from an absent request, page
concatenation differs from the pinned full result, cancellation admits new work
or publishes late uncommitted data, an already-committed effect loses its
reconciliation/receipt publication, an effect lacks a durable receipt, a
planner/model/fan-out executor enters a primitive dependency, or a focused
application check compiles transport or concrete storage.

PR11 records `cargo tree -p tracedecay-application --edges normal` at
`benchmarks/pr11-application-boundary/dependency-tree.txt` and same-host warm
incremental medians at
`benchmarks/pr11-application-boundary/compile-baseline.json`. The dependency
gate requires zero transport/concrete-store/provider/runtime packages and a
strict reduction in legacy root-crate normal-dependency fan-in. The compile gate
blocks a median regression above 10 percent unless the PR11 owner records the
measured cause and accepted disposition in that JSON artifact.

## Acceptance

- Every shipped product operation has a typed application contract and focused unit tests.
- CLI, MCP, HTTP, hooks, automations, and dashboard paths share those contracts rather than reimplementing behavior.
- Dependency checks prove the crate is transport- and storage-neutral.
- Authorization, scope, cancellation, idempotency, freshness, coverage, and error semantics have direct tests.
- `GitIndexTransaction` tests use a real repository and index lock and cover
  preview immutability, CAS drift, conflicting hunks, concurrent index change,
  idempotent replay, crash-safe receipts, lock release, and exact effect-class
  enforcement without permitting generic or history-mutating Git commands.
- Unsupported, absent, indexing, stale, cancelled, timed-out, failed, and partial
  provider states have direct tests; none collapse to a clean empty result.
  Empty output is valid only for supported, successfully completed requests with
  complete coverage and zero matches.
- No generic bus/framework, developer-plan parser/executor, generated inventory,
  or JavaScript workflow runtime exists in this layer.
- PR11 leaves no product orchestration in transport handlers or the legacy root crate.
- PR17 tests prove graph-version/readiness CAS, cycle and acceptance failures,
  exact scope/worktree binding, one-to-one task-step/runtime mapping, stale
  lease/attempt projection, cancellation/recovery, and no second
  runtime-clock/scheduler/lease/attempt/effect authority.
- PR17 proposal tests prove read-only assessment, recommendation, and
  calibration calls cannot mutate; proposal decisions reject stale graph,
  evidence, scope, policy/config, or runtime state; executor grants cannot
  self-grade or accept their own route/resize proposal; and every accepted
  decomposition or resize names the resulting immutable graph version and any
  separate Plan 32 control decision.
- PR17 auxiliary-attempt tests prove exact lineage/scope/context
  revalidation, rejection of shell strings/raw environment and preservation of
  typed execution fields for Plan 32 lowering, deterministic provider
  selection, no Hermes Anthropic substitution for Claude Code, explicit Codex
  app-server-to-CLI fallback policy, secret/environment filtering,
  cancellation/deadline propagation, exhaustive result mapping, and no
  recursive dispatch or provider-originated graph/runtime mutation.
- A focused application check or test does not compile transport, dashboard,
  provider, or concrete-storage targets, and the legacy root crate's dependency
  fan-in is measurably reduced.
- PR11 feedback-cycle fixtures cover one-shot advisory semantics, every
  producer class (post-edit diagnostics/impact, CI-localization input, ingested
  GitHub review threads, proximity), finding lifecycle transitions, Plan 13
  anchor attachment, Plan 05 evidence translation without a second store,
  pagination/continuation metadata, dirty-overlay non-durability, and exact
  termination reasons for branch/head/content/generation change, duplicate
  triggers, cancellation, and budget exhaustion.
- PR17 TaskId fixtures prove authorization parity across lookup, paging,
  hydration, history modes and exact expansion; lossless source recovery;
  origin/destination impact disagreement; calibrated-versus-heuristic output;
  and no application-local task store, scheduler, query kernel, label
  vocabulary, or Doctor authority.
