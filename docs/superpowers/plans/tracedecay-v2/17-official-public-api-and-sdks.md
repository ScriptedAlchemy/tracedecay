# TraceDecay V2 Official Public API and SDK Plan

> **For agentic workers:** implement this plan only after the V2 domain, query, policy, tool-catalog, application, and API contracts in plans 01, 05, 06, 08, 09, and 10 are stable enough to generate against. Use test-first, reviewable PR slices; this document adds no separate business-logic layer.

**Goal:** Make TraceDecay's full supported capability surface directly queryable by agents and integrations through one stable, documented, contract-first API with first-party Rust, TypeScript, and Python SDKs, while preserving semantic parity with CLI, MCP, HTTP, dashboard, exports, and live streams.

**Architecture:** `tracedecay-application` remains the sole use-case boundary and `tracedecay-tool-catalog` remains the capability registry. `tracedecay-api` publishes the official HTTP/SSE/OpenAPI contract; generated schema packages and small hand-written runtimes expose it as idiomatic SDKs. CLI and MCP use the same application/catalog definitions but do not loop through HTTP. Every binding is verified against the same semantic fixtures, typed scopes, stable anchors, errors, coverage, and replay rules.

**Initial deployment:** Local-first. A user-owned Unix-domain socket or authenticated loopback HTTP endpoint is supported. Remote or hosted service operation is not assumed by this plan and must not weaken the local trust, privacy, or authorization contract.

**Publication baseline (2026-07-10):** `origin/master` `6c4b8b91`; #407/#410/#411/#413/#414/#415/#416/#417/#419/#420/#422/#423/#424 merged, #418 open. Regenerate contract/capability fixtures before implementation. The official surface includes ordinary-profile Hermes, current #410 message views, #411 doctor authority, #414/#419 race-safe `move_symbol`, #417 typed identity split, #415 release-integrity gates, #420 proxy-before-store/reconnect/no-write-replay semantics, #422 negotiated `tools.listChanged` daemon-generation refresh, #423 fact-rank/counter semantics, and #424 exact aggregate-before-sample analytics.

---

## 1. Relationship to the Other V2 Plans

This plan complements, rather than replaces, the following ownership:

- `01-domain-crate.md` owns canonical IDs, entities, events, scopes, evidence, provenance, privacy labels, versions, and stable error primitives.
- `01-domain-crate.md` owns the bounded `TraceQueryV1` AST/value/schema contract; `05-query-crate.md` owns parsing, validation, canonicalization, planning, ranking evidence, frozen snapshots, distributed cursors, partial coverage, export rows, and live deltas.
- `06-policy-crate.md` owns capability routing and deterministic hint/policy evaluation.
- `08-tool-catalog-crate.md` owns use-case identity, input/output schema references, binding metadata, availability, effects, version state, and discovery copy.
- `09-application-crate.md` owns authorization, use-case orchestration, command semantics, jobs, idempotency, and audit receipts.
- `10-api-crate.md` owns Axum HTTP/SSE framing, loopback security, the checked OpenAPI artifact, and dashboard TypeScript client.
- `13-research-provenance-and-context-anchors.md` owns the cross-plan research manifest and durable evidence recipes.
- `15-search-quality-evaluation-and-retrieval-research.md` owns relevance judgments and retrieval evaluation.
- `16-cross-project-repository-worktree-scope.md` owns the resolver/routing UX and cross-repository regression corpus while consuming the exact domain selector.
- `18-secret-detection-redaction-and-private-data-safety.md` owns the sanitizer, taint-state wrappers, privacy status semantics, and forbidden-sink conformance. Public contracts only expose eligible content or explicit redacted/denied/unknown states.
- `20-configuration-control-plane.md` owns configuration descriptors, resolution, provenance, history, impact, credentials, and autonomous-curation policy; generated SDK/API surfaces expose those exact use cases without inventing client defaults.
- `21-cli-mcp-tool-surface-and-output-unification.md` owns the generated binding and presentation parity contract; public SDK JSON shares its sealed views, typed outcomes, pages, retrieval anchors, notices, freshness, and provenance without scraping human Markdown or CLI envelopes.
- `22-incremental-context-scout-and-suggestion-envelopes.md` owns scout status/replay/feedback/system-control and suggestion-envelope semantics; SDKs cannot trigger delivery or bypass read-only lab guards.
- `23-session-lcm-temporal-retrieval-and-evaluation.md` owns temporal search/context/lineage/replay/evaluation semantics; SDK modes, anchors, cursors, coverage, no-answer reasons, and hydration are generated from that same contract.
- `24-canonical-task-plan-graph-and-multi-agent-executor.md` owns initiative/plan/work-item/executor/scheduler/context-packet semantics and the many-host adapter protocol; this plan generates supported orchestration/monitoring clients without turning an SDK into a scheduler, route selector, lease authority, or board database. The task/executor read and command surface is the inventory in plan 09 §§9–10 and plan 10 §8: reads are GET and every mutation is a POST command envelope.
- This plan owns the declaration that the HTTP contract is an **official public integration surface**, agent-oriented discovery and documentation, first-party Rust/TypeScript/Python packages, direct-client lifecycle, compatibility policy, and public conformance program.

There is no independent "SDK API" business layer. SDKs serialize generated request types, call the official transport, deserialize generated response types, and provide bounded ergonomics such as pagination and reconnect helpers. They may not recreate ranking, scope resolution, command authorization, replay classification, or retry policy by guesswork.

## 2. User and Agent Outcomes

An agent should be able to:

1. Discover the endpoint and current protocol without scraping logs or dashboard HTML.
2. Mint or receive a least-privilege, time-bounded credential through an explicit user-approved path.
3. Ask what TraceDecay can do and receive catalog-backed methods, schemas, cost/freshness classes, examples, required scopes, and current availability.
4. Resolve "rspack", a repository path, a worktree path, a branch, PR, session, agent, or `All` into canonical typed scope IDs without silently querying the active project.
5. Query code, Git, sessions, messages, agents, workflows, goals, memory, skills, automation, analytics, hints, costs, and health through one consistent envelope.
6. Traverse the Graph-of-Graphs from any stable entity to related entities while seeing evidence, confidence, time validity, source watermarks, and missing coverage.
7. Enumerate large stores and cross-project results using opaque resumable cursors rather than result caps or ephemeral response handles.
8. Subscribe to authorized changes, reconnect deterministically, detect gaps, and resynchronize.
9. Replay a historical or synthetic hint/search decision in a no-write lab and compare historical versus current policy without altering live analytics, facts, counters, claims, or outcomes.
10. Preview and, only with explicit mutation authority, execute a supported command with idempotency, optimistic version checks, audit, and a durable operation receipt.
11. Recover from every typed error using a machine-readable retry/restart/current-binding directive rather than prose parsing.
12. Cite stable TraceDecay retrieval anchors in its own plan, report, PR, or handoff so a later agent can recover the exact evidence.
13. Query or operate one cross-repository initiative, assign bounded work sets to Codex and Claude routes with explicit provider/model/reasoning-effort/tool/budget constraints, inspect dependencies/packets/runs/outcomes, and subscribe to task events without MCP or dashboard mediation.

Human developers should be able to accomplish the same work with `curl`, generated documentation, or an SDK without learning MCP wire details or reverse-engineering the dashboard.

## 3. Non-Goals and Explicit Boundaries

- No public SQL, FTS syntax, raw SQLite connection, arbitrary graph-store query language, filesystem traversal, shell execution, or renderer/plugin code upload.
- No hidden chain-of-thought endpoint. Only provider-exposed, retained, access-authorized reasoning summaries/artifacts and explicit coverage markers may be returned.
- No unbounded "dump everything" JSON response. Enumeration, graph expansion, search, timeline, export, and event delivery are all bounded and cursor/stream based.
- No automatic remote bind, cloud account, multi-tenant identity server, or internet exposure in the initial V2 release.
- No SDK-specific behavior. Rust, TypeScript, Python, raw HTTP, CLI JSON, MCP JSON, dashboard, export, and SSE snapshot must agree before presentation differences.
- No stale-client behavior emulation. Data migration and rollback may preserve user data; they do not keep obsolete live names, schemas, or semantics executing.
- No general GraphQL surface in V2. The bounded typed query/graph operations are easier to cost, authorize, version, and replay.
- No WebSocket requirement. Request/response, SSE, bounded NDJSON, and asynchronous operation polling cover the initial official surface.
- No agent credential minted merely because a local process can connect. Endpoint reachability is not authorization.
- No "helpful" SDK fallback from an explicit cross-project scope to the current project.

