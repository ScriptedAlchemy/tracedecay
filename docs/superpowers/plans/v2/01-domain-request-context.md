# Domain DTO and RequestContext Convergence Plan

> **Archived provenance — not current requirements.** This document records
> historical planning and execution evidence. Current scope and acceptance come
> only from [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate its task checklists, file inventories,
> branch/worktree/SHA or commit protocol, Gate A/B, timing/JUnit receipts, exact
> test names/counts, generated-byte/source-shape checks, PR closure gates, or
> platform gate lattice.
> Historical version/compatibility/migration language cannot resurrect
> branch-only scaffolding; without released or live predecessor evidence, the
> current numbered plan changes the contract in place.

**Goal:** Make query-facing DTOs and request scope independent of the root
crate before extracting query.

**Historical inputs:** Plan 12 leaves-first sequencing, Plan 09 application
ownership, and the [archived parent plan](../2026-07-28-v2-delivery-root-crate-breakup.md).

## Historical file and interface inventory

- Modify `crates/tracedecay-domain/src/code_intelligence/{mod.rs,graph.rs}` to
  own graph/query DTOs.
- Create `crates/tracedecay-domain/src/code_intelligence/chunk.rs` for the
  current `src/code_index/chunks.rs` query-facing chunk contract.
- Modify `crates/tracedecay-application/src/lib.rs` and request-context modules
  to expose `RequestContext` and `ResolvedScope`.
- Modify root façade/callers under `src/query/`, `src/mcp/`, `src/dashboard/`,
  `src/commands/`, `src/daemon/`, and `src/application/`.
- Tests: domain/application contract tests, `tests/architecture_boundaries/`,
  dashboard generated-contract checks when schemars ownership moves.

Public handoff:

```rust
pub struct RequestContext {
    pub project: ProjectIdentity,
    pub scope: ResolvedScope,
}

pub enum ResolvedScope {
    ExactRoot(ProjectIdentity),
    AuthorizedSet(AuthorizedScopeSet),
}
```

The domain chunk DTO retains byte-exact identity, source range, language,
content digest, generation, and owning project identity. Root compatibility is
`pub use tracedecay_domain::code_intelligence::*`; it owns no duplicate types.

## Historical task checklist

- [ ] Add compile-fail architecture coverage that domain imports neither root
      query nor root code-index modules.
- [ ] Move the query-facing chunk DTO and conversion tests into domain.
- [ ] Converge production entry points on application `RequestContext` plus
      `ResolvedScope`; keep a delegating root façade only if release evidence
      proves external compatibility requires it, otherwise replace it in place.
- [ ] Regenerate dashboard contracts if a schemars owner changes and prove
      byte-stable generated output or review the explicit schema delta.
- [ ] Remove every query dependency on `crate::code_index::chunks`.

## Product outcome contributed

Query-facing DTO and request-scope ownership moved toward domain/application
boundaries while preserving identity, digest, range, and fail-closed scope
behavior. Current direct behavior and acceptance live in the applicable
numbered V2 plan.

## Historical migration, rollback, and measurement notes

Migrate callers one surface at a time behind the root re-export. Rollback by
reverting the caller slice; persisted data does not change. Measure warm domain
leaf and root all-feature leaf checks before/after, recording rebuilt units.

Commit slices: domain DTO; RequestContext callers; generated contracts;
compatibility deletion. Delete root DTOs/re-exports only when graph search
shows zero production callers and default/all/lite/package gates pass.
