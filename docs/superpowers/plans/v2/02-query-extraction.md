# `tracedecay-query` Extraction Plan

> **Archived provenance — not current requirements.** This document records
> historical planning and execution evidence. Current scope and acceptance come
> only from [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate its task checklists, file inventories,
> branch/worktree/SHA or commit protocol, Gate A/B, timing/JUnit receipts, exact
> test names/counts, generated-byte/source-shape checks, PR closure gates, or
> platform gate lattice.

**Goal:** Move the query kernel and focused tests out of the root crate while
preserving every public path and result byte.

**Historical dependency assumption:** Domain owned query-facing DTOs and
application owned request scope before this extraction.

## Historical file and interface inventory

- Create `crates/tracedecay-query/{Cargo.toml,src/lib.rs}`.
- Move `src/query/**` into `crates/tracedecay-query/src/**`.
- Modify workspace `Cargo.toml`, root `Cargo.toml`, `src/lib.rs`, feature maps,
  package includes, and `tests/architecture_boundaries/query_kernel.rs`.
- Move query/temporal unit and focused integration coverage to the crate;
  retain root façade contract tests.

Public compatibility:

```rust
pub use tracedecay_query as query;
```

`tracedecay-query` consumes domain code-intelligence DTOs, application
`RequestContext`/`ResolvedScope`, and explicit read ports. It must not depend
on root, daemon, MCP, dashboard, commands, global DB adapters, or
`crate::code_index::chunks`.

## Historical task checklist

- [ ] Add architecture tests rejecting root/daemon/MCP imports from the crate.
- [ ] Create the manifest with the minimum direct dependencies and feature map.
- [ ] Move modules without changing code, then repair imports through domain
      DTOs and read-port interfaces.
- [ ] Move query unit, temporal, cursor, graph, and semantic-search tests to
      focused ownership; preserve root façade tests.
- [ ] Wire the root re-export and all production callers.
- [ ] Verify default, all-feature, no-default/lite, package, and generated-doc
      contracts.

## Product outcome contributed

The query kernel and focused ownership moved out of the root crate while query
results, public compatibility, authorization, and truthful partial/unavailable
states remained equivalent. Current direct behavior and acceptance live in the
applicable numbered V2 plan.

## Historical migration, rollback, measurement, and deletion notes

The recorded sequence used separate compile-green commits and `git revert`;
there was no data migration. Its Gate A threshold and rebuilt-unit protocol
were historical experiment criteria and are not current acceptance.

Delete old root files and the `code_index::chunks` exception only after all
production callers use the crate and default/all/lite/package contracts pass.
