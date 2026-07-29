# MCP–Daemon Cycle-Break Plan

**Goal:** Remove MCP imports of daemon internals and daemon imports of MCP
handlers through one typed invocation boundary.

## Files and interfaces

- Modify `src/mcp/server/**`, `src/mcp/tools/**`, `src/daemon/core_*`,
  `src/daemon/service/invocation.rs`, and construction/lifecycle wiring.
- Reuse the production `DaemonInvocationExecutor` boundary; do not create a
  configuration-only or test-only executor.

Required handoff:

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

## Tasks and tests

- [ ] Add dependency-direction tests that fail on MCP↔daemon concrete imports.
- [ ] Route one read, one write/CAS, one stream, and one cancellation journey
      through the executor.
- [ ] Migrate all production handlers and host construction.
- [ ] Remove direct daemon/global-store access from MCP handlers.

Direct tests compare MCP results/receipts with CLI/application invocation for
the same requests. Negative tests cover missing project, stale route, denied
grant, unavailable executor, cancellation, deadline, stream disconnect,
oversized payload, and changed-input idempotency conflict.

## Migration, rollback, measurement, deletion

Migrate handler families independently behind compatibility adapters.
Rollback by reverting a family; no data migration. Gate B requires either a
15% identical warm MCP-handler edit improvement or the complete dependency
test plus truthful timing disposition. Delete concrete cross-imports and
test-only executors only after every production construction path uses the
boundary.
