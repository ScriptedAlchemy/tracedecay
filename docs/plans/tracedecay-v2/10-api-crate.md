# TraceDecay V2 API Crate Implementation Plan

**Goal:** Build `tracedecay-api`, the secure loopback-first Axum HTTP V2 and SSE boundary for the one official contract, with generated OpenAPI/dashboard-client artifacts and semantic parity across HTTP, CLI, MCP, SDKs, dashboard, exports, and live subscriptions.

**Architecture:** HTTP handlers authenticate, validate bounded transport inputs, map them to `tracedecay-application` use cases, and map typed results/errors back without changing scope, ordering, coverage, freshness, evidence, command, or replay semantics. Live reads use an authorized subscription resource followed by resumable snapshot/delta SSE; OpenAPI and transport bindings are generated from plan 17's contract IR — itself built from application/domain schemas plus the generated tool catalog — while CLI/MCP remain separate thin adapters tested against the same semantic fixtures.

**Tech Stack:** Rust 2024 workspace; Axum; Tower/Tower HTTP; `serde`; `schemars`; `utoipa` with Axum integration (validation-only reflection against the IR-generated OpenAPI, Section 11); `tracedecay-domain`; `tracedecay-application`; `tracedecay-tool-catalog`; HMAC-SHA256 authenticated cursors/event IDs; SSE; `openapi-typescript`; TypeScript `fetch` runtime; contract/property/fuzz/E2E tests.

[`20-configuration-control-plane.md`](20-configuration-control-plane.md) owns configuration keys, precedence, effective-state, history, and impact semantics. This API exposes only generated application use cases and OpenAPI/SSE bindings for that contract; it cannot maintain a parallel `/settings` model.

[`21-cli-mcp-tool-surface-and-output-unification.md`](21-cli-mcp-tool-surface-and-output-unification.md) owns cross-transport binding/output parity. HTTP/OpenAPI JSON serializes the same sealed semantic views as CLI/MCP JSON and shares errors, pages, retrieval anchors, notices, freshness, and provenance; HTTP framing is not another business contract.

[`22-incremental-context-scout-and-suggestion-envelopes.md`](22-incremental-context-scout-and-suggestion-envelopes.md) and [`23-session-lcm-temporal-retrieval-and-evaluation.md`](23-session-lcm-temporal-retrieval-and-evaluation.md) own scout and temporal-search semantics. This crate exposes their generated status/replay/search/context/lineage/evaluation routes and SSE events without embedding model orchestration, ranking, temporal resolution, or delivery selection.

