# `tracedecay-usecases` seams

This crate owns transport-neutral product orchestration extracted from the
root crate. Its production library compiles with all features without reaching
up into root `daemon`, `mcp`, `dashboard`, `hooks`, or diagnostics modules.

It deliberately does not depend on `tracedecay-agent-hosts`,
`tracedecay-dashboard-api`, or `tracedecay-migrate`.

## Dependency edges and cycle proofs

Every internal edge below is proven acyclic with
`cargo tree -e normal -p <dep> | grep tracedecay-usecases` returning **no
matches** (verified at `96fc109f1`, all 19 edges, 0 occurrences each):

| Internal dependency | Cycle proof (`grep tracedecay-usecases` hits) |
| --- | --- |
| `tracedecay-api` | 0 |
| `tracedecay-application` (`native-git`) | 0 |
| `tracedecay-automation` | 0 |
| `tracedecay-code-index` (`default-features = false`) | 0 |
| `tracedecay-domain` | 0 |
| `tracedecay-global-db` | 0 |
| `tracedecay-hooks` | 0 |
| `tracedecay-host-integration` | 0 |
| `tracedecay-lsp` | 0 |
| `tracedecay-policy` | 0 |
| `tracedecay-query` | 0 |
| `tracedecay-runtime-core` | 0 |
| `tracedecay-rusqlite-runtime` | 0 |
| `tracedecay-search-eval` | 0 |
| `tracedecay-semantic` | 0 |
| `tracedecay-sessions` | 0 |
| `tracedecay-store` | 0 |
| `tracedecay-temporal-query` | 0 |
| `tracedecay-tool-catalog` | 0 |

External deps track sibling manifests: `cap-std`, `getrandom`, `fs2`, `gix`,
`glob`, `hex`, `rusqlite`, `regex`, `same-file`, `schemars`, `serde`,
`serde_json`, `sha2`, `tempfile`, `thiserror`, `toml`, `ureq`, `tokio`,
`tokio-stream`, `tracing`, `url`, `zeroize`, `ignore`.

### Forbidden-edge proof

`cargo tree -e normal -p tracedecay-usecases` contains no `tracedecay` (root),
`tracedecay-agent-hosts`, `tracedecay-dashboard-api`, or `tracedecay-migrate`
node. The reverse edge is the real one: `tracedecay-dashboard-api` depends on
`tracedecay-usecases`, so any edge back would be a Cargo cycle. Root-owned
seams must therefore be resolved by port inversion, never by a dependency.

## Root wiring required

### Graph runtime

The composition root must implement `tracedecay_usecases::tracedecay::GraphRuntimePort`
for its graph runtime. The port covers graph reads, search/statistics,
diagnostics, redundancy and health reads, source edits, and source-edit
recovery.

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

The observation, external-source, and vector-generation adapters are owned
here and are constructed from `RegisteredGlobalDb::{runtime,authority}`.
Evidence-assembly and retrieval-anchor persistence remain canonical store and
runtime capabilities; their unmounted use-case adapters were removed until a
production journey needs them.

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

### Test-target status (not yet in scope)

`cargo check -p tracedecay-usecases --all-features` (lib) is **0 errors**.
`--all-targets` still fails on the `lib test` target with 175 errors. This is
a repo-wide phase of the split, not a defect local to this crate: at the same
commit `tracedecay-global-db` (53) and `tracedecay-sessions` (52) `lib test`
targets fail the same way. Fixing it is a coordinated lane, because it needs
new `test-support` features in dependency crates rather than edits here.

Two distinct blocker classes:

1. **Upstream test-only surfaces are `#[cfg(test)]`-gated**, so they are
   invisible downstream no matter how visibility is widened here. Each needs a
   `#[cfg(any(test, feature = "test-support"))]` gate plus a feature in the
   owning crate:
   - `tracedecay_runtime_core::db::engine::TestConnection`
     (`db/engine/mod.rs:10,23` — `mod test_support` and its re-export are both
     `#[cfg(test)]`)
   - `tracedecay_runtime_core::config::{PinnedUserDataDir,
     lock_user_data_dir_test_env}` (absent from the extracted config surface)
   - `tracedecay_global_db::tests`
   - `session_pool::test_support` (via `tracedecay-semantic`)
   - `code_index::projection`
   - `tracedecay_sessions::repository_provenance::RepositoryProvenanceAdmissionContext::new`
     (exists but its trait bounds are unsatisfied from here)

2. **Root-owned seams still named by `#[cfg(test)]` fixtures**, concentrated in
   `host_admission.rs` (~60), `edit.rs` (~45), and smaller counts in
   `diagnostics_*.rs`, `dashboard_diagnostics.rs`, `semantic_runtime/*`,
   `observation_test.rs`, `event_lane.rs`: `crate::{mcp, daemon, hooks,
   automation, dashboard}`, `crate::tracedecay::TraceDecayOpenOptions`,
   `crate::store::{session, GlobalDbGitCorrelationStore, GlobalDbTranscriptStore,
   GlobalDbWorkflowStore}`, and `tracedecay_migrate`. The `edit.rs` cluster is
   one root cause repeated 40 times: the `FixtureGraph` test double predates
   `GraphRuntimePort` and no longer satisfies it. Several fixtures also call
   `GraphRuntimePort::init`/`open_*` as if the port were a concrete type,
   producing `dyn GraphRuntimePort` unsized errors — these fixtures must move to
   the composition root, which is the same conclusion as the section above.

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
