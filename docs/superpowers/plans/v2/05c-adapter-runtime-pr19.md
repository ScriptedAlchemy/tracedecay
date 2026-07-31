# PR19 Adapter and Runtime Extraction Plan

> **Archived provenance — not current requirements.** This document records
> historical planning and execution evidence. Current scope and acceptance come
> only from [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate its task checklists, file inventories,
> branch/worktree/SHA or commit protocol, Gate A/B, timing/JUnit receipts, exact
> test names/counts, generated-byte/source-shape checks, PR closure gates, or
> platform gate lattice.
> Historical version/compatibility/migration language applies only to APIs or
> data proven on `origin/master`, in a published release, or in live
> persistence. Pure branch-local adapter/source shapes change in place; any
> branch-written store, spool, file, journal, checkpoint, receipt, or
> projection remains recoverable until a separately authorized
> registered-store/profile census proves absence.

**Goal:** After dependency inversion, extract rusqlite, MCP/LSP, daemon, and
remaining root adapters without changing product authority.

## Historical file and interface inventory

- Extend `crates/tracedecay-rusqlite-runtime` and parity crates.
- Create runtime crates only for dependency-closed adapter families proven by
  the graph: MCP transport, LSP gateway, daemon runtime, and retained DB/global
  DB adapters.
- Modify workspace manifests, root façades, distribution/package includes,
  host bundles, and cross-platform CI.

Interfaces are the already accepted application/domain/store/API ports. New
runtime crates implement them; they do not redefine requests, receipts,
project identity, one-writer authority, or migration policy.

## Historical task checklist

- [ ] Produce a dependency/disposition ledger for every root adapter module.
- [ ] Complete Plan 34 apply and PR19 cutover prerequisites before extraction.
- [ ] Extract one dependency-closed runtime family per commit.
- [ ] Preserve rusqlite parity, daemon lifecycle, MCP/LSP transport, packaging,
      installation, and host construction.
- [ ] Retire compatibility aliases only after released-data cutover evidence.

## Product outcome contributed

Dependency-inverted rusqlite, daemon, MCP/LSP, and packaging adapters became
separable from the root while product authority, lifecycle, storage parity,
recovery, and supported-host behavior remained equivalent. Current direct
behavior and acceptance live in the applicable numbered V2 plan.

## Historical migration, rollback, measurement, and deletion notes

Runtime extraction follows successful atomic one-writer cutover; it is never
the mechanism for cutover. Data migrations are resumable and roll-forward,
with backup verification before promotion. Code rollback is `git revert`;
runtime fallback never re-enables dual write or reverse cutover.

Measure each adapter-private edit, root leaf edit, package build, and focused
test compile. Delete root adapters, aliases, V1 paths, and expired archives
only when the historical released-data, package, host, and rollback evidence
was satisfied; that evidence list is not a current closure gate.