## 4. Public Contract Artifacts and Repository Layout

The contract build produces deterministic, checked artifacts from one registry snapshot:

```text
crates/tracedecay-api/
├── contract/
│   └── tracedecay-contract-ir.v1.json  # canonical checked contract IR snapshot (Section 5.1)
├── src/openapi/                     # hosted by plan 10; generated by this plan's contract-IR pipeline
│   └── generated.json               # canonical checked public OpenAPI 3.1
├── openapi/tracedecay-v2.yaml       # generated review/release rendering
└── schemas/
    ├── catalog.schema.json
    ├── scope-selector.schema.json
    ├── trace-query.schema.json
    ├── graph-query.schema.json
    ├── event-stream.schema.json
    ├── api-problem.schema.json
    └── retrieval-anchor.schema.json

crates/tracedecay-client/
├── Cargo.toml
├── src/{lib,client,transport,pager,events,operation,error}.rs
├── tests/{contract,live_fixture,compile_examples}.rs
└── examples/{discover,search,graph,timeline,replay}.rs

packages/tracedecay-client/
├── package.json
├── src/{generated,client,pager,events,operation,error}.ts
├── test/{contract,live-fixture,examples}.test.ts
└── examples/{discover,search,graph,timeline,replay}.ts

python/tracedecay-client/
├── pyproject.toml
├── src/tracedecay/{generated,client,pager,events,operation,errors}.py
├── tests/{test_contract,test_live_fixture,test_examples}.py
├── examples/{discover,search,graph,timeline,replay}.py
└── py.typed

docs/api/
├── index.md
├── quickstart/{curl,rust,typescript,python}.md
├── concepts/{identity,scope,coverage,consistency,anchors,replay,commands}.md
├── capabilities/                    # generated catalog pages with curated guides
├── recipes/{cross-project,graph-of-graphs,agent-timeline,hint-replay,search-eval}.md
├── errors.md
├── versioning.md
├── security.md
├── limits.md
└── migration.md

tests/public_api_conformance/
├── fixtures/
├── semantic/
├── security/
├── sdk/
├── streams/
├── compatibility/
└── runner.rs
```

Generated files carry:

- source Git commit;
- API major/minor/patch;
- domain schema digest;
- application use-case registry digest;
- capability catalog digest;
- OpenAPI generator and SDK generator versions;
- generation timestamp excluded from byte-stability comparisons or normalized to `SOURCE_DATE_EPOCH`;
- a "generated, do not hand edit" marker and the exact check command.

CI generates twice and fails if the second output differs or if the checked tree is stale.

`docs/api/capabilities/` pages are generated by the Section 5.1 IR pipeline as the public-API reference rendering; plan 08's `generated/capability-reference.md` remains the internal catalog rendering of the same registry, and neither document duplicates the other's role.

## 5. Contract-First Source of Truth

### 5.1 Generation pipeline

```text
domain schemas + application use cases + capability catalog
                         │
                         ▼
             canonical contract IR snapshot
                │          │          │
                ▼          ▼          ▼
             OpenAPI    JSON Schema  binding manifests
                │          │          │
                └──────┬───┴──────┬───┘
                       ▼          ▼
               SDK type trees   docs/catalog pages
                       │          │
                       └────┬─────┘
                            ▼
                  conformance fixtures
```

Generation authority (single source): plan 17's contract IR is the only source of generated public contract artifacts. Pipeline: domain schemas + application use-case registry + plan 08 capability catalog → canonical contract IR snapshot (`crates/tracedecay-api/contract/tracedecay-contract-ir.v1.json`, owned by plan 17) → generated OpenAPI 3.1 (`crates/tracedecay-api/src/openapi/generated.json`, hosted by plan 10), the review rendering `crates/tracedecay-api/openapi/tracedecay-v2.yaml`, and the public JSON Schemas (`crates/tracedecay-api/schemas/*.schema.json`) → plan 10's Axum adapters conform to the IR-generated document, with utoipa reflection retained as validation only (CI regenerates the utoipa-derived document and fails unless it is semantically identical to the IR-generated artifact) → the generated TypeScript schema core at `packages/tracedecay-client/src/generated/` is produced from the IR-generated OpenAPI and hosted per plan 10, while plan 17 owns SDK packaging and conformance. The capability catalog remains the registry of record for capability/binding identity; the contract IR is its frozen public projection, and no plan or adapter maintains a second route registry.

The canonical contract IR snapshot is a named, checked artifact, not an in-memory build step:

- Path: `crates/tracedecay-api/contract/tracedecay-contract-ir.v1.json`.
- Format: canonical JSON (UTF-8, sorted object keys, LF line endings, no floats) with one top-level `ContractIrV1` object carrying `ir_version` (integer, bumped only for IR-format changes), `protocol_version`, `source_digests` (domain schema digest, application use-case registry digest, capability catalog digest, generator versions), and `use_cases` sorted by `use_case_id`.
- Each `use_cases[]` entry carries exactly the fields listed below; unknown fields fail generation.
- Uniqueness: `use_case_id` is the primary key; a duplicate ID or duplicate HTTP binding fails the build.
- Lifecycle: regenerated deterministically from the registry snapshot; CI generates twice and diffs; hand edits are rejected by the generated-file marker; IR diffs are reviewed like code and drive the compatibility manifest.

The contract intermediate representation contains, for every public use case:

- stable `UseCaseId`, semantic version, lifecycle state, owning domain, and summary;
- exact request, response, event, error, and retry schema references;
- allowed typed scope kinds and whether multiple roots/exclusions are legal;
- read, preview, mutate, destructive, or administrative effect class;
- required authorization grants, privacy domain, sensitivity, and audit behavior;
- idempotency, optimistic version, operation/job, and compensation semantics;
- pagination, streaming, export, and maximum inline result behavior;
- consistency/freshness requirements and expected partial-result behavior;
- cost/latency class, default/max budgets, rate-limit bucket, and availability prerequisites;
- bindings to HTTP operation, SDK method, CLI command, MCP tool, dashboard action, hook route, and export profile;
- stable examples containing synthetic data only;
- replacement/current-binding details when a contract is removed.

Compile/generation fails on duplicate IDs, undocumented routes, missing authorization, missing stable error codes, unbounded collections, transport-only fields leaking into domain schemas, or a binding without semantic fixtures.

### 5.2 OpenAPI and JSON Schema rules

- Publish OpenAPI 3.1 and JSON Schema 2020-12.
- Every union uses an explicit discriminator; SDKs never infer a variant from missing fields.
- IDs use named string formats such as `tracedecay-entity-id`, not plain undocumented strings.
- Timestamps use RFC 3339 UTC and retain source precision/uncertainty metadata where relevant.
- Durations use integer microseconds or named ISO-8601 fields consistently, never ambiguous numbers.
- Integer counts use 64-bit-safe representations; TypeScript generation must not silently narrow values above `Number.MAX_SAFE_INTEGER`.
- Optional and nullable are distinct. Unknown, unavailable, redacted, not-applicable, and zero are distinct states.
- `additionalProperties` is disabled for closed request objects. Forward-compatible event/provider payloads live only in explicit `extensions`/opaque fields with size/privacy limits.
- Examples and descriptions are generated from synthetic fixtures and secret-scanned.
- Every operation declares all normal, partial, auth, scope, version, limit, conflict, and internal problem responses.

## 6. Version and Compatibility Contract

### 6.1 Version identities

The public contract exposes separate identities:

- **API major:** path namespace, initially `/api/v2`.
- **Protocol version:** exact wire/semantic compatibility version, returned by discovery and every response.
- **Catalog digest/version:** available use cases and binding definitions.
- **Schema digest:** canonical request/response/event definitions.
- **Data/projection versions:** returned in snapshot/freshness/coverage metadata, not confused with protocol compatibility.
- **Policy/ranking/model versions:** attached to explain/replay results, not used as API version substitutes.

Clients send their supported protocol range and generated schema digest through standard client headers. The server returns its selected protocol and digests in response metadata/headers. If the client's supported range does not intersect the server's, the server performs no semantic work and rejects with HTTP 426 and a stale-client registry code — `client_update_required` when the client is older, `daemon_restart_required` when a newer daemon/binary is installed but not yet serving — carrying minimum/current protocol, current binding, and the exact update/restart command; it never guesses a protocol.

### 6.2 Change policy

