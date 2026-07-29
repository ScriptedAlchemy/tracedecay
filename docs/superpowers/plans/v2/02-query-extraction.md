# `tracedecay-query` Extraction Plan

**Goal:** Move the query kernel and focused tests out of the root crate while
preserving every public path and result byte.

**Dependencies:** `01-domain-request-context.md` is green; domain owns all
query-facing DTOs and application owns request scope.

## Files and interfaces

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

## Tasks

- [ ] Add architecture tests rejecting root/daemon/MCP imports from the crate.
- [ ] Create the manifest with the minimum direct dependencies and feature map.
- [ ] Move modules without changing code, then repair imports through domain
      DTOs and read-port interfaces.
- [ ] Move query unit, temporal, cursor, graph, and semantic-search tests to
      focused ownership; preserve root façade tests.
- [ ] Wire the root re-export and all production callers.
- [ ] Verify default, all-feature, no-default/lite, package, and generated-doc
      contracts.

## Tests

Direct: lexical, graph, temporal, cursor, semantic, exact-flat, redaction, and
pagination results are byte-for-byte equal at the pinned fixture generation.

Negative: malformed/forged cursor, unauthorized root, unavailable semantic
provider, partial generation, deleted source, and stale scope never become
successful empty results.

Run:

```bash
cargo check -p tracedecay-query
cargo check -p tracedecay-query --all-features
cargo nextest run -p tracedecay-query --all-features
cargo nextest run --all-features -E 'test(query_kernel)'
cargo check -p tracedecay --lib --all-features
cargo package -p tracedecay-query --locked --allow-dirty --no-verify
```

## Migration, rollback, measurement, deletion

Land manifest/scaffold, mechanical move, caller wiring, test relocation, and
façade/deletion as separate compile-green commits. Roll back with `git revert`;
there is no data migration. Gate A-query passes only when the identical warm
private query edit improves at least 20% or 8s, root no longer compiles query
sources inline, and rebuilt-unit evidence confirms focused ownership.

Delete old root files and the `code_index::chunks` exception only after all
production callers use the crate and default/all/lite/package contracts pass.
