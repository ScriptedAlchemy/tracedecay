# tracedecay-usecases root seams

The one-shot crate split moved the whole former `src/application/` tree into
`crates/tracedecay-usecases/src/` (the former `mod.rs` became `lib.rs`) without
resolving references back into the root binary crate.

```
cargo check -p tracedecay-usecases   # 256 errors, all catalogued below
cargo check -p tracedecay-runtime-core   # 0 errors (unchanged)
```

Every remaining error is a `crate::<root module>::…` path that no longer
resolves because the module stayed in the root binary crate, or because its
owning crate does not currently build. Nothing else is broken: there are no
type errors, no trait-resolution errors, and no borrow errors.

## Why this crate, and why it cannot cycle

`src/application` could not merge into `tracedecay-application`: that crate is
the **bottom** ports crate with 14 dependents. `tracedecay-usecases` is
therefore a **new crate above the whole stack**. No crate in the workspace
depends on it — the root binary reaches it through the `src/application.rs`
shim — so every dependency edge listed below is trivially acyclic. A cycle is
only possible if a future change makes a lower crate depend on this one; that
must not happen.

### Dependencies actually taken

| Crate | Why |
| --- | --- |
| `tracedecay-runtime-core` | `errors`, `db`, `storage`, `types`, `privacy`, `git`, `sync`, `worktree`, `timeutil`, `path_scope`, `lifecycle_lease`, `project_registry`, `memory`, `branch`, `cancellation` |
| `tracedecay-application` | request/scope contracts, `source_edit` result types, `git` read contracts, `historical_query` |
| `tracedecay-domain` | identity, configuration and diagnostics value types (largest single edge: 327 refs) |
| `tracedecay-store` | store DTO/port contracts (266 refs) |
| `tracedecay-sessions` | session runtime (`crate::sessions`) and `repository_provenance` |
| `tracedecay-code-index` | `crate::code_index`, `crate::ast_grep_search` |
| `tracedecay-semantic` | `crate::semantic_code` |
| `tracedecay-search-eval` | `crate::search_eval` |
| `tracedecay-query` | `crate::query` |
| `tracedecay-tool-catalog`, `tracedecay-temporal-query`, `tracedecay-lsp`, `tracedecay-policy`, `tracedecay-hooks`, `tracedecay-capture`, `tracedecay-api`, `tracedecay-rusqlite-runtime` | direct `tracedecay_*::` references already present in the moved tree |

### Dependencies deliberately **not** taken

`tracedecay-global-db`, `tracedecay-migrate`, `tracedecay-agent-hosts` and
`tracedecay-dashboard-api` do not build today:

```
cargo check -p tracedecay-global-db     # 499 errors
cargo check -p tracedecay-migrate       #  59 errors
cargo check -p tracedecay-agent-hosts   #  28 errors
```

Adding those edges would make this crate uncheckable, exactly as
`tracedecay-sessions` reasoned about `tracedecay-global-db`. Their references
are catalogued as seams instead. **When those parallel lanes land, the
`global_db`, `store`, `migrate`, `automation`, `agents` and `dashboard` rows
below become plain dependency edges — no port inversion is required for them.**

## Seam catalog

Counts are references in the moved tree, measured after the mechanical
repointing pass.

### Blocked only on a parallel lane (add the edge once it is green)

| Root path | Refs | Files | Resolution |
| --- | --- | --- | --- |
| `crate::global_db::…` | 64 | 11 | Depend on `tracedecay-global-db`; rewrite to `tracedecay_global_db::`. Hot items: `RegisteredGlobalDb` (9), `ParseOffset` (9), `configuration::OwnedGlobalDbConfigurationControlStore` (6), `CodeProjectRecord` (4), `AnalyticsEventInsert` (4). |
| `crate::store::…` | 19 | 3 | The root `src/store` `GlobalDb*` wrappers (`GlobalDbGitCorrelationStore`, `GlobalDbTranscriptStore`, `GlobalDbSessionTemporalStore`, `GlobalDbWorkflowStore`, `GlobalDbObservationStore`, `vector_generations::*`). They wrap `global_db`, so they follow that lane. |
| `tracedecay_migrate::…` | 8 | 1 | `consolidate::{ManifestRetirementReport, retire_applied_input_manifests}` and `registry::{RegistryReconstruction*, apply_*}` in `host_admission.rs`. Re-add the `tracedecay-migrate` edge once it builds. |
| `crate::automation::…` | 3 | 2 | `automation::{scheduler, config}` → `tracedecay-agent-hosts` / `tracedecay-automation`. |
| `crate::agents::…` | 2 | 2 | `agents::host_bundle_v2` → `tracedecay-agent-hosts`. |
| `crate::dashboard::…` | 2 | 1 | `DashboardHostAdmissionTestAuthorityV1` → `tracedecay-dashboard-api`. |
| `crate::retention::…` | 1 | 1 | `retention::code_index_generations` → `tracedecay-automation`. |