- Additive optional response fields and new enum variants require generated clients to retain/represent unknown values safely; they do not permit changing existing meaning.
- Request-side evolution is equally explicit: request objects stay closed (`additionalProperties: false`), servers reject unknown named fields, and forward-compatible request additions travel only in each request's declared bounded `extensions` slot, which servers accept and ignore when unrecognized. A client may send a new named request field only once the server's advertised protocol version includes it; anything else requires a protocol version bump. This one rule replaces any per-schema discretion in transport plans (plan 10 §7.3 cites it).
- New required request fields, removed fields, changed defaults, changed ordering, changed error semantics, or changed effect behavior require a new protocol version and usually a new API major.
- Capability lifecycle is explicit: `experimental`, `current`, `scheduled_for_removal`, `removed`. Experimental use requires an opt-in header/grant and never silently becomes stable.
- Deprecation within a current protocol may warn and provide the exact current binding, but the deprecated binding has a declared short removal release and cannot change behavior to imitate a replacement. The warning channel is typed: a `capability_deprecated` `ApplicationWarning` in `meta.warnings` carrying the current binding and the removal release/date, plus a standard HTTP `Sunset` header; SDKs surface both rather than parsing prose.
- At cutoff, obsolete clients/routes/tools receive a typed stale-client response from this plan's contract-IR error registry — `client_update_required`, `daemon_restart_required`, or `capability_replaced { current_binding }` — with HTTP 426 where appropriate and exact restart/update/current-route/current-SDK guidance. They are not proxied to legacy handlers or translated with guessed defaults.
- Rollback restores a prior **compatible V2 server and data snapshot** under an explicit operator receipt. It never revives obsolete live V1 names as fallback behavior.
- Support windows are published as dates/releases in a machine-readable compatibility manifest. Clients must not infer support from a successful TCP connection.

## 7. Endpoint and Client Discovery

### 7.1 Local endpoint lifecycle

Add an explicit operator surface:

```text
tracedecay api serve
tracedecay api status --json
tracedecay api token create --read-only --ttl 1h --scope <selector>
tracedecay api token list
tracedecay api token revoke <token-id>
tracedecay api openapi --output <path>
tracedecay api docs
```

`api status --json` returns only safe discovery material: endpoint kind, socket path or loopback origin, server/protocol version, health, catalog/schema digest, authentication method, docs/OpenAPI path, and current profile ID. It never returns bearer/session/CSRF secrets. `api token create|list|revoke` bind the audited application commands `auth.tokens.create/list/revoke` (plan 09 §10); the per-launch bootstrap bearer may execute only `auth.tokens.create` for the initial admin-class token (plan 10 §10.2).

Discovery precedence for SDKs is explicit:

1. caller-supplied endpoint and credential;
2. `TRACEDECAY_API_ENDPOINT` and a supported credential provider, never a token embedded in the endpoint URL;
3. user-owned runtime discovery file with mode `0600`, process identity, expiry, endpoint, and public digests;
4. deterministic default Unix socket or loopback status probe;
5. typed `endpoint_not_found` with the exact command to start/check the service.

SDKs never scan processes, ports, parent directories, dashboards, MCP config, or transcript files to guess an endpoint.

### 7.2 Bootstrap endpoints

- `GET /api/v2/meta` returns protocol, server version, instance/profile identity, catalog/schema digests, time, health summary, limits profile, and current compatibility policy. It is authenticated — plan 10's rule that every route except static assets and the one-time bootstrap exchange requires authentication holds without exception; endpoint-without-credential discovery uses `tracedecay api status --json` or the `0600` runtime discovery file (Section 7.1), never an anonymous HTTP handshake.
- `GET /api/v2/openapi.json` returns the exact checked contract for the selected current protocol.
- `GET /api/v2/schemas/{digest}/{name}` returns an allowlisted public schema artifact.
- `GET /api/v2/capabilities` provides cursor-based capability discovery, not one unbounded registry blob.
- `GET /api/v2/bindings/{use_case_id}` provides current CLI/MCP/HTTP/SDK/dashboard bindings and prerequisites.
- `POST /api/v2/scopes:resolve` resolves one or many human locators into canonical scopes with ambiguity and coverage.

## 8. Capability Parity and Agent-Friendly Discovery

Every current V2 application use case must have exactly one catalog disposition:

- public and bound;
- public but unavailable with typed prerequisite/remediation;
- internal implementation detail;
- destructive/administrative and explicit-grant only;
- migration-only;
- removed with a current replacement;
- intentionally unsupported with rationale and review owner.

There is no accidental API surface from Axum routes and no undocumented CLI/MCP-only capability. A public capability may omit a particular transport only when the catalog declares why; for example, a browser-only bootstrap handshake or a local host hook callback.

Capability discovery returns:

```json
{
  "use_case_id": "usecase.query.search-universal",
  "version": "2.0.0",
  "summary": "Search authorized TraceDecay evidence across selected scopes",
  "effects": "read",
  "scopes": ["all", "collection", "repository", "project", "worktree", "session", "agent"],
  "availability": {"state": "available", "requirements": []},
  "cost_class": "interactive",
  "freshness": "frozen_or_eventual",
  "pagination": "opaque_cursor",
  "bindings": {
    "http": "POST /api/v2/search",
    "rust": "Client::search",
    "typescript": "client.search",
    "python": "client.search",
    "cli": "tracedecay search",
    "mcp": "tracedecay_search"
  }
}
```

Agent-oriented descriptions are concise routing metadata, not a second prompt-only catalog. Long tutorials live in docs; short catalog entries include when to use, when not to use, scope/freshness traps, estimated cost, and a synthetic example.

The conformance gate compares the complete generated inventory with:

- application registry;
- HTTP routes/OpenAPI operation IDs;
- Rust/TypeScript/Python SDK method manifests;
- CLI command/flag manifest and `tool` bindings;
- MCP tool schemas/names and JSON results;
- dashboard action manifest;
- supported hook callback catalog.

Missing, duplicated, or semantically divergent binding blocks release.

## 9. Typed ScopeSelectorV2

Scope must be identical across API, SDKs, CLI, MCP, dashboard, saved views, exports, and retrieval anchors. `project_key` and a process's active checkout are internal/provider locators, not the public identity model.

### 9.1 Selector model

```rust
pub struct ScopeSelectorV2 {
    pub version: u16,
    pub roots: Vec<ScopeRootV2>, // validated nonempty
    pub exclude: Vec<ScopeRootV2>,
    pub time: Option<TimePredicate>,
    pub activity_attribution: ActivityAttributionModeV2,
    pub coverage: ScopeCoveragePolicyV2,
    pub freshness: ScopeFreshnessPolicyV2,
    pub traversal: ScopeTraversalV2,
    pub ambiguity: ScopeAmbiguityPolicyV2,
    pub limits: ScopeLimitsV2,
}

pub enum ScopeTargetV2 {
    Canonical(EntityRef),
    Locator(ScopeLocatorV2),
}
```

This is the exact plan 01 domain type, not an SDK variant. `ScopeRootV2` variants are `CurrentInvocation`, `AllAuthorized { profile_id }`, `Profile`, `ProjectSet`, `Collection`, `Repository`, `Project`, `Checkout`, `Worktree`, `Ref`, `Commit`, `CodeSnapshot`, `GraphGeneration`, `PullRequest`, `Session`, `Thread`, `Turn`, `Agent`, `Goal`, `Workflow`, `Initiative`, `Plan`, `WorkItem`, `ExecutionAttempt`, `Executor`, `AutomationRun`, `SavedView`, and `GraphNeighborhood`; targeted variants use `ScopeTargetV2`. The `Initiative`/`Plan`/`WorkItem`/`ExecutionAttempt`/`Executor` roots match plans 01 and 16 and target plan 24's canonical task graph through the plan 09 §§9–10 / plan 10 §8 inventories. `ScopeLocatorV2` is the separate tagged locator union for safe name/path/remote/worktree/ref/PR/provider identifiers. Resolution returns the canonical selector and candidates before query planning.

### 9.2 Resolution rules

- Canonical ID is exact and preferred.
- A named external repository/worktree/project never falls back to the active project.
- One exact candidate resolves automatically and records the evidence/alias used.
- Multiple candidates return `scope_ambiguous`, safe disambiguating labels, candidate canonical IDs, and a ready-to-retry request object.
- No candidate returns `scope_not_found`, searched registries/stores, safe near matches, registration/index status, and legal next actions.
- Same-basename repositories are disambiguated with safe parent/common-dir/registry identity, never credential-bearing remote URLs.
- Repository, checkout, worktree, branch/ref, and code snapshot remain different identities. A worktree query cannot silently read the base checkout graph.
- `AllAuthorized { profile_id }` means all authorized, registered selected-profile evidence. `CurrentInvocation` is legal only when the binding catalog declares it; `ScopeResolutionV2.defaulted_current` makes that choice visible. Skipped/locked/stale/unavailable stores appear in coverage.
- A session or agent may relate to zero, one, or many repositories/worktrees. The API does not force one provider project key into canonical ownership.
- Scope resolution is versioned and produces a `ScopeResolutionId` usable in the query/retrieval anchor. The server revalidates authorization and liveness; it does not trust a client-cached path mapping forever.

