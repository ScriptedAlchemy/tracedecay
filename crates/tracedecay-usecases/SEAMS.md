# `tracedecay-usecases` seams

This crate owns transport-neutral product orchestration extracted from the
root crate. Its production library compiles with all features without reaching
up into root `daemon`, `mcp`, `dashboard`, `hooks`, or diagnostics modules.

It deliberately does not depend on `tracedecay-agent-hosts`,
`tracedecay-dashboard-api`, or `tracedecay-migrate`.

## Root wiring required

### Graph runtime

The composition root must implement `tracedecay_usecases::tracedecay::GraphRuntimePort`
for its graph runtime. The port covers graph reads, search/statistics,
diagnostics, redundancy and health reads, source edits, API migrations, and
source-edit recovery.

Source-edit preview/apply wiring must use the use-case-owned functions and
value type:

- `capture_source_edit_plan`
- `apply_source_edit_plan`
- `capture_planned_source_edit`
- `validate_planned_source_edit`
- `PlannedSourceEditFile`
- `read_source_edit_candidate`
- `validate_source_edit_candidate_parent`
- `try_acquire_sync_lock_at`

The root graph adapter should forward its existing edit capture/validation
hooks to these functions. This preserves one task-local plan authority rather
than creating a second root-owned plan.

### Runtime configuration

Before opening configuration-backed use cases, root must install a
`RuntimeConfigurationAuthorityPort` with
`install_runtime_configuration_authority`. The authority resolves the
registered database, pinned configuration snapshot, and optional daemon
client. Process-local daemon-client helpers remain explicit test/composition
hooks.

Configuration value and persistence contracts are re-exported from
`tracedecay_global_db::configuration::contracts`; they are not duplicated in
this crate.

### Stores and response handles

The observation, external-source, evidence-assembly, retrieval-anchor, and
vector-generation adapters are owned here and are constructed from
`RegisteredGlobalDb::{runtime,authority}`.

Transport-independent response handles are owned by
`tracedecay_usecases::response_handles`; MCP adapters should call that module
instead of retaining a parallel handle store.

### LSP and primitive support

LSP runtime adapters/factory code is owned by `lsp_support`. Lexical grep,
affected-test traversal, and graph-health/redundancy orchestration are local
use-case helpers or `GraphRuntimePort` calls. Root should remove duplicate
copies after its re-export/wiring lane lands.

## Composition-heavy test transport

`HostAdmissionTestRuntimeV1` and its root-coupled fixture helpers are compiled
only under `cfg(test)`, not the `test-transport` feature. External integration
fixtures that need daemon, MCP, migration, automation, hooks, or dashboard
authorities must live in the root crate or a higher adapter crate. They must
not be restored by adding forbidden dependency edges here.

Some existing unit-test-only modules still name root paths and therefore need
that composition-root relocation before `cargo test -p tracedecay-usecases`
can be a standalone target. They do not enter the production/all-features
library build.

## Moved read modules

The crate declares and owns the copied `graph/scc.rs` and
`context/read_cache.rs` modules, plus graph queries/health, context read modes,
source reads, git query/intelligence, request identity, retention, user config,
and application-surface helpers. Root copies should become compatibility
re-exports and then be removed.

## Packaging

`semantic_runtime/bundled_query.rs` uses `include_str!` on repository fixtures
outside this package. Workspace builds resolve them, but publishing requires
vendoring those fixtures under this package or generating them during build.