### Root modules that should move down (no crate owns them yet)

These are not layering violations — they are modules that belong at or below
this crate but have not been extracted. Each row is a follow-up move, not a
port inversion.

| Root path | Refs | Files | Recommended destination |
| --- | --- | --- | --- |
| `crate::config::{retrieval, registry, resolver, scope_control}` | 34 | — | `src/config/{retrieval,registry,resolver,scope_control}.rs` depend on **nothing but `tracedecay-domain`**. They are the cheapest remaining win: move them into `tracedecay-domain` (or a new `tracedecay-configuration`) and this whole group resolves. |
| `crate::config::{PinnedRuntimeConfiguration, PinnedUserDataDir, ConfigurationDaemonClient, *_runtime_configuration_for_registered_database, TraceDecayConfig}` | 30 | — | Root-owned runtime configuration wiring. Depends on `global_db`; follows that lane, then moves here. |
| `crate::diagnostics_publication::{CodeIndexPublicationIdentityPortV1, CodeIndexPublicationIdentityV1, CodeIndexPublicationIdentityFuture, code_index_logical_path}` | 14 | 8 | `src/diagnostics_publication.rs` depends only on `tracedecay-domain`, `tracedecay-lsp` and `crate::diagnostics_store`. Move both files into this crate (or domain) — 13 of the 14 refs are the one-method `CodeIndexPublicationIdentityPortV1` trait. |
| `crate::diagnostics_store::DiagnosticsStore` | 3 | 3 | Move with `diagnostics_publication` above; the two are a unit. |
| `crate::request_identity::{GlobalRequestSurface, mint_global_request_id, mcp_connection_request_id, GlobalOpaqueIdentityKind, LogicalEffectIdempotencyDomain}` | 9 | 7 | `src/request_identity.rs` is a partial shim over `tracedecay-application`. Finish the extraction into `tracedecay-application`. |
| `crate::user_config::{config_path, ConfigSaveError}` | 4 | 2 | Partial shim over `tracedecay-domain`; finish that extraction. |
| `crate::git_query::GitQueryEngine` | 2 | 1 | Partial shim over `tracedecay_domain::code_intelligence`; finish that extraction. |
| `crate::diagnostics_query::…` | 1 | 1 | Same — partial shim over `tracedecay-domain`. |
| `crate::context::{read_modes, source_read}` | 2 | 1 | `src/context/{read_modes,source_read}.rs` are pure source-read helpers; move them beside the primitives that use them (`primitives/concrete.rs`). |
| `crate::graph::{queries::GraphQueryManager, health::{dependency_depth, depth_score}}` | 2 | 1 | Belongs in `tracedecay-query`. |
| `crate::hooks::hint_outcomes::{HintOutcomeStats, correlate_hint_outcomes}` | 3 | 1 | Belongs in `tracedecay-hooks` (already a dependency). |
| `crate::analytics_bridge::{HookImportSource, HookImportOutcome, import_hook_analytics}` | 3 | 1 | Root-owned bridge; depends on `global_db`, so it follows that lane. |
| `crate::application_surface::{ConfigurationProtectedApplySurfaceRequest, ConfigurationProtectedPreviewSurfaceRequest}` | 1 | 1 | Belongs in `tracedecay-api`. Do not edit `src/application_surface.rs` in this lane. |

### Genuine layering violations (require port inversion)

