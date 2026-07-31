# API Route Ownership Plan

> **Archived provenance — not current requirements.** This document records
> historical planning and execution evidence. Current scope and acceptance come
> only from [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate its task checklists, file inventories,
> branch/worktree/SHA or commit protocol, Gate A/B, timing/JUnit receipts, exact
> test names/counts, generated-byte/source-shape checks, PR closure gates, or
> platform gate lattice.

**Goal:** Move HTTP/dashboard route families incrementally into
`tracedecay-api` while preserving daemon-owned invocation and generated
contracts.

## Historical file and interface inventory

- Modify `crates/tracedecay-api/src/http.rs` and add focused route modules.
- Migrate route wiring from `src/dashboard/**`, `src/daemon/**`, and root HTTP
  composition.
- Modify schemars owners and generated dashboard contracts only through
  `npm run contracts:generate`.

`tracedecay-api` owns request/response DTOs, Axum routers, HTTP/SSE protocol
mapping, and typed error/status rendering. It consumes
`Arc<dyn DaemonInvocationExecutor>`; it owns no daemon, DB, policy, project
registry, or business-use-case implementation.

## Historical task checklist

- [ ] Add architecture tests rejecting API-to-root daemon/store imports.
- [ ] Move health/Doctor read routes, then configuration/remediation writes,
      dashboard reads, and SSE families in reviewable commits.
- [ ] Regenerate and diff TypeScript contracts for every schema move.
- [ ] Prove daemon-hosted construction and embedded-dashboard routing.

## Product outcome contributed

HTTP/dashboard route ownership moved toward `tracedecay-api` while daemon
invocation, generated contracts, typed failures, cancellation, and SSE behavior
remained equivalent. Current direct behavior and acceptance live in the
applicable numbered V2 plan.

## Historical migration, rollback, measurement, and deletion notes

Move one route family at a time; old routes delegate during migration and never
own duplicate logic. Revert the family and regenerated contract together.
Measure API-private and root route edits. Delete old routers/DTOs only after all
production mounts, generated contracts, HTTP/SSE journeys, and package gates
use `tracedecay-api`.
