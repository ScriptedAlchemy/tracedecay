# Uncontracted routes

Every schema under this directory is a **backend gap**, not a design choice.

`src/contracts/generated.ts` is the one wire boundary: it is emitted by
`npm run contracts:generate` from the Rust `DashboardContractCatalogV1`
(`src/dashboard/contract_schema.rs`) and verified by `npm run contracts:check`.
A route whose response type is in that catalog cannot drift without failing the
gate.

The routes modelled here are **not in the catalog**. Their Rust handlers return
`serde_json::Value` built with `json!{}` rather than a `JsonSchema` DTO, so
there is nothing to generate from, and the only way the dashboard can read them
is to describe the shape by hand against the producer source.

That hand-writing is the hazard this directory exists to make visible:

- A hand-written schema can be **stricter than the producer**, in which case a
  legitimate response fails to parse and the surface shows a schema error
  instead of live data. Explorer shipped exactly this (`freshness` as a
  five-member enum against a Rust `String`), and so did both of the
  `/api/projects` copies this directory replaced (`summary`/`project_tree`
  non-nullable against a producer that sends explicit `null` on every degraded
  response).
- The same route can be modelled **twice** under different names in different
  workspaces and drift apart, which is what happened to `/api/projects`
  (brain + delivery) and `/api/plugins/graph/*` (brain + code).

So: **one module per route family**, named for its Rust producer, holding every
route in that family exactly once. A workspace never declares a wire shape; it
imports one from here or from the generated barrel.

There is deliberately **no barrel file**. Callers import the specific module
(`contracts/uncontracted/projects.ts`), so the import line always states which
gap is being read and never sits at a shorter, more convenient path than
`contracts/wire.ts`. A short convenient path with everything behind it is the
property that let the per-workspace `contracts.ts` modules shadow generated
names in the first place.

## Adding to this directory

Don't, if you can avoid it. The fix for an uncontracted route is to give its
Rust handler a `JsonSchema` DTO and add it to `DashboardContractCatalogV1`;
then the schema is generated, the gate covers it, and the module here is
deleted. Anything added here should be accompanied by that as the follow-up.

## Current gap register

| Module | Routes | Rust producer |
| --- | --- | --- |
| `projects.ts` | `GET /api/projects`, `GET /api/projects/{id}` | `src/dashboard/projects.rs`, `src/project_registry.rs` |
| `graph.ts` | `GET /api/plugins/graph/{overview,search,subgraph,node/{id}/neighbors}` | `src/dashboard/graph_service.rs` |
| `memory.ts` | `GET /api/plugins/holographic/{,status,fact/{id}}` | `src/dashboard/memory_api.rs`, `src/dashboard/memory_service/facts.rs` |
| `analytics.ts` | `GET /api/plugins/analytics/overview` | `src/dashboard/analytics_api.rs` |
| `savings.ts` | `GET /api/plugins/savings/overview` | `src/dashboard/savings_api.rs` |
| `sessions.ts` | `GET /api/plugins/savings/sessions`, `GET /api/plugins/hermes-lcm/{session/{id},timeline}`, `GET /api/loom/temporal` | `src/dashboard/savings_api.rs`, `src/dashboard/lcm_api.rs`, `src/dashboard/loom_api.rs` |
| `delivery.ts` | `GET /api/delivery/overview` | `src/dashboard/delivery_api.rs` |
| `explorer.ts` | `GET /api/explorer/session/{id}/{size,read}` | `src/dashboard/explorer_api.rs` |

`explorer.ts` is the narrowest gap and the best first candidate to close:
`/api/explorer/query` on the same Rust module is already contracted
(`ExplorerQueryRunV1`), so these two sibling routes are the only reason the
Explorer workspace still hand-writes anything.
