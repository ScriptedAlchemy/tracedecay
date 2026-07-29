# Domain DTO and RequestContext Convergence Plan

**Goal:** Make query-facing DTOs and request scope independent of the root
crate before extracting query.

**Authority:** Plan 12 leaves-first sequencing; Plan 09 application ownership;
the [super plan](../2026-07-28-v2-delivery-root-crate-breakup.md).

## Files and interfaces

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

## Tasks

- [ ] Add compile-fail architecture coverage that domain imports neither root
      query nor root code-index modules.
- [ ] Move the query-facing chunk DTO and conversion tests into domain.
- [ ] Converge production entry points on application `RequestContext` plus
      `ResolvedScope`; keep only a deprecated delegating root façade.
- [ ] Regenerate dashboard contracts if a schemars owner changes and prove
      byte-stable generated output or review the explicit schema delta.
- [ ] Remove every query dependency on `crate::code_index::chunks`.

## Tests

Direct: exact-root CLI/MCP/HTTP/LSP calls resolve the same project and scope;
query DTO round trips preserve digests and ranges.

Negative: CWD fallback, missing registry, unauthorized sibling root, ambiguous
scope, stale project generation, and root/domain type mismatch fail closed.

Run:

```bash
cargo check -p tracedecay-domain --lib --all-features
cargo test -p tracedecay-domain --all-features
cargo check -p tracedecay-application --lib --all-features
cargo nextest run --all-features -E 'test(architecture_boundaries)'
cd dashboard && npm run contracts:check
```

## Migration, rollback, and measurement

Migrate callers one surface at a time behind the root re-export. Rollback by
reverting the caller slice; persisted data does not change. Measure warm domain
leaf and root all-feature leaf checks before/after, recording rebuilt units.

Commit slices: domain DTO; RequestContext callers; generated contracts;
compatibility deletion. Delete root DTOs/re-exports only when graph search
shows zero production callers and default/all/lite/package gates pass.
