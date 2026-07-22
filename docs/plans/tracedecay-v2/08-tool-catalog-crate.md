# V2 tool catalog crate

## Status / Role

- Status: pending for PR11.
- PR11 implements the minimal runtime catalog with application and policy consumers.
- PR12 binds the catalog to CLI, MCP, HTTP, LSP, and discovery surfaces.
  PR14 first ships dashboard binding, dashboard actions, and dashboard parity
  over the same CapabilityIds and application handlers. PR18 adds SDK bindings
  only when the official SDK methods ship.
- PR17 adds Plan 24 task/work graph and Plan 32 runtime capabilities through
  the same catalog; it creates no task-specific registry.
- tracedecay-tool-catalog describes callable product capabilities. It does not discover them by parsing source code.

## Outcome

Every public surface resolves stable capability IDs to the same application
use cases, scope rules, effects, availability, and output semantics without
duplicating business logic.

## Owns

- Stable CapabilityId, UseCaseId, and BindingId values.
- Small immutable definitions for capability identity, user-facing description,
  input/output schema references, effect class, scope requirements,
  availability, deprecation, surface binding, protocol revision range,
  required negotiated features, result-schema revision, lifecycle class
  (`stateless | connection_stateful | session_stateful | resumable`),
  streaming support, cancellation semantics, and deprecation window.
- PR17 auxiliary-provider descriptors for backend/executable/protocol identity
  and version, model/reasoning selectors, typed argv/stdin and structured-event
  support, sandbox/approval/environment/egress classes, tool and artifact
  capabilities, deadline/cancellation/kill behavior, progress/heartbeat,
  resume/reconnect, and explicit fallback eligibility.
- Explicit surface profiles, including bounded MCP profiles whose ceilings are
  reviewed per profile/model and never treated as universal tool-count truth.