### 9.3 Binding ergonomics

- HTTP accepts the full tagged selector.
- SDKs expose builders and typed constructors, never stringly `scope="all"` conventions.
- MCP uses the same schema under one `scope` property; convenience `project_id`/`project_path` fields are generated aliases only while current and cannot conflict.
- CLI exposes consistent `--all`, `--collection`, `--repo`, `--project`, `--worktree`, `--ref`, `--session`, and `--agent` flags generated from the selector registry.
- Every response echoes the resolved canonical scope, safe labels, snapshot watermarks, and coverage. Defaults such as "active project" are explicit in metadata.

## 10. Stable IDs, Retrieval Anchors, and Deep Links

All durable public IDs are opaque typed values with stable prefixes/check digits or equivalent validation. They never encode a raw path, prompt text, secret, database row number without namespace, or mutable display name.

The public identity families include profile, repository, project, checkout, worktree, code snapshot, ref, commit, PR, session, thread, message, Turn, agent, workflow, goal, event, entity, relation, fact/version, skill/version, automation run/artifact, policy bundle, query/replay run, export, operation, and research anchor.

Domain `RetrievalAnchorRecordV1`, keyed by opaque `RetrievalAnchorId`, contains the following contract; public results/deep links expose the ID, and the API/SDK must not create a transport-specific anchor record:

- canonical target ID and entity kind;
- resolved scope ID and access/privacy-domain digest;
- source/store identity class without a sensitive backing path;
- immutable source/event/message/commit identifiers when available;
- snapshot/vector watermarks and data/projection/schema versions;
- view/representative mode and expansion recipe;
- minimal typed retrieval use case plus canonical request digest;
- evidence/provenance links and redaction/retention state;
- creation time and a declared durability class.

Rules:

- Ephemeral response handles, page cursors, bearer tokens, event subscription IDs, and browser state are never the only retrieval citation.
- A copied anchor can be resolved with `GET /api/v2/anchors/{id}` or `POST /api/v2/anchors:resolve` under current authorization.
- Resolution returns exact, moved/adopted identity, retained-but-redacted, expired-by-retention, unavailable-store, or denied. It never silently points to a similar row.
- Deep links contain an anchor ID or saved-view ID, not sensitive query text. Authorization is always rechecked.
- SDK result types surface `anchor` directly and provide `resolve_anchor`; convenience `.data` access must not hide it.
- Export manifests include anchors and hashes so a later agent can verify the source snapshot.

## 11. Request, Response, Coverage, and Consistency Envelopes

### 11.1 Requests

Every request carries or inherits:

- resolved typed scope;
- caller-selected consistency: `eventual`, `frozen`, `at_least_watermark`, or allowed domain-specific mode;
- bounded deadline and resource budget;
- requested fields/payload policy;
- result/page bound;
- optional trace/correlation ID;
- explicit replay/as-of mode when applicable.

The server owns actual authorization, plan cost, selected shards, and captured watermarks. Client-supplied estimates are hints only.

### 11.2 Responses

Every success uses one canonical envelope:

```rust
pub struct ApiResponse<T> {
    pub data: T,
    pub meta: ApiMeta,
}

pub struct ApiMeta {
    pub request_id: RequestId,
    pub use_case: UseCaseRef,
    pub protocol: ProtocolRef,
    pub catalog_digest: CatalogDigest,
    pub resolved_scope: ScopeResolutionV2,
    pub snapshot: Option<FrozenSnapshot>,
    pub coverage: CoverageReportV1,
    pub freshness: FreshnessReport,
    pub redactions: RedactionReport,
    pub retention: EvidenceRetentionWatermark,
    pub limits: AppliedLimits,
    pub warnings: Vec<ApplicationWarning>,
}
```

`CoverageReportV1` is plan 01's canonical shared coverage type. No SDK convenience method may discard `meta` by default. A deliberate `into_data()` can consume the response only after making metadata loss obvious in code.

### 11.3 Truthful partial results

- Useful rows with one unavailable/stale/locked/redacted shard return success with `coverage.complete=false`.
- Each shard/source coverage item declares selected/skipped disposition, requested/captured watermark, schema/capability version, freshness, rows considered/returned when known, and safe reason.
- Zero results plus incomplete coverage is not represented as "no matches".
- Counts declare exact, lower-bound, estimate, sampled, capped, or unknown.
- Search/graph scores declare algorithm/version and are not comparable across profiles unless explicitly normalized.
- SDK iterators aggregate coverage across pages and retain the least-complete state; they do not expose only the last page's metadata.

## 12. Error and Machine-Actionable Retry Contract

Errors use RFC 9457-compatible `application/problem+json` plus stable fields:

```rust
pub struct ApiProblem {
    pub problem_type: ProblemType,
    pub title: CatalogSafeText,
    pub status: u16,
    pub code: ApplicationErrorCode,
    pub instance: RequestId,
    pub detail: Option<CatalogSafeText>,
    pub retry: RetryDirective,
    pub current_version: Option<AggregateVersion>,
    pub restart: Option<RestartDirective>,
    pub current_binding: Option<BindingRef>,
    pub candidates: Vec<SafeCandidate>,
    pub invalid: Vec<InvalidField>,
    pub operation: Option<OperationRef>,
}
```

This is byte-semantic with plan 10's generated `ApiProblem`. `ApplicationErrorCode` and retry/restart/candidate/version/operation meaning come from application/domain; HTTP adds status/RFC 9457 fields. Language SDKs preserve unknown safe problem extensions but do not define a competing code/status hierarchy.

Stable classes include authentication/authorization, scope not found/ambiguous/denied, capability unavailable, invalid request/query, budget/rate/deadline, cursor mismatch/expired/schema/ranking/index/retention, snapshot unavailable, partial-all-unavailable, conflict/expected version/idempotency, operation pending/failed, payload redacted/unavailable, stale client (`client_update_required`, `daemon_restart_required`, `capability_replaced`), stream gap/resync, and safe internal invariant. The stale-client error registry is defined once in this plan's contract IR; plans 09, 10, 12, and 21 use exactly those three codes and mint no variants.

`RetryDirective` is a tagged union owned by plan 09's `error.rs` (application owns the retry classes) and reproduced here verbatim:

- `Never`;
- `SameRequestAfter { delay, condition }`;
- `RetryWith { canonical_request }`;
- `RestartPagination { request_without_cursor, reason }`;
- `PollOperation { operation_id, after }`;
- `RefreshAuth { method }`;
- `UpdateClient { minimum_protocol, current_binding, command }`;
- `ResolveScope { candidates, canonical_request_template }`;
- `Resubscribe { snapshot_request, reason }`.

SDKs implement only declared safe automatic behavior:

- retry idempotent reads for transport failures and explicit `SameRequestAfter`, under deadline and attempt limits;
- retry commands only with the same idempotency key and only when the problem/operation receipt permits it;
- never silently change scope, consistency, payload visibility, query, or capability;
- surface 426/version, ambiguity, denied, destructive-preview, gap, and retention errors to the caller.

Error logs and exception strings are secret-scanned and must not echo bearer tokens, raw prompts, query vectors, credential-bearing URLs, sensitive paths, or payload text.

## 13. Pagination, Cursors, Bulk, Batch, and Asynchronous Operations

### 13.1 Cursor pages

Every collection result uses one page envelope, defined here in the contract IR and used unchanged by plan 10's HTTP lists and plan 21's CLI/MCP pages:

```rust
pub struct CursorPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<OpaqueCursor>,
    pub truncation: Option<TruncationReason>,
    pub count_semantics: CountSemantics, // exact | lower_bound | estimate | sampled | capped | unknown
    pub ordering: OrderingContract,      // declared sort keys, direction, and tie-break rule
}
```

