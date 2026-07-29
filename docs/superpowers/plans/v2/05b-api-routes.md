# API Route Ownership Plan

**Goal:** Move HTTP/dashboard route families incrementally into
`tracedecay-api` while preserving daemon-owned invocation and generated
contracts.

## Files and interfaces

- Modify `crates/tracedecay-api/src/http.rs` and add focused route modules.
- Migrate route wiring from `src/dashboard/**`, `src/daemon/**`, and root HTTP
  composition.
- Modify schemars owners and generated dashboard contracts only through
  `npm run contracts:generate`.

`tracedecay-api` owns request/response DTOs, Axum routers, HTTP/SSE protocol
mapping, and typed error/status rendering. It consumes
`Arc<dyn DaemonInvocationExecutor>`; it owns no daemon, DB, policy, project
registry, or business-use-case implementation.

## Tasks and tests

- [ ] Add architecture tests rejecting API-to-root daemon/store imports.
- [ ] Move health/Doctor read routes, then configuration/remediation writes,
      dashboard reads, and SSE families in reviewable commits.
- [ ] Regenerate and diff TypeScript contracts for every schema move.
- [ ] Prove daemon-hosted construction and embedded-dashboard routing.

Direct tests call the same route through in-process and real daemon-hosted
composition and compare typed results/receipts. Negative tests cover missing
authority, stale CAS, unavailable project, cancellation, SSE lag/overflow,
unsupported operation, malformed payload, and response-size bounds.

Run API package checks/tests, `dashboard_api_test`, contracts checks, dashboard
typecheck/Vitest, and root all-feature integration.

## Migration, rollback, measurement, deletion

Move one route family at a time; old routes delegate during migration and never
own duplicate logic. Revert the family and regenerated contract together.
Measure API-private and root route edits. Delete old routers/DTOs only after all
production mounts, generated contracts, HTTP/SSE journeys, and package gates
use `tracedecay-api`.