A use-case crate must not reach into adapters. These rows need a port owned by
this crate with the adapter implementing it at the composition root.

| Root path | Refs | Files | Proposed port |
| --- | --- | --- | --- |
| `crate::tracedecay::TraceDecay` (+ `TraceDecayOpenOptions`, `SyncLockGuard`, `PlannedSourceEditFile`, `capture/apply_source_edit_plan`, `read/validate_source_edit_candidate`, `current_timestamp`, `is_test_file`, `try_acquire_sync_lock_at`) | 55 | 16 | The central graph facade. Invert behind a `GraphRuntimePort` (open/branch/init + registered-configuration variants) plus a `SourceEditPlanPort` for the `PlannedSourceEditFile` group. Largest single inversion; the source-edit half is separable and cheaper. |
| `crate::mcp::{tools::{SessionAuthorities, ToolResult, ToolCallRegistryOptions, handle_*, handlers::*}, server::McpServerConstructionContext, response_handles::*}` | 22 | 4 | Transport adapter. Use cases must not construct MCP servers or dispatch tool calls. Invert behind narrow request-handling ports, or move the offending call sites up into `src/mcp`. |
| `crate::daemon::{store_runtime::session_registry::DaemonSessionRuntimeRegistryV1, profile_identity::load_or_create, store_runtime::registry::StoreRuntimeHandle, project_open_owners::resolved_scope_for_project, code_index_scheduler::identity::repository_id_for, session_temporal_refresh_scheduler::*}` | 21 | 9 | Composition-root adapter. `tracedecay-sessions` already inverted the same registry behind `SessionStoreAuthority`/`SessionIngestAuthority`; mirror that. |
| `crate::git_intelligence::NativeGitIntelligence` | 2 | 2 | The native `git` spawn adapter. Its request/response contracts already live in `tracedecay_application::git` (repointed in this lane); only the spawner is left. Invert behind the existing `GitHistoricalBlobReadPort`. |
| `crate::diagnostics::{Scope, run_all}` | 2 | 1 | Diagnostics driver adapter used from `edit.rs`. Invert behind a `PostEditDiagnosticsPort`. |
| `crate::{graph_semantic_capabilities, production_semantic_authorities}` | 2 | 1 | Root re-exports of `src/diagnostics/lsp/semantic.rs`, imported by `lsp_runtime.rs`. Same driver as the row above. |

### Concentration

`host_admission.rs` + `host_admission/` + `host_admission_test.rs` carry **130
of the ~312** original outward references (42%). Closing that one module — it is
the host-admission composition facade, and most of its couplings are
`global_db` / `store` / `config` / `daemon` — resolves most of this catalog.

## Non-seam follow-ups

### Packaging

`semantic_runtime/bundled_query.rs` byte-pins the shipped query profile with
`include_str!` against two repo-root fixtures:

- `tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json`
- `benchmarks/search-quality/query-fallback-report-v1.json`

The paths were re-anchored for the deeper module location and `cargo check`
resolves them, but they live **outside this package**, so `cargo package` will
not carry them (`include = ["/src/**"]`, and cargo `include` patterns cannot
escape the package root). Before publishing, vendor both fixtures under
`crates/tracedecay-usecases/src/` or generate them from a build script.

### Root wiring

- `src/application.rs` is a `pub use tracedecay_usecases::*;` shim. It keeps
  **root** paths alive only. Sibling crates whose own `SEAMS.md` mention "root
  application" seams (notably `tracedecay-global-db`, which reaches
  `crate::application::external_source_store::RuntimeExternalSourceStore`) need
  a direct `tracedecay-usecases` dependency; a root shim cannot serve them.
- 48 `pub(crate)` declarations were widened to `pub` so the glob re-export
  reaches everything root previously used, plus the `event_lane` and
  `external_source_store` module declarations. `retrieval_anchor_store` stayed
  `pub(crate)` — nothing outside the crate reaches it.
- The widening list was derived from the 264 distinct `crate::application::…`
  paths referenced outside the crate. It could not be compiler-verified because
  the root crate does not build while `tracedecay-global-db` is red; re-run the
  check once it does.
