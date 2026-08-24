# TraceDecay V2 API Crate

## Status / Role

Normative public-service delivery contract; completion status is owned solely by
`00-plan-set-index.md`. `tracedecay-api` is the canonical thin Axum
HTTP, SSE, and static-dashboard adapter over `tracedecay-application`. The
executable-work journey adds typed Plan 24 task/work and Plan 32 runtime routes
through this same adapter only when their application operations ship.

Existing callable HTTP conversion and application-surface paths are the
starting implementation, not proof that the route family is complete.
Current handler, DTO, module, and fixture names are evidence only unless a DTO
or route is explicitly published and versioned. A required application
operation with no callable HTTP/SSE journey is a gap; the absence of an older
endpoint registry, packet schema, or planned file is not.

## Outcome

Local and remote clients receive one stable, bounded, observable public service without HTTP handlers becoming a second application layer.

## Owns

- HTTP server lifecycle, routing, middleware, request extraction, and response encoding.
- Authentication transport, origin policy, request limits, and request IDs.
- Stable JSON DTOs and application-error-to-HTTP mapping.
- Cursor pagination, conditional requests, compression, and cache headers.
- SSE framing, heartbeats, resume tokens, disconnect handling, and backpressure.
- Static dashboard assets, SPA fallback, content types, and cache policy.
- API documentation generated from the actual typed handlers and DTOs.

## Does not own

- Business rules, authorization policy, queries, commands, or transaction boundaries.
- Database connections, SQL, indexing, migration, or direct filesystem access.
- MCP or CLI presentation.
- LSP lifecycle, JSON-RPC framing, document synchronization, analyzer
  supervision, or an LSP tunnel.
- An exhaustive hand-maintained endpoint registry or generated compatibility inventory.
- Developer-roadmap, Markdown-plan, board-local, generic executor/scheduler,
  edit-bundle, or arbitrary workflow-edit APIs. Executable work exposes only typed
  Plan 24 work-graph and Plan 32 runtime application operations.
- JavaScript execution. Executable-work endpoints adapt typed product operations only.

## Required behavior

- Handlers extract transport inputs, build `RequestContext`, call one application use case, and encode its result.
- HTTP/SSE and the daemon LSP gateway in
  [35](35-daemon-lsp-gateway-and-universal-diagnostics.md) are sibling adapters
  over shared typed application contracts. HTTP neither owns nor tunnels LSP
  JSON-RPC.
- Do not add an HTTP endpoint that forwards an arbitrary LSP method or payload.
- All mutable routes require explicit authentication, capability checks in the application layer, bounded bodies, and idempotency where applicable.
- Read routes preserve project/repository/worktree scope and expose freshness, coverage, provenance, and pagination metadata.
- Use one stable error envelope with machine code, safe message, request ID, retry guidance, and bounded details.
- Map one canonical application problem record to HTTP without changing its
  meaning. The record carries code and revision, owning layer, terminality,
  retryability and retry scope, bounded retry delay, request/trace identity,
  safe details, legal next actions, and partial-coverage state.
- Never expose secrets, filesystem internals, SQL errors, or unredacted private content.
- SSE streams follow `open -> (item | heartbeat | warning)* -> completed |
  cancelled | failed`, use monotonic correlation/sequence identity, and publish
  exactly one canonical terminal outcome. Resume contracts define retention,
  the first unavailable sequence, duplicate convergence, and typed
  `resume_expired` and `resume_gap` outcomes.
- Terminal, error, authorization, and mutation-receipt events are never
  dropped. Superseded progress may coalesce by identity and version; optional
  heartbeat or advisory progress may drop only with explicit coverage
  accounting and bounded memory.
- Cancellation distinguishes request receipt, publication suppression,
  upstream acknowledgement, execution stop, completed-before-cancel,
  committed/too-late, and terminal outcome. Disconnect is not application
  cancellation unless that operation's revisioned contract explicitly says it
  is.
- Static assets use immutable caching when fingerprinted; the HTML shell revalidates; API paths never fall through to the SPA.
- Health and readiness distinguish process health from daemon/store readiness without performing destructive repair.
- OpenAPI or equivalent documentation is derived from shipped handlers and DTOs, not a parallel source of truth.
- Root HTTP behavior moves into this crate and duplicate legacy handler logic is deleted.
- The executable-work journey adds concrete workflow product routes plus bounded task/work reads,
  graph/history/projection reads,
  explicit versioned graph commands, route-review reads, and task-step runtime
  admission/control backed by Plan 24/32 application methods. There is no
  generic status setter, arbitrary task payload, server path, shell command,
  or route that bypasses Plan 32 effect authority.

## Acceptance

- A direct journey invokes the canonical feedback and representative
  read/write operations over HTTP, compares their typed semantics with the
  application and CLI/MCP paths, and exercises an SSE stream through terminal
  outcome, disconnect, resume, gap, cancellation, and backpressure behavior.
- Contract tests cover authentication, scope, canonical problem mapping,
  pagination, limits, caching, SSE ordering, duplicate convergence,
  resume gaps/expiry, disconnect, cancellation races, backpressure,
  slow-consumer bounds, and static fallback.
- Tests prove handlers delegate to application use cases and do not access stores directly.
- Public DTO compatibility is intentional and versioned; no shadow compatibility generator is required.
- Route documentation matches executable handlers automatically.
- Executable-work HTTP/SSE fixtures prove Kanban, DAG, timeline, causal, workload, and
  history reads preserve the same canonical IDs, versions, scope, watermarks,
  coverage, and runtime refs as direct application calls.
- No developer-plan executor, generic/untyped task editor, arbitrary
  JavaScript, generated inventory, or duplicated business logic remains.
