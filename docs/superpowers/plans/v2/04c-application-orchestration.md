# Application Orchestration Convergence Plan

**Goal:** Move session, memory, feedback, and configuration use-case sequencing
behind application ports while retaining root SQL/store adapters.

## Files and interfaces

- Modify `crates/tracedecay-application/src/**` for use cases and ports.
- Migrate orchestration from `src/application/**`, `src/global_db/**`,
  `src/query/**`, `src/dashboard/**`, and MCP/CLI handlers.
- Keep database connections, transactions, native Git, process execution, and
  daemon lifecycle in root adapters.

Every use case accepts `RequestContext`, explicit grants/revisions, an
idempotency key, deadline/cancellation, and typed ports; it returns a typed
result plus receipt. Required operation families are
`SessionApplication`, `MemoryApplication`, `FeedbackApplication`, and
`ConfigurationApplication`.

## Tasks

- [ ] Add architecture tests rejecting root/global-DB/transport imports from
      application.
- [ ] Move one complete read/write journey per family, including policy,
      cancellation, receipt, and truthful partial outcomes.
- [ ] Bind CLI, MCP, HTTP/dashboard, and daemon callers to the same operation.
- [ ] Remove handler-local sequencing and duplicate authorization.

## Tests

Direct: each family executes through application plus production adapter and
produces the same persisted result and surface rendering across CLI/MCP/HTTP.

Negative: missing registry, unavailable authority, stale CAS, wrong project,
cancelled deadline, partial source, adapter failure, duplicate idempotency key,
and changed-input replay remain typed and cannot commit partial effects.

Run package checks/nextest, relevant integration suites, surface parity tests,
and dependency-direction contracts.

## Migration, rollback, measurement, deletion

Land one operation family at a time with compatibility delegation. Do not
dual-write. Revert by family if its direct journey fails. Measure application
private edits and root handler edits before/after. Delete root orchestration
only after production callers and all exposed surfaces reach the application
owner and architecture search finds no reverse dependency.
