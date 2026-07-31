# MCP–Daemon Cycle-Break Plan

> **Archived provenance — not current requirements.** This document records
> historical planning and execution evidence. Current scope and acceptance come
> only from [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate its task checklists, file inventories,
> branch/worktree/SHA or commit protocol, Gate A/B, timing/JUnit receipts, exact
> test names/counts, generated-byte/source-shape checks, PR closure gates, or
> platform gate lattice.
> Historical version/compatibility/migration language cannot resurrect
> source-only/internal branch scaffolding. Potentially deployed callable names
> remain compatible until an authorized installed-client/host census proves
> absence; the current numbered plan governs final scope.

**Goal:** Remove MCP imports of daemon internals and daemon imports of MCP
handlers through one typed invocation boundary.

## Historical file and interface inventory

- Modify `src/mcp/server/**`, `src/mcp/tools/**`, `src/daemon/core_*`,
  `src/daemon/service/invocation.rs`, and construction/lifecycle wiring.
- Reuse the production `DaemonInvocationExecutor` boundary; do not create a
  configuration-only or test-only executor.

Historical handoff:

```rust
pub trait DaemonInvocationExecutor: Send + Sync {
    async fn invoke(
        &self,
        context: RequestContext,
        request: ApplicationRequest,
    ) -> Result<ApplicationResponse, InvocationError>;
}
```

MCP owns protocol parsing/rendering and cancellation mapping. Daemon owns
project routing, admission, application composition, and lifecycle. Neither
imports the other's concrete handler/runtime modules.

## Historical task checklist

- [ ] Add dependency-direction tests that fail on MCP↔daemon concrete imports.
- [ ] Route one read, one write/CAS, one stream, and one cancellation journey
      through the executor.
- [ ] Migrate all production handlers and host construction.
- [ ] Remove direct daemon/global-store access from MCP handlers.

## Product outcome contributed

MCP protocol handling and daemon routing/composition became separated by one
production invocation boundary while results, cancellation, authorization, and
typed failure behavior remained equivalent. Current direct behavior and
acceptance live in the applicable numbered V2 plan.

## Historical migration, rollback, measurement, and deletion notes

Move handler families independently. Use a compatibility adapter for released
or potentially deployed dogfood/host callers until the authorized
installed-client/host census proves absence; move only source-only/internal
callers in place. Rollback by reverting a family; no data migration. The historical Gate B
experiment used a warm MCP-handler edit comparison or dependency test plus
timing disposition; it is not current acceptance. Delete concrete cross-imports and
test-only executors only after every production construction path uses the
boundary.