- All collection endpoints use opaque authenticated cursors.
- A cursor encodes exactly the domain `CursorClaimsV1` binding set (plan 01, codec owned by plan 05): query fingerprint, caller/access digest, canonical scope digest, catalog generation, schema/ranking/index versions, frozen watermarks and per-shard positions, sort cutoff, temporal mode/cutoff, intent-profile version, partial-shard dispositions, and expiry. Interactive cursors default to a 15-minute expiry; export/bulk continuations last their job lifetime; overrides are catalog-declared only.
- Cursor and SSE event-ID authentication uses a persisted profile-local HMAC key set, not the per-launch secret. Each key record is `{key_id (primary key), created_at, activated_at, state: active | retiring | revoked}` stored in the profile catalog shard (plan 02) with at most one `active` key; cursors and event IDs embed `key_id`. Rotation mints a new active key on schedule or on demand; `retiring` keys validate existing tokens for the maximum outstanding cursor/subscription/export lifetime, then become `revoked`. Keys survive server restart, so a restart does not invalidate otherwise-valid cursors; revoking a key invalidates its outstanding tokens with a typed `RestartPagination`/`Resubscribe` directive. Plan 05's cursor codec and plan 10's SSE event IDs consume this one key registry.
- Frozen snapshots referenced by outstanding cursors/subscriptions are pinned against GC/compaction/projection retirement for the cursor's declared lifetime by the store/query retention contract (plans 02/05); a pin that cannot be honored fails with a typed restart reason, never silently different data.
- Page bounds are operation-specific; the default interactive maximum is conservative and documented.
- The server never holds a SQLite read transaction across client think time.
- Cursor invalidation returns an exact restart reason and request; no SDK restarts silently unless the caller opts in.
- SDK pagers support page-at-a-time and async item iteration while exposing page metadata, accumulated coverage, cancellation, and maximum-pages/items guards.

### 13.2 Typed batch

`POST /api/v2/batch` accepts at most the declared number/bytes of typed catalog invocations:

- read-only invocations may run concurrently under a shared deadline/budget;
- each item has its own success/problem envelope and stable caller item ID;
- authorization, scope, cost, and response limits apply per item and to the whole batch;
- no arbitrary URL/method/header forwarding and no nested batch;
- batch never provides transactionality across independent stores/use cases.

Mutating multi-operation workflows use explicit application commands, not generic batch. Atomicity is available only when one named use case declares one transactional owner. Otherwise each command has a separate idempotency key and receipt. `/api/v2/batch` is an API transport multiplexer over existing cataloged read use cases: it appears in plan 10 §8.1's route inventory and, by design, has no entry in plan 09's use-case inventory.

### 13.3 Bulk and export

- Bounded NDJSON streams support large canonical row sequences where immediate streaming is useful.
- Large or expensive exports create an asynchronous `ExportOperation`, expose progress/coverage, and finish with a signed, expiring, contained download resource plus manifest/hash.
- Parquet/JSONL schemas are generated from canonical row types and versioned in the manifest.
- Client disconnect cancels uncommitted read work. Durable exports/jobs continue only when explicitly requested and remain pollable.
- Exports never accept an arbitrary output path through the public API.

## 14. Streaming and Change Subscription

The official live contract is snapshot plus typed delta over SSE:

1. `POST /api/v2/subscriptions` submits the sensitive typed query/scope and returns a session-bound subscription ID, initial snapshot reference, expiry, and stream path.
2. `GET /api/v2/subscriptions/{id}/events` emits the matching snapshot first, then ordered deltas/progress/coverage/operation/gap events.
3. `Last-Event-ID` uses an authenticated opaque event cursor bound to subscription, authorization, protocol, and sequence.

Rules:

- query text and tokens never enter the URL or event ID;
- heartbeats carry no semantic sequence;
- finite replay retention is declared;
- duplicate/out-of-order delivery can be applied idempotently;
- only semantically idempotent updates may coalesce;
- terminal operation, removal, coverage, gap, policy/version, and audit events never coalesce away;
- bounded per-connection frames/bytes, principal/global connection caps, and slow-client termination prevent memory growth;
- an unrecoverable gap emits `resync_required` and closes; SDKs fetch a new snapshot only under explicit reconnect policy;
- auth revocation, scope loss, privacy change, or protocol cutoff terminates the stream with a typed reason;
- SDK streams expose snapshot, delta, progress, heartbeat visibility option, gap, reconnect, and terminal events rather than hiding them behind an untyped callback.

## 15. Graph-of-Graphs API

The public graph surface treats code, Git, threads, sessions, Turns, agents, workflows, goals, memory, skills, automation, time, and delivery as lenses over one evidence graph, not unrelated endpoint-specific node models.

### 15.1 Typed operations

- `POST /api/v2/graph/neighborhood` — bounded expansion from entity/anchor roots.
- `POST /api/v2/graph/path` — bounded evidence path with allowed edge/entity kinds and maximum depth/cost.
- `POST /api/v2/graph/subgraph` — query-driven subgraph with LOD and cluster limits.
- `POST /api/v2/graph/impact` — downstream/upstream effect paths with confidence/evidence.
- `POST /api/v2/graph/diff` — compare frozen graph snapshots, refs, sessions, policies, or time windows.
- `POST /api/v2/entities:batch` — batch hydrate stable IDs already returned by another operation; no duplicate graph-only hydration use case.
- `POST /api/v2/timeline/events` and `/timeline/density` — temporal projection of the same entities/relations.

### 15.2 Node and edge contracts

Every node includes stable typed ID, kind, safe label, time validity, owning/related scopes, availability, evidence summary, payload-access state, and retrieval anchor. Every edge includes kind, direction, valid/transaction time, evidence IDs, confidence/trust class, inference/projector version, and contradiction/uncertainty state.

The API distinguishes:

- observed relationship;
- provider-declared relationship;
- deterministic projection;
- heuristic/inferred correlation;
- user/agent annotation;
- unresolved candidate.

Graph results include exact membership or declared sample/cluster membership, edge aggregation semantics, LOD/layout/community algorithm version, expansion cursor, selected watermarks, and partial coverage. A visualization cluster is not serialized as a factual domain entity.

### 15.3 Safety and cost

- Query declares allowed node/edge kinds, direction, depth, maximum nodes/edges/paths, time range, and payload fields.
- Server estimates and enforces cost before expansion; high-fanout edges require aggregation or explicit narrowed continuation.
- Payload hydration is separate from topology and independently authorized/redacted.
- No arbitrary Cypher/SQL/GraphQL string is accepted.
- Cross-project traversal respects every source privacy domain and reports denied/redacted boundaries without leaking existence beyond authorization.
- Code graph nodes bind repository/worktree/ref/code-snapshot identity; the base checkout never substitutes for a requested parallel worktree.

## 16. Search, Hint, and Policy Replay APIs

Replay endpoints are official but safe by construction. Their application use cases are read-only and have no command binding.

### 16.1 Replay modes

- `exact_deterministic`: resolve the exact executable evaluator, schema, config, policy, catalog, index/model, project/memory/skill, and prompt-template digests.
- `recorded_result`: inspect exact historical inputs/candidates/results/payloads when executable artifacts are unavailable.
- `current_best_effort`: run the current evaluator against explicitly selected historical/synthetic inputs and label all substitutions/missing evidence.

Missing artifacts yield incomplete fidelity, never silent approximation.

### 16.2 Hint replay

`POST /api/v2/labs/hints:replay` accepts a stable message/Turn/session/hook anchor or an explicit synthetic input, selected policy bundle, scope, candidate catalog, and replay mode. It returns:

- normalized safe input facts and redactions;
- all candidate capabilities/hints with features/scores;
- eligibility/privacy/availability decisions;
- repetition/cooldown/token/latency budget decisions;
- suppressions with stable reasons;
- exact rendered payload reference only when authorized;
- historical delivery/outcome evidence kept distinct from candidate prediction;
- current-versus-historical diff;
- artifact/fidelity manifest and retrieval anchors.

Replay never injects a hint, invokes a tool, publishes presence/claim, modifies memory/fact trust, increments usage/counters, records an acted outcome, or emits live analytics. A separately named export/share command is required to persist a replay artifact.

### 16.3 Search replay and evaluation

`POST /api/v2/labs/search-quality:replay` and `/api/v2/labs/search-quality:compare` expose the pipeline in plan 15:

- exact/phrase/lexical/fuzzy/entity/sparse/dense/graph/recency candidate lanes;
- dedupe/representative/group membership and hidden counts;
- per-stage caps, component ranks/scores, fusion, diversity, reranker, and final explanation;
- index/model/corpus/profile versions and selected watermarks;
- coverage, no-answer decision, latency/resource measurements;
- relevance judgments only when authorized and never as hidden live labels.

Experiments can compare named retrieval profiles over a frozen local evaluation manifest. They cannot silently switch a live agent's retrieval profile, write judgments, or send private queries to a network model. Applying a new profile is a separate versioned policy command with preview, gates, audit, and rollback.