[`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md) owns task/plan/executor semantics and the stricter many-host adapter protocol. This crate exposes generated `/api/v2/initiatives`, plans, work-items, attempts, executors, scheduler, task views, idempotent commands, and canonical subscription read-model deltas without implementing readiness, routing, fencing, packet assembly, workspace safety, event truth, or board logic. There is no separate task query AST or `/task-events` stream.

---

## 1. Contract Lock

This plan refines master-plan PRs 24B–24E and owns the V2 HTTP/SSE boundary and cross-transport semantic parity harness; it hosts the checked OpenAPI artifact and the generated core of the one official TypeScript client, whose single generation source is plan 17's contract IR (Section 11). Plan 17 completes and publishes that same client; the dashboard adds only a browser-auth binding.

Plan 17 declares this same HTTP/OpenAPI surface official and adds the contract IR, public docs/explorer/sandbox, and Rust/transport-independent TypeScript/Python SDKs. It does not create another server or envelope. Plan 18 owns the sanitizer/taint types and privacy workflows; API extraction/output may map eligible wrappers but cannot classify content or invent a weaker “safe string.”

- Axum handlers depend on `tracedecay-application` use cases and generated catalog bindings. They contain no SQL, shard selection, ranking, policy, Git/GitHub, source ingest, migration, command ownership, or dashboard business logic.
- HTTP does not define a second domain model. Rust request/response schemas reference domain/application types or explicit transport wrappers whose lossless mapping is fixture-tested.
- Task list/query/context and saved-board bodies use the one domain `TraceQueryV1` plus `ScopeSelectorV2`; convenience routes compile to the same canonical request/digest and cannot define `TaskQueryV1`, `TaskContextSelectorV1`, or a board-filter DSL.
- `POST` carries all text queries, saved content, policy inputs, exports, and mutations. URLs contain only nonsensitive enum filters, bounded time/number parameters, opaque IDs, and authenticated cursors/subscription IDs.
- A partial application result is a successful typed response with `!coverage.is_complete()`; it is not transformed into an empty success or generic server error.
- Every mutation routes to one application `execute` command with catalog-owned `ExecutionModeV2`, idempotency, expected version, audit, and optional durable workflow. Confirmed destructive operations use separately named preflight and start/cutover/retire commands; there is no transport-generic preview/apply mode. An HTTP retry cannot create a second semantic effect.
- SSE begins from one frozen application snapshot, then maps query-owned ordered deltas/progress/gap/resync events. API owns wire framing, heartbeats, browser resume, and transport backpressure only.
- Canonical command events commit in the owning journal before API visibility. SSE/subscriptions project those journal sequences after commit; HTTP acceptance, outbox/notifier delivery, or an SSE event can never create or acknowledge domain truth independently.
- Browser auth, CSRF, Host/Origin checks, CSP, export containment, and body/decompression limits are mandatory even on loopback. No capability is unauthenticated merely because it binds localhost.
- OpenAPI operation identity comes from `tracedecay-tool-catalog` `BindingId`/`UseCaseId`. The catalog is the registry of record; plan 17's contract IR is its frozen public projection and the single generation source (Section 11). Generated client drift and missing/duplicate route bindings fail CI.
- CLI and MCP do not call HTTP by requirement and do not depend on this crate. Their adapters call application in-process or through daemon composition; parity tests compare typed semantics before CLI/markdown rendering.
- V1 routes/tools exist only inside the bounded migration/shadow harness. At V2 cutover, stale live routes, names, schemas, and clients fail with a typed incompatible-version problem carrying restart/update/current-route guidance; they are never silently proxied to V1.
- Session/message endpoints completely enumerate sanitized native transcript rows and are lossless for retained non-secret structure/semantics. They expose domain `MessageOrigin` and `MessageView` unchanged, including native, representative, human-best-effort, direct-user, delegated-agent, tool-result, and provider-protocol views with exact representative provenance from merged PR #410.
- Scope-sensitive fact/skill/policy/automation/saved-state bodies carry generated domain `DeclaredScope`; handlers never infer ownership from the route, active project, referrer, or browser investigation filter.
- Content-bearing request fields decode as bounded `Unclassified<T>` and are passed to the application sanitizer workflow; they are never converted to domain/store-ready strings in an extractor. Response/SSE/problem/export mapping accepts only `TransportEligibleView`, plan 18 eligible wrappers, or explicit redacted/denied/unknown variants.

## 2. Goals

- Expose every V2 read, command, lab, export, health, and subscription required by the master plan through bounded, versioned routes.
- Make `GET /sessions` and `GET /messages` completely enumerable for sanitized native rows without a text predicate, using authenticated stable cursors and a captured watermark.
- Make Brain, graph-of-graphs, Universal Explorer, Causal Loom, domain workspaces, Observatory, Costs, all replay labs, and Evolution Studio possible without dashboard-side SQL or private endpoints.
- Generate one checked-in OpenAPI document and TypeScript client that preserve IDs, enums, errors, coverage, cursors, replay fidelity, command receipts, and SSE payload schemas.
- Resume live streams after ordinary disconnects, expose gaps and required resync, coalesce only idempotent updates, and terminate slow clients without silent loss.
- Prevent DNS rebinding, cross-origin browser calls, CSRF, token leakage, path traversal, unsafe exports, content sniffing, framing, and sensitive URL/history/log leakage.
- Preserve identical query/command semantics across HTTP, CLI JSON, MCP JSON, dashboard client, export, and SSE snapshot.
- Provide explicit removal gates for V1 routes, response handles, plugin gateways, and direct dashboard APIs.

## 3. Non-Goals

- No GraphQL primary surface, WebSocket requirement, hosted service, multi-tenant identity server, remote libSQL gateway, or arbitrary network bind in the first V2 default.
- No HTML/dashboard component implementation beyond secure SPA/static delivery, bootstrap, history fallback, and asset headers.
- No application authorization rules, query planning, policy evaluation, command transaction, export sink, migration runner, or subscription semantics duplicated in handlers.
- No arbitrary client-supplied filesystem path, shell command, SQL/FTS fragment, renderer code, GitHub mutation, or provider credential over HTTP.
- No event-source query text in the SSE URL. Sensitive subscription/query bodies are posted once and referenced by opaque ID.
- No hidden chain-of-thought API. Only authorized retained provider-exposed reasoning artifacts and coverage markers are representable.
- Internal migration renderers need not preserve every V1 presentation byte. Typed semantics and checked parity receipts are authoritative; no old renderer is a live post-cutover surface.

## 4. Incoming-Master and V1 Inputs

### 4.1 Master and incoming changes verified on 2026-07-10

The normative publication snapshot is [master §2.6](../2026-07-09-tracedecay-brain-rewrite.md#26-current-master-accepted-changes) plus [plan 13](13-research-provenance-and-context-anchors.md). Regenerate contracts before every API slice; fact ranking, exact analytics, restart-safe applied-manifest retirement, and operator-only split-store consolidation are accepted-base behavior, not autonomous curation.

| Change | API consequence |
|---|---|
| Merged PR #405 legacy identity adoption | Scope/project routes return one adopted canonical ID with legacy aliases as provenance. Migration/health responses expose ambiguity and receipts without leaking raw sensitive locators. |
| Merged PR #412 safe daemon drain during upgrades | Operation/daemon/update responses expose lease epoch, accepting/draining/stopped state, in-flight counts, progress, last durable receipt, recovery/takeover, and safe retry. SSE cannot collapse drain or terminal transitions. |
| PR #407 Hermes user-profile consolidation | No Hermes profile route or compatibility store is introduced. Hermes/curator/reflector/skill-writer actors appear through normal workflow/agent/automation/knowledge APIs in the active profile. |
| Merged PR #410 message-query dedupe/classification | Message/session/search/export schemas use domain `MessageOrigin`/`MessageView`, plus `representative_for`, rule/version, suppression count, and native expansion cursor. V2 `NativeRows` completely enumerates sanitized rows and preserves retained non-secret structure/semantics. |
| Merged PR #411 foreign-installation doctor severity | Doctor schemas expose `severity`, observed owner, remediation authority, evidence, and legal actions; foreign/unknown ownership cannot serialize an apply/update action. |
| Merged PR #414 `tracedecay_move_symbol` | Preserve V1 dry-run evidence while generating distinct V2 `code.move_symbol.inspect` (read-shaped) and confirmed `code.move_symbol.commit` bindings with impact evidence, snapshot/version, applied imports, recovery/reindex operation, and no implicit caller rewrite. |
| Merged PR #415 release integrity | Generated OpenAPI/SDK/dashboard artifacts and conformance fixtures require an allowlisted release manifest and tracked-ignored-file guard. |
| Merged PRs #413/#416/#418 releases v0.0.46/v0.0.47/v0.0.48 | Regenerate server/protocol/catalog/OpenAPI/compatibility fixtures from accepted master; no behavior depends on release-PR layout or implies the planning host upgraded from installed 0.0.47. |
| Merged PR #417 identity-split visibility | Serialize `identity_split` distinctly from absent index, with safe candidates/evidence and legal backup/consolidation preview; never emit an initialize action. |
| Merged PR #425 explicit split-store consolidation (`de3d05dc`, final head `d3bb28b5`) | Generate admin-only inspect/plan/start/status/resume/recover bindings for the accepted workflow: two explicit nonempty source identities, path-plus-file/inode holder/freeze/reservation state, backups, per-table/artifact dispositions, staging/verification/cutover ledger, deterministic confirmation, and exact recovery. No raw store path in URLs; no handler opens/merges SQLite; no Settings auto-start or curation binding. |
| Merged PR #419 race-safe edits | The move-symbol inspect/commit schemas expose exact source/destination identities/versions, same-file/symlink evidence, pre-commit revalidation, and commit/recovery conflict receipts; no generic success hides concurrent drift. |
| Merged PR #420 daemon proxy/hot swap plus #422 catalog refresh | API/MCP problems and capability metadata distinguish safe per-request reconnect from uncertain-write non-replay and compatible generation-scoped `tools.listChanged` versus incompatible new-session refresh; no adapter opens local stores before authority selection. |

Refresh live master/open PRs before every 24B–24E slice. The OpenAPI source manifest records commit, catalog digest, domain registry digest, application registry digest, and generation tool versions so stale artifacts fail rather than masquerade as current.

### 4.2 V1 transport seams

| V1 seam | V2 disposition |
|---|---|
| `src/dashboard/server.rs`, project gateways, plugin routers | Use only under the bounded migration flag and internal parity harness. New V2 router has explicit methods/typed handlers; cutover removes `/api/v1` live resolution and no `ANY` semantic gateway remains. |
| Direct dashboard plugin APIs for Holographic, LCM, Graph, Analytics, Diagnostics, Savings, Settings, Automation | Map one surface/domain at a time to application. V1 behavior inventory in `08-tool-catalog-crate.md` blocks route retirement until all actions have parity or replacement. |
| `src/mcp/server.rs` and handler schemas/renderers | Exercise old schemas/renderers only in migration parity. Current MCP calls application directly; stale names/clients fail with update/restart/current binding and never proxy through V2 HTTP or V1 semantics. |
| CLI parsers/handlers | Build current generated flags/JSON/text adapters over application. Old flags/shapes are internal parity fixtures, not post-cutover aliases; HTTP schemas do not become CLI implementation types. |
| Response handles for renderer truncation | Migration-only V1 rendering may wrap a V2 cursor/export ID, but every research result also carries a stable canonical anchor/retrieval recipe. V2 never uses an ephemeral response handle as pagination, persistence, deep link, or the only way to recover a session/thread/message/subagent/workflow/Git result. |
| Static dashboard/plugin asset serving | V2 serves one SPA with safe history fallback, hashed assets, and CSP. Migration-only redirects disappear at cutover; asset-path misses never return HTML. |

## 5. Exact Crate and Companion File Tree

```text
crates/tracedecay-api/
├── Cargo.toml
├── src/
│   ├── lib.rs                         # router/config/public server contracts
│   ├── error.rs                       # ApiError and RFC 9457-style problem mapping
│   ├── state.rs                       # application registry/auth/catalog/stream state
│   ├── config.rs                      # loopback/listen/origin/limits/session settings
│   ├── router.rs                      # explicit V2 route composition
│   ├── extract.rs                     # authenticated context, IDs, cursor, bounded query
│   ├── response.rs                    # success/meta/problem/cache/header mapping
│   ├── limits.rs                      # body/decompression/query/timeout concurrency limits
│   ├── auth/
│   │   ├── mod.rs                     # AuthService and principal mapping
│   │   ├── launch.rs                  # per-launch secret and one-time bootstrap nonce
│   │   ├── session.rs                 # browser session/bearer lifecycle
│   │   ├── csrf.rs                    # mutation token validation/rotation
│   │   ├── token.rs                   # constant-time token digest/expiry/revocation
│   │   └── uds.rs                     # user-owned Unix-domain socket listener and peer-credential checks
│   ├── security/
│   │   ├── mod.rs
│   │   ├── host.rs                    # strict Host and forwarded-header rejection
│   │   ├── origin.rs                  # exact Origin/Sec-Fetch-Site enforcement
│   │   ├── headers.rs                 # CSP/referrer/frame/sniff/cache headers
│   │   ├── request_id.rs              # safe correlation IDs
│   │   └── rate_limit.rs              # bounded auth/session/mutation abuse controls
│   ├── http/
│   │   ├── mod.rs
│   │   ├── auth.rs                    # bootstrap/session/logout/csrf refresh
│   │   ├── capabilities.rs
│   │   ├── scopes.rs
│   │   ├── query.rs
│   │   ├── search.rs
│   │   ├── retrieval_evaluation.rs    # generated plan-15 artifact/run/report/profile routes
│   │   ├── brain.rs
│   │   ├── entities.rs
│   │   ├── graph.rs
│   │   ├── timeline.rs
│   │   ├── activity.rs
│   │   ├── sessions.rs
│   │   ├── messages.rs
│   │   ├── agents.rs
│   │   ├── goals.rs
│   │   ├── workflows.rs
│   │   ├── coordination.rs
│   │   ├── code.rs
│   │   ├── delivery.rs
│   │   ├── knowledge.rs
│   │   ├── automation.rs
│   │   ├── accounting.rs
│   │   ├── observatory.rs
│   │   ├── privacy.rs
│   │   ├── settings.rs
│   │   ├── operations.rs
│   │   ├── research.rs
│   │   ├── saved.rs
│   │   ├── exports.rs
│   │   ├── labs/
│   │   │   ├── mod.rs
│   │   │   ├── hints.rs
│   │   │   ├── retrieval.rs
│   │   │   ├── ingest.rs
│   │   │   ├── query.rs
│   │   │   ├── search_quality.rs
│   │   │   ├── scope_federation.rs
│   │   │   ├── privacy.rs
│   │   │   ├── correlation.rs
│   │   │   ├── coordination.rs
│   │   │   ├── scheduler.rs
│   │   │   ├── orchestration.rs
│   │   │   ├── memory.rs
│   │   │   ├── policy_diff.rs
│   │   │   └── evolution.rs
│   │   └── commands/
│   │       ├── mod.rs
│   │       ├── projects.rs
│   │       ├── operations.rs
│   │       ├── automation.rs
│   │       ├── skills.rs
│   │       ├── proposals.rs
│   │       ├── memory.rs
│   │       ├── policy.rs
│   │       ├── settings.rs
│   │       ├── diagnostics.rs
│   │       ├── payloads.rs
│   │       ├── capture.rs
│   │       ├── projections.rs
│   │       ├── migrations.rs
│   │       ├── delivery.rs
│   │       ├── coordination.rs
│   │       ├── research.rs
│   │       ├── retrieval_evaluation.rs
│   │       ├── exports.rs
│   │       ├── saved.rs
│   │       └── labs.rs
│   ├── sse/
│   │   ├── mod.rs                     # Axum SSE response adapter
│   │   ├── subscription.rs            # POST-created authorized resource
│   │   ├── event.rs                   # typed event name/data mapping
│   │   ├── event_id.rs                # authenticated opaque resume ID
│   │   ├── resume.rs                  # Last-Event-ID validation and replay
│   │   ├── coalesce.rs                # bounded semantics-preserving coalescing
│   │   ├── heartbeat.rs               # comment heartbeat without semantic sequence
│   │   └── backpressure.rs             # slow-client termination/resync
│   ├── openapi/
│   │   ├── mod.rs                     # IR-generated document hosting and utoipa validation reflection
│   │   ├── schemas.rs                 # transport wrappers and domain refs
│   │   ├── security.rs                # auth/CSRF schemes and headers
│   │   ├── validate.rs                # catalog/route/schema parity
│   │   └── generated.json             # deterministic checked-in artifact
│   └── static_app/
│       ├── mod.rs                     # SPA/asset service
│       ├── bootstrap.rs               # nonce injection without URL/token logging
│       ├── history.rs                 # V2 route fallback only
│       └── headers.rs                 # immutable asset and HTML policies
├── tests/
│   ├── support/mod.rs
│   ├── router_contract.rs
│   ├── request_response.rs
│   ├── sessions_messages.rs
│   ├── commands.rs
│   ├── labs.rs
│   ├── security.rs
│   ├── sse_resume.rs
│   ├── sse_backpressure.rs
│   ├── exports.rs
│   ├── openapi_drift.rs
│   ├── static_history.rs
│   └── v1_compatibility.rs
├── fuzz/
│   ├── fuzz_targets/cursor_problem_query.rs
│   └── corpus/
└── benches/
    ├── http.rs
    └── sse.rs
```

Companion generated/client and adapter files:

```text
packages/tracedecay-client/
├── package.json
├── src/generated/schema.ts
├── src/{client,pager,events,operation,error}.ts
└── test/{contract,live-fixture,examples}.test.ts   # module/test layout owned by plan 17 §4

dashboard/packages/api-client/
├── package.json
├── src/{browser-auth,client,errors,sse}.ts  # thin binding/re-exports only; no generated schema
└── test/browser-auth.test.ts

src/cli/v2_adapter/{mod,query,sessions,git,memory,automation,operations}.rs
src/mcp/v2_adapter/{mod,query,sessions,git,memory,automation,operations,render}.rs
tests/v2_transport_parity/{mod,reads,commands,errors,streams}.rs
tests/fixtures/v2/transport-semantics.json
```

No API production file exceeds 800 lines. Route modules contain extraction/mapping only; repeated transport mapping uses generated helpers, not macro-hidden business logic.

## 6. Dependency Direction and Forbidden Imports

```text
domain/query/policy/tool-catalog/store ports
                    ↑
        tracedecay-application
                    ↑
          tracedecay-api (HTTP/SSE)
                    ↑
            dashboard API client

        CLI adapter ─┐
        MCP adapter ─┴──→ tracedecay-application
```

- API may depend on application, domain schemas, and tool-catalog binding metadata. It may not depend on concrete store/projector/capture/policy/query implementations or root V1 service types.
- CLI/MCP adapter modules depend on application and catalog, not on API. Their tests may share serialized fixtures only.
- Dashboard generated client depends on OpenAPI/schema artifacts, never Rust internals or V1 plugin response guesses.
- Reject API imports of `rusqlite`, `libsql`, graph/session/memory repository modules, provider parsers, Git/GitHub clients, policy evaluators, query rankers, command store, or migration runner.
- A catalog drift test asserts every route operation maps to one `BindingId`/`UseCaseId`, and every catalog HTTP binding maps to exactly one method/path/handler.

## 7. HTTP Envelope, Cursor, Error, and Version Contracts

### 7.1 Success responses

```rust
#[derive(Serialize, Deserialize, ToSchema)]
pub struct ApiResponse<T> {
    pub data: T,
    pub meta: ApiMeta,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ApiMeta {
    pub request_id: RequestId,
    pub use_case: UseCaseRef,
    pub protocol: ProtocolRef,
    pub catalog_snapshot: CatalogSnapshotRefV1,
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

`CoverageReportV1` is plan 01's canonical shared coverage type. The mapper is exhaustive over `ApplicationResponse<T>`. Compile tests fail if application adds metadata without an API disposition. `!coverage.is_complete()` remains HTTP 200 when useful data exists. An application top-level error maps to a problem response; a partial shard failure already represented in coverage does not become 500.

List responses use the single `CursorPage<T>` envelope defined once in plan 17's contract IR (plan 17 §13.1) — `{ items, next_cursor, truncation, count_semantics, ordering }` — and serialized here unchanged; plan 21's CLI/MCP pages are the same type, restated in neither plan.

Cursor strings are authenticated, opaque, URL-safe, size-limited to 8 KiB, and never decoded client-side; they encode exactly the domain `CursorClaimsV1` binding set (plan 01, codec owned by plan 05): query fingerprint, caller/access digest, canonical scope digest, profile-catalog generation, schema/ranking/index versions, frozen watermarks and per-shard positions, sort cutoff, temporal mode/cutoff, intent-profile version, partial-shard dispositions, and expiry. Interactive cursors use plan-20 `query.cursor.interactive_ttl` (default 15 minutes); export/bulk continuations last their catalog-declared job lifetime. Cursor and SSE event-ID authentication uses the persisted profile-local HMAC key set of plan 17 §13.1 (key ID plus rotation), so a server restart does not invalidate otherwise-valid cursors. Frozen snapshots referenced by outstanding cursors/subscriptions are pinned against GC/compaction/projection retirement for the cursor's lifetime by the store/query retention contract (plans 02/05); a pin that cannot be honored fails with a typed `RestartPagination` reason, never silently different data. A cursor mismatch/expiry returns typed restart instructions.

### 7.2 Errors

Errors use `application/problem+json` and stable machine fields:

```rust
pub struct ApiProblem {
    pub problem_type: ProblemType,
    pub title: CatalogSafeText,
    pub status: u16,
    pub code: ApplicationErrorCode,
    pub detail: Option<CatalogSafeText>,
    pub instance: RequestId,
    pub retry: RetryDirective,
    pub current_version: Option<AggregateVersion>,
    pub restart: Option<RestartDirective>,
    pub current_binding: Option<BindingRef>,
    pub candidates: Vec<SafeCandidate>,
    pub invalid: Vec<InvalidField>,
    pub operation: Option<OperationRef>,
}
```

This is the same `ApiProblem` generated for plan 17's SDKs and consumed by plan 11. `ApplicationErrorCode`, `RetryDirective`, `RestartDirective`, scope candidates, version, binding, invalid-field, and operation semantics come from application/domain contracts. HTTP supplies only RFC 9457 fields and status; SDKs/transports do not define an `ApiErrorCode` fork or infer recovery from status. Plan 21's `SurfaceProblemV2` is exactly this shared shape plus its exit-class mapping table — same field names, `restart` and `current_binding` included. Stale-client codes are exactly plan 17 §12's contract-IR registry: `client_update_required`, `daemon_restart_required`, and `capability_replaced { current_binding }`.

Status mapping is fixed:

| Condition | HTTP status |
|---|---:|
| Invalid schema/filter/cursor syntax | 400 |
| Missing/invalid/expired authentication | 401 |
| Authenticated but scope/payload/effect denied | 403 |
| Canonical opaque entity/operation/job/export/subscription absent | 404 |
| Method mismatch | 405 |
| Query/body/media limits | 413 |
| Unsupported media/schema version | 415 |
| Stale client/binding requiring restart or update (`client_update_required`, `daemon_restart_required`, `capability_replaced`) | 426 |
| Version/idempotency/preview/revalidation or storage-identity split conflict | 409 |
| Expired cursor/preview/subscription replay | 410 |
| Validation with field details | 422 |
| Request rate/concurrency limit | 429 |
| Deadline exceeded | 504 |
| All selected shards unavailable or internal invariant | 503/500 according to typed error |

Problem `detail` never reflects raw parser input, query literal, filesystem path, token, provider payload, or secret. Server logs record request ID, operation/use-case ID, safe error code, timing, byte counts, and coverage—not request bodies or sensitive URLs.

Transport-created reasons—including JSON/schema failures, bounded/truncated MCP parity failures, export errors, static-route errors, and upstream disconnects—must be converted to safe reason enums plus `LogSafeText` before `ApiProblem`. The API cannot wrap `Display` from an arbitrary error. A unique synthetic canary in each request/error field proves it reaches neither problem JSON, headers, logs, traces, response handles, nor SSE notices.

### 7.3 Versioning and content negotiation

- All new routes live under `/api/v2`; path version changes only for incompatible transport contracts.
- Request/response bodies are UTF-8 JSON with `Content-Type: application/json`; exports/downloads use declared formats. Operations whose catalog entry declares bulk support also stream the same canonical rows as bounded NDJSON under `Accept: application/x-ndjson`, equal to paged rows at the same frozen watermark (plan 17 §13.3). Request schemas are closed: unknown named body fields are rejected, and forward-compatible request additions travel only in the declared bounded `extensions` slot per plan 17 §6.2 — never silently promoted into query semantics.
- OpenAPI records schema/registry/catalog/application versions. Response `Vary` includes only headers that truly affect representation; no cache varies on bearer token values.
- Immutable public-within-session manifests/bundles may use private ETag revalidation. Payload-bearing, query, lab, message, export-status, and command responses use `Cache-Control: no-store`.

## 8. Complete HTTP V2 Surface

Every route is authenticated except the static HTML/assets and one-time bootstrap exchange described in Section 10. HTTP operation IDs come from catalog `BindingId`; method/path aliases do not create new use cases.

### 8.1 Capability, scope, health, and OpenAPI

| Method and path | Application operation |
|---|---|
| `GET /api/v2/meta` | Authenticated protocol/server/profile identity, catalog/schema digests, time, health summary, limits profile, and current compatibility policy. |
| `GET /api/v2/capabilities` | Capability/use-case/binding availability and digests. |
| `GET /api/v2/bindings/{use_case_id}` | Current CLI/MCP/HTTP/SDK/dashboard bindings and prerequisites from the generated catalog. |
| `GET /api/v2/scopes` / `GET /api/v2/scopes/{id}` | Cursor-based lazy All/repository/project/worktree/ref/snapshot tree and exact scope children/relations with parent/depth/search/changed-since/health. |
| `POST /api/v2/scopes:resolve` | Exact name/path/alias/ID resolution returning `Resolved` or bounded same-name `Ambiguous` candidates plus one-step retry request. |
| `GET /api/v2/projects` / `GET /api/v2/projects/{id}` | Canonical project inventory/detail. |
| `GET /api/v2/health` | System health summary. |
| `GET /api/v2/doctor` | Doctor findings and exact runtime/store identities. |
| `GET /api/v2/coverage` | Shard/source/domain coverage. |
| `GET /api/v2/migrations` / `GET /api/v2/migrations/{id}` | Migration receipts/status. |
| `GET /api/v2/projections` / `GET /api/v2/projections/{id}` | Projector status/watermarks/dead letters. |
| `GET /api/v2/privacy/status` / `GET /api/v2/privacy/scans` / `GET /api/v2/privacy/scans/{id}` / read-shaped `POST /api/v2/privacy/scans:inspect` | Effective policy/safety floor, source/sink/detector coverage and versions, legacy unknowns, restore eligibility, bounded scan status, and protected scope/source inspection without persistence. |
| `GET /api/v2/privacy/findings` / `GET /api/v2/privacy/findings/{id}` / `GET /api/v2/privacy/detectors` / `POST /api/v2/privacy/detectors:diff` / `GET /api/v2/privacy/remediations/{id}` / `GET /api/v2/privacy/quarantine/status` | Safe finding classes/states, synthetic-only detector comparison, remediation, and elevated quarantine metadata; never candidate content/fingerprint. |
| `GET /api/v2/daemon` / `GET /api/v2/watchers` / `GET /api/v2/index` | Operational status/freshness only. |
| `POST /api/v2/settings/effective` / `POST /api/v2/settings/sources` | Effective settings and source/default/owner/restart/reindex/privacy impact for an explicit declared scope; no literals in URLs. |
| `GET /api/v2/operations` / `GET /api/v2/operations/{id}` | Durable command/job/workflow/export/migration/automation progress and explicit terminal disposition. |
| `POST /api/v2/retrieval-anchors:metadata-batch` | `retrieval_anchors.metadata_batch_get`: bounded safe identity/state/tombstone metadata only; never content or an authorized payload. |
| `POST /api/v2/retrieval-anchors:resolve` | `retrieval_anchors.resolve`: authorized record/payload resolution for one or more canonical IDs at a frozen watermark, with drift and coverage. |
| `POST /api/v2/retrieval-recipes:execute` | `retrieval_recipes.execute`: bounded read-only execution of a versioned protected recipe with exact scope/version/watermark and coverage. |
| `GET /api/v2/openapi.json` | Authenticated deterministic OpenAPI document. |
| `GET /api/v2/schemas/{digest}/{name}` | Authenticated allowlisted checked JSON Schema artifact for the current protocol. |
| `POST /api/v2/batch` | Bounded typed multi-invocation per plan 17 §13.2: each item is one catalog-bound read invocation with its own success/problem envelope and caller item ID; no mutation items, no nested batch, no cross-item transactionality. Batch is an API transport multiplexer over existing use cases, not a separate application use case. |

Scope query parameters are bounded opaque IDs/enums and pagination only. Project search text uses `POST /api/v2/projects/search` so literal text does not enter URLs. Every binding has a catalog-declared default that is inserted as an explicit canonical selector request: Brain/Observatory use `AllAuthorized { profile_id }`; code-local bindings may use `CurrentInvocation`. Handlers never infer cwd, last project, route, or referrer independently.

Domain `ScopeSelectorV2` and `ScopeResolutionV2` are serialized unchanged. The selector is exactly `version`, nonempty `roots`, `exclude`, `time`, `activity_attribution`, `coverage`, `freshness`, `traversal`, `ambiguity`, and `limits`; canonical/locator targets use `ScopeTargetV2`. Resolution includes `resolution_id`, selector digest/canonical selector, selected/ambiguous/stale/unavailable/quarantined/missing sets, `defaulted_current`, catalog generation, and watermark. A client resubmits the preserved canonical request plus selected candidate once; query/filter/time/message-view bodies are not reconstructed client-side. CLI/MCP/API/SDK/dashboard parity fixtures require identical candidate identity/order, resolution, provenance, and errors.

Doctor/provider payloads use generated `DoctorFindingView` and `ProviderIntegrationState`. `severity`, `observed_owner`, `remediation_authority`, evidence, legal actions, hook/tool/session coverage, missing pieces, and last verified time are required. An apply/update action for foreign or unknown ownership is unrepresentable in the application view itself (plan 09 §9.1); this serializer adds no second gate and cannot express one, and it never maps `Partial`/`Degraded` to healthy branding.

### 8.2 Query, Brain, graph, timeline, and inspector

| Method and path | Contract |
|---|---|
| `POST /api/v2/query` | Generic bounded `TraceQueryV1`. |
| `POST /api/v2/search` | Opinionated universal search with explicit ranking profile. |
| `POST /api/v2/search/benchmark` | Read-only one/two-profile run over the versioned redacted benchmark corpus with per-slice metrics and blockers. |
| `POST /api/v2/entities:batch` | Bounded universal inspector hydration. |
| `POST /api/v2/brain/overview` | All reading path at requested scope/time/snapshot. |
| `POST /api/v2/brain/lens` | One bounded graph-of-graphs lens. |
| `POST /api/v2/brain/clusters/{id}:expand` | Stable aggregate expansion by opaque cluster ID. |
| `POST /api/v2/graph/neighborhood` | Evidence-filtered bounded neighborhood. |
| `POST /api/v2/graph/path` | Bounded stable paths. |
| `POST /api/v2/graph/subgraph` | Query-driven bounded subgraph using the same graph query/operator contract. |
| `POST /api/v2/graph/impact` | Evidence-bearing impact. |
| `POST /api/v2/graph/diff` | Frozen graph snapshot/ref/session/policy/time comparison. |
| `POST /api/v2/graph/affected-tests` | Bounded affected tests. |
| `POST /api/v2/timeline/density` | Server-side density/LOD buckets. |
| `POST /api/v2/timeline/events` | Cursor event/Turn/agent lanes. |
| `POST /api/v2/timeline/as-of` | Valid-time and observed-time historical state. |
| `POST /api/v2/timeline/follow-agent` | Agent/subagent/collaborator/delivery lanes. |
| `POST /api/v2/timeline/compare` | Aligned comparison anchors and deltas. |
| `POST /api/v2/activity/events` / `POST /api/v2/activity/facets` | Consequential cross-domain activity and facets over one frozen/live event model; routine-noise counts remain explicit. |

All graph/timeline inputs require explicit node/edge/event/lane/bucket/page/depth/byte budgets within query hard limits. Federated inputs accept explicit multi-repository/project/worktree/ref scope, preserve each node/edge's repository/snapshot provenance and per-shard freshness/coverage, and reject same-name collapse. The API rejects missing bounds before calling application. Read POST bodies carry the application `ReadRequirementsV1` envelope (consistency, budget, payload policy; plan 09 §7.2) as a top-level `read` object; GET enumerations accept only its bounded enum/watermark forms as query parameters.

`POST /search` accepts generated exact-token/field, phrase, fuzzy, entity/alias, semantic, graph-neighborhood, and recency profile controls plus origin/kind/provider/session/agent/repository/project/worktree/ref/time filters and grouping/dedupe policy. It returns per-stage candidates/caps/exclusions/versions, final score components, missing features, grouping membership, native expansion, repository/snapshot provenance, coverage, and benchmark-profile ID. “Semantic” is never an implicit default: clients request a versioned evaluated profile, and the server may disable vector contribution when unavailable or regression-gated. The benchmark endpoint includes the named Rspack/Rsbuild/React Router cross-repo disambiguation slice.

### 8.3 Sessions, messages, turns, agents, and workflows

| Method and path | Contract |
|---|---|
| `GET /api/v2/sessions` | No-text cursor enumeration with provider/host/time/kind/opaque scope filters. |
| `POST /api/v2/sessions/search` | Text/semantic session search. |
| `GET /api/v2/sessions/{id}` | Summary, participants, goals/workflows, snapshots, coverage. |
| `POST /api/v2/sessions/{id}/replay` | Read-only historical assembly and replay fidelity. |
| `POST /api/v2/threads/{id}/replay` | Read-only thread assembly across its authorized session/Turn lineage. |
| `GET /api/v2/sessions/{id}/context-lineage` | LCM source/summary DAG, compression decisions, payload coverage and source ranges. |
| `GET /api/v2/sessions/{id}/turns` | Bounded first-class Turns. |
| `GET /api/v2/turns/{id}` | Turn hub with authorized linked evidence. |
| `GET /api/v2/messages` | No-text cursor enumeration with domain message view plus provider/time/role/kind filters. |
| `POST /api/v2/messages/search` | Literal/semantic search with the same domain message-view contract. |
| `GET /api/v2/messages/{id}` | Native or representative message view. |
| `GET /api/v2/messages/{id}/native` | Cursor expansion to represented sanitized native rows and source observations. Plaintext forensic access is not a transcript route; it is confined to the separately authorized protected-quarantine workflow. |
| `GET /api/v2/agents` / `GET /api/v2/agents/{id}` | Actors/instances/lifecycle/parents/goals/outcomes. |
| `GET /api/v2/goals` / `GET /api/v2/goals/{id}` | Codex goals and provider-native objectives with versioned updates, Turns, owners, and terminal evidence. |
| `GET /api/v2/workflows` / `GET /api/v2/workflows/{id}` | Provider-native and canonical workflow graph. |
| `POST /api/v2/context:assemble` | One canonical bounded context-assembly use case for session/thread/Turn/task selectors with exact manifests, omissions, coverage, and source anchors. |
| `GET /api/v2/temporal-assertions/{id}/lineage` | Supersession/conflict/authority/evidence lineage with valid-time and observed-time bounds. |
| `POST /api/v2/coordination/presence` / `POST /api/v2/coordination/nearby` / `POST /api/v2/coordination/overlaps` | Expiring presence/work claims and same/parallel-worktree overlap with evidence class, safe compact summary, stable research anchors/recipe, legal actions, coverage, and no raw sensitive payload. |

Allowed non-content message query parameters use the domain enum unchanged:

```text
view=native_rows,representative_rows,human_best_effort,direct_user,delegated_agents,tool_results,provider_protocol
provider=<opaque-or-enum>&role=<enum>&kind=<enum>&time_start=<utc>&time_end=<utc>
cursor=<opaque>&page_size=1..1000
```

Representative responses always include represented entity IDs, source observations, algorithm/rule version, suppression count, and native expansion cursor. A client obtains sanitized native rows through that cursor or a second `view=native_rows` request bound to the same snapshot; the API does not invent a combined “both” count. The catalog-declared export default — owned by application/catalog metadata, not by this transport — is the complete sanitized-native enumeration; a caller may explicitly request another `MessageView`, and the manifest declares that projection.

### 8.4 Domain workspace reads

These routes are typed profiles over application/query, not separate service implementations:

| Family | Routes |
|---|---|
| Code | `POST /api/v2/code/search`, `/find-exact-symbol`, `/grep`, `/context`, `/callers`, `/callees`, `/path`, `/impact`, `/affected-tests`, `/test-map`, `/diagnose`, `/move-symbol:inspect`; `GET /api/v2/code/files`, `/health`, `/diagnostics`. `move-symbol:inspect` is `EffectClass::Read` despite its protected request body. |
| Git/delivery | `GET /api/v2/git/branches`; `POST /api/v2/git/branch-search`, `/branch-diff`, `/pr-context`, `/changelog`, `/commit-context`, `/sessions-for`, `/workflows-for`, `/reconcile`; `GET /api/v2/delivery/repositories/{id}`, `/pulls/{id}`, `/checks`, `/reviews`, `/releases`. |
| Knowledge | `GET /api/v2/knowledge/facts`, `/facts/{id}`, `/entities`, `/entities/{id}`, `/trust-history`, `/conflicts`, `/retrievals`, `/feedback`; `POST /api/v2/knowledge/search`, `/deletion-impact`. |
| Automation | `GET /api/v2/automation/jobs`, `/jobs/{id}`, `/scheduler`, `/runs`, `/runs/{id}`, `/runs/{id}/artifacts`, `/candidates`, `/candidates/{id}`, `/decisions`, `/decisions/{id}`, `/effects`, `/effects/{id}`, `/outcomes`, `/outcomes/{id}`, `/recoveries`, `/recoveries/{id}`, `/history`, `/skills`, `/skills/{id}`; `POST /api/v2/automation/workflow-graph`. Legacy proposals/approvals/applies are labeled records returned only through history. |
| Research provenance | `GET /api/v2/research/manifests`, `GET /api/v2/research/manifests/{id}`. Manifest routes return immutable `ResearchAnchorId` entry identity and nonempty canonical `RetrievalAnchorId` references; anchor metadata, authorized resolution, and recipe execution use only the three routes inventoried in §8.1. |
| Search evaluation | `GET /api/v2/retrieval/corpus-versions`, `/corpus-versions/{id}`, `/qrel-versions`, `/qrel-versions/{id}`, `/candidate-pools`, `/candidate-pools/{id}`, `/judgments`, `/judgments/{id}`, `/adjudications`, `/adjudications/{id}`, `/evaluation-runs`, `/evaluation-runs/{id}`, `/evaluation-reports`, `/evaluation-reports/{id}`, `/profiles`, `/profiles/{id}`. These are versioned artifact/operation reads; list metadata is payload-free and protected rationales/examples require an authorized payload policy. |
| Hints/policy | `GET /api/v2/hints/evaluations`, `/evaluations/{id}`, `/outcomes`, `/opportunities`, `/policy/bundles`, `/policy/bundles/{id}`, `/policy/coverage`. |
| Accounting/Observatory | `GET /api/v2/accounting/usage`, `/costs`, `/savings`, `/adoption`, `/denominators`, `/api/v2/observatory`. |
| Tasks/orchestration | `GET /api/v2/initiatives`, `/initiatives/{id}`, `/initiatives/{id}/graph`; `GET /api/v2/plans`, `/plans/{id}/versions/{version}` plus read-shaped `POST /plans:diff`; `GET /api/v2/work-items`, `/work-items/{id}`, `/work-items/{id}/dependencies`, `/work-items/{id}/context`; `POST /api/v2/work-items/query`; `GET /api/v2/execution-attempts`, `/execution-attempts/{id}`, `/execution-attempts/{id}/timeline`; `GET /api/v2/task-offers`, `/task-offers/{id}`; `GET /api/v2/context-packets`, `/context-packets/{id}`; `GET /api/v2/task-notifications`, `/task-notifications/{id}`; `GET /api/v2/executors`, `/executors/{id}`; `POST /api/v2/executors:match` (read-only); `GET /api/v2/task-scheduler/status`; read-shaped `POST /api/v2/task-scheduler:explain`; `GET /api/v2/task-graph/status`; read-shaped `POST /api/v2/task-graph:doctor`; `GET /api/v2/task-views`, `/task-views/{id}`. Semantics are owned by plan 24 §9.1; offer reads require the authenticated registration and packet reads require the attempt scope. Query/explain/doctor POST bodies carry protected scope/evidence while remaining `EffectClass::Read`; every mutation uses the Section 8.7 command envelope with no `PATCH` route. Catalog capability `task_graph.events` binds to canonical subscription read-model kinds delivered through `POST /api/v2/subscriptions` plus `GET /api/v2/subscriptions/{id}/events`, not a separate `/task-events` stream. |

Git request/response schemas retain local semantic generation/ref/merge-base/watermark separately from live provider/fetched-at/base/head/changed-file cap/digest. Drift responses cannot serialize a combined impact claim.

Task-view schemas import the complete plan-24 `SavedTaskViewV1`: protected canonical `TraceQueryV1`/query and derived-scope digests, projection/lens/group/sort/layout, owner/share grants, live versus exact frozen manifests/watermarks, config/catalog/schema versions, optimistic view version, timestamps, and revocation. API mapping may not drop fields, copy result rows, add a second scope selector, or silently reopen a frozen view as current.

### 8.5 Replay labs and Evolution Studio

All evaluation routes are `POST` and read-only:

```text
/api/v2/labs/hints:replay
/api/v2/labs/retrieval:replay
/api/v2/labs/ingest:replay
/api/v2/labs/query:replay
/api/v2/labs/search-quality:replay
/api/v2/labs/search-quality:compare
/api/v2/labs/scope-federation:replay
/api/v2/labs/correlation:replay
/api/v2/labs/coordination:replay
/api/v2/labs/scheduler:replay
/api/v2/labs/orchestration:replay
/api/v2/labs/memory:replay
/api/v2/labs/policy-diff:compare
/api/v2/labs/privacy:test
/api/v2/labs/evolution:inspect
/api/v2/labs/evolution:simulate
```

Each response includes requested replay mode, actual fidelity, bundle/input/environment/catalog/index/memory/config refs, vector watermark, substitutions, unavailable/redacted/retained inputs, decision/explanation digests, and read-only guarantee. A route cannot label best-effort output as historical truth. Fixture promotion uses the command route below.

### 8.6 Saved views, annotations, exports, and subscriptions

| Method and path | Contract |
|---|---|
| `GET /api/v2/saved-views` / `GET /api/v2/saved-views/{id}` | List/read authorized views. |
| `POST /api/v2/saved-views:create` / `POST /api/v2/saved-views/{id}:update` / `:delete` | Direct typed commands with idempotency and expected version. |
| `POST /api/v2/saved-views/{id}:share-plan` / `:share-start` / `:share-revoke` | Classification/redaction/expiry plan, confirmed local share-bundle creation, and revocation; no remote publication. |
| Equivalent explicit read and typed command routes for `/collections` and `/annotations` | Protected investigation content, declared owner, and versions. |
| `POST /api/v2/exports:create` | Resumable workflow command that creates a frozen export job and operation receipt; no path field. |
| `GET /api/v2/exports` / `GET /api/v2/exports/{id}` | Status/manifest/coverage. |
| `GET /api/v2/exports/{id}/download` | Authorized completed artifact bytes with safe headers. |
| `POST /api/v2/exports/{id}:cancel` / `POST /api/v2/exports/{id}:delete` | Distinct versioned cancel/delete commands; neither is overloaded by HTTP method. |
| `POST /api/v2/subscriptions` | Authorize query/read-model body and create finite opaque subscription. |
| `POST /api/v2/subscriptions/{id}:revoke` | `subscriptions.revoke`: idempotent command-envelope revocation with ownership/auth/audit receipt. |
| `GET /api/v2/subscriptions/{id}/events` | SSE with `Last-Event-ID`; the opaque subscription resource was created from a protected request body and contains no query literal. |

### 8.7 Typed command routes

Every command accepts `CommandHttpRequest<C> { idempotency_key, scope, expected_version, payload }`. Field mapping onto the application `CommandEnvelopeV1<C>` is fixed and fixture-tested: application deterministically allocates `command_id`; `idempotency_key` maps to the plan 09 §8.1 reservation key; `scope` maps to declared/canonical scope; `expected_version` is the optimistic aggregate version; and `payload` is the typed command body. A catalog-declared confirmed destructive command has a distinct input type containing its operation-specific `OperationPreflightId` and protected confirmation token. Direct, autonomous, workflow, and internal modes have no inert preview fields. The route set covers every application command:

```text
POST /api/v2/commands/projects/{register,update-alias,unenroll}
POST /api/v2/commands/index/{refresh,pause,resume}
POST /api/v2/commands/watchers/{start,stop}
POST /api/v2/commands/daemon/{start,stop,drain,restart}
POST /api/v2/commands/runtime-update/{plan,start,recover}
POST /api/v2/commands/diagnostics/refresh
POST /api/v2/commands/doctor/run
POST /api/v2/repair:inspect
POST /api/v2/commands/repair:start
POST /api/v2/commands/backups/{create,restore}
POST /api/v2/storage-consolidation:inspect|plan                       # read-shaped operator preflight with sensitive source refs in body
POST /api/v2/commands/storage-consolidation/{start,resume,recover}    # operator-only merged-#425 mutations
GET  /api/v2/storage-consolidation/operations/{id}                   # status/receipts/exact recovery
POST /api/v2/commands/capture/{ingest,pause,resume}
POST /api/v2/commands/lcm/{compress,boundary,lifecycle-preflight,lifecycle-repair}
POST /api/v2/commands/automation/jobs/{create,update,delete,run,cancel,pause,resume}
POST /api/v2/commands/automation/scheduler/{enable,disable}
POST /api/v2/commands/curation/{pause,resume,run-now,pin,protect,exclude}
GET  /api/v2/curation/{status,history,decisions,outcomes}
POST /api/v2/commands/facts/{create,update,delete,feedback,pin,protect,exclude}  # explicit user-authored/admin facts, never candidate approval
POST /api/v2/commands/policy/{publish,activate,rollback}
GET  /api/v2/config/catalog
GET  /api/v2/config/catalog/{key}
POST /api/v2/config/catalog:search
POST /api/v2/config/targets:resolve
POST /api/v2/config/effective:query
POST /api/v2/config/{explain,diff,validate,status}
POST /api/v2/config/history:query
POST /api/v2/config/exports
POST /api/v2/commands/config/{patch,unset}
POST /api/v2/commands/config/batch:commit
POST /api/v2/commands/config/imports:commit
POST /api/v2/commands/config/history:restore-values
POST /api/v2/commands/config/credentials/{bind,unbind}
POST /api/v2/commands/config/drift:reconcile  # exact generated plan-20 inventory; no set/import aliases
POST /api/v2/commands/payloads/{gc-plan,gc-start}
POST /api/v2/commands/retention/{plan,start}
POST /api/v2/commands/holds/{create,release}
POST /api/v2/commands/entities/{retire-plan,retire-start}
POST /api/v2/commands/code/move-symbol:commit
POST /api/v2/commands/privacy/scans/{start,cancel}
POST /api/v2/commands/privacy/remediations/{plan,start,verify}
POST /api/v2/commands/privacy/quarantine/{hold,release}
POST /api/v2/commands/projections/{rebuild,pause,resume,publish,rollback}
POST /api/v2/commands/migrations/{backfill,reconcile,cutover,rollback}
POST /api/v2/commands/delivery/refresh
POST /api/v2/commands/coordination/{message,handoff,ack,suppress}
POST /api/v2/research/manifests:create-version
POST /api/v2/retrieval/corpus-versions:create
POST /api/v2/retrieval/corpus-versions/{id}:freeze
POST /api/v2/retrieval/qrel-versions:create
POST /api/v2/retrieval/qrel-versions/{id}:freeze
POST /api/v2/retrieval/candidate-pools:create
POST /api/v2/retrieval/judgments:record
POST /api/v2/retrieval/judgments/{id}:supersede
POST /api/v2/retrieval/adjudications:record
POST /api/v2/retrieval/evaluation-runs:run
POST /api/v2/retrieval/evaluation-runs/{id}:cancel
POST /api/v2/retrieval/evaluation-reports/{id}:publish
POST /api/v2/retrieval/fixtures:promote
POST /api/v2/retrieval/profiles:publish
POST /api/v2/retrieval/profiles/{id}:activate
GET  /api/v2/auth/tokens                              # auth.tokens.list; elevated read, no secrets/hashes
POST /api/v2/commands/auth/tokens:create              # auth.tokens.create over plan 17 §18.2's registry
POST /api/v2/commands/auth/tokens:revoke              # auth.tokens.revoke; never DELETE/query token material
POST /api/v2/commands/labs/fixtures/promote
POST /api/v2/initiatives:create
POST /api/v2/initiatives/{id}:update|pause|resume|retire   # former GET/PATCH mutation shapes are these commands
POST /api/v2/plans:create-version
POST /api/v2/plans:diff                                          # read-shaped protected comparison body
POST /api/v2/plans/{id}:activate|decompose
POST /api/v2/work-items:create
POST /api/v2/work-items/{id}:update|replace|retire|link|unlink|assign|reassign|pause|resume|cancel|reopen|archive|retry
POST /api/v2/work-items/{id}:record-attestation|record-review|record-decision|record-exception|handoff|reverse-transition
POST /api/v2/work-items:assign-set
POST /api/v2/execution-attempts/{id}:heartbeat|progress|complete|block
POST /api/v2/task-offers/{id}:accept|decline|revoke
POST /api/v2/context-packets/{id}:accept
POST /api/v2/task-notifications:create
POST /api/v2/task-notifications/{id}:update|delete
POST /api/v2/executors:register|heartbeat|drain|unregister
POST /api/v2/task-scheduler:explain                              # EffectClass::Read
POST /api/v2/task-scheduler:pause|resume|run-once
POST /api/v2/task-graph:doctor                                   # EffectClass::Read
POST /api/v2/task-views:create
POST /api/v2/task-views/{id}:update|delete|share-plan|share-start|share-revoke
```

The task/orchestration `:action` routes are the sole HTTP bindings for plan 24 §9.2's command use cases — no duplicate `/commands/**` aliases and no `PATCH` route exist — and they use the same `CommandHttpRequest` envelope, idempotency, expected version, operation-specific validation, and audit contract. `work-items:assign-set` is one bounded all-or-none owner-shard use case with plan/item expected versions and deterministic per-item receipts. `task-offers/{id}:accept` carries the expected offer/work/plan versions and readiness digest and is the sole public route that invokes the atomic sealed packet/attempt/lease transaction from plan 24 §9.4. Attempt lifecycle and packet acceptance require the active fence; packet acceptance also requires the exact prior packet and safe Turn boundary. Task-notification mutations never create implicit subscriptions. `WorkClaimV1` remains only under coordination reads/events; no task command is named “claim.”

Anchor operation IDs are exactly `retrieval_anchors.metadata_batch_get`, `retrieval_anchors.resolve`, and `retrieval_recipes.execute`, bound only to `POST /api/v2/retrieval-anchors:metadata-batch`, `POST /api/v2/retrieval-anchors:resolve`, and `POST /api/v2/retrieval-recipes:execute`. Research-manifest operations remain `research.manifests.list`, `research.manifests.get`, and `research.manifests.create_version`. Plan 17 generates SDK methods from these OpenAPI operations; no SDK resolves a `ResearchAnchorId`, treats safe metadata as payload authority, invents a research-specific evidence-anchor type, or bypasses the generic resolver.

Search-evaluation mutation routes bind one-to-one to `retrieval.corpus_versions.create/freeze`, `retrieval.qrel_versions.create/freeze`, `retrieval.candidate_pools.create`, `retrieval.judgments.record/supersede`, `retrieval.adjudications.record`, `retrieval.evaluation_runs.run/cancel`, `retrieval.evaluation_reports.publish`, `retrieval.fixtures.promote`, and `retrieval.profiles.publish/activate`. They use the ordinary command envelope and immutable/superseding artifacts: no route edits a frozen corpus/qrel, rewrites a prior judgment, hides source labels during adjudication, publishes private report content, promotes an unscanned fixture, or changes an in-flight query's profile.

The storage-consolidation routes bind only plan 09's operator workflow. `inspect/plan/status` are read-shaped application results; POST is used for inspect/plan because protected source references do not belong in URLs. `start/resume/recover` require administrative capability, exact source IDs, durable operation state, deterministic confirmation where applicable, and fail-closed path-plus-file/inode holder/lease/write-reservation evidence. V1 `preview/apply` names exist only in the compatibility adapter/inventory. These bindings are absent from curation credentials and cannot be invoked by task executors, scheduler, dashboard auto-save, or a generic Settings patch.

The explicit saved-view/collection/annotation and export action routes in Section 8.6 are the sole HTTP bindings for those commands; duplicate `/commands/**` aliases are not added. They use the same generated application command envelope, idempotency, expected version, direct validation or operation-specific share plan/start contract, and audit. Scope-sensitive create bodies require `declared_scope`; existing-target bodies carry the opaque target and the application resolves its canonical owner. No current V1 dashboard mutation may bypass this inventory.

Coordination commands carry application preconditions (plan 09 §10) that this API validates as transport shape and surfaces unchanged: an unexpired presence/overlap claim, stable research anchor, explicit target agent/capability, disclosed-summary/effect digest, idempotency key, and disclosure-safe summary. `message` and `handoff` record attempted delivery separately from target receipt; `ack` requires authorized target evidence; `suppress` is scoped to agent/pair/work-claim plus expiry. The API accepts no arbitrary provider address, hidden prompt payload, free-form tool invocation, or client assertion that delivery succeeded.

## 9. SSE Snapshot, Delta, Resume, Gap, and Backpressure

### 9.1 Subscription creation

Clients post the sensitive query/read-model body to `/subscriptions`. Application authorizes it, captures a frozen snapshot and access digest, and returns:

```rust
pub struct SubscriptionCreated {
    pub subscription_id: SubscriptionId,
    pub expires_at: UtcMicros,
    pub snapshot_watermark: VectorWatermark,
    pub replay_retention: Duration,
    pub stream_path: SafeRelativePath,
}
```

The subscription ID is 256-bit random plus server-side digest lookup, bound to authenticated session/principal, access digest, use case, query fingerprint, schema/ranking/catalog versions, expiry, and snapshot. It contains no query plaintext. Re-authentication with a different access digest invalidates it.

### 9.2 Wire events

```rust
#[serde(tag = "type", content = "payload")]
pub enum ApiStreamEvent {
    Snapshot(SubscriptionSnapshot),
    Delta { watermark: VectorWatermark, changes: Vec<SubscriptionDelta> },
    Operation(OperationProgress),
    Projection(ProjectionProgress),
    Coverage(CoverageReportV1),
    Gap { expected: VectorWatermark, available_from: VectorWatermark },
    ResyncRequired { reason: ResyncReason, restart: RestartDirective },
    ServerNotice { code: NoticeCode, retry_after_ms: Option<u64> },
}
```

`SubscriptionSnapshot` and `SubscriptionDelta` are generated tagged unions for the authorized canonical `TraceQueryV1` read-model kind. Task deltas include the causing canonical `task_graph_events` sequence range plus projector watermark; they never expose a parallel task event identifier or accept writes. `OperationProgress` covers command, job, workflow, export, migration, and automation state with stable operation/event/audit refs and an explicit terminal disposition. A command apply returns HTTP `200` when its semantic effect is complete and `202` with the same `CommandReceipt` when a durable operation/workflow/job remains; clients follow the operation event and can recover through `GET /operations/{id}`, never infer completion from transport acceptance.

- The first semantic event is `snapshot` unless a valid `Last-Event-ID` resumes after it.
- Every semantic event has `id: <authenticated-opaque-event-id>`, `event: <stable-name>`, one canonical JSON `data:` payload, and no server-generated retry that exceeds client policy.
- Heartbeat is an SSE comment every 15 seconds; it consumes no sequence and requires no client state change.
- Event IDs bind subscription, stream sequence, vector watermark, schema/projector versions, expiry, and access digest through canonical CBOR + HMAC-SHA256. Tamper/mismatch returns 409/410 before stream start.
- Replay retention defaults to five minutes or 10,000 semantic events per subscription, bounded globally. Expiry/gap emits `resync_required` when possible, then closes; reconnect creates a new snapshot.
- Duplicate/out-of-order source changes are suppressed by query semantics before wire mapping. Client still applies deltas idempotently by event ID/entity stable key.

### 9.3 Backpressure and disconnects

- Each connection has capacity 256 semantic frames and 2 MiB serialized queued bytes; per-principal and global connection caps are explicit configuration.
- Repeated upserts for one entity may coalesce to the newest upsert only when no remove, coverage, gap, progress terminal, or ordering boundary lies between them.
- Coverage, remove, gap, resync, operation/workflow terminal, privacy, and command/projection failure events never coalesce away.
- At soft pressure, coalesce eligible updates and emit safe metrics. At hard pressure, enqueue one `resync_required { slow_consumer }` if possible and close. Never discard and continue as if complete.
- Disconnect cancels the transport stream and releases API buffers; application/query subscription retention owns finite replay, not an Axum task leak.
- Browser/client reconnects with capped exponential backoff and `Last-Event-ID`. `401/403` stops retry; `409/410` creates a new subscription/snapshot; `429/503` honors bounded retry advice.
- SSE headers include `Content-Type: text/event-stream`, `Cache-Control: no-store`, `X-Accel-Buffering: no`, and security headers. Compression/proxy buffering is disabled for the event stream.

## 10. Loopback Authentication, CSRF, Host/Origin, CSP, and Request Security

### 10.1 Bind and Host policy

- Default listeners bind `127.0.0.1` and `[::1]` only. Startup fails rather than silently falling back to wildcard.
- Non-loopback bind requires explicit protected-mode configuration, TLS, an architecture/security review, and is outside first-default support.
- Accept only exact configured authorities: `127.0.0.1:<port>`, `[::1]:<port>`, and `localhost:<port>` when enabled. Reject missing, malformed, multiple, userinfo, trailing-dot, wildcard, unexpected port, and untrusted `Forwarded`/`X-Forwarded-*` headers.
- Absolute-form request targets and proxy mode are rejected by default, preventing DNS-rebinding/proxy confusion.
- A user-owned Unix-domain socket listener is a first-class transport built by this crate (plan 17 §18.1 prefers it for local nonbrowser clients; plan 17 §24 owns its conformance suite). The socket directory and file are owner-only (`0700`/`0600`), peer credentials (`SO_PEERCRED`/`getpeereid`) must match the profile owner, and application token authentication still applies. Cookie, CSRF, and Host/Origin rules are HTTP-listener browser rules and neither apply to nor weaken over the socket; browser sessions are not accepted on it.

### 10.2 Launch, browser session, and bearer authentication

1. Server generates a 256-bit per-launch secret from the OS CSPRNG and never logs it.
2. Static HTML receives one single-use, 60-second bootstrap nonce in a nonce-authorized bootstrap script/meta block. It is not placed in URL, referer, local storage, analytics, logs, or cache.
3. Browser posts the nonce to `POST /api/v2/auth/session` under strict Host/Origin. Server consumes it and creates an HttpOnly, SameSite=Strict, Path `/api/v2` session cookie; `Secure` is required whenever HTTPS is used and loopback HTTP is the only permitted exception.
4. Response returns a separate CSRF token held in page memory. Every cookie-authenticated state-changing request supplies `X-TraceDecay-CSRF`; token is session/method-family bound, rotates on login/privilege change, and compares constant-time.
5. The per-launch `Authorization: Bearer` token obtained from the owner-only (`0600`) runtime endpoint/file managed by composition is a bootstrap credential, not the operating credential: its only permitted operation is `auth.tokens.create` (plan 09 §10), which mints the initial admin-class entry in plan 17 §18.2's token registry. CLI/MCP/daemon clients then authenticate with scoped, TTL-bounded, revocable registry tokens. No token appears in a query parameter or command output.
6. Sessions expire at process exit and after bounded idle/absolute lifetimes; logout/restart revokes them. Authentication failures reveal no token validity detail.

All cookie-authenticated unsafe methods require valid CSRF, exact Origin, `Sec-Fetch-Site: same-origin` when supplied, JSON content type, and authenticated principal. Bearer clients do not require CSRF but still cannot bypass Host, method, schema, authorization, or limits. CORS is disabled by default; no wildcard origin or credentials response exists.

### 10.3 Browser/security headers

HTML policy:

```text
default-src 'none'; script-src 'self' 'nonce-<per-response>'; style-src 'self';
img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; worker-src 'self' blob:;
object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'
```

No `unsafe-inline` or `unsafe-eval`. Bootstrap nonce authorizes only the minimal generated bootstrap block. All responses set `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `Cross-Origin-Opener-Policy: same-origin`, and frame denial through CSP; HTML additionally uses `Cache-Control: no-store`. Hashed assets use immutable private caching and correct MIME. Source maps are off by default in release and never expose embedded user data.

### 10.4 Limits and timeouts

- Default JSON body 1 MiB; explicit lab/query bodies 4 MiB; fixture-promotion manifest 16 MiB after authorization; no generic unlimited override.
- Reject compressed request bodies by default. If later enabled for a declared route, enforce compressed and decompressed limits plus ratio/time ceilings before JSON parsing.
- Header section 32 KiB, URI 8 KiB, cursor 8 KiB, 100 query parameters, depth-limited JSON, bounded strings/arrays per schema, and finite numeric validation.
- Route-specific request timeout is less than application deadline; long work returns a job ID instead of holding HTTP open.
- Global/per-principal concurrency limits distinguish reads, expensive queries/labs, commands, exports, and SSE. Queue wait counts toward deadline.
- Cancellation propagates on disconnect/deadline to application/query, but cannot undo a committed command transaction; receipt lookup recovers outcome.

### 10.5 Export containment

- HTTP export create accepts format, query/view ID, redaction/payload policy, row/byte limits, expiry, and a display filename hint only. It never accepts a path, URI, device, symlink, or overwrite flag.
- Application/store select the private profile export root, exclusive staging file, permissions, fsync, hash, manifest, and atomic publication. API receives only `ExportId`/authorized byte stream.
- Display filename is Unicode-normalized, strips separators/control/reserved names, length-capped, and always emitted through standards-compliant `Content-Disposition` with a safe ASCII fallback.
- Download sets exact type, length/hash/ETag, `nosniff`, `Content-Disposition: attachment`, `Cache-Control: no-store`, and supports bounded byte range only for immutable completed parts.
- Incomplete/cancelled/expired/redacted-denied exports cannot download. API never reveals the backing filesystem path.

## 11. OpenAPI and Generated TypeScript Client

### 11.1 Source and generation

- Generation authority (single source): plan 17's contract IR is the only source of generated public contract artifacts. Pipeline: domain schemas + application use-case registry + plan 08 capability catalog → canonical contract IR snapshot (`crates/tracedecay-api/contract/tracedecay-contract-ir.v1.json`, owned by plan 17) → generated OpenAPI 3.1 (`crates/tracedecay-api/src/openapi/generated.json`, hosted by plan 10), the review rendering `crates/tracedecay-api/openapi/tracedecay-v2.yaml`, and the public JSON Schemas (`crates/tracedecay-api/schemas/*.schema.json`) → plan 10's Axum adapters conform to the IR-generated document, with utoipa reflection retained as validation only (CI regenerates the utoipa-derived document and fails unless it is semantically identical to the IR-generated artifact) → the generated TypeScript schema core at `packages/tracedecay-client/src/generated/` is produced from the IR-generated OpenAPI and hosted per plan 10, while plan 17 owns SDK packaging and conformance. The capability catalog remains the registry of record for capability/binding identity; the contract IR is its frozen public projection, and no plan or adapter maintains a second route registry.
- The checked-in document uses deterministic key/path/schema ordering and strips nondeterministic build timestamps.
- Operation ID equals the current catalog HTTP `BindingId`; extension fields carry canonical `UseCaseId`, capability ID/version, read/mutate, idempotency, preview, streaming, freshness, privacy, and cost/latency. Old/migration bindings never enter shipped OpenAPI.
- Security schemes describe browser cookie + CSRF header and bearer auth. Every operation declares one valid scheme and required scopes; no accidental anonymous operation.
- The generator validates method/path uniqueness, request/response/error schema mappings, status sets, cursor/page semantics, command headers/body, SSE event union, and catalog parity.
- `packages/tracedecay-client/src/generated/schema.ts` is generated with `openapi-typescript`. Its transport-neutral runtime owns request IDs, problem decoding, cancellation, cursor helpers, and SSE state. `dashboard/packages/api-client` imports it and adds only browser cookie/CSRF/bootstrap handling; it has no generated schema or competing error/event model.
- Generated files carry source commit/catalog/domain/application/OpenAPI digests and are changed only by the generation command.

Generation command:

```bash
cargo run -p tracedecay-api --bin generate-openapi -- --check   # regenerate from the contract IR; assert utoipa-reflection equality
pnpm --dir packages/tracedecay-client generate
pnpm --dir packages/tracedecay-client test
```

Expected in check mode: regenerated OpenAPI and TypeScript bytes exactly match checked-in files; all route/catalog/schema digests agree.

### 11.2 Client behavior

- `ApiClient` methods accept typed request plus `AbortSignal`; queries/commands never use untyped JSON maps.
- `ApiResult` preserves `meta`; convenience methods cannot return `data` without exposing meta to the caller.
- `ApiProblemError` preserves code/retry/current-version/restart/current-binding/candidates/invalid/operation fields, while redacting body/token from logs.
- SSE client keeps subscription IDs and `Last-Event-ID` in page memory only; they are session-bound capability material, not a nonsensitive IndexedDB key. It applies deltas idempotently, reports stale/offline/gap, and requests a new snapshot when required.
- Client sends no sensitive query/annotation/saved-view text in URL, analytics, error telemetry, or clipboard links.
- Runtime schema smoke tests feed every checked-in semantic fixture through Rust serialization, OpenAPI validation, generated TypeScript decoding, and reserialization.
- Stable research anchors/recipes round-trip as generated types. Client helpers may follow an opaque page cursor, but saved state, copy links, exports, and recovery APIs reject an ephemeral response handle as their only locator.

## 12. CLI, MCP, Dashboard, and HTTP Semantic Parity

The parity harness invokes one application use case through each applicable adapter and compares canonical semantic JSON after removing transport-only request IDs/timing:

```rust
pub struct TransportSemanticFixture {
    pub use_case: UseCaseId,
    pub input: CanonicalRequestFixture,
    pub expected: CanonicalResponseFixture,
    pub bindings: Vec<BindingId>,
    pub allowed_presentation_differences: Vec<PresentationDifference>,
}
```

Required comparisons:

- catalog-declared explicit default (`AllAuthorized` for Brain/Observatory, `CurrentInvocation` only where declared); repository/project/worktree/ref identity; same-name candidates/order; one-step retry token; provenance/freshness/partial state;
- entity/row/edge identity, order, facets, aggregates, cursor claims/restart codes, snapshot/watermark, coverage, freshness, redaction, retention, evidence, confidence, ranking explanation;
- native/representative/human-best-effort/direct-user/delegated-agent/tool-result/provider-protocol message views and representative provenance;
- command preview, idempotency, expected/current version, effect/audit/workflow receipt, conflict/retry;
- replay mode/fidelity, digests, substitutions, read-only behavior;
- Git local/live revisions, reconciliation/drift, fallback state;
- error code/retry semantics before CLI exit-code, HTTP status, or MCP markdown rendering;
- SSE initial snapshot equals ordinary HTTP query snapshot at the same watermark; export logical rows equal paged query rows at its frozen watermark.

MCP markdown and CLI text may differ only in checked presentation fixtures. JSON modes use the typed schema. Handler source is linted to reject store/query/policy imports and direct SQL. V1 aliases exist only in the internal migration harness; current help, hints, catalogs, and live dispatch never advertise or execute them after domain cutover.

## 13. Static SPA, History Fallback, and Bounded Migration Paths

- `/` and known V2 product routes serve release HTML; hashed `/assets/*` serves only assets. Missing asset paths return 404, never HTML.
- The authenticated API explorer and docs shell (plan 17 §22) are served by this same static_app under `/docs` with identical CSP/bootstrap/auth rules; its "try" calls use ordinary authenticated `/api/v2` routes, and the fixture-backed sandbox (plan 17 §22.3) is a separate process/profile, never a production route family.
- History fallback recognizes the route manifest generated by the dashboard build. Unknown `/api/*`, dotted paths, traversal, encoded separators, NUL/control characters, and unsupported methods never fall through to SPA.
- While the explicit migration flag is active, legacy `?tab=` and plugin URLs may issue a safe same-origin redirect to equivalent V2 state using opaque IDs. After cutover, stale paths return `client_update_required` — or `capability_replaced` with the current binding — plus the current route/update action, and never silently redirect or revive a stale name.
- Direct reload, base path, back/forward, embedded asset, stale asset hash, CSP nonce, service-worker absence, and host-wrapped dashboard are contract-tested.
- Old and new shells may coexist only behind explicit migration feature state until every V1 view/action parity row passes. Cutover removes old live route/name resolution atomically; retained plugin assets, if still needed for rollback evidence, are not served as a fallback.

## 14. PR and TDD Execution Plan

Commands run from repository root using checkout-local `target/`; do not set target/data-dir overrides unless Cargo reports target-lock contention. Each red test must fail for the named missing route/security/contract before production work.

### PR 24B1: Crate boundary, envelopes, explicit router, and core reads

**Files:** workspace/API `Cargo.toml`; `src/{lib,error,state,config,router,extract,response,limits}.rs`; `src/http/{mod,capabilities,scopes,query,search,entities,observatory}.rs`; `tests/{router_contract,request_response}.rs`.

- [ ] Add tests `every_catalog_http_binding_has_one_route`, `route_has_one_use_case`, `brain_default_is_all_authorized`, `current_invocation_only_when_catalog_declares_it`, `same_name_scope_candidates_round_trip`, `candidate_retry_replays_original_body_once`, `scope_parity_matches_cli_and_mcp`, `partial_result_stays_success_with_coverage`, `application_error_maps_stably`, `identity_split_never_maps_to_not_initialized`, `foreign_doctor_action_is_unrepresentable`, `partial_provider_never_serializes_healthy`, `research_anchor_does_not_depend_on_response_handle`, `unknown_api_route_never_returns_spa`, `text_query_requires_post`, and `disconnect_cancels_application`.
- [ ] Run `cargo test -p tracedecay-api --test router_contract --test request_response -- --nocapture`. Expected: compilation fails because API crate/router do not exist.
- [ ] Implement Sections 6–8 core router/envelopes/extractors/error mapping/limits and capability/scope/query/entity/health routes over fixture application registry.
- [ ] Re-run the command. Expected: all tests pass; route/catalog sets match exactly; fixture application receives canonical input once.
- [ ] Commit `feat(api): add bounded HTTP V2 core`.

### PR 24B2: Loopback auth, Host/Origin/CSRF, CSP, and abuse limits

**Files:** `src/auth/*.rs`; `src/security/*.rs`; `src/http/auth.rs`; `tests/security.rs`; security fuzz corpus.

- [ ] Add cases for wildcard bind refusal, DNS-rebinding Host, forwarded-host spoof, missing/foreign/null Origin, cross-site fetch, bootstrap replay/expiry, cookie mutation without CSRF, CSRF rotation, bearer-in-query rejection, token timing class, oversized/decompression body, header/URI/JSON depth, clickjacking, MIME sniff, and secret-free logs.
- [ ] Run `cargo test -p tracedecay-api --test security -- --nocapture`. Expected: tests fail because security/auth middleware is absent.
- [ ] Implement Section 10 with CSPRNG launch/bootstrap/session/CSRF tokens, constant-time digests, exact middleware order, body/time/concurrency limits, and security headers.
- [ ] Re-run the command. Expected: all attack fixtures fail closed with safe problem bodies; valid same-origin cookie and bearer requests pass; logs contain no supplied secret needle.
- [ ] Run the cursor/problem/query fuzz target for the CI smoke duration. Expected: no panic, allocation blowup, secret reflection, or auth bypass.
- [ ] Commit `feat(api): secure the loopback boundary`.

### PR 24B3: Product/domain reads and #410 session/message surface

**Files:** remaining `src/http/*.rs` excluding commands/labs/exports; `tests/sessions_messages.rs`; post-#410 fixtures.

- [ ] Add contract cases for no-text list-all sessions/messages, stable cursor pages, every domain `MessageView`, sanitized-native row completeness, representative provenance/native expansion, stable session/thread/message/subagent/workflow/Git anchors and retrieval recipes, metadata-batch returning no content, authorized resolution returning the exact record/payload state, recipe execution preserving versions/watermark/coverage, same/parallel-worktree nearby-agent claims, expired-presence unknown state, safe coordination summaries, Turn/agent/workflow/goal links, settings source/owner reads, Brain Work/plan/task/attempt/blocker/lease/acceptance clusters and bounds, federated Rspack/Rsbuild/React Router provenance and same-name disambiguation, Git drift, lexical/phrase/fuzzy/entity/semantic/graph/recency search stages and caps, locked/partial domain reads, and unknown denominators.
- [ ] Run `cargo test -p tracedecay-api --test sessions_messages --test request_response product -- --nocapture`. Expected: tests fail because routes/schemas are absent.
- [ ] Implement Sections 8.2–8.4 typed mappings with no business branching beyond extraction and application error mapping.
- [ ] Re-run the command. Expected: sanitized-native counts/manifest digests match; representative expands exactly; all responses retain metadata and bound sizes.
- [ ] Commit `feat(api): expose V2 investigation reads`.

### PR 24B4: Commands, jobs, labs, saved state, and contained exports

**Files:** `src/http/{commands/**,labs/**,saved,exports}.rs`; `tests/{commands,labs,exports}.rs`.

- [ ] Add tests `command_retry_returns_same_receipt`, `idempotency_conflict_is_409`, `version_conflict_includes_current_version`, `confirmed_operation_requires_current_preflight_token`, `direct_command_rejects_generic_mode_fields`, `scope_sensitive_create_requires_declared_scope`, `route_filter_never_selects_owner`, `daemon_exit_without_drain_receipt_is_not_success`, `update_recovery_is_pollable`, `coordination_target_must_be_unexpired`, `delivery_is_not_ack`, `suppression_is_bounded`, `coordination_lab_cannot_invoke_command`, `share_bundle_preflight_expires`, `lab_has_no_mutating_route`, `evolution_simulation_is_read_only`, `export_rejects_path_fields`, `download_never_exposes_backing_path`, and `incomplete_export_cannot_download`.
- [ ] Run `cargo test -p tracedecay-api --test commands --test labs --test exports -- --nocapture`. Expected: tests fail because command/lab/export handlers are absent.
- [ ] Implement Sections 8.5–8.7 and export containment/header mapping; use generated command mapping to avoid handler-specific command semantics.
- [ ] Add search-evaluation command cases for corpus/qrel create/freeze immutability, pool creation, judgment supersession, adjudication source preservation, durable run/cancel, aggregate-only report publication, unscanned fixture refusal, profile publish/activation CAS, and every read/SDK operation binding; add task attestation/review/decision/exception/handoff/reopen/reverse-transition cases with no derived-state setter or generic undo.
- [ ] Re-run the command. Expected: all tests pass; application receives exact command envelope; path/symlink/filename attack corpus cannot influence backing path.
- [ ] Commit `feat(api): expose audited commands and replay labs`.

### PR 24C1: Subscription resource, snapshot, and SSE event framing

**Files:** `src/sse/{mod,subscription,event,event_id,heartbeat}.rs`; route additions; `tests/sse_resume.rs`.

- [ ] Add tests `subscription_body_never_enters_url_or_event_id`, `subscription_capabilities_are_memory_only`, `first_event_is_matching_snapshot`, `event_id_rejects_tamper_and_access_change`, `last_event_id_resumes_in_order`, `operation_terminal_is_not_coalesced`, `heartbeat_consumes_no_sequence`, `expired_replay_requires_resync`, and `auth_revocation_stops_stream`.
- [ ] Run `cargo test -p tracedecay-api --test sse_resume -- --nocapture`. Expected: tests fail because subscription/SSE modules are absent.
- [ ] Implement Section 9 creation, opaque IDs, event mapping, authenticated resume, finite replay, heartbeat, cancellation, and headers.
- [ ] Re-run the command. Expected: all tests pass; resumed union equals reference stream once; secret query needle is absent from URLs/IDs/logs.
- [ ] Commit `feat(api): add resumable snapshot-delta SSE`.

### PR 24C2: Coalescing, backpressure, gaps, and reconnect fault matrix

**Files:** `src/sse/{resume,coalesce,backpressure}.rs`; `tests/sse_backpressure.rs`; `benches/sse.rs`.

- [ ] Add duplicate/out-of-order delta, repeated upsert, remove/upsert boundary, coverage change, gap, unavailable shard, projector version change, 257-frame slow client, 2 MiB queue, disconnect, 1,000 reconnects, and global/principal cap fixtures.
- [ ] Run `cargo test -p tracedecay-api --test sse_backpressure -- --nocapture`. Expected: tests fail because pressure/gap behavior is absent.
- [ ] Implement bounded queues and only the coalescing rules in Section 9.3; slow clients receive resync/close, never silent continuation.
- [ ] Re-run the command. Expected: all tests pass; noncoalescible event sequence is identical; memory remains bounded.
- [ ] Run `cargo bench -p tracedecay-api --bench sse -- --save-baseline pr24c2`. Expected: report connections/events/bytes/p50/p95/RSS, bounded queue memory, and reconnect recovery latency.
- [ ] Commit `feat(api): make live streams loss-aware and bounded`.

### PR 24D: Deterministic OpenAPI and generated TypeScript client

**Files:** `src/openapi/*.rs` including `src/openapi/generated.json`; `packages/tracedecay-client/**`; `tests/openapi_drift.rs`.

- [ ] Add tests `every_route_has_catalog_operation`, `every_operation_declares_auth_and_problems`, `schemas_preserve_domain_message_origin_and_view`, `coordination_claim_summary_anchor_and_actions_are_typed`, `search_stage_caps_and_group_membership_are_typed`, `task_query_uses_trace_query_v1`, `canonical_task_refs_round_trip`, `sealed_context_packet_round_trips_every_field_and_anchor`, `no_task_events_route_or_stream`, `task_assignment_set_and_view_revoke_have_full_parity`, `sse_union_includes_operation_and_projection`, `task_delta_cites_canonical_journal_range`, `pending_receipt_has_pollable_operation`, `commands_require_idempotency_version_and_declared_scope`, `storage_consolidation_is_admin_only_and_not_curation`, `generation_is_byte_deterministic`, `utoipa_reflection_equals_ir_generated_document`, and `typescript_round_trip_matches_rust`.
- [ ] Run `cargo test -p tracedecay-api --test openapi_drift -- --nocapture` and the package client tests. Expected: fail because artifacts/client do not exist.
- [ ] Implement Section 11 generators, metadata extensions, TypeScript schema/client/problem/SSE runtime, and digest headers.
- [ ] Re-run generation in write mode, then check mode and all tests. Expected: no diff after second generation; Rust/OpenAPI/TypeScript semantic fixtures round-trip.
- [ ] Commit `feat(api): generate OpenAPI and official TypeScript client core`.

### Companion requirements for PR 24E series: CLI/MCP adapters and cross-transport parity

**Files:** companion adapter/parity files in Section 5 and one V1 handler family per PR.

- [ ] For each domain, add fixtures that invoke in-process, HTTP, CLI JSON, MCP JSON, dashboard client, export, and SSE snapshot where applicable; compare all fields listed in Section 12.
- [ ] Run the domain parity test. Expected: fail while one adapter reads V1 stores/services directly, uses renderer truncation as pagination, or omits metadata.
- [ ] Move one CLI/MCP domain to the current generated application mapping. Preserve old command/tool arguments and presentation only inside the migration parity fixture; shipped adapters expose no stale aliases after cutover.
- [ ] Re-run V1 compatibility and V2 parity suites. Expected: semantic JSON matches; presentation differences are the checked-in allowed set; no handler store/query/policy imports.
- [ ] Commit each slice as `refactor(<transport>): use V2 <domain> application contracts`.

### PR 25 companion: Secure SPA history/static delivery

**Files:** `src/static_app/*.rs`; `tests/static_history.rs`; dashboard route manifest/build integration.

- [ ] Add direct reload for every V2 route, missing asset, unknown API, dotted/traversal/encoded path, base path, hashed cache, stale asset, CSP/bootstrap, migration-flag tab mapping, post-cutover stale-path failure, back/forward, and source-map release cases.
- [ ] Run `cargo test -p tracedecay-api --test static_history -- --nocapture`. Expected: fail before route-aware history service exists.
- [ ] Implement Section 13 and connect generated dashboard route manifest. No API or asset miss may return HTML.
- [ ] Re-run command and browser E2E. Expected: all route/security/cache assertions pass in standalone and host-wrapped modes.
- [ ] Commit `feat(api): serve the secure V2 workbench shell`.

## 15. Performance, Reliability, Privacy, and Migration Gates

- API mapping overhead p95 is at most 5 ms for ordinary JSON reads and at most 3 ms for command mapping, excluding application time and network stack, on the reference machine.
- Core JSON serialization sustains current and 10x bounded pages without exceeding 1 MiB default response target; larger results paginate/stream/export.
- SSE supports the declared connection/event benchmark with bounded 256-frame/2 MiB per-connection queue, no unbounded task/channel growth, and reconnect/resume p95 recorded.
- HTTP cancellation reaches application/query promptly; command retry recovers a committed receipt rather than applying twice.
- Host/Origin/CSRF/token/path/body/header/URI/JSON/fuzz attack matrix fails closed with no sensitive reflection/logging.
- Secret corpus and plan 18 seam canaries yield zero URL, access log, problem body, summary, response-handle payload, OpenAPI/SDK example, source map, browser cache, export filename, cursor, anchor, event ID, or generated fixture hit. Privacy status derives from policy/coverage evidence, never lossy-row existence.
- No response omits application coverage/freshness/redaction/retention; no command omits idempotency/version/audit and optional operation/workflow receipt; pending work is pollable after stream loss.
- Native message HTTP/export rows equal the source manifest; representative view expands exactly and declares its algorithm/provenance.
- Search contract fixtures expose every lexical/phrase/fuzzy/entity/semantic/graph/recency stage, filters, caps, grouping/dedupe membership, score features, and benchmark profile; vector absence or disablement is a typed state, not a zero score.
- All/repository/project/worktree/ref scope responses and ambiguity retries are byte-semantic across HTTP/CLI/MCP/dashboard; federated cross-repo graphs/search retain per-entity repository/snapshot provenance and per-shard stale/partial state.
- Coordination fixtures expose expiring claim evidence and safe summaries; message/handoff/ack/suppress require separate receipts, and one overlap horizon cannot emit repeated dynamic hints.
- SSE snapshot equals ordinary query semantics at the same watermark; gap/slow client never continues as complete.
- OpenAPI/client generation is byte-deterministic and catalog-complete. Every route has auth, bounds, errors, operation/use-case ID, and semantic parity fixture.
- Internal V1 HTTP/CLI/MCP/dashboard fixtures remain until parity/backfill/rollback receipts close, but V2 default exposes only current generated routes and bindings. No non-disposable store is removed before verified migration; no old live route survives as a fallback or stale alias.
- New production files target at most 800 lines; formatting, lint, test, fuzz smoke, E2E, benchmark, and dependency gates pass.

## 16. Cutover, Rollback, and Removal

1. Serve `/api/v2` disabled except authenticated contract probes; V1 remains default.
2. Enable read routes per domain in shadow/explicit-client mode and compare typed semantics against application/V1 fixtures.
3. Enable V2 dashboard client for one read-only vertical slice while the legacy shell remains available only under the bounded migration flag.
4. Enable each command's catalog-declared execution mode per domain only after application parity/recovery receipts and API security tests pass; never stage a universal preview/apply layer.
5. Enable SSE after snapshot parity, finite replay, gap/backpressure, and reconnect E2E pass under load.
6. Switch CLI/MCP bindings per domain to the current generated application contract; remove old names from live help/hints/catalog at the same cutover.
7. During bounded migration only, an operator rollback may restore the V1 owner from its receipt. After V2 default, rollback uses a prior compatible V2 artifact/data snapshot, closes streams with typed restart, and never revives stale clients/names.
8. At each domain cutover, revoke old live API/plugin/tool bindings. Stale clients receive the plan 17 §12 stale-client codes — `client_update_required`, `daemon_restart_required`, or `capability_replaced` with the current binding — plus restart/update/current operation/path; current help/hints/catalog never advertise old names.
9. Remove retained route/handler code after archived parity/security/backfill/rollback receipts prove data safety. Store preservation is receipt-bounded and independent from live transport fallback.

## 17. Final Verification

- [ ] Run `cargo fmt --check`. Expected: exit 0.
- [ ] Run `cargo clippy -p tracedecay-domain -p tracedecay-application -p tracedecay-api --all-targets -- -D warnings`. Expected: exit 0, no warnings.
- [ ] Run `cargo test -p tracedecay-api --all-features`. Expected: every unit/integration/property/security/SSE/OpenAPI/static test passes, none ignored.
- [ ] Run the API fuzz smoke suite. Expected: no crash, leak, unbounded allocation, auth bypass, secret reflection, or invalid successful decode.
- [ ] Run OpenAPI and TypeScript generation twice, second time in check mode, then client tests. Expected: byte-identical artifacts and Rust/OpenAPI/TypeScript semantic round trips.
- [ ] Run V1 dashboard/HTTP, CLI, MCP, session/LCM, Git, memory, automation, settings, diagnostics, export/response-handle suites from compatibility inventory. Expected: green until each declared retirement.
- [ ] Run all cross-transport parity fixtures. Expected: application, HTTP, CLI JSON, MCP JSON, dashboard client, export, and SSE snapshot agree before presentation.
- [ ] Run HTTP/SSE benchmarks at current and 10x bounded corpora. Expected: Section 15 gates pass with reference machine, corpus, watermarks, p50/p95, bytes, connection count, allocations, and peak RSS recorded.
- [ ] Run `rg -n 'rusqlite|libsql|git2|octocrab|reqwest|sessions::|memory::store|global_db|db::connection' crates/tracedecay-api/src`. Expected: no matches.
- [ ] Inspect router/catalog/OpenAPI sets. Expected: one-to-one bindings, no `ANY` V2 gateway, no anonymous operation, no unbounded list/query/graph/timeline/export route, and no missing command.
- [ ] Complete #405/#407/#410 ownership/message migration, #411 doctor authority, #412 drain/update recovery, merged #425 operator consolidation contract/recovery fixtures, #413 inventory refresh, stable research anchor/recipe, DNS rebinding/Origin/CSRF/CSP, cursor/event tamper, SSE gap/slow client, export containment, SPA history, stale-client failure, cutover, and rollback drills before V2 API becomes default.
