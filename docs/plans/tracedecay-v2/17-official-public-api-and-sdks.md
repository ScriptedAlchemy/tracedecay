# TraceDecay V2 Official Public API and SDK Plan

> **For agentic workers:** implement this plan only after the V2 domain, query, policy, tool-catalog, application, and API contracts in plans 01, 05, 06, 08, 09, and 10 are stable enough to generate against. Use test-first, reviewable PR slices; this document adds no separate business-logic layer.

**Goal:** Make TraceDecay's full supported capability surface directly queryable by agents and integrations through one stable, documented, contract-first API with first-party Rust, TypeScript, and Python SDKs, while preserving semantic parity with CLI, MCP, HTTP, dashboard, exports, and live streams.

**Architecture:** `tracedecay-application` remains the sole use-case boundary and `tracedecay-tool-catalog` remains the capability registry. The private root `v2::api` module serves the official HTTP/SSE/OpenAPI contract; generated schema packages and small hand-written runtimes expose it as idiomatic SDKs. CLI and MCP use the same application/catalog definitions but do not loop through HTTP. Every binding is verified against the same semantic fixtures, typed scopes, stable anchors, errors, coverage, and replay rules. External callers consume protocol artifacts/SDKs, so the server adapter is not separately published.

**Initial deployment:** Local-first. Strong mode uses a service-owned Unix-domain socket or Windows named pipe with connect-only client ACL; authenticated loopback HTTP is also supported. Portable same-user local transport is explicitly degraded. Remote or hosted service operation is not assumed by this plan and must not weaken the local trust, privacy, or authorization contract.