- Typed standard-LSP bindings from navigation and diagnostic methods to the
  existing code and diagnostic capabilities and application handlers defined
  for [35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- The typed binding and schema revision for Plan 35's owned TraceDecay LSP
  context extension: negotiated experimental capability metadata, provider
  contribution requirements, bounded request/result envelopes, and the
  canonical handlers for diagnostics, impact, affected tests, test results,
  and opaque authorized expansion. The catalog does not own its framing,
  session state, provider data, or projection behavior.
- Immutable catalog snapshot assembly from reviewed contributions registered beside implemented application use cases.
- Pure validation and lookup by stable ID, surface, profile, and availability.

## Does not own

- A source, Clap, Axum, dashboard, hook, Markdown, plan, or workflow parser.
- catalog-gen, inventory JSON, generated architecture views, frozen tool counts, checked-in generated SDK/UI/plugin trees, or CI that reconstructs the product from source text.
- A universal operation registry containing speculative future features.
- Capability execution, authorization, persistence, network I/O, rendering, host probing, installation, or daemon routing.
- LSP lifecycle, JSON-RPC framing, document synchronization notifications,
  connection state, or other protocol mechanics.
- A generic invoke-anything tool or compatibility aliases for retired names.
- An arbitrary JSON-RPC proxy capability.
- Task/work graph semantics, readiness, model scoring, workflow execution, or
  generated agent orchestration logic. The catalog does declare the shipped
  Plan 24/32 application capabilities and their effect/scope metadata.

## Required behavior

- **PR11 — definitions:** create compact immutable catalog records and stable IDs for application use cases implemented through PR11. Every entry points to a real typed application handler.
- **PR11 — canonical operations:** structural search, source outline, source
  rewrite, exact/symbol edit, temporal retrieval, configuration, health, and
  every other tool bind stable typed application operations. A surface name or
  alias has zero business logic.
- **PR11 — Git index capability:** catalog the daemon-owned
  `GitIndexTransaction` operations `stage_hunks`, `unstage_hunks`, and
  `commit_index` as typed application handlers with distinct effect classes,
  immutable preview/CAS requirements, idempotency keys, and receipt contracts.
  Do not catalog generic Git execution or autonomous merge, rebase,
  cherry-pick, branch/tag/ref mutation, or history rewriting.
- **PR11 — configuration boundary:** code/config-file inspection is a scoped
  source operation; product settings use the typed configuration authority.
  Similar presentation does not merge their authorization or effects.
- **PR11 — contributions:** register catalog records beside their owning application feature, then assemble one immutable snapshot at composition. No central file duplicates every request/response definition.
- **PR11 — validation:** reject duplicate IDs/bindings, missing handlers,
  incompatible schema references, invalid scope/effect combinations, profile
  overflow, and dependency cycles.
- **PR11 — agent-facing contracts:** treat names, descriptions, and examples as
  versioned routing contract fields. Validate same-profile discriminability
  with positive, negative, ambiguous, and insufficient-capability fixtures;
  no paper-derived fixed 10/20/30-tool ceiling becomes product policy.
- **PR11 — policy:** expose read-only capability metadata to policy routing. Availability and effect metadata inform a decision but never execute it.
- **PR11 — daemon:** bind each executable capability to the single tracedecayd/application authority. Catalog consumers never open a database or bypass application authorization.
- **PR11 — profiles:** define explicit capability sets and hard ceilings for default, compact, administrative, and host-limited surfaces. Absence is explicit, not a hidden fallback.
- **PR11 — compatibility:** retain a supported deprecated name only as a
  `SurfaceBinding` when a direct compatibility test requires it. Once removed,
  it returns the same unknown-operation problem as any unknown name; authorized
  callers may separately use ordinary discovery guidance. No retired-name
  tombstone registry or compatibility ledger exists.
- **PR12 — bindings:** map CLI commands, MCP tools, HTTP operations, and LSP
  methods to the same CapabilityId and typed application handler where the
  protocol exposes a callable product operation. Dashboard binding, dashboard
  actions, and dashboard parity remain owned by PR14; PR12 does not ship
  dashboard adapters.
- **PR14 — dashboard binding:** map dashboard actions to the same CapabilityId
  and typed application handler as CLI, MCP, HTTP, and LSP. Dashboard parity
  tests submit equivalent typed requests through dashboard and non-dashboard
  adapters and compare semantic results before rendering.
- **PR12 — LSP bindings:** map each supported standard navigation or diagnostic
  method to an existing typed code or diagnostic capability and handler.
  Lifecycle, framing, and document notifications remain protocol mechanics,
  not callable catalog capabilities. `prepareRename` and `rename` bind only to
  read-only candidate/preview UseCaseIds owned by
  [34](34-workspace-refactoring-and-api-migration.md); they never bind directly
  to `tracedecay_rename_symbol`, API-migration apply, another write-effect
  entry, `workspace/applyEdit`, or opaque server commands. No separate
  `lsp_*` capability is cataloged for them, and no binding may apply an edit
  on their behalf. `textDocument/codeAction` is cataloged only when its owning
  typed candidate-consumption operation, effect classification, canonical
  preview/`EditTransaction` route, and direct acceptance behavior ship
  together.
- **PR12 — LSP extensions:** require every vendor extension to have an explicit
  typed catalog entry, bounded schema, effect classification, and tested
  handler. Ship the versioned TraceDecay context extension only through
  standard LSP/JSON-RPC framing and explicit experimental capability
  negotiation. Its compact envelope carries exact authorized scope, content/
  graph generation, producer and coverage state, bounded diagnostics/impact/
  affected-test/test-result projections, omissions, and opaque expansion
  handles. Expansion reauthorizes and calls the same canonical application
  reads; handles are not durable evidence identity. The provider contribution
  point admits a contribution only when its typed handler is callable, so
  PR13 can add GitHub review, CI localization, and proximity without changing
  the reader transport. Never expose arbitrary method or payload forwarding.
- **PR12 — feedback reads:** bind canonical diagnostics, impact, affected-test,
  test-result, feedback get/list, and exact expansion handlers as real callable
  reads on their supported CLI, MCP, HTTP, and negotiated LSP surfaces. No
  placeholder handler or advertised-but-unavailable binding may stand in for
  a PR12 operation. PR13 adds GitHub/CI/proximity producers and catalog
  contributions to these readers; it does not replace or fork their transport.
- **PR12 — schemas:** surface adapters use reviewed typed schemas or schema references from the owning contract. The catalog does not generate domain types from prose or source parsing.
- **PR12 — Git bindings:** expose exactly `git_preview` and `git_apply` to CLI
  and MCP. Both surfaces share one request/result schema: preview returns the
  immutable transaction plan and digest; apply accepts that identity plus CAS
  evidence and returns the canonical receipt or typed stale/conflict state.
  Internal `stage_hunks`, `unstage_hunks`, and `commit_index` remain application
  operations, not additional public tools.
- **PR12 — discovery:** return bounded capability metadata filtered by surface, profile, availability, scope, and authorization. Never expose secrets, config bodies, private paths, or unavailable administrative details.
- **PR12 — output:** all surfaces consume the same typed application result before rendering. Markdown is the human/agent default where appropriate; structured JSON remains explicit.
- **PR12 — drift:** direct tests enumerate compiled bindings and assert each references a valid catalog entry and handler. This is runtime contract validation, not source extraction.
- **PR13 — hooks:** hook adapters use cataloged host capabilities only through application/daemon responses; hooks do not resolve or execute catalog entries locally.
- **PR17 — work graph:** catalog explicit initiative/work-item/version,
  dependency, evidence/history, saved-projection, assignment/review, runtime
  admission/control, task-shape assessment, decomposition review,
  routing-recommendation, resize/re-route proposal, independent-review grade,
  outcome-record, and calibration-report operations backed by Plan 24/32
  application handlers. Proposal reads, proposal acceptance/rejection, human
  graph mutations, runtime controls, and executor-lifecycle calls use distinct
  capabilities, effects, and grants. These are semantic capability families;
  PR17 catalog IDs do not prematurely freeze PR18 public SDK method names.
  There is no generic status setter, invoke-anything task tool, board-local
  command, local model scorer, or route that bypasses Plan 32 effect authority.
- **PR17 — auxiliary providers:** catalog native Claude Code CLI, Codex
  app-server, and policy-eligible Codex CLI fallback as distinct provider
  adapter capabilities backed by Plan 32. Descriptors name executable and
  protocol version constraints, supported models/reasoning/capabilities,
  structured stream and lifecycle behavior, sandbox/approval/environment
  requirements, and supported terminal outcomes. Hermes Anthropic is not a
  Claude Code execution capability. An app-server descriptor never aliases the
  CLI descriptor, and fallback eligibility never means automatic fallback.
  Catalog IDs are internal PR17 capability identity and do not freeze PR18
  public SDK operation names.
- **PR17 — TaskId retrieval:** keep distinct typed capabilities for TaskId
  lookup, compact context, current/as-of/evolution/forensic history,
  thread/attempt traversal, impact and affected tests, handoff, proposal
  review, escalation, governed experience recall, and runtime control. Read,
  proposal-decision, and runtime-effect grants remain separate; no generic
  task query DSL, invoke-anything operation, task-local scheduler, or hidden
  alias is admitted.
- **PR18 — SDK bindings:** add Rust, TypeScript, and Python SDK BindingIds only
  with shipped typed methods and behavioral/lifecycle conformance fixtures.
  Every supported public operation, including operations accepted before
  PR17, must receive bindings in all three SDKs; absence from a generated list
  or late milestone is not permission to omit a family. PR12 may describe
  future SDK availability as unavailable protocol metadata but cannot
  advertise an unimplemented SDK method.

## Capability and retrieval contracts

PR11 adds the following catalog-owned records. The Rust names are internal
contract names, not frozen PR18 command, MCP-tool, route, or SDK method names.

```rust
pub struct CapabilityManifestV1 {
    pub capability_id: CapabilityId,
    pub use_case_id: UseCaseId,
    pub request_schema: SchemaRef,
    pub result_schema: SchemaRef,
    pub effect: EffectClass,
    pub scope: ScopeRequirement,
    pub authority: AuthorityRequirement,
    pub denied_disclosure: DeniedDisclosurePolicy,
    pub lifecycle: LifecycleClass,
    pub streaming: StreamingContract,
    pub cancellation: CancellationContract,
    pub deadline: DeadlineContract,
    pub pagination: Option<PaginationContract>,
    pub idempotency: IdempotencyContract,
    pub authority_revalidation: RevalidationContract,
    pub reconciliation: ReconciliationContract,
    pub receipt: ReceiptContract,
    pub terminal_states: TerminalStateContract,
    pub availability: AvailabilityContract,
    pub binding_ids: Vec<BindingId>,
    pub profile_eligibility: Vec<ProfileId>,
    pub required_features: Vec<FeatureId>,
}

pub struct RetrievalPrimitiveManifestV1 {
    pub capability_id: CapabilityId,
    pub family: RetrievalFamily,
    pub retriever_id: RetrieverId,
    pub request_schema: SchemaRef,
    pub evidence_packet_schema: SchemaRef,
    pub coverage_contract: CoverageContractRef,
    pub omission_contract: OmissionContractRef,
    pub scoring_contract: ScoringContractRef,
    pub contribution_contract: ContributionContractRef,
    pub deterministic_order: SortContract,
    pub default_page_size: u32,
    pub maximum_page_size: u32,
    pub temporal_modes: Vec<TemporalMode>,
    pub cancellation_points: Vec<CancellationPoint>,
    pub deadline_behavior: DeadlineBehavior,
}
```

A primitive executes one bounded retrieval contract and returns the Plan 09
evidence packet; it cannot invoke a model, select another capability, synthesize
a plan, recursively call the dispatcher, or create a Plan 32 run. Primitive
manifests contain no planning field or extension point. Plan 24 consumes
immutable packets and owns task decomposition and declared fan-out; Plan 32
alone admits and executes parallel fan-out. Neither authority is hidden inside
a catalog entry or retriever.

The PR11/PR12 core mostly LLM-free primitive families are deliberately narrow:

- symbol identity: name/pattern search, exact identity, qualified-name,
  signature, implementation, and type-hierarchy reads;
- source evidence: bounded lines, body, outline, module API, and file metadata;
- graph evidence: callers, callees, call chain, file dependents, bounded
  impact, and dependency depth;
- test evidence: test mapping and affected-test attribution;
- temporal evidence: authorized session lookup, message search, shipped
  current/as-of session narrative, and exact Plan 13 anchor expansion; and
- operational evidence: catalog, configuration, project, storage/runtime, and
  health inspection where the owning application use case is already shipped.

PR17 extends those families with TaskId/WorkItemId lookup and compact context,
current/as-of/evolution/forensic task history, thread/attempt traversal, and
auxiliary-provider capability inspection only after the Plan 24/32 application
handlers ship. Those operations are absent from PR12 profiles and discovery.

Literal grep, AST matching, semantic symbol lookup, source reads, graph
traversal, test attribution, temporal retrieval, and context assembly remain
separate manifests because their authority, temporal, coverage, scoring, and
omission semantics differ. Context assembly is an explicit consumer capability
over evidence packets, never an alias that adds a hidden model call. Each
manifest declares whether results can score matches; `ScoreKind` and
calibration semantics come from Plan 09 and cannot be inferred from a generic
floating-point field.

`DeniedDisclosurePolicy::Indistinguishable` is mandatory for resource-addressed
lookup, cursor resume, anchor expansion, task history, and provider inspection.
Discovery filters an unauthorized capability before rendering, while direct
invocation maps absent, unauthorized, and scope-hidden resource identity to the
same application problem contract. Catalog metadata cannot reveal whether a
hidden resource, administrative binding, provider, page, or anchor exists.
Direct invocation of an unauthorized or profile-hidden BindingId/name is
indistinguishable from an unknown operation and cannot return alternative
binding guidance.

Read capabilities use `EffectClass::Read` and `ReceiptContract::Operation`;
preview capabilities use `EffectClass::Preview` and
`ReceiptContract::Operation`, validated against Plan 09 `PreviewResult`;
commands name their exact effect class and `ReceiptContract::DurableEffect`. A
command manifest without an idempotency field, authority revalidation point,
deadline/cancellation contract, reconciliation state, and typed effect receipt
fails snapshot validation. `Cancelled`, `TimedOut`, `Failed`, `EffectUnknown`,
`Partial`, and `Completed` are distinct terminal contracts.

## Files and ownership

PR11 creates these Plan-08-owned files:

- `Cargo.toml` — workspace membership and dependency declaration for the new
  crate, without adapter/application dependencies;
- `crates/tracedecay-tool-catalog/Cargo.toml` — feature-minimal crate manifest;
- `crates/tracedecay-tool-catalog/src/lib.rs` — narrow public re-exports only;
- `crates/tracedecay-tool-catalog/src/id.rs` — `CapabilityId`, `UseCaseId`,
  `BindingId`, `RetrieverId`, `ProfileId`, and stable serialization;
- `crates/tracedecay-tool-catalog/src/manifest.rs` — capability, effect,
  authority, lifecycle, streaming, idempotency, revalidation,
  cancellation, deadline, reconciliation, terminal-state, and receipt records;
- `crates/tracedecay-tool-catalog/src/retrieval.rs` — retrieval-family and
  primitive-manifest records with explicit packet-component references and no
  planning hook;
- `crates/tracedecay-tool-catalog/src/binding.rs` — CLI/MCP/HTTP/LSP/dashboard
  binding references and protocol revision ranges;
- `crates/tracedecay-tool-catalog/src/profile.rs` — explicit profile membership,
  schema/routing budgets, and reviewed per-profile ceilings;
- `crates/tracedecay-tool-catalog/src/snapshot.rs` — immutable contribution
  assembly and deterministic lookup; and
- `crates/tracedecay-tool-catalog/src/validation.rs` — duplicate, handler,
  schema, authority/effect, pagination, receipt, dependency, and profile-budget
  validation.

The owning product PR places contributions beside their use cases in
`crates/tracedecay-application/src/retrieval/catalog.rs`,
`crates/tracedecay-application/src/edit/catalog.rs`,
`crates/tracedecay-application/src/git/catalog.rs`,
`crates/tracedecay-application/src/feedback/catalog.rs`,
`crates/tracedecay-application/src/work/catalog.rs`, and
`crates/tracedecay-application/src/workflow/catalog.rs`; Plan 09 and the owning
vertical PR own those files and handler references. Plan 21 owns adapter
bindings and renderers. Plans 05/13/23 own query, anchor, and temporal kernels.
Plan 24 owns task graph, decomposition, and fan-out intent. Plan 32 owns
workflow/auxiliary parallel execution, leases, attempts, runtime cancellation,
runtime effects, and runtime receipts; Plan 09 retains operation-specific
`EditTransaction` and `GitIndexTransaction` authority. Plan 08 imports only
contract types needed to describe those owners and never imports their
implementations.

`src/catalog_composition.rs` is the root-composition-owned assembly point. The
dependency direction is acyclic: `tracedecay-tool-catalog` defines inert record
and builder types; `tracedecay-application` depends on those types and exports
typed contributions plus the closed validation-only
`ApplicationHandlerDescriptors` from
`crates/tracedecay-application/src/handlers.rs`; the root crate depends on both,
assembles the immutable snapshot, and validates every `UseCaseId` against those
descriptors. Descriptors contain only UseCaseId and request/result schema refs,
not function pointers, dynamic dispatch, invocation, policy, or runtime lookup;
ordinary typed service methods remain the sole execution path. The catalog
crate never imports application, and application never imports the root
composition file.

Tests are fixed at:

- `crates/tracedecay-tool-catalog/tests/manifest_contract.rs`;
- `crates/tracedecay-tool-catalog/tests/retrieval_contract.rs`;
- `crates/tracedecay-tool-catalog/tests/snapshot_contract.rs`;
- `crates/tracedecay-tool-catalog/tests/profile_budget.rs`; and
- `tests/catalog_composition_contract.rs` for root contribution-to-handler
  validation;
- `tests/architecture_boundaries.rs` for forbidden dependencies and facade
  growth.

The facade budget remains explicit: `lib.rs` only re-exports reviewed records
and lookup entry points; it contains no assembly, execution, rendering, policy,
or compatibility logic. Host packaging keeps one semantic integration catalog
and independently installable context, work, and operator MCP companions over
the same daemon/catalog/types. Every workflow has one primary discovery
surface; skills are not duplicated as MCP prompts. Deferred tool discovery is
an optimization, never justification for exceeding an eager client's reviewed
schema/routing budget or for inventing a universal tool-count limit.
`ProfileDefinition` stores hard `maximum_bindings`, `maximum_schema_bytes`, and
`maximum_routing_tokens` values per profile/companion. The checked-in values in
`profile.rs` are the thresholds; any increase requires Plan 08 and Plan 21
owner approval plus an eager-client routing fixture in `profile_budget.rs`.
There is no aggregate or universal count shared by all profiles.

## Runtime composition and migration

1. **Crate contract gate:** add workspace/manifests, then land `id.rs`,
   `manifest.rs`, `retrieval.rs`, and serialization tests without application
   or adapter imports. Run
   `cargo test -p tracedecay-tool-catalog --test manifest_contract --test retrieval_contract`.
2. **Application contribution gate:** Plan 09 lands executable handlers and
   contribution functions against the inert Plan 08 record types. No snapshot
   assembly occurs inside either crate.
3. **Root composition gate:** land profiles, builder validation, and
   `src/catalog_composition.rs`. Every shipped contribution must resolve one
   real Plan 09 handler and one request/result schema; startup rejects an
   incomplete snapshot. Run
   `cargo test -p tracedecay-tool-catalog --test snapshot_contract --test profile_budget`
   and `cargo test --test catalog_composition_contract`.
4. **Primitive gate:** register the symbol, source, graph, test, temporal, and
   operational primitive families only as their owning use cases ship. Contract
   tests prove deterministic order, bounded pages, declared temporal modes,
   explicit omissions/contributions, no planner/model/dispatcher dependency,
   and indistinguishable authorization failures.
5. **Binding gate:** PR12 may switch CLI/MCP/HTTP/LSP discovery to the immutable
   snapshot only after Plan 21 parity fixtures map every enabled BindingId to
   the same CapabilityId, schemas, effect, cursor, deadline/cancellation, and
   receipt contract.
6. **Deletion gate:** delete the legacy `admin_cli`/handler registries and
   duplicate discovery metadata only after direct old-versus-new compatibility
   fixtures pass for supported names. Do not retain a shadow registry or
   generated inventory.
Plan 24/32 contributions enter the runtime snapshot only with their shipped
typed application handlers. SDK BindingIds enter only with shipped official
methods and direct conformance. Internal capability families and existing
CLI/MCP bindings do not reserve or freeze later public SDK vocabulary. Active
executors cannot self-grade, decide proposals, create undeclared fan-out, or
invoke runtime effects outside Plan 32.

The final PR11 checks are
`cargo test -p tracedecay-tool-catalog --all-features`,
`cargo check -p tracedecay-tool-catalog --all-features`, and
`cargo test --test architecture_boundaries`. The PR12/PR17 gate additionally
runs `cargo test --test mcp_suite`.

## Acceptance

- PR11 unit tests cover stable ID serialization, immutable snapshots, duplicate/conflict rejection, profile ceilings, explicit absence, deprecation, availability, and deterministic lookup.
- PR11 integration tests prove every catalog entry resolves to one real
  application handler with matching scope, effect, and schema contracts.
- Policy tests cover routing among available entries, missing capability, denied scope, stale availability, and no silent substitution.
- PR12 parity tests invoke representative read, write, administrative, streaming, and long-running use cases through CLI, MCP, HTTP, and LSP adapters and compare typed results before rendering.
- PR12 feedback parity tests call the canonical diagnostics/impact readers
  through CLI, MCP, and HTTP, then negotiate the TraceDecay experimental LSP
  capability and obtain real diagnostics, impact, affected-test, and
  test-result projections. They compare exact scope, generation, coverage,
  omissions, and authorized opaque-handle expansion before rendering; reject
  absent handlers, unnegotiated calls, arbitrary methods/payloads, and any LSP
  data ownership.
- PR13 provider-contribution tests prove GitHub review, CI localization, and
  proximity appear through the unchanged reader contract only when their
  cataloged typed handlers are callable, with typed unavailable state
  otherwise.
- PR12 Git parity tests compare CLI and MCP `git_preview`/`git_apply` semantic
  results before rendering, including Markdown/JSON equivalence, stale CAS,
  conflicting index state, idempotent replay, and receipt identity.
- PR14 contract tests add dashboard binding, dashboard actions, and dashboard parity on the same typed requests and CapabilityIds.
- PR17 catalog tests prove every Plan 24/32 binding resolves to one application
  handler, Kanban and other lenses expose no mutation-only shortcut, and no
  catalog entry parses developer-roadmap Markdown or creates a second
  scheduler/effect path. Tests also prove assessment/recommendation/report
  capabilities are read-only, proposal decisions are explicit versioned
  commands, and active-executor profiles cannot grade themselves or accept a
  split/merge/resize/re-route proposal.
- Task retrieval tests prove compact summaries expand losslessly through
  Plan 13 anchors, authorization is rechecked on every page/hydration, and no
  catalog binding introduces task, query, scheduling, policy, or Doctor
  semantics beside its owning plan.
- PR17 provider-descriptor fixtures cover missing executables, version drift,
  unsupported model/reasoning/sandbox/event/resume capabilities, explicit
  app-server-versus-CLI fallback eligibility, native Claude Code selection,
  deterministic lookup, and rejection of shell-string, recursive-dispatch, or
  graph/runtime-mutation capabilities.
- PR18 SDK parity tests require every supported public operation—not only
  PR17 task/runtime additions—to resolve through shipped Rust, TypeScript, and
  Python methods to the canonical application handler, with matching
  authorization, paging/streaming, problems/retry directives, cancellation,
  receipts, reconnect/resume, and unavailable/partial lifecycle semantics.
- Discovery tests prove compact profiles stay bounded and administrative/private capabilities are filtered correctly.
- Compatibility tests cover only currently supported names and typed guidance for retired names; no frozen total-count assertion is allowed.
- Architecture tests reject source parsers, generators, generated inventories, plan/workflow dependencies, execution logic, storage, transport implementations, and UI code from tracedecay-tool-catalog.
- Repository checks verify no public adapter has a handler-local query, policy, persistence, or authorization path that bypasses the cataloged application use case.

## Refactoring workflow boundary

The catalog owns discovery and typed definitions for refactoring capabilities,
not a second refactoring engine. Read-only `tracedecay_rename_preview`, existing
symbol/string edit primitives, callers/reference discovery, diagnostics, and
shipped apply operations remain independently callable base tools. Composed
refactoring workflow bundles reference those canonical tools instead of copying
handlers or schemas.

[Workspace refactoring and API migration](34-workspace-refactoring-and-api-migration.md)
owns the behavior and acceptance contract for apply-grade previews, atomic symbol
rename, and semantic API migration. In particular:

- pure symbol rename and compatibility-aware API promotion are separate operations;
- apply tools consume immutable preview/plan identifiers and digests and fail closed on stale evidence;
- catalog capability metadata is granular by language and symbol/site kind;
- unsupported or not-yet-shipped apply operations are never advertised as callable;
- human-readable and JSON results render one typed changed/unchanged/skipped/blocked manifest; and
- neutral adoption evals must prove that agents preview before apply and choose
  semantic migration rather than rename when compatibility or coordinated
  definition changes are required.