## 17. Commands and Mutation Safety

The official API may expose the broad writable capability surface, but direct-agent credentials default to read-only.

Every command request includes:

- stable use-case ID;
- explicit canonical scope and declared owner scope for created state;
- idempotency key;
- expected aggregate/config version;
- preview digest where meaningful;
- authorization grant and approval provenance;
- bounded deadline/resource policy;
- optional client correlation ID.

Every result includes effect/audit receipt, current/new version, compensation/rollback availability, and either terminal output or durable operation/workflow ID.

Destructive or broad non-curation operations such as wipe, retention deletion, payload GC, migration apply, external delivery, policy activation, and automation enablement require a capability-specific grant and preview/confirmation. A generic `write` token is insufficient.

Fact/memory/managed-skill/profile curation is not exposed as item-level approve/apply/install/rollback endpoints. A dedicated least-privilege curation service grant plus versioned autonomy configuration authorizes the application worker to apply only owned, policy-eligible effects after transactional revalidation; every effect/outcome/recovery is audited. Public clients can read status/history/decisions/outcomes, configure policy, pause/resume/run-now, pin/protect/exclude, and submit feedback. Unsafe/foreign/out-of-authority candidates are automatically rejected/deferred/quarantined, never converted into a human approval endpoint.

Current `code.move_symbol` is a first-class edit command, not a generic filesystem mutation: generated clients expose preview by default, exact source/destination/snapshot/version, impact classes and applied imports, confirmation digest, repository/worktree edit grant, destination-first rollback/reindex operation, and no automatic caller rewrite. Raw paths/source/diffs use protected/eligible fields and never enter URLs or client logs.

SDKs separate `ReadClient` and `AdminClient` surfaces where the language permits. Mutation methods do not appear on a read-only typed client. Raw HTTP still enforces the same server-side grant.

## 18. Authentication, Local Trust, Privacy, and Secret Handling

### 18.1 Transports

- Prefer a user-owned Unix-domain socket with OS ownership/mode checks for local nonbrowser clients, plus application authentication. Plan 10 builds the socket listener and its peer-credential checks (plan 10 §10.1); this plan owns UDS conformance (Section 24).
- Loopback HTTP binds only exact loopback by default and enforces strict Host/Origin/forwarded-header policy.
- Browser uses per-launch bootstrap, secure session cookie, and CSRF token.
- Agent/SDK uses a bearer token or local credential-provider handshake. Tokens never appear in URLs, process titles, command history examples, OpenAPI examples, or logs.
- Non-loopback bind requires a future explicit deployment profile with TLS, stronger identity, documented threat model, and separate review; changing the address flag alone is insufficient.

### 18.2 Credential model

Tokens are:

- random, hashed at rest, user/profile/instance bound;
- named by safe token ID for audit/revocation;
- time-bounded by default;
- constrained by read/preview/mutate/admin capability grants;
- constrained by scope selectors and sensitivity/payload grants;
- optionally process/installation identity bound where supported;
- revocable immediately with stream/operation implications declared.

The per-launch bootstrap bearer of plan 10 §10.2 is not a parallel credential model: it is the bootstrap credential whose only permitted operation is `auth.tokens.create` (plan 09 §10), minting the initial admin-class token in this registry. Every operating credential is a registry token.

The CLI prints a token only through an explicit secure creation flow and warns about shell history/agent context. Prefer delivering credentials by inherited file descriptor, OS keyring/credential helper, or `0600` file reference instead of environment variables for long-lived automation.

### 18.3 Data privacy

- Authorization is checked at capability, scope, entity, edge, payload, field, export, and stream stages.
- Topology visibility does not imply payload visibility.
- Secret-classified content never enters FTS/vector indexes, API examples, problems, telemetry, cursors, anchor labels, source maps, or conformance fixtures.
- Prompt/tool/provider sanitized-native payload access is an explicit sensitivity grant and every access is audited. Plaintext forensic access, when protected retention exists, is a distinct elevated quarantine workflow and never a normal entity/message/session/graph route.
- Durable graph-resident facts and memory are user data; backup, migration, export, delete, and corruption APIs never treat the whole graph database as disposable derived state.
- Replay/sandbox inputs are retained only when a separate explicit save/export command succeeds.
- Every endpoint documents retention, redaction, and deletion consequences.
- Content-bearing request fields enter as bounded `Unclassified<T>` and cross the application sanitizer; SDK/runtime code never marks raw strings or JSON trusted. Responses, problems, events, examples, anchors, and generated docs contain only plan 18 sink-eligible wrappers or explicit redacted/denied/unknown states.
- `PrivacyProtectionStatusV1` reports configured policy, effective non-disableable floor, source/sink/detector coverage and versions, last verified scan, legacy/unscanned/unknown counts, and restore eligibility. No SDK property named merely `redaction_enabled` is generated, and lossy-row existence is not status evidence.
- Bounded failures, decoder exceptions, `Debug`/`Display`, and automatic retry diagnostics never retain or echo the request body. They preserve safe codes/IDs/directives and discard candidate content after decoding.

## 19. Limits, Fairness, and Resource Budgets

Limits are cataloged and returned by `/meta` and capability discovery:

- maximum request/compressed/decompressed bytes, JSON depth, headers, URI, batch items, page items, graph nodes/edges/depth, timeline bins, payload bytes, export bytes, and stream queues;
- per-principal concurrent reads, streams, exports, jobs, and mutations;
- token-bucket request/query-cost budgets by capability class;
- absolute server deadline plus client-requested shorter deadline;
- selected-shard and representation/vector/model budgets;
- fair scheduling across parent/subagents and profiles so one broad query cannot starve hook capture or interactive queries.

429/413/422/budget responses declare applied limit, safe current usage when available, reset/retry time, and legal narrowing actions. They never recommend broadening scope or dropping privacy filters merely to succeed.

Hook hot paths and capture writers do not call the public HTTP API. Public API load is isolated from append durability and bounded so replay/search experiments cannot delay provider hooks.

## 20. SDK Design

### 20.1 Common behavior

All SDKs provide:

- endpoint discovery and explicit client construction;
- credential-provider abstraction with redacted debug output;
- protocol/catalog/schema handshake;
- typed capability and scope resolution;
- one method per public use case plus generic catalog invocation only for forward-compatible tooling;
- response envelopes with metadata preserved;
- cancellable request deadlines;
- page and async-item iteration with maximum guards;
- SSE reconnect/gap/resync primitives;
- operation polling with backoff bounded by server directives;
- typed problems and retry directives;
- stable anchor parsing/resolution;
- user-agent containing SDK/runtime version but no project/query identity;
- optional OpenTelemetry propagation with payload-free defaults.

The generic invocation API accepts a `UseCaseId` and schema-validated typed/JSON value for exploratory agents. It still passes catalog authorization/cost/effect checks and returns canonical envelopes. Generated named methods remain preferred and are the only methods shown in normal docs.

### 20.2 Rust

- `tracedecay-client` exposes async traits and a default client runtime without depending on server/store crates.
- Support Unix socket and loopback HTTP transports behind features.
- Generated domain/schema module is public; hand-written client/pager/stream/operation code is small and reviewable.
- Errors preserve `ApiProblem`; `Debug`/`Display` redact credentials and sensitive bodies.
- Compile examples and MSRV/toolchain policy are release gates.
- Optional in-process transport exists only for TraceDecay workspace composition/tests and invokes the same application contract; it is not a different semantic API.

### 20.3 TypeScript

- Publish an ESM-first typed package for Node and browsers, with explicit browser auth constraints.
- Use `fetch`, `AbortSignal`, async iterators, and a tested SSE implementation that can send required auth safely.
- Preserve 64-bit counts as `bigint` or validated string-backed named types where necessary.
- Runtime decoding validates discriminators and reports schema/protocol mismatch rather than accepting malformed JSON.
- Browser package cannot read local discovery/token files; dashboard bootstrap supplies an authenticated client.
- Node package supports the local socket transport when the runtime permits it.

### 20.4 Python

- Publish typed synchronous and asynchronous clients with Python version policy declared before implementation.
- Use generated immutable models plus a small HTTP/socket runtime; ship `py.typed`.
- Provide sync/async pagers, context-managed streams/operations, cancellation/timeouts, and typed exceptions retaining `ApiProblem`.
- Avoid import-time endpoint discovery or network calls.
- Validate large integers, discriminated unions, timezones, and unknown enum behavior in contract tests.

### 20.5 Generation quality