**Publication snapshot:** [master §2.6](../2026-07-09-tracedecay-brain-rewrite.md#26-current-master-accepted-changes) plus [plan 13](13-research-provenance-and-context-anchors.md) are normative. Historical 0.0.47/0.0.48 fixture counts remain version-labelled evidence, not the current surface. The official surface includes ordinary-profile Hermes, current message views, doctor authority, race-safe `move_symbol`, typed identity split and merged consolidation (`de3d05dc`), release-integrity gates, proxy/reconnect/no-write-replay, negotiated catalog refresh, fact rank/counters, exact analytics, untracked/divergent data recovery, lifecycle-safe maintenance, restart-safe applied-manifest retirement, and operator-only consolidation/recovery.

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
- `22-incremental-context-scout-and-suggestion-envelopes.md` owns scout status/feedback/system-control and suggestion-envelope semantics; scout replay uses the generic experiment contract, and SDKs cannot trigger delivery or bypass its hermetic zero-production-effect guard.
- `23-session-lcm-temporal-retrieval-and-evaluation.md` owns temporal search/context/lineage/replay/evaluation semantics; SDK modes, anchors, cursors, coverage, no-answer reasons, and hydration are generated from that same contract.
- `24-canonical-task-plan-graph-and-multi-agent-executor.md` owns initiative/plan/work-item/executor/scheduler/context-packet semantics and the many-host adapter protocol; this plan generates supported orchestration/monitoring clients without turning an SDK into a task query engine, scheduler, route selector, event journal, lease authority, or board database. Task reads use registered entity/attribute/traversal/projection values inside canonical `TraceQueryV1`; `WorkClaimV1` is advisory and `task_offers.accept` is the sole public command that invokes atomic execution admission. The read/command surface is the inventory in plan 09 §§9–10 and plan 10 §8: reads are GET/query POST, every mutation is a POST command envelope, and task deltas use the ordinary subscription protocol rather than `/task-events`.
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
10. Inspect an operation-specific preflight when required and, only with explicit mutation authority, execute its named command with idempotency, optimistic version checks, audit, and a durable operation receipt.
11. Recover from every typed error using a machine-readable retry/restart/current-binding directive rather than prose parsing.
12. Cite stable TraceDecay retrieval anchors in its own plan, report, PR, or handoff so a later agent can recover the exact evidence.
13. Query or operate one cross-repository initiative, call transactional `work_items.assign_set` to assign bounded work sets to Codex and Claude routes with explicit provider/model/reasoning-effort/tool/budget constraints, inspect attempt list/detail/timeline and start-versus-current accepted packet state, list/accept/decline addressed offers, administer offers through an authorized revoke command, manage direct task notifications, and subscribe to canonical task read-model deltas without MCP, dashboard mediation, or a separate task-event stream.

Human developers should be able to accomplish the same work with `curl`, generated documentation, or an SDK without learning MCP wire details or reverse-engineering the dashboard.

## 3. Non-Goals and Explicit Boundaries

- No public SQL, FTS syntax, raw SQLite connection, arbitrary graph-store query language, filesystem traversal, shell execution, or renderer/plugin code upload.
- No hidden chain-of-thought endpoint. Only provider-exposed, retained, access-authorized reasoning summaries/artifacts and explicit coverage markers may be returned.
- No unbounded "dump everything" JSON response. Enumeration, graph expansion, search, timeline, export, and event delivery are all bounded and cursor/stream based.
- No automatic remote bind, cloud account, multi-tenant identity server, or internet exposure in the initial V2 release.
- No SDK-specific behavior. Rust, TypeScript, Python, raw HTTP, CLI JSON, MCP JSON, dashboard, export, and SSE snapshot must agree before presentation differences.
- No stale-client behavior emulation. Data migration and rollback may preserve user data; they do not keep obsolete live names, schemas, or semantics executing.
- No general GraphQL surface in V2. The bounded typed query/graph operations are easier to cost, authorize, version, and replay.
- No task-specific query language. Generated task helpers build/import the one `TraceQueryV1`, return its canonical digest, and round-trip through generic query, saved views, subscriptions, CLI/MCP, and dashboard without semantic conversion.
- No WebSocket requirement. Request/response, SSE, bounded NDJSON, and asynchronous operation polling cover the initial official surface.
- No agent credential minted merely because a local process can connect. Endpoint reachability is not authorization.
- No "helpful" SDK fallback from an explicit cross-project scope to the current project.

## 4. Public Contract Artifacts and Repository Layout

The contract build produces deterministic, checked artifacts from one registry snapshot:

```text
contracts/api/
├── tracedecay-contract-ir.v1.json   # canonical checked contract IR snapshot (Section 5.1)
├── openapi/generated.json           # hosted by root v2::api; canonical checked public OpenAPI 3.1
└── schemas/
    ├── catalog.schema.json
    ├── scope-selector.schema.json
    ├── trace-query.schema.json
    ├── graph-query.schema.json
    ├── event-stream.schema.json
    ├── api-problem.schema.json
    └── retrieval-anchor.schema.json

docs/api/tracedecay-v2.yaml          # generated review/release rendering

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

Generation authority (single source): plan 17's contract IR is the only source of generated public contract artifacts. Pipeline: domain schemas + application use-case registry + plan 08 capability catalog → canonical contract IR snapshot (`contracts/api/tracedecay-contract-ir.v1.json`, owned by plan 17) → generated OpenAPI 3.1 (`contracts/api/openapi/generated.json`, hosted by root `v2::api`), the review rendering `docs/api/tracedecay-v2.yaml`, and the public JSON Schemas (`contracts/api/schemas/*.schema.json`) → plan 10's Axum adapters conform to the IR-generated document, with utoipa reflection retained as validation only (CI regenerates the utoipa-derived document and fails unless it is semantically identical to the IR-generated artifact) → the generated TypeScript schema core at `packages/tracedecay-client/src/generated/` is produced from the IR-generated OpenAPI and hosted per plan 10, while plan 17 owns SDK packaging and conformance. The capability catalog remains the registry of record for capability/binding identity; the contract IR is its frozen public projection, and no plan or adapter maintains a second route registry.

The canonical contract IR snapshot is a named, checked artifact, not an in-memory build step:

- Path: `contracts/api/tracedecay-contract-ir.v1.json`.
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

`api status --json` returns only safe discovery material: endpoint kind, socket path/loopback origin or configured protected authority, server/protocol version, health, catalog/schema digest, authentication method, docs/OpenAPI path, current profile/`BrainId`, node role, and safe placement generation. It never returns bearer/session/CSRF secrets, raw addresses beyond the configured endpoint, database locations, node keys, or grants. `api token create|revoke` bind audited application commands; `api token list` binds the elevated read use case `auth.tokens.list` and returns metadata only. The per-launch bootstrap bearer may execute only `auth.tokens.create` for the initial admin-class token (plan 10 §10.2).

Discovery precedence for SDKs is explicit:

1. caller-supplied endpoint and credential;
2. `TRACEDECAY_API_ENDPOINT` and a supported credential provider, never a token embedded in the endpoint URL;
3. user-owned runtime discovery file with mode `0600`, process identity, expiry, endpoint, and public digests;
4. deterministic default service-owned Unix socket/Windows named-pipe or loopback status probe;
5. typed `endpoint_not_found` with the exact command to start/check the service.

SDKs never scan processes, ports, parent directories, dashboards, MCP config, or transcript files to guess an endpoint.

### 7.2 Bootstrap endpoints

- `GET /api/v2/meta` returns protocol, server version, instance/profile identity, catalog/schema digests, time, health summary, limits profile, and current compatibility policy. It is authenticated — plan 10's rule that every route except static assets and the one-time bootstrap exchange requires authentication holds without exception; endpoint-without-credential discovery uses `tracedecay api status --json` or the `0600` runtime discovery file (Section 7.1), never an anonymous HTTP handshake.
- `GET /api/v2/openapi.json` returns the exact checked contract for the selected current protocol.
- `GET /api/v2/schemas/{digest}/{name}` returns an allowlisted public schema artifact.
- `GET /api/v2/capabilities` provides cursor-based capability discovery, not one unbounded registry blob.
- `GET /api/v2/bindings/{use_case_id}` provides current CLI/MCP/HTTP/SDK/dashboard bindings and prerequisites.
- `POST /api/v2/scopes:resolve` resolves one or many human locators into canonical scopes with ambiguity and coverage.

Plan 28's public family is imported exactly from plan 08: `brain.status.get`, `brain.topology.get`, `brain.nodes.list|get`, `brain.join|leave`, `brain.nodes.rotate|revoke`, `brain.placements.list|plan|apply|verify`, `brain.sync.status|run|pause|resume|repair`, `brain.replicas.list|seed|verify|retire`, `brain.backup.status|verify`, `brain.failover.plan|promote|verify`, and `brain.repositories.candidates|adopt|split`. OpenAPI operation IDs and Rust/TypeScript/Python methods are generated one-to-one; `join` is the enrollment workflow and no `nodes.enroll` alias exists. Read clients receive status/topology/list/get/candidates only. Admin clients receive effects according to grants; MCP/CLI/UI are bindings of the same rows.

Plan 10 §8.8's enrolled-node synchronization routes are deliberately outside that public family. They are generated from a separate internal node-protocol IR into the server router and one private node client; they do not enter `contracts/api/tracedecay-contract-ir.v1.json`, public OpenAPI/JSON Schema, Rust/TypeScript/Python SDK packages, docs explorer, CLI, MCP, dashboard bindings, skills, hints, or agent capability discovery. Internal protocol rows have `BindingId` but no public `UseCaseId`; they invoke the existing `brain.sync.*`, membership, placement, replica, tombstone, and purge ports under mutual node authentication and fencing. Public clients can plan/start/status/repair synchronization but cannot send observation batches, fetch snapshot pages/tails, or forge acknowledgements.

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

### 8.1 Native task orchestration client lock

The following public bindings are a generated client-parity lock over plan 24's canonical application/catalog entries. They are exhaustive for these families; an SDK cannot collapse them into `get_work_item`, invent a client-side packet pointer update, or implement an offer/notification workflow locally. The HTTP column is a generated mirror for conformance examples; plan 10 §8 remains the sole router inventory and any mismatch fails generation.

| Capability | Official HTTP binding | Rust | TypeScript | Python |
|---|---|---|---|---|
| `attempts.list` | `GET /api/v2/execution-attempts` | `list_execution_attempts` | `listExecutionAttempts` | `list_execution_attempts` |
| `attempts.get` | `GET /api/v2/execution-attempts/{id}` | `get_execution_attempt` | `getExecutionAttempt` | `get_execution_attempt` |
| `attempts.timeline` | `GET /api/v2/execution-attempts/{id}/timeline` | `get_execution_attempt_timeline` | `getExecutionAttemptTimeline` | `get_execution_attempt_timeline` |
| `attempts.heartbeat` | `POST /api/v2/execution-attempts/{id}:heartbeat` | `heartbeat_execution_attempt` | `heartbeatExecutionAttempt` | `heartbeat_execution_attempt` |
| `attempts.progress` | `POST /api/v2/execution-attempts/{id}:progress` | `report_execution_attempt_progress` | `reportExecutionAttemptProgress` | `report_execution_attempt_progress` |
| `attempts.complete` | `POST /api/v2/execution-attempts/{id}:complete` | `complete_execution_attempt` | `completeExecutionAttempt` | `complete_execution_attempt` |
| `attempts.block` | `POST /api/v2/execution-attempts/{id}:block` | `block_execution_attempt` | `blockExecutionAttempt` | `block_execution_attempt` |
| `task_offers.list` | `GET /api/v2/task-offers` | `list_task_offers` | `listTaskOffers` | `list_task_offers` |
| `task_offers.get` | `GET /api/v2/task-offers/{id}` | `get_task_offer` | `getTaskOffer` | `get_task_offer` |
| `task_offers.accept` | `POST /api/v2/task-offers/{id}:accept` | `accept_task_offer` | `acceptTaskOffer` | `accept_task_offer` |
| `task_offers.decline` | `POST /api/v2/task-offers/{id}:decline` | `decline_task_offer` | `declineTaskOffer` | `decline_task_offer` |
| `task_offers.revoke` | `POST /api/v2/task-offers/{id}:revoke` | `revoke_task_offer` | `revokeTaskOffer` | `revoke_task_offer` |
| `context_packets.list` | `GET /api/v2/context-packets` | `list_context_packets` | `listContextPackets` | `list_context_packets` |
| `context_packets.get` | `GET /api/v2/context-packets/{id}` | `get_context_packet` | `getContextPacket` | `get_context_packet` |
| `context_packets.accept` | `POST /api/v2/context-packets/{id}:accept` | `accept_context_packet` | `acceptContextPacket` | `accept_context_packet` |
| `task_notifications.list` | `GET /api/v2/task-notifications` | `list_task_notifications` | `listTaskNotifications` | `list_task_notifications` |
| `task_notifications.get` | `GET /api/v2/task-notifications/{id}` | `get_task_notification` | `getTaskNotification` | `get_task_notification` |
| `task_notifications.create` | `POST /api/v2/task-notifications:create` | `create_task_notification` | `createTaskNotification` | `create_task_notification` |
| `task_notifications.update` | `POST /api/v2/task-notifications/{id}:update` | `update_task_notification` | `updateTaskNotification` | `update_task_notification` |
| `task_notifications.delete` | `POST /api/v2/task-notifications/{id}:delete` | `delete_task_notification` | `deleteTaskNotification` | `delete_task_notification` |
| `saved_views.share.plan` | `POST /api/v2/saved-views/{id}:share-plan` | `plan_saved_view_share` | `planSavedViewShare` | `plan_saved_view_share` |
| `saved_views.share.start` | `POST /api/v2/saved-views/{id}:share-start` | `start_saved_view_share` | `startSavedViewShare` | `start_saved_view_share` |
| `saved_views.share.revoke` | `POST /api/v2/saved-views/{id}:share-revoke` | `revoke_saved_view_share` | `revokeSavedViewShare` | `revoke_saved_view_share` |
| `executors.list/get` | `GET /api/v2/executors`, `GET /api/v2/executors/{id}` | `list_executors`, `get_executor` | `listExecutors`, `getExecutor` | `list_executors`, `get_executor` |
| `executors.match` | `POST /api/v2/executors:match` | `match_executors` | `matchExecutors` | `match_executors` |
| `executors.register` | `POST /api/v2/executors:register` | `register_executor` | `registerExecutor` | `register_executor` |
| `executors.heartbeat` | `POST /api/v2/executors:heartbeat` | `heartbeat_executor` | `heartbeatExecutor` | `heartbeat_executor` |
| `executors.drain` | `POST /api/v2/executors:drain` | `drain_executor` | `drainExecutor` | `drain_executor` |
| `executors.unregister` | `POST /api/v2/executors:unregister` | `unregister_executor` | `unregisterExecutor` | `unregister_executor` |
| `scheduler.status/explain` | `GET /api/v2/task-scheduler/status`, `POST /api/v2/task-scheduler:explain` | `get_scheduler_status`, `explain_scheduler` | `getSchedulerStatus`, `explainScheduler` | `get_scheduler_status`, `explain_scheduler` |
| `task_graph.status/doctor/events` | `GET /api/v2/task-graph/status`, `POST /api/v2/task-graph:doctor`, canonical subscription operations | `get_task_graph_status`, `diagnose_task_graph`, `subscribe_task_graph_events` | `getTaskGraphStatus`, `diagnoseTaskGraph`, `subscribeTaskGraphEvents` | `get_task_graph_status`, `diagnose_task_graph`, `subscribe_task_graph_events` |
| `work_items.record_attestation` | `POST /api/v2/work-items/{id}:record-attestation` | `record_work_item_attestation` | `recordWorkItemAttestation` | `record_work_item_attestation` |
| `work_items.record_review` | `POST /api/v2/work-items/{id}:record-review` | `record_work_item_review` | `recordWorkItemReview` | `record_work_item_review` |
| `work_items.record_decision` | `POST /api/v2/work-items/{id}:record-decision` | `record_work_item_decision` | `recordWorkItemDecision` | `record_work_item_decision` |
| `work_items.record_exception` | `POST /api/v2/work-items/{id}:record-exception` | `record_work_item_exception` | `recordWorkItemException` | `record_work_item_exception` |
| `work_items.handoff` | `POST /api/v2/work-items/{id}:handoff` | `handoff_work_item` | `handoffWorkItem` | `handoff_work_item` |
| `work_items.reopen` | `POST /api/v2/work-items/{id}:reopen` | `reopen_work_item` | `reopenWorkItem` | `reopen_work_item` |
| `work_items.reverse_transition` | `POST /api/v2/work-items/{id}:reverse-transition` | `reverse_work_item_transition` | `reverseWorkItemTransition` | `reverse_work_item_transition` |

Attempt detail/timeline return both immutable `context_packet` (start authority) and monotonic `accepted_context_packet`. `accept_context_packet` carries the current attempt/lease/fence, expected accepted packet, higher candidate packet, explicit safe Turn boundary, and idempotency key; no SDK exposes a general packet-pointer setter. Offer list is registration-scoped for executors, accept is the only offer command that may atomically yield a lease/start manifest, decline yields no authority, and revoke requires scheduler/admin authority. Notification mutations are direct expected-version/idempotent commands with ordinary receipts—never preview/apply pairs. Attestation, review, decision, exception, and handoff append typed evidence and cannot directly set derived readiness/acceptance; reopen creates a new work-item version; reverse-transition is a cataloged compensating transition over exact versions, never a generic undo or terminal-attempt reopen.

#### 8.1.1 Task-graph edit-bundle client lock

The edit-bundle family is the only public bulk-edit contract for an agent to export a complex task graph, edit sharded frontmatter Markdown, validate it repeatedly, and atomically submit it. Its seven operation IDs and generated methods are exact; plan 21 alone owns MCP tool/resource names.

| Capability | Official HTTP binding | Rust | TypeScript | Python |
|---|---|---|---|---|
| `task_graph.edit_bundles.export` | `POST /api/v2/task-graph/edit-bundles:export` | `export_task_graph_edit_bundle` | `exportTaskGraphEditBundle` | `export_task_graph_edit_bundle` |
| `task_graph.edit_bundles.get` | `GET /api/v2/task-graph/edit-bundles/{workspace_id}` | `get_task_graph_edit_bundle` | `getTaskGraphEditBundle` | `get_task_graph_edit_bundle` |
| `task_graph.edit_bundles.validate` | `POST /api/v2/task-graph/edit-bundles/{workspace_id}:validate` | `validate_task_graph_edit_bundle` | `validateTaskGraphEditBundle` | `validate_task_graph_edit_bundle` |
| `task_graph.edit_bundles.diff` | `POST /api/v2/task-graph/edit-bundles/{workspace_id}:diff` | `diff_task_graph_edit_bundle` | `diffTaskGraphEditBundle` | `diff_task_graph_edit_bundle` |
| `task_graph.edit_bundles.rebase` | `POST /api/v2/task-graph/edit-bundles/{workspace_id}:rebase` | `rebase_task_graph_edit_bundle` | `rebaseTaskGraphEditBundle` | `rebase_task_graph_edit_bundle` |
| `task_graph.edit_bundles.submit` | `POST /api/v2/task-graph/edit-bundles/{workspace_id}:submit` | `submit_task_graph_edit_bundle` | `submitTaskGraphEditBundle` | `submit_task_graph_edit_bundle` |
| `task_graph.edit_bundles.delete` | `POST /api/v2/task-graph/edit-bundles/{workspace_id}:delete` | `delete_task_graph_edit_bundle` | `deleteTaskGraphEditBundle` | `delete_task_graph_edit_bundle` |

No request model has `path`, `server_path`, `output_dir`, arbitrary URI, archive member, or overwrite fields. `export` returns `TaskGraphEditWorkspaceId`, the current `TaskGraphEditCandidateRefV1`, and content-free base/schema/count/expiry/anchor metadata; `get` returns metadata or a bounded stream selected by generated `Accept` helpers. `validate` accepts a byte/async-byte stream and returns an application-minted candidate reference. A language SDK may offer local conveniences that deterministically pack a caller-owned sharded directory or stream a caller-owned archive, but paths are consumed entirely in the client process and are neither serialized nor logged. All three SDKs preserve plan 01's exact `TaskGraphEditDiagnosticV1` fields and enclosing response coverage without a transport-local part/work-item/action schema.

The SDK workflow object pins `TaskGraphEditWorkspaceId`, exact `TaskGraphEditCandidateRefV1`, validation-receipt digest, base revision, expiry, and catalog/schema versions across calls. It never silently re-exports, rebases, retries an uncertain submit, or follows a conflict by overwriting. `rebase` is explicit and returns a successor candidate reference or typed conflict set. `submit` sends that exact reference plus every CAS/idempotency precondition and returns the atomic graph/event/audit receipt. Successful submit transitions the handle to `retired` and makes further upload/get calls fail locally before the server rechecks; `delete` is explicit/idempotent cleanup for abandoned work. Dropping a client handle is not claimed as cleanup, so SDKs expose `close/delete` and surface expiry while the server crash sweeper remains authoritative.

### 8.2 Search-evaluation client lock

The generated clients expose the complete plan 15 §0.1 family. These methods are absent from search-read credentials unless their distinct write grants are present.

| Capability family | Official HTTP bindings | Rust / TypeScript / Python methods |
|---|---|---|
| `retrieval.corpus_versions.list/get` | `GET /api/v2/retrieval/corpus-versions`, `GET /api/v2/retrieval/corpus-versions/{id}` | `list_corpus_versions` / `listCorpusVersions` / `list_corpus_versions`; `get_corpus_version` / `getCorpusVersion` / `get_corpus_version` |
| `retrieval.qrel_versions.list/get` | `GET /api/v2/retrieval/qrel-versions`, `GET /api/v2/retrieval/qrel-versions/{id}` | `list_qrel_versions` / `listQrelVersions` / `list_qrel_versions`; `get_qrel_version` / `getQrelVersion` / `get_qrel_version` |
| `retrieval.candidate_pools.list/get` | `GET /api/v2/retrieval/candidate-pools`, `GET /api/v2/retrieval/candidate-pools/{id}` | `list_candidate_pools` / `listCandidatePools` / `list_candidate_pools`; `get_candidate_pool` / `getCandidatePool` / `get_candidate_pool` |
| `retrieval.judgments.list/get`, `retrieval.adjudications.list/get` | `GET /api/v2/retrieval/judgments`, `GET /api/v2/retrieval/judgments/{id}`, `GET /api/v2/retrieval/adjudications`, `GET /api/v2/retrieval/adjudications/{id}` | `list_judgments` / `listJudgments` / `list_judgments`; `get_judgment` / `getJudgment` / `get_judgment`; `list_adjudications` / `listAdjudications` / `list_adjudications`; `get_adjudication` / `getAdjudication` / `get_adjudication` |
| Search Quality experiment/run/cell/stage/comparison reads | Plan 10 §8.5 generic top-level list/get routes filtered to `LabKindV1::SearchQuality` | Generic `list/get` methods for experiments, runs, cells, stages, comparisons, comparison cells, and reductions with typed filters and idiomatic casing |
| `retrieval.evaluation_reports.list/get` | `GET /api/v2/retrieval/evaluation-reports`, `GET /api/v2/retrieval/evaluation-reports/{id}` | `list_evaluation_reports` / `listEvaluationReports` / `list_evaluation_reports`; `get_evaluation_report` / `getEvaluationReport` / `get_evaluation_report` |
| `retrieval.profiles.list/get` | `GET /api/v2/retrieval/profiles`, `GET /api/v2/retrieval/profiles/{id}` | `list_retrieval_profiles` / `listRetrievalProfiles` / `list_retrieval_profiles`; `get_retrieval_profile` / `getRetrievalProfile` / `get_retrieval_profile` |
| corpus/qrel/pool writes | `POST /api/v2/retrieval/corpus-versions:create`, `/corpus-versions/{id}:freeze`, `/qrel-versions:create`, `/qrel-versions/{id}:freeze`, `/candidate-pools:create` | `create_corpus_version`, `freeze_corpus_version`, `create_qrel_version`, `freeze_qrel_version`, `create_candidate_pool` with idiomatic casing per SDK |
| judgment/adjudication writes | `POST /api/v2/retrieval/judgments:record`, `/judgments/{id}:supersede`, `/adjudications:record` | `record_judgment`, `supersede_judgment`, `record_adjudication` with idiomatic casing per SDK |
| experiment/report writes | Plan 10 §8.5 generic experiment create and run create/cancel/resume/retry/minimize; `POST /api/v2/retrieval/evaluation-reports/{id}:publish` | Generic experiment builders/operation methods plus `publish_evaluation_report` with idiomatic casing per SDK |
| fixture/profile promotion | Shared `POST /api/v2/experiments/fixtures:promote`; retrieval `/profiles:publish`, `/profiles/{id}:activate` | Generic typed `promote_experiment_fixture` plus `publish_retrieval_profile`, `activate_retrieval_profile` with idiomatic casing per SDK |

Every mutation is an ordinary expected-version/idempotent command or durable operation. Frozen versions and prior judgments remain immutable; reports expose only authorized aggregate/redacted material; fixture promotion requires sanitization and secret-scan receipts; profile activation is CAS-pinned and cannot alter an in-flight query.

### 8.3 Automation admission-observability client lock

The contract IR generates exactly these read methods from plan 08's semantic operations. The HTTP column mirrors plan 10's sole router inventory; no SDK convenience method creates a second skip-episode, frontier, retry, circuit, or quarantine operation.

| Capability | Official HTTP binding | Rust | TypeScript | Python |
|---|---|---|---|---|
| `automation.dirty_scopes.list` | `GET /api/v2/automation/dirty-scopes` | `list_automation_dirty_scopes` | `listAutomationDirtyScopes` | `list_automation_dirty_scopes` |
| `automation.admissions.list` | `GET /api/v2/automation/admissions` | `list_automation_admissions` | `listAutomationAdmissions` | `list_automation_admissions` |
| `automation.admissions.get` | `GET /api/v2/automation/admissions/{id}` | `get_automation_admission` | `getAutomationAdmission` | `get_automation_admission` |

`list_automation_dirty_scopes` returns the generated page of application views containing exact work/scope identity, per-shard current/considered/consumed/included frontiers, pending delta, unconsumed generation/count/reasons, quiet/retry deadlines, active-writer/coverage proof, semantic/evaluation digests, last-terminal input/outcome, and shared operation/policy health references for retry, circuit, pause, quarantine, reconciliation, and coverage. `list_automation_admissions` accepts the one generated request type with `representation = Receipts | CoalescedSkipEpisodes`; its episode variant preserves stable anchor, first/last evaluation time, evaluation count, latest policy-evaluation ID, job/scope, exact reason, semantic-input/frontier tuple, reconsideration time, and avoided model/tool/token/cost work. `get_automation_admission` returns the exact `AutomationAdmissionReceiptV1`. Pagers preserve the frozen scope, ordering, cursor, watermarks, coverage, and representation across every page.

Job/scheduler methods and these methods import the existing operation, `RetryDirective`, policy-health/circuit/pause, privacy-quarantine, and coverage types from the canonical generated schema packages. SDK runtimes may not infer those states from timestamps or error codes. The generated `run_now` request has no force/ignore-digest field: it may shorten cadence for a dirty scope but cannot bypass identical successful/`NoChange` input fencing, backoff, circuit, pause, quarantine, or coverage policy. Historical/unchanged execution uses a generic experiment method.

### 8.4 Host-integration administration client lock

Host integration is an official but admin-scoped contract generated from plan 09's application feature and plan 10's sole route inventory. The contract IR generates `HostIntegrationSummaryV1`, `HostIntegrationDetailV1`, `HostIntegrationDifferenceV1`, plan 09's exact `HostIntegrationDifferenceRowV1`, `HostIntegrationStatusV1`, distinct `HostIntegration{Install,Update,Repair,Uninstall,Verify}RequestV1` types, and the referenced package/component/registration/profile/ownership/trust/freshness/drift/restart enums. There is no action-string request. A difference row preserves independent desired, exact `HostCapabilityDispositionV1`, installed, observed, and effective axes plus safe reason/evidence/action refs; it never flattens or renames states. No SDK derives compatibility, health, or restart from versions or timestamps.

| Capability | Official HTTP binding | Rust | TypeScript | Python |
|---|---|---|---|---|
| `integrations.list` | `GET /api/v2/integrations` | `AdminClient::list_host_integrations` | `admin.listHostIntegrations` | `admin.list_host_integrations` |
| `integrations.get` | `GET /api/v2/integrations/{id}` | `AdminClient::get_host_integration` | `admin.getHostIntegration` | `admin.get_host_integration` |
| `integrations.diff` | `POST /api/v2/integrations:diff` | `AdminClient::diff_host_integration` | `admin.diffHostIntegration` | `admin.diff_host_integration` |
| `integrations.status` | `POST /api/v2/integrations:status` | `AdminClient::get_host_integration_status` | `admin.getHostIntegrationStatus` | `admin.get_host_integration_status` |
| `integrations.install` | `POST /api/v2/integrations:install` | `AdminClient::install_host_integration` | `admin.installHostIntegration` | `admin.install_host_integration` |
| `integrations.update` | `POST /api/v2/integrations/{id}:update` | `AdminClient::update_host_integration` | `admin.updateHostIntegration` | `admin.update_host_integration` |
| `integrations.repair` | `POST /api/v2/integrations/{id}:repair` | `AdminClient::repair_host_integration` | `admin.repairHostIntegration` | `admin.repair_host_integration` |
| `integrations.uninstall` | `POST /api/v2/integrations/{id}:uninstall` | `AdminClient::uninstall_host_integration` | `admin.uninstallHostIntegration` | `admin.uninstall_host_integration` |
| `integrations.verify` | `POST /api/v2/integrations/{id}:verify` | `AdminClient::verify_host_integration` | `admin.verifyHostIntegration` | `admin.verify_host_integration` |

Each lifecycle or active-probe method consumes canonical `HostProfileRef`/`HostInstanceId` and the generated desired component-set request with expected manifest/config/observation versions plus idempotency; it returns the shared `OperationRef`. The ordinary operation handle provides bounded polling, progress subscription, terminal receipt, cancellation boundary, retry directive, restart requirement, and uncertain-effect reconciliation. `verify` performs no repair, and SDKs never silently retry an uncertain install/update/uninstall/verify effect. These models and methods are absent from `ReadClient` and task-executor/curation clients even if a caller knows an operation ID.

No integration request, model, problem, event, example, debug representation, or operation receipt exposes a host filesystem path, raw host configuration body, command line, environment value, credential value, or arbitrary package manifest. Clients use opaque target/installation/component/credential refs, generated manifest/profile digests, safe capability differences, ownership/trust states, and content-free effect receipts. A local SDK convenience may display a caller-owned label, but it cannot serialize that label as host authority.

### 8.5 Provider freshness operation client lock

The generated command clients expose exactly one `capture.refresh` binding (`AdminClient::refresh_capture`, `admin.refreshCapture`, `admin.refresh_capture`) over `POST /api/v2/commands/capture/refresh`. It accepts the canonical profile/provider/source scope, committed frontier and target watermark plus expected config/catalog versions and idempotency, then returns the shared `OperationRef`. Equivalent calls join one daemon operation; polling/subscription exposes source opens, records/bytes, progress, partial coverage, cancellation boundary, and the identical terminal receipt. `ReadClient` search/session/LCM methods have no `catch_up`, `refresh`, or hidden-write option and never call this method automatically. `capture.ingest` is reserved for the authenticated source-broker client and is absent from ordinary public/admin agent conveniences.

## 9. Typed ScopeSelectorV2

Scope must be identical across API, SDKs, CLI, MCP, dashboard, saved views, exports, and retrieval anchors. `project_key` and a process's active checkout are internal/provider locators, not the public identity model.

### 9.1 Selector model

Plan 01 §14 solely defines `ScopeSelectorV2`, `ScopeRootV2`, `ScopeTargetV2`, and `ScopeLocatorV2`; plan 16 owns their federation and resolution behavior. The official contract IR imports their generated schema digest unchanged rather than restating a client variant. The task roots `Initiative`/`Plan`/`WorkItem`/`ExecutionAttempt`/`Executor` target plan 24's canonical task graph through the plan 09 §§9–10 / plan 10 §8 inventories. Resolution returns the canonical selector and candidates before query planning.

### 9.2 Resolution rules

- Canonical ID is exact and preferred.
- A named external repository/worktree/project never falls back to the active project.
- One exact candidate resolves automatically and records the evidence/alias used.
- Multiple candidates return `scope_ambiguous`, safe disambiguating labels, candidate canonical IDs, and a ready-to-retry request object.
- No candidate returns `scope_not_found`, searched registries/stores, safe near matches, registration/index status, and legal next actions.
- Same-basename repositories are disambiguated with safe parent/common-dir/registry identity, never credential-bearing remote URLs.
- Repository, checkout, worktree, branch/ref, and code snapshot remain different identities. A worktree query cannot silently read the base checkout graph.
- `AllAuthorized { profile_id }` means all authorized, registered selected-profile evidence. `CurrentInvocation` is legal only when the binding catalog declares it; `ScopeResolutionV2.defaulted_current` makes that choice visible. Skipped/locked/stale/unavailable stores appear in coverage.
- A selector containing only `ScopeRootV2::Profile { profile_id }` resolves before project discovery and routes directly to profile activity; a canonical query predicate distinguishes `DeclaredScope::Profile` from `DeclaredScope::ZeroProject` rows. A client CWD, provider home, or host profile cannot supply an implicit project or data owner. Canonical builders may explicitly compose authorized Profile+Project roots for read federation.
- A session or agent may relate to zero, one, or many repositories/worktrees. The API does not force one provider project key into canonical ownership.
- Scope resolution is versioned and produces a `ScopeResolutionId` usable in the query/retrieval anchor. The server revalidates authorization and liveness; it does not trust a client-cached path mapping forever.

### 9.3 Binding ergonomics

- HTTP accepts the full tagged selector.
- SDKs expose builders and typed constructors, never stringly `scope="all"` conventions.
- SDKs generate `profile(profile_id)` plus typed declared-scope query filters; they do not invent a `ZeroProject` scope-root constructor. Import-only `memory_scope=user`/`storage_scope=user` shims lower to a single Profile root with the legal declared-scope predicate and reject compatibility project fields; they are absent from current generated docs after compatibility cutoff. The ordinary canonical builder still supports explicit Profile+Project multi-root reads.
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

| Capability | Official HTTP binding | Rust | TypeScript | Python |
|---|---|---|---|---|
| `retrieval_anchors.metadata_batch_get` | `POST /api/v2/retrieval-anchors:metadata-batch` | `get_retrieval_anchor_metadata_batch` | `getRetrievalAnchorMetadataBatch` | `get_retrieval_anchor_metadata_batch` |
| `retrieval_anchors.resolve` | `POST /api/v2/retrieval-anchors:resolve` | `resolve_retrieval_anchors` | `resolveRetrievalAnchors` | `resolve_retrieval_anchors` |
| `retrieval_recipes.execute` | `POST /api/v2/retrieval-recipes:execute` | `execute_retrieval_recipe` | `executeRetrievalRecipe` | `execute_retrieval_recipe` |

Rules:

- Ephemeral response handles, page cursors, bearer tokens, event subscription IDs, and browser state are never the only retrieval citation.
- `retrieval_anchors.metadata_batch_get` is bound only to `POST /api/v2/retrieval-anchors:metadata-batch` and returns bounded safe identity/state/tombstone metadata without content.
- `retrieval_anchors.resolve` is bound only to `POST /api/v2/retrieval-anchors:resolve` and performs authorized exact record/payload resolution at a frozen watermark.
- `retrieval_recipes.execute` is bound only to `POST /api/v2/retrieval-recipes:execute` and performs bounded versioned recipe execution with scope/version/watermark drift and coverage.
- Resolution returns exact, moved/adopted identity, retained-but-redacted, expired-by-retention, unavailable-store, or denied. It never silently points to a similar row.
- Deep links contain an anchor ID or saved-view ID, not sensitive query text. Authorization is always rechecked.
- SDK result types surface `anchor` directly and provide only the generated methods in the table above; convenience `.data` access must not hide it or conflate metadata with payload authority.
- Export manifests include anchors and hashes so a later agent can verify the source snapshot.

## 11. Request, Response, Coverage, and Consistency Envelopes

### 11.1 Requests

Every request carries or inherits:

- resolved typed scope;
- caller-selected consistency: plan 01/28's `Authoritative`, `BoundedStale`, `OfflineCache`, or `AsOfWatermark`; generated language conveniences may not invent weaker aliases;
- bounded deadline and resource budget;
- requested fields/payload policy;
- result/page bound;
- optional trace/correlation ID;
- explicit replay/as-of mode when applicable.

The server owns actual authorization, plan cost, selected shards, and captured watermarks. Client-supplied estimates are hints only.

### 11.2 Responses

Every success uses plan 10's one canonical envelope. Contract IR projects and SDK generators re-export it; this plan does not redefine it:

```rust
pub use generated::contracts::{ApiMeta, ApiResponse};
```

The generated `ApiResponse<T>` contains `data` and `meta`; `ApiMeta` contains the exact plan-10 request/use-case/protocol/catalog/scope/snapshot/coverage/freshness/redaction/retention/limits/warnings fields with no SDK-only omission or addition.

`CoverageReportV1` is plan 01's canonical shared coverage type. No SDK convenience method may discard `meta` by default. A deliberate `into_data()` can consume the response only after making metadata loss obvious in code.

### 11.3 Truthful partial results

- Useful rows with one unavailable/stale/locked/redacted shard return success with `!coverage.is_complete()`.
- Each shard/source coverage item declares selected/skipped disposition, requested/captured watermark, schema/capability version, freshness, rows considered/returned when known, and safe reason.
- Multi-machine coverage also carries `BrainId`, placement generation, authority/replica/node identities and epochs, cache age/sync lag, unreachable/local-only/policy-excluded state, and pending local counts separately from canonical totals.
- Zero results plus incomplete coverage is not represented as "no matches".
- Counts declare exact, lower-bound, estimate, sampled, capped, or unknown.
- Search/graph scores declare algorithm/version and are not comparable across profiles unless explicitly normalized.
- SDK iterators aggregate coverage across pages and retain the least-complete state; they do not expose only the last page's metadata.

## 12. Error and Machine-Actionable Retry Contract

Errors use the exact RFC 9457-compatible `application/problem+json` `ApiProblem` shape owned by plan 10 §7.2. The public contract IR imports that schema and digest unchanged; generated language SDKs preserve unknown safe problem extensions but define neither a competing struct nor a code/status hierarchy. `ApplicationErrorCode` and retry/restart/candidate/version/operation meaning still come from application/domain, while HTTP alone supplies status and RFC 9457 fields.

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
- A cursor encodes exactly the domain `CursorClaimsV1` binding set (plan 01, codec owned by plan 05): query fingerprint, caller/access digest, canonical scope digest, profile-catalog generation, schema/ranking/index versions, frozen watermarks and per-shard positions, sort cutoff, temporal mode/cutoff, intent-profile version, partial-shard dispositions, and expiry. Interactive cursors use plan-20 `query.cursor.interactive_ttl` (default 15 minutes); export/bulk continuations last their catalog-declared job lifetime.
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

`work_items.assign_set` is the named transactional orchestration exception, not generic batch. Its bounded request pins one initiative/plan version, one owner shard, distinct work-item/assignment expected versions, and an explicit route constraint per item (adapter/provider/model/reasoning effort/tools/budget). Application validates the entire set, rejects cross-owner or active-lease-stealing input, and commits all assignments or none; the result includes deterministic per-item validation plus one transaction receipt. Generated Rust/TypeScript/Python SDKs expose `assign_work_item_set`, not a client-side loop over singular assignment calls.

### 13.3 Bulk and export

- Bounded NDJSON streams support large canonical row sequences where immediate streaming is useful.
- Large or expensive exports create an asynchronous `ExportOperation`, expose progress/coverage, and finish with a signed, expiring, contained download resource plus manifest/hash.
- Parquet/JSONL schemas are generated from canonical row types and versioned in the manifest.
- Client disconnect cancels uncommitted read work. Durable exports/jobs continue only when explicitly requested and remain pollable.
- Exports never accept an arbitrary output path through the public API.

### 13.4 Contained task-graph edit bundles

Task-graph edit bundles are not generic exports or generic batch. They use plan 10 §8.4.1's deterministic `application/vnd.tracedecay.task-graph-edit-bundle.v1+tar`: `manifest.md` plus sharded CommonMark documents, each beginning with one strict-subset YAML 1.2 frontmatter mapping. SDK encoders/decoders reject duplicate keys, tags, aliases, anchors, merge keys, implicit timestamps, floats, multiple documents, raw HTML, unknown fields, invalid UTF-8, unmanifested parts, duplicate/case-colliding names, and cross-part reference mismatch before presenting a candidate as locally well formed; server validation remains authoritative and repeats every check.

Download and upload are streaming and cancellation-aware. Implementations never buffer the whole archive by default, never automatically extract to a caller directory, and enforce the advertised two-hour/64-MiB/2-MiB/4,096-file/50,000-item defaults and 24-hour/256-MiB/8-MiB/16,384-file/100,000-item hard ceilings. Optional caller-side extraction uses a generated containment helper into a newly created caller-owned directory and applies the same relative-name/link/device/depth rules; it cannot alter what the server accepts. Failed ordinary validation keeps the server candidate repairable until TTL; secret/unknown-scan or containment failure retires bytes immediately. Submit success and explicit delete purge bytes immediately. All durable and SDK-visible receipts retain only opaque IDs, digests, counts, dispositions, and anchors—not CommonMark/YAML/archive bytes or client/server paths.

## 14. Streaming and Change Subscription

The official live contract is snapshot plus typed delta over SSE:

1. `POST /api/v2/subscriptions` submits the sensitive typed canonical query (including its explicit scope) and returns a session-bound subscription ID, initial snapshot reference, expiry, and stream path.
2. `GET /api/v2/subscriptions/{id}/events` emits the matching snapshot first, then ordered deltas/progress/coverage/operation/gap events.
3. `Last-Event-ID` uses an authenticated opaque event cursor bound to subscription, authorization, protocol, and sequence.
4. `POST /api/v2/subscriptions/{id}:revoke` invokes canonical `subscriptions.revoke`; disconnect never substitutes for the idempotent audit/release receipt.

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
- Task subscriptions are ordinary `TraceQueryV1` read-model subscriptions. Each delta carries the causing canonical task-journal sequence range and projector watermark; there is no `/task-events` endpoint, task-specific cursor, or client-side merge of an outbox/adapter stream into task truth.

## 15. Graph-of-Graphs API

The public graph surface treats code, Git, threads, sessions, Turns, agents, tasks/plans, workflows, goals, memory, skills, automation, time, and delivery as lenses over one evidence graph, not unrelated endpoint-specific node models. One `GraphCompositionSpecV1` can request a primary lens, at most two overlays, and explicit bridge kinds while preserving all lens semantics.

### 15.1 Typed operations

- `POST /api/v2/graph/neighborhood` — bounded expansion from entity/anchor roots.
- `POST /api/v2/graph/path` — bounded evidence path with allowed edge/entity kinds and maximum depth/cost.
- `POST /api/v2/graph/subgraph` — query-driven subgraph with LOD and cluster limits.
- `POST /api/v2/graph/impact` — downstream/upstream effect paths with confidence/evidence.
- `POST /api/v2/graph/diff` — compare frozen graph snapshots, refs, sessions, policies, or time windows.
- `POST /api/v2/brain/lens` — bounded composed lens over the same `GraphSliceViewV1`; no combined-graph model.
- `POST /api/v2/brain/atlas-tiles` — published profile-atlas generation, viewport/zoom-band tiles, prefetch ring, and anchor lineage.
- `POST /api/v2/query:compose-from-selection` — typed visual selection/action to visible canonical query delta.
- `POST /api/v2/entities:batch` — batch hydrate stable IDs already returned by another operation; no duplicate graph-only hydration use case.
- `POST /api/v2/timeline/events`, `/timeline/density`, `/timeline/replay-frames`, and `/timeline/derived-lane` — temporal projection, synchronized replay, and query-derived tracks over the same entities/relations.

### 15.2 Node and edge contracts

Every node includes stable typed ID, kind, safe label, time validity, owning/related scopes, availability, evidence summary, payload-access state, and retrieval anchor. Every edge includes kind, direction, valid/transaction time, evidence IDs, confidence/trust class, inference/projector version, and contradiction/uncertainty state.

The API distinguishes:

- observed relationship;
- provider-declared relationship;
- deterministic projection;
- heuristic/inferred correlation;
- user/agent annotation;
- unresolved candidate.

Graph results include exact membership or declared sample/cluster membership, per-node/edge lens membership and bridge role, atlas generation/anchor lineage where applicable, edge aggregation semantics, LOD/layout/community algorithm version, expansion cursor, selected watermarks, and partial coverage. A visualization cluster/tile is not serialized as a factual domain entity. Graph, timeline, and metric payloads share `VisualizationEnvelopeV1<T>` so ontology, query-delta affordances, camera/layout hint, bounded structured `AccessibilitySceneV1`, coverage, and export metadata cannot diverge by transport.

### 15.3 Safety and cost

- Query declares allowed node/edge kinds, direction, depth, maximum nodes/edges/paths, time range, and payload fields.
- Server estimates and enforces cost before expansion; high-fanout edges require aggregation or explicit narrowed continuation.
- Payload hydration is separate from topology and independently authorized/redacted.
- No arbitrary Cypher/SQL/GraphQL string is accepted.
- Cross-project traversal respects every source privacy domain and reports denied/redacted boundaries without leaking existence beyond authorization.
- Code graph nodes bind repository/worktree/ref/code-snapshot identity; the base checkout never substitutes for a requested parallel worktree.

## 16. Experiment, Search, Hint, and Policy Replay APIs

Replay is an official generic experiment resource, not one endpoint family per lab. `POST /api/v2/experiments:draft-from-selection` returns a typed non-persisted draft; `experiments:create` freezes `ExperimentSpecV1`; `experiments/{id}/runs:create` returns the ordinary pollable cohort operation; run cancel/resume/retry/minimize and all experiment/run/cell/stage/comparison/comparison-cell/reduction list/get reads are exactly the plan-10 §8.5 top-level routes. The generated stage-list method requires a cell and returns `ReplayTraceV1`; stage get returns `ReplayStageV1`. SDKs generate a closed `LabKindV1` input/parameter/output union and typed builder per evaluator, but every builder lowers to the same create/run/status lifecycle and pages typed cells rather than hiding variant/sweep coordinates in blobs.

### 16.1 Replay modes

- `exact_deterministic`: resolve the exact executable evaluator, schema, config, policy, catalog, index/model, project/memory/skill, and prompt-template digests.
- `recorded_result`: inspect exact historical inputs/candidates/results/payloads when executable artifacts are unavailable.
- `current_best_effort`: run the current evaluator against explicitly selected historical/synthetic inputs and label all substitutions/missing evidence.

Missing artifacts yield incomplete fidelity, never silent approximation.

### 16.2 Hint replay

An experiment with `lab=hint` accepts a stable message/Turn/session/hook anchor or an explicit synthetic input, selected policy bundle, scope, candidate catalog, and replay mode. Its run/trace returns:

- normalized safe input facts and redactions;
- all candidate capabilities/hints with features/scores;
- eligibility/privacy/availability decisions;
- repetition/cooldown/token/latency budget decisions;
- suppressions with stable reasons;
- exact rendered payload reference only when authorized;
- historical delivery/outcome evidence kept distinct from candidate prediction;
- current-versus-historical diff;
- artifact/fidelity manifest and retrieval anchors.

The hint evaluator never injects a hint, invokes a tool, publishes presence/claim, modifies memory/fact trust, increments usage/counters, records an acted outcome, or emits live analytics. Experiment persistence contains only immutable replay artifacts and explicit model/egress cost. Sharing uses the ordinary saved-view/export contracts; fixture promotion remains separate.

### 16.3 Search replay and evaluation

Experiments with `lab=search_quality` expose the pipeline in plan 15 through baseline/variant `ExperimentCellV1` coordinates inside one run cohort and the shared paged `ReplayComparisonV1`:

- exact/phrase/lexical/fuzzy/entity/sparse/dense/graph/recency candidate lanes;
- dedupe/representative/group membership and hidden counts;
- per-stage caps, component ranks/scores, fusion, diversity, reranker, and final explanation;
- index/model/corpus/profile versions and selected watermarks;
- coverage, no-answer decision, latency/resource measurements;
- relevance judgments only when authorized and never as hidden live labels.

Experiments can branch, sweep, ablate, align stages, and compare named retrieval profiles over a frozen local evaluation manifest. They cannot silently switch a live agent's retrieval profile, write judgments, or send private queries to a network model. Publishing and activating a profile use the direct immutable/CAS commands in §8.2 with locked gates and audit; prior versions remain available for an explicit later activation, not a fictional mutation rollback.

### 16.4 Hermetic lifecycle and reproducibility

Every SDK exposes operation polling/streaming, cancellation/resume/retry, run/stage/cell retrieval anchors, branch parentage, sweep coverage, manifest/fidelity/substitutions, and `ReplaySideEffectReceiptV1`. Exact replay freezes clock/RNG and verifies every executable/input/version digest; recorded-result mode never executes; best-effort lists every substitution. Network/model access is deny-by-default and requires an explicit budgeted grant. The receipt enumerates allowed opens and denied attempts and must report zero production effects. A redacted reproducibility bundle includes the manifest, variants, outputs, anchors, annotations, environment, and equivalent CLI/MCP/HTTP/SDK recipe without secret or quarantine content.

## 17. Commands and Mutation Safety

The official API may expose the broad writable capability surface, but direct-agent credentials default to read-only.

Every command request includes:

- stable use-case ID;
- explicit canonical scope and declared owner scope for created state;
- idempotency key;
- expected aggregate/config version;
- operation-specific inspection or confirmation digest where meaningful;
- authorization grant and approval provenance;
- bounded deadline/resource policy;
- optional client correlation ID.

Every result includes effect/audit receipt, current/new version, compensation/rollback availability, and either terminal output or durable operation/workflow ID.

Destructive or broad non-curation operations such as wipe, retention deletion, payload GC, migration cutover, external delivery, policy activation, and automation enablement require a capability-specific grant and their cataloged operation-specific inspect/plan plus confirmation contract. A generic `write` token is insufficient.

Merged #425's split-store consolidation is one such operator workflow. The public/admin contract exposes typed inspect/plan/start/status/resume/recover methods over two explicit source identities, path-plus-file/inode holder/freeze/reservation evidence, backup/staging/verification/cutover receipts, deterministic confirmation, per-table/artifact dispositions, and exact recovery. SDKs never accept arbitrary raw store paths, implement merge logic, or automatically retry an uncertain start. These methods appear only on `AdminClient`; task-executor and curation-service grants cannot discover or invoke them.

Fact/memory/managed-skill/profile curation is not exposed as item-level approve/apply/install/rollback endpoints. A dedicated least-privilege curation service grant plus versioned autonomy configuration authorizes the application worker to execute only owned, policy-eligible effects after transactional revalidation; every effect/outcome/recovery is audited. Public clients can read status/history/decisions/outcomes, configure policy, pause/resume/run-now, pin/protect/exclude, and submit feedback. Unsafe/foreign/out-of-authority candidates are automatically rejected/deferred/quarantined, never converted into a human approval endpoint.

Move-symbol is a first-class two-operation edit workflow, not a generic filesystem mutation: generated clients expose read-shaped `code.move_symbol.inspect` and separately authorized `code.move_symbol.commit`. Inspect returns exact source/destination/snapshot/version, impact classes, proposed imports, and a confirmation digest without writing; commit consumes that digest, revalidates both endpoints, requires the repository/worktree edit grant, and returns commit/recovery/reindex receipts with no automatic caller rewrite. Raw paths/source/diffs use protected/eligible fields and never enter URLs or client logs.

| Operation | HTTP | Rust | TypeScript | Python |
|---|---|---|---|---|
| `code.move_symbol.inspect` | `POST /api/v2/code/move-symbol:inspect` | `inspect_move_symbol` | `inspectMoveSymbol` | `inspect_move_symbol` |
| `code.move_symbol.commit` | `POST /api/v2/commands/code/move-symbol:commit` | `commit_move_symbol` | `commitMoveSymbol` | `commit_move_symbol` |

SDKs separate `ReadClient` and `AdminClient` surfaces where the language permits. Mutation methods do not appear on a read-only typed client. Raw HTTP still enforces the same server-side grant.

## 18. Authentication, Local Trust, Privacy, and Secret Handling

### 18.1 Transports

- Prefer the service-owned Unix-domain socket or Windows named pipe for local nonbrowser clients in strong mode. The endpoint ACL grants connect-only access to authorized client identities while the daemon identity retains ownership; peer identity and application authentication are both required. Plan 10 builds the platform adapters (plan 10 §10.1); this plan owns UDS/named-pipe conformance (Section 24). Owner-only same-user sockets are portable degraded mode, never isolation proof.
- Loopback HTTP binds only exact loopback by default and enforces strict Host/Origin/forwarded-header policy.
- Browser uses per-launch bootstrap, secure session cookie, and CSRF token.
- Agent/SDK uses a bearer token or local credential-provider handshake. Tokens never appear in URLs, process titles, command history examples, OpenAPI examples, or logs.
- Plan 28's optional protected-remote profile permits an allowlisted non-loopback authority with TLS 1.3, enrolled-node mTLS or scoped token, pinned proxy trust, strict Host/Origin, and application authorization. Changing an address flag alone is insufficient. Tailscale or another VPN is optional reachability and never substitutes for TraceDecay identity/grants.

### 18.2 Credential model

Tokens are:

- random, hashed at rest, user/profile/instance bound;
- named by safe token ID for audit/revocation;
- time-bounded by default;
- constrained by read/preview/mutate/admin capability grants;
- constrained by scope selectors and sensitivity/payload grants;
- optionally process/installation identity bound where supported;
- revocable immediately with stream/operation implications declared.

Remote nodes additionally use an enrolled asymmetric identity stored through the OS credential provider, with explicit `BrainId`, node epoch, key rotation/revocation, placement, and sync grants. A Tailscale node identity may narrow an enrollment/grant decision but is not the durable TraceDecay node identity. Revocation closes streams and prevents new reads/writes. Offline cache access requires a signed `CacheGrantSnapshotV1` with mandatory expiry and policy/revocation/purge frontier; SDKs lock expired caches and apply/acknowledge tombstones before serving after reconnect.

The per-launch bootstrap bearer of plan 10 §10.2 is not a parallel credential model: it is the bootstrap credential whose only permitted operation is `auth.tokens.create` (plan 09 §10), minting the initial admin-class token in this registry. Every operating credential is a registry token.

The CLI prints a token only through an explicit secure creation flow and warns about shell history/agent context. Prefer delivering credentials by inherited file descriptor, OS keyring/credential helper, or `0600` file reference instead of environment variables for long-lived automation.

### 18.3 Data privacy

- Authorization is checked at capability, scope, entity, edge, payload, field, export, and stream stages.
- Topology visibility does not imply payload visibility.
- Secret-classified content never enters FTS/vector indexes, API examples, problems, telemetry, cursors, anchor labels, source maps, or conformance fixtures.
- Prompt/tool/provider sanitized-native payload access is an explicit sensitivity grant and every access is audited. Plaintext forensic access, when protected retention exists, is a distinct elevated quarantine workflow and never a normal entity/message/session/graph route.
- Durable graph-resident facts and memory are user data; backup, migration, export, delete, and corruption APIs never treat the whole graph database as disposable derived state.
- Replay retention follows plan 02's one target-bearing dependency-closure policy: drafts persist nothing; audit skeletons/anchors/receipts remain; payloads default to 180 days; saved views, exports, reports/profiles, promoted fixtures, pins, and legal/audit holds extend the exact run/cell/artifact closure. SDKs expose expiry/hold/unavailable state and never promise that only save/export can retain an input.
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

All three SDKs generate named methods for the existing `search.universal`, `code.search_symbols`, `representations.artifacts.list/get/status/install/import/activate/deactivate/evict/verify`, and `representations.generations.list/rebuild` use cases. They share the HTTP/CLI/MCP typed request, operation, and result models: exact benchmark-promoted FastEmbed embedding, optional BGE rerank, artifact, and generation pins; desired/activated/effective/observed enablement; install/import/download consent and verified cache/offline state; CPU/device/thread/batch/RAM/disk/residency budgets; native rerank toggle and top-N default/hard maximum of 25; strict semantics versus byte-stable lexical fallback; stage/rank/rerank deltas, latency/RSS/cache/vector/index coverage, rebuild status, provenance, and typed errors. SDKs never open model caches or databases, start native sessions, infer activation, or choose an alternate model.

The same search profile may name a separately registered optional Codex Spark/app-server-style rerank capability. Generated SDK types preserve discovery state, credential reference, privacy/egress decision, exact model, cost/token/deadline/top-N budgets, requested and actual route receipts, and unavailable/timeout fallback that preserves pre-rerank order. This toggle is distinct from the promoted FastEmbed embedding and native BGE reranker, disabled by default, supplies no embeddings, and is omitted/unavailable rather than silently rerouted when the capability is absent. Search Quality experiment helpers expose the corresponding replay/ablation coordinates; plan 22 may reuse the registered capability for active hinting/scout under its own policy, without an SDK-only operation.

### 20.2 Rust

- `tracedecay-client` exposes async traits and a default client runtime that imports only generated public request/response/event schemas, canonical `ApiProblem`, and its own transport/pager/stream runtime. It has no dependency on domain, store, application, or server implementation crates and reaches TraceDecay only through an injected transport at runtime.
- Support Unix socket and loopback HTTP transports behind features.
- Generated public contract/schema module is public; hand-written client/pager/stream/operation code is small and reviewable.
- Errors preserve `ApiProblem`; `Debug`/`Display` redact credentials and sensitive bodies.
- Compile examples and MSRV/toolchain policy are release gates.
- Optional in-process transport exists only as a test/root-composition adapter implementing the public client transport trait; that external adapter invokes the same application contract without adding any application/server dependency to the client crate or defining a different semantic API.

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
- The release publishes a signed generated host component set. `core` carries skills, CLI recipes, and thin hooks and is the shell-capable default with no MCP dependency. Independently installable `context`, `work`, and `operator` facade companions may be composed in any supported subset and all invoke the same TraceDecay binary/catalog; a headless facade-only set is explicit. Plan 20 owns installed-component enablement, one profile per registration, grant ceilings, reconnect rules, and host-target configuration.
- MCP progressive disclosure is a client capability, not an MCP protocol guarantee and not a plugin guarantee. Some clients may collect every `tools/list` page and place all schemas in model context. Release therefore enforces each generated profile's tool-count/schema/description definition budget even when a tested host supports deferred tool search; pagination or `notifications/tools/list_changed` is never counted as prompt-budget control.
- Profile widening, catalog/profile-digest change, or newly enabled operation requires a fresh MCP connection and credential handshake. Disablement or narrowing revokes authority immediately; list-change notification is only an optimization for clients that implement it and never authorizes continued use of a stale catalog.
- Server-side CLI/MCP adapters call `tracedecay-application` directly; they do not make recursive loopback HTTP calls.
- External plugins and agents use the official HTTP/SDK contract rather than internal databases or unstable root modules.
- Native FastEmbed model sessions and residency are daemon/root runtime concerns. CLI, MCP, dashboard, plugins, and SDKs observe and command them only through the representation/search application contracts above; transport parity includes exact desired/activated/effective/observed and requested/actual route receipts.
- MCP human-facing defaults remain compact Markdown; explicit JSON mode uses the canonical typed view model and preserves all machine fields. HTTP/SDK always use canonical machine JSON.
- Markdown and JSON render from the same typed application view, with parity tests for missing registries, active markers, repeated basenames, limits, truncation, and coverage.
- Tool catalog entries link directly to API docs and SDK examples. API discovery links back to CLI/MCP bindings so an agent can choose the cheapest available surface.
- Host integrations handshake catalog/protocol digest. If an installed plugin is stale, it receives one current restart/update/replacement instruction; no dual namespace or legacy behavioral shim.
- Host-integration inventory, compatibility/difference, status, and lifecycle methods are generated only on the admin client described in §8.4. Dashboard and CLI use the same sealed models and `OperationRef`; external automation never scrapes host files or asks an SDK to edit raw host configuration.
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
- Internal enrolled-node protocol conformance: the private client/server route set is byte-deterministic and mutually authenticated; epochs/placements/versions/grants/frontiers, bounded append receipts/acks, signed snapshot/tail pages, gaps/repair, tombstone/purge acknowledgements, and fault recovery pass, while every internal row remains absent from public contract IR/OpenAPI/SDK/docs/tool manifests and every database/WAL/path/key/raw-payload field is unrepresentable.
- OpenAPI/JSON Schema validation, discriminator, unknown variant, optional/nullable, bigint, time, and round-trip properties.
- Multi-repository/project/checkout/worktree/ref/session/agent/All scope resolution, ambiguity, stale registry, same basename, wrong active checkout, and denied store fixtures.
- Cursor tamper/access/query/schema/ranking/index/retention/expiry and distributed-page stable-order fixtures.
- Partial/locked/corrupt/stale/unavailable/redacted store coverage and zero-result truthfulness.
- Graph high-fanout/cycle/depth/path/LOD/cluster/partial/privacy and worktree-snapshot identity fixtures.
- Search/hint exact/recorded/current replay, missing artifact, no-write, privacy, grouping, ranking explanation, and current-versus-historical diff fixtures.
- Anchor-operation separation: metadata batch returns no content, resolve rechecks authorization and returns exact state/payload, recipe execute preserves protected input/version/watermark/coverage, and every stale GET/`anchors:*`/combined-hydration alias is absent from route and SDK manifests.
- Search-evaluation parity: every corpus/qrel/pool/judgment/adjudication/report/profile read and create/freeze/record/supersede/adjudicate/publish/promote/activate command plus generic Search Quality experiment create/run/cancel/resume/retry/minimize and run/stage/comparison reads matches application, HTTP, all SDKs, CLI, MCP, and dashboard; frozen artifacts remain immutable and private report/fixture content cannot publish.
- Automation-admission parity: dirty scopes, exact admission receipts, receipt/coalesced-skip-episode list representations, current/considered/consumed/included frontiers, and shared retry/circuit/pause/quarantine/reconciliation/coverage state match application, HTTP, all SDKs, CLI, MCP, and dashboard; no fake run, state enum fork, fourth read operation, or `run_now` identical-input bypass exists.
- Host-integration parity: all nine `integrations.*` operations are admin-only and bijective across application, HTTP, SDKs, CLI, generated MCP exposure, and dashboard; difference rows, ownership/trust/cache/drift/restart state and operation polling match exactly, while every schema/fixture rejects host paths, raw configuration bodies, environment values, credentials, and arbitrary manifests.
- Auth/token/Unix socket/Host/Origin/CSRF/DNS rebinding/revocation/expiry/scope/sensitivity and constant-time handling tests.
- Rate/body/decompression/header/URI/JSON depth/batch/export/stream queue/deadline/cancellation tests.
- SSE duplicate/out-of-order/resume/expiry/gap/resync/coalescing/slow client/auth change/protocol cutoff tests.
- Command idempotency/version conflict/operation-specific inspection/confirmation/recovery/audit/destructive-grant tests.
- Local transport conformance on Linux/macOS/Windows: service ownership, endpoint ACL/DACL, connect-only ordinary-client access, peer-credential/token mismatch, token authentication, browser-credential rejection, and negative database-root traversal/read/open probes executed as the client identity (adapters built by plan 10 §10.1).
- Executor-adapter compatibility/security matrix from plan 24 as a dedicated conformance lane: provider/model/route constraint enforcement, fenced lease-acquisition/heartbeat/terminal transitions, advisory-claim separation, broker/grant revocation, non-preemptible-effect quarantine, and workspace-safety refusals.
- Task orchestration parity lane: canonical `DependencyId`/`WorkClaimRefV1`/manifest-ID+ordinal+digest `ContextPacketManifestRefV1`, canonical `TraceQueryV1`, complete saved-view round trip/share revoke, transactional assignment-set receipt, fully anchored packet entries, attempt list/get/timeline with immutable start plus current accepted packet, registration-scoped offer list/get/accept/decline and admin revoke, fenced packet accept, direct notification list/get/create/update/delete with no preview/apply alias, journal-sequence subscription deltas with no `/task-events`, and plan-26 workload/fleet accounting attribution. Every operation must match application/HTTP/Rust/TypeScript/Python/CLI/MCP manifests.
- Task-graph edit-bundle lane: all seven `task_graph.edit_bundles.*` operation IDs and HTTP/SDK methods are bijective; streamed export/get/validate preserves bounded memory; no request serializes a path; strict YAML/CommonMark, sharding, line/column diagnostics, semantic diff, explicit rebase, submit CAS/atomicity, ordinary validation retention, secret/containment immediate purge, explicit delete, expiry, and crash cleanup behave identically across runtimes; success leaves only content-free digest/count/anchor receipts.
- Plugin/MCP exposure lane: skills/CLI-only install has no MCP dependency; optional context/work/operator registrations all launch the thin `tracedecay` integration binary and connect to private `tracedecayd`, while retaining distinct immutable profile/grant/budget digests; eager all-tools, paginated-list, deferred-tool-search, ignored-list-change, no-resource, and no-experimental-task client fixtures all remain usable and least-privilege; widening is unavailable until reconnect.
- Operator storage lane for merged #425: AdminClient-only consolidation discovery, deterministic plan/confirmation, path-plus-file/inode holder/freeze/backup/stage/verify/cutover/resume/recover state, uncertain-write non-replay, exact recovery action, and proof that curation/task credentials cannot discover or invoke it.
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
- [ ] Mark every capability public/internal/admin/migration/removed and fail on unknown disposition; generate the nine host-integration operations and sealed admin models with no read-client exposure.
- [ ] Publish protocol/version/change/cutoff policy and compatibility manifest schema.
- [ ] Commit `feat(api): freeze the official public contract`.

### PR 24D-API2: Scope resolution, anchors, problems, and direct-agent discovery

**Files:** public schemas, meta/openapi/schema/binding/scope/anchor routes; CLI `api` lifecycle commands; docs concepts/quickstart; conformance fixtures.

- [ ] Add cross-project/worktree/ref/session/agent/All, same-basename, ambiguity, wrong-active-project, stable-anchor, endpoint-discovery, token-redaction, and retry-directive tests.
- [ ] Implement Sections 7, 9, 10, 11, and 12 through application use cases; no handler-side resolution.
- [ ] Add `tracedecay api status/token/openapi/docs` with secret-safe JSON and user approval.
- [ ] Prove no response handle/cursor/token/path is used as a durable anchor.
- [ ] Commit `feat(api): expose agent discovery and stable scopes`.

### PR 24D-API3: Complete Graph-of-Graphs and safe experiment/replay contract

**Files:** graph/replay OpenAPI/schema/catalog bindings, synthetic fixtures, API docs recipes, conformance tests.

- [ ] Add graph composition/atlas/entity/edge/evidence/LOD/cost/privacy/worktree snapshot cases and experiment create/run/status/cancel/resume/retry, branch/sweep/stage-alignment/anchor/receipt plus search/hint exact/recorded/current/no-live-effect cases.
- [ ] Bind the plan 10 routes to complete official schemas and capability docs.
- [ ] Verify the hermetic evaluator cannot reach any live command, hook, fact/trust, analytics outcome, claim/lease, cache/counter write, or ungranted network/model effect, and that the side-effect receipt proves zero production effects.
- [ ] Add curl examples and direct links to dashboard visual labs.
- [ ] Commit `feat(api): publish graph and replay contracts`.

### PR 24D-SDK1: Rust client and hermetic sandbox

**Files:** `crates/tracedecay-client/**`; sandbox fixture process/profile; Rust quickstart/examples; conformance runner.

- [ ] Add compile/round-trip/error/pager/stream/operation/socket/auth tests against the synthetic sandbox.
- [ ] Add host-integration admin-model/method/operation-polling tests, read-client absence checks, compatibility-difference fixtures, and recursive rejection of host path/config/credential fields.
- [ ] Generate types/descriptors and implement the compact Rust runtime.
- [ ] Prove credential/payload redaction, protocol handshake, bounded iteration, gap visibility, and command idempotency.
- [ ] Publish as workspace-only until the public contract and release process pass twice.
- [ ] Commit `feat(sdk): add the official Rust client`.

### PR 24D-SDK2: Complete and publish the one official TypeScript client

**Files:** `packages/tracedecay-client/**`; Node/browser examples; docs; conformance adapters.

- [ ] Harden the generated schema core (produced from the contract IR per Section 5.1 and hosted in this same package per plan 10) and the transport-neutral runtime; add no dashboard dependency. Make the dashboard browser binding consume/re-export it without generating another schema tree.
- [ ] Test ESM, Node local socket/HTTP, browser bootstrap, bigint, runtime decoding, pager, SSE gap/resume, and typed problems.
- [ ] Test generated host-integration admin methods, difference unions, operation polling, and compile-time absence from the read client.
- [ ] Prove browser builds cannot read local discovery/token files and generated bundles contain no fixtures/secrets.
- [ ] Commit `feat(sdk): publish the official TypeScript client`.

### PR 24D-SDK3: Python sync/async package

**Files:** `python/tracedecay-client/**`; Python examples/docs; conformance adapters.

- [ ] Add supported-version matrix, typing, model round-trip, sync/async transport/pager/stream/operation, timezone/bigint/enum, and redaction tests.
- [ ] Test sync/async host-integration admin methods and operation polling while proving models expose no host path, raw config body, environment, or credential value.
- [ ] Generate models/descriptors and implement the compact sync/async runtime.
- [ ] Validate package build/install in an empty environment and against the sandbox.
- [ ] Commit `feat(sdk): publish the official Python client`.

### PR 24D-API4: Docs explorer, SDK/reference generation, and full parity gate

**Files:** `docs/api/**`; authenticated explorer; generated capability pages; `tests/public_api_conformance/**`; release manifests.

- [ ] Generate and curate quickstarts/concepts/recipes/reference; compile/run every example.
- [ ] Add an authenticated read-only explorer that can render operation-specific plan/confirmation request schemas and metadata/problems but cannot execute mutations.
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
- [ ] Attempt list/get/timeline, offer list/get/accept/decline/revoke, packet list/get/fenced accept, and direct notification list/get/create/update/delete have generated Rust/TypeScript/Python methods and exact HTTP/CLI/MCP semantic parity, including immutable start-versus-current accepted packet state.
- [ ] Automation dirty-scope/admission list/get methods have generated Rust/TypeScript/Python signatures and exact HTTP/CLI/MCP/dashboard parity, including coalesced skip episodes, current/considered/consumed/included frontiers, shared health/reconciliation state, and identical-input fencing.
- [ ] Hint/search replay is reproducible at declared fidelity, explainable, privacy-safe, and demonstrably no-write.
- [ ] Direct-agent credentials are least-privilege, scoped, expiring, auditable, revocable, and never leaked by SDK/docs/errors/logs.
- [ ] Commands require explicit authority, idempotency, versions, operation-specific inspection/confirmation where applicable, and durable audit/operation receipts.
- [ ] Errors provide stable machine codes and exact retry/restart/update/scope-resolution payloads.
- [ ] API/SDK load cannot starve hook capture or concurrent event writers; limits and fairness pass current and 10x reference scenarios.
- [ ] Official docs explain the mental model and every example runs against the synthetic sandbox.
- [ ] Current protocol cutoff rejects stale clients without executing live compatibility fallbacks.
- [ ] Release publishes server/catalog/schema/SDK compatibility artifacts coherently and can roll back only to a compatible V2 artifact/data snapshot.
