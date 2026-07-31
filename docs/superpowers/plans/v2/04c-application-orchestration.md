# Application Orchestration Convergence Plan

> **Archived provenance — not current requirements.** This document records
> historical planning and execution evidence. Current scope and acceptance come
> only from [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate its task checklists, file inventories,
> branch/worktree/SHA or commit protocol, Gate A/B, timing/JUnit receipts, exact
> test names/counts, generated-byte/source-shape checks, PR closure gates, or
> platform gate lattice.

**Goal:** Move session, memory, feedback, and configuration use-case sequencing
behind application ports while retaining root SQL/store adapters.

## Historical file and interface inventory

- Modify `crates/tracedecay-application/src/**` for use cases and ports.
- Migrate orchestration from `src/application/**`, `src/global_db/**`,
  `src/query/**`, `src/dashboard/**`, and MCP/CLI handlers.
- Keep database connections, transactions, native Git, process execution, and
  daemon lifecycle in root adapters.

The historical design gave every use case `RequestContext`, explicit
grants/revisions, an idempotency key, deadline/cancellation, and typed ports,
returning a typed result plus receipt. The recorded operation families were
`SessionApplication`, `MemoryApplication`, `FeedbackApplication`, and
`ConfigurationApplication`.

## Historical task checklist

- [ ] Add architecture tests rejecting root/global-DB/transport imports from
      application.
- [ ] Move one complete read/write journey per family, including policy,
      cancellation, receipt, and truthful partial outcomes.
- [ ] Bind CLI, MCP, HTTP/dashboard, and daemon callers to the same operation.
- [ ] Remove handler-local sequencing and duplicate authorization.

## Product outcome contributed

Session, memory, feedback, and configuration sequencing converged behind
application ports while persisted effects, cross-surface rendering, typed
failures, cancellation, and idempotency behavior remained equivalent. Current
direct behavior and acceptance live in the applicable numbered V2 plan.

## Historical migration, rollback, measurement, and deletion notes

Land one operation family at a time with compatibility delegation. Do not
dual-write. Revert by family if its direct journey fails. Measure application
private edits and root handler edits before/after. Delete root orchestration
only after production callers and all exposed surfaces reach the application
owner and architecture search finds no reverse dependency.