Do not check in an enormous generic generator runtime without review. Generate stable models, endpoint descriptors, and method signatures from the contract IR; maintain compact language-native transport/pagination/stream runtimes by hand. Generated diffs are deterministic and human-reviewable.

SDK release versions declare the exact supported protocol range. Server, CLI, MCP plugin, dashboard, and SDK release automation publishes the compatibility manifest atomically or fails before partial release.
The trusted release job also compares changed files with the generated allowlist, rejects tracked ignored/omitted contract artifacts and dirty generation, builds/packages SDKs from clean inputs, and secret-scans every generated derivative before publication.

## 21. CLI, MCP, Dashboard, Plugin, and Tool Integration

- CLI, MCP, and dashboard bindings are generated/audited from the same catalog and application schemas.
- Server-side CLI/MCP adapters call `tracedecay-application` directly; they do not make recursive loopback HTTP calls.
- External plugins and agents use the official HTTP/SDK contract rather than internal databases or unstable root modules.
- MCP human-facing defaults remain compact Markdown; explicit JSON mode uses the canonical typed view model and preserves all machine fields. HTTP/SDK always use canonical machine JSON.
- Markdown and JSON render from the same typed application view, with parity tests for missing registries, active markers, repeated basenames, limits, truncation, and coverage.
- Tool catalog entries link directly to API docs and SDK examples. API discovery links back to CLI/MCP bindings so an agent can choose the cheapest available surface.
- Host integrations handshake catalog/protocol digest. If an installed plugin is stale, it receives one current restart/update/replacement instruction; no dual namespace or legacy behavioral shim.
- `tracedecay tool <name> --args ...` remains a useful shell fallback but is not the only direct machine API.
- Plugin authors receive a minimal integration guide, conformance fixture runner, synthetic sandbox, and version matrix.

## 22. Documentation, Examples, and Sandbox/Playground

### 22.1 Documentation requirements

The official docs contain:

- a five-minute read-only quickstart for curl, Rust, TypeScript, and Python;
- endpoint/credential discovery without secret leakage;
- the scope mental model with multi-repository/worktree examples;
- coverage/freshness/partial-result and count semantics;
- pagination, stream resume/gap, operation polling, and retry recipes;
- stable retrieval anchors and citation examples;
- Graph-of-Graphs traversal and LOD/cost rules;
- safe search/hint replay examples;
- command preview/idempotency/authorization examples;
- all stable error codes and retry directives;
- version compatibility and cutoff behavior;
- security/privacy/retention/export guidance;
- generated reference for every public capability and SDK method.

Examples use a generated synthetic profile containing multiple repositories, two worktrees of one repository, parent/subagents, sessions/Turns, a workflow, Git branch/PR, code changes, memory/facts, automation, hints, and known partial/stale stores. No local user data is committed.

### 22.2 Interactive API explorer

