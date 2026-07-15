# TraceDecay V2 API Crate

## Status / Role

Normative PR12 plan. `tracedecay-api` is a thin Axum HTTP, SSE, and static-dashboard adapter over `tracedecay-application`.

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
- An exhaustive hand-maintained endpoint registry or generated compatibility inventory.
- Task-plan, Kanban, executor, scheduler, edit-bundle, or arbitrary workflow-edit APIs.
- JavaScript execution. PR17 workflow endpoints adapt typed product operations only.

## Required behavior

- Handlers extract transport inputs, build `RequestContext`, call one application use case, and encode its result.
- All mutable routes require explicit authentication, capability checks in the application layer, bounded bodies, and idempotency where applicable.
- Read routes preserve project/repository/worktree scope and expose freshness, coverage, provenance, and pagination metadata.
- Use one stable error envelope with machine code, safe message, request ID, retry guidance, and bounded details.
- Never expose secrets, filesystem internals, SQL errors, or unredacted private content.
- SSE streams are bounded per client, send heartbeats, stop promptly on disconnect, and support defined replay/resume semantics.
- Static assets use immutable caching when fingerprinted; the HTML shell revalidates; API paths never fall through to the SPA.
- Health and readiness distinguish process health from daemon/store readiness without performing destructive repair.
- OpenAPI or equivalent documentation is derived from shipped handlers and DTOs, not a parallel source of truth.
- PR12 moves root HTTP behavior into this crate and deletes duplicate legacy handler logic.
- PR17 adds only the concrete workflow product routes backed by typed application methods.

## Acceptance

- Contract tests cover authentication, scope, errors, pagination, limits, caching, SSE resume/disconnect/backpressure, and static fallback.
- Tests prove handlers delegate to application use cases and do not access stores directly.
- Public DTO compatibility is intentional and versioned; no shadow compatibility generator is required.
- Route documentation matches executable handlers automatically.
- No plan executor, task editor, arbitrary JavaScript, generated inventory, or duplicated business logic remains.