Serve an authenticated API explorer from the local docs endpoint (plan 10's static_app serves it under `/docs` with the same loopback auth/CSP/bootstrap rules, plan 10 §13):

- schema browsing and synthetic examples need no mutation grant;
- "try" uses the current authenticated session and clearly displays canonical request, scope, expected cost, and response metadata;
- mutation operations open in preview mode and cannot apply from generic reference pages without capability-specific confirmation;
- tokens are never saved to local storage, URL, docs source, or generated curl snippets;
- response panels show coverage/freshness/redaction/limits and problems, not only `data`;
- an anchor can open the dashboard inspector under reauthorization.

### 22.3 Safe sandbox

Provide a fixture-backed sandbox process/profile:

- deterministic synthetic corpus and frozen clock;
- no access to real profile stores, credentials, network providers, GitHub mutations, or host hooks;
- resettable state and seeded error/partial/gap/version scenarios;
- same OpenAPI/protocol and SDK clients as production;
- read-only hint/search replay by default;
- conformance runner can launch it hermetically.

The dashboard Hint/Search/Coordination/Query labs use application use cases, not a special undocumented API. The API explorer and sandbox link to those richer visual labs when available.

## 23. Observability and Audit

Every API request records safe operational telemetry:

- request/correlation ID, use-case/binding ID, server/protocol/catalog versions;
- authenticated principal/token ID class, never token value;
- canonical scope kind/count and privacy domain digest, not sensitive paths/query text;
- deadline/budget/limit class;
- rows/bytes/shards and complete/partial/redacted state;
- latency by auth/extract/application/serialize/queue, plus cancellation/retry/error code;
- stream connections, resume distance, coalescing, gaps, slow-client closes;
- SDK name/version and transport;
- command idempotency/effect/operation/audit receipt IDs.

OpenTelemetry spans and `Server-Timing` expose safe stage timings. Trace propagation is allowlisted; untrusted baggage is rejected. Logs, traces, metrics, and error aggregations pass secret and high-cardinality review.

Product analytics distinguish capability discovery, invocation, useful result continuation, error/retry, and abandonment. They do not treat API call volume as success, and replay/debug calls do not count as live hint/tool outcomes.

An API Observatory view reports protocol/client versions, catalog parity, endpoint health, latency/error/partial distributions, rate-limit pressure, stream gaps, SDK adoption, stale clients, and conformance status with explicit denominators/horizons.

## 24. Conformance, Evaluation, and Release Gates

### 24.1 Semantic parity matrix

For every use case, run a canonical fixture through each applicable path:

```text
application in-process
HTTP JSON
Rust SDK
TypeScript SDK
Python sync SDK
Python async SDK
CLI JSON
MCP JSON
dashboard client
export rows
SSE initial snapshot
```

Compare canonical semantic JSON after removing only declared transport fields such as request timing. Verify identity/order, scope, snapshot/watermarks, coverage/freshness/redaction/retention, evidence/confidence, ranks/explanations, cursor claims, anchors, errors/retries, replay fidelity, command receipts, and operation state. Conformance fixtures reuse plan 10 §12's `TransportSemanticFixture` schema, serialized as canonical JSON under `tests/public_api_conformance/fixtures/` — one file per use case and scenario, named `<use_case_id>.<scenario>.json`.

### 24.2 Required test suites

- Contract generation/determinism and route/catalog/schema/SDK manifest bijection.
- OpenAPI/JSON Schema validation, discriminator, unknown variant, optional/nullable, bigint, time, and round-trip properties.
- Multi-repository/project/checkout/worktree/ref/session/agent/All scope resolution, ambiguity, stale registry, same basename, wrong active checkout, and denied store fixtures.
- Cursor tamper/access/query/schema/ranking/index/retention/expiry and distributed-page stable-order fixtures.
- Partial/locked/corrupt/stale/unavailable/redacted store coverage and zero-result truthfulness.
- Graph high-fanout/cycle/depth/path/LOD/cluster/partial/privacy and worktree-snapshot identity fixtures.
- Search/hint exact/recorded/current replay, missing artifact, no-write, privacy, grouping, ranking explanation, and current-versus-historical diff fixtures.
- Auth/token/Unix socket/Host/Origin/CSRF/DNS rebinding/revocation/expiry/scope/sensitivity and constant-time handling tests.
- Rate/body/decompression/header/URI/JSON depth/batch/export/stream queue/deadline/cancellation tests.
- SSE duplicate/out-of-order/resume/expiry/gap/resync/coalescing/slow client/auth change/protocol cutoff tests.
- Command idempotency/version conflict/preview/approval/operation recovery/audit/destructive grant tests.
- Unix-domain socket transport conformance: ownership/mode checks, peer-credential mismatch, token authentication over the socket, and browser-credential rejection (listener built by plan 10 §10.1).
- Executor-adapter compatibility/security matrix from plan 24 as a dedicated conformance lane: provider/model/route constraint enforcement, fenced claim/heartbeat/terminal transitions over the public surface, and workspace-safety refusals.
- Secret corpus across source, generated artifacts, examples, logs, errors, cursors, anchors, exports, docs, source maps, and telemetry.
- SDK compile/type/lint/unit/integration examples on supported Rust/Node/browser/Python matrices.
- Fuzz/property tests for request parsing, cursor/event/anchor IDs, problem decoding, batch, graph limits, replay inputs, and stream events.
- Current V1 internal parity fixtures until each domain's explicit cutover; post-cutover negative tests prove stale live clients fail rather than execute a fallback.

### 24.3 Performance gates

Record reference machine/corpus, server/build versions, profile/store counts, watermarks, p50/p95/p99, allocations, bytes, and peak RSS for:

- metadata/capability/scope resolution;
- ordinary entity/search/timeline/graph pages;
- cross-project frozen query and distributed next page;
- 100-agent/parallel-worktree proximity query;
- hint/search replay with each enabled retrieval/policy stage;
- batch at limits;
- NDJSON/export throughput;
- SSE connections/event rates/reconnect/gap recovery;
- SDK encode/decode/pager overhead.

API transport/mapping targets inherit plan 10's p95 gates. SDK overhead is separately budgeted and must not dominate local server work. Large graph/search/replay operations publish capability-specific budgets rather than hiding them under one global latency claim.

### 24.4 Historical evidence anchors

- Public-API intent and this plan request: parent session `019f4906-a411-7a11-ad3f-0d58deb0e847`; copied child-visible session `019f496a-fae5-7ff3-a301-f4f7e59fe4db`. Treat the parent as the canonical research context and the child as provenance, not duplicate independent user evidence.
- MCP conformance/error semantics evidence: session `95561c21-5d89-4c6d-8864-a6add1c1f748` recorded an unknown-tool error-code mismatch and the need to distinguish stdio versus HTTP conformance rather than validating through an accidental proxy. Use it as a regression seed, not as normative protocol text.
- Canonical implementation provenance must also include the Git commit, contract/catalog/schema digests, fixture manifest, and stable research anchor from plan 13. Session IDs alone are insufficient.

## 25. Rollout and Reviewable PR Slices

These are companion slices to plan 10's PR 24B–24E work. Renumber during implementation only if the master plan reserves a conflicting identifier; preserve dependency order and ownership.

### PR 24D-API1: Freeze public contract IR and official support declaration

**Files:** contract IR/generator modules in tool-catalog/API; `docs/api/{index,versioning,security,limits}.md`; conformance manifest tests.

- [ ] Add failing tests for use-case/binding/schema bijection, missing authorization/limits/errors, unstable generation, and transport-specific semantic fields.
- [ ] Build the canonical contract IR and deterministic manifest from domain/application/catalog definitions.
- [ ] Mark every capability public/internal/admin/migration/removed and fail on unknown disposition.
- [ ] Publish protocol/version/change/cutoff policy and compatibility manifest schema.
- [ ] Commit `feat(api): freeze the official public contract`.

### PR 24D-API2: Scope resolution, anchors, problems, and direct-agent discovery

**Files:** public schemas, meta/openapi/schema/binding/scope/anchor routes; CLI `api` lifecycle commands; docs concepts/quickstart; conformance fixtures.

- [ ] Add cross-project/worktree/ref/session/agent/All, same-basename, ambiguity, wrong-active-project, stable-anchor, endpoint-discovery, token-redaction, and retry-directive tests.
- [ ] Implement Sections 7, 9, 10, 11, and 12 through application use cases; no handler-side resolution.
- [ ] Add `tracedecay api status/token/openapi/docs` with secret-safe JSON and user approval.
- [ ] Prove no response handle/cursor/token/path is used as a durable anchor.
- [ ] Commit `feat(api): expose agent discovery and stable scopes`.

### PR 24D-API3: Complete Graph-of-Graphs and safe replay contract

**Files:** graph/replay OpenAPI/schema/catalog bindings, synthetic fixtures, API docs recipes, conformance tests.

- [ ] Add graph entity/edge/evidence/LOD/cost/privacy/worktree snapshot cases and search/hint exact/recorded/current/no-write cases.
- [ ] Bind the plan 10 routes to complete official schemas and capability docs.
- [ ] Verify replay cannot reach any command, live hook, fact/trust, analytics outcome, claim, or external network effect.
- [ ] Add curl examples and direct links to dashboard visual labs.
- [ ] Commit `feat(api): publish graph and replay contracts`.

### PR 24D-SDK1: Rust client and hermetic sandbox

**Files:** `crates/tracedecay-client/**`; sandbox fixture process/profile; Rust quickstart/examples; conformance runner.

- [ ] Add compile/round-trip/error/pager/stream/operation/socket/auth tests against the synthetic sandbox.
- [ ] Generate types/descriptors and implement the compact Rust runtime.
- [ ] Prove credential/payload redaction, protocol handshake, bounded iteration, gap visibility, and command idempotency.
- [ ] Publish as workspace-only until the public contract and release process pass twice.
- [ ] Commit `feat(sdk): add the official Rust client`.

### PR 24D-SDK2: Complete and publish the one official TypeScript client

**Files:** `packages/tracedecay-client/**`; Node/browser examples; docs; conformance adapters.

- [ ] Harden the generated schema core (produced from the contract IR per Section 5.1 and hosted in this same package per plan 10) and the transport-neutral runtime; add no dashboard dependency. Make the dashboard browser binding consume/re-export it without generating another schema tree.
- [ ] Test ESM, Node local socket/HTTP, browser bootstrap, bigint, runtime decoding, pager, SSE gap/resume, and typed problems.
- [ ] Prove browser builds cannot read local discovery/token files and generated bundles contain no fixtures/secrets.
- [ ] Commit `feat(sdk): publish the official TypeScript client`.

### PR 24D-SDK3: Python sync/async package

**Files:** `python/tracedecay-client/**`; Python examples/docs; conformance adapters.

- [ ] Add supported-version matrix, typing, model round-trip, sync/async transport/pager/stream/operation, timezone/bigint/enum, and redaction tests.
- [ ] Generate models/descriptors and implement the compact sync/async runtime.
- [ ] Validate package build/install in an empty environment and against the sandbox.
- [ ] Commit `feat(sdk): publish the official Python client`.

### PR 24D-API4: Docs explorer, SDK/reference generation, and full parity gate

**Files:** `docs/api/**`; authenticated explorer; generated capability pages; `tests/public_api_conformance/**`; release manifests.

- [ ] Generate and curate quickstarts/concepts/recipes/reference; compile/run every example.
- [ ] Add authenticated explorer with preview-only mutation UX and metadata/problem visibility.
- [ ] Run the full application/HTTP/SDK/CLI/MCP/dashboard/export/SSE matrix.
- [ ] Add release automation that blocks partial server/SDK/catalog/schema publication.
- [ ] Record performance/security/privacy evidence and obtain API, SDK, and security review.
- [ ] Commit `docs(api): ship the official integration surface`.

### PR 24E-API5: Domain-by-domain cutover and stale-client rejection

For each application domain, after plan 10's adapter parity passes:

- [ ] Enable the official current bindings and supported SDK methods.
- [ ] Verify capability discovery and docs expose exactly the current binding.
- [ ] Verify obsolete route/tool/schema/client receives typed update/restart/replacement guidance and performs no semantic work.
- [ ] Preserve migration/rollback data and receipts without retaining a live compatibility path.
- [ ] Record the domain cutover in the compatibility manifest.

## 26. Final Definition of Done

- [ ] Every supported application use case has one reviewed public/internal/admin/migration/removed disposition and complete binding manifest.
- [ ] OpenAPI 3.1, JSON Schemas, SDK models/descriptors, docs reference, and conformance fixtures regenerate byte-deterministically.
- [ ] Rust, TypeScript, and Python clients pass the semantic, type, stream, security, examples, and packaging matrices against the same sandbox.
- [ ] Raw HTTP, SDKs, CLI JSON, MCP JSON, dashboard, exports, and SSE snapshot preserve canonical semantics and metadata.
- [ ] Multi-repository/project/checkout/worktree/ref/session/agent/All selection is exact, explicit, easy to discover, and cannot silently fall back to the active project.
- [ ] Large enumeration and graph/search/timeline results page/stream/export without hidden caps; incomplete coverage is truthful.
- [ ] Stable retrieval anchors resolve or fail with an exact reason; no response handle, page cursor, token, or UI URL is the sole citation.
- [ ] Graph-of-Graphs queries preserve evidence, confidence, time, worktree/snapshot identity, LOD, bounds, privacy, and partial coverage.
- [ ] Hint/search replay is reproducible at declared fidelity, explainable, privacy-safe, and demonstrably no-write.
- [ ] Direct-agent credentials are least-privilege, scoped, expiring, auditable, revocable, and never leaked by SDK/docs/errors/logs.
- [ ] Commands require explicit authority, idempotency, versions, preview/approval where applicable, and durable audit/operation receipts.
- [ ] Errors provide stable machine codes and exact retry/restart/update/scope-resolution payloads.
- [ ] API/SDK load cannot starve hook capture or concurrent event writers; limits and fairness pass current and 10x reference scenarios.
- [ ] Official docs explain the mental model and every example runs against the synthetic sandbox.
- [ ] Current protocol cutoff rejects stale clients without executing live compatibility fallbacks.
- [ ] Release publishes server/catalog/schema/SDK compatibility artifacts coherently and can roll back only to a compatible V2 artifact/data snapshot.
