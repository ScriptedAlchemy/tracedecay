# tracedecay-runtime-core seams

Written by the kernel mover during the one-shot crate split
(`docs/superpowers/plans/2026-07-31-one-shot-crate-split.md`). Everything here
is an edge that crossed the new crate boundary. Resolved edges are recorded so
the lead knows the mechanism; open edges are what the fix-to-green campaign
still owes.

Status at hand-off:

- `cargo check -p tracedecay-runtime-core` — **green**
- `cargo check -p tracedecay-runtime-core --features test-transport` — 1 error (seam 1)
- `cargo check -p tracedecay-runtime-core --tests` — 1 error (seam 1)
- Root crate — red by design; the movers land first, the lead fixes to green.

---

## Open seams

### 1. Fixture store runtimes (the one remaining compile error)

`crates/tracedecay-runtime-core/src/db/connection.rs:29` still imports
`crate::daemon::store_runtime::registry::{LifecycleShardRuntimePublisher,
ProfileAuthorityPinResult, ResolvedStoreLocator, StoreRuntimeKey,
StoreRuntimeOpenMode, StoreRuntimeOpenRequest, StoreRuntimeOpenResult,
StoreRuntimeRegistry, StoreRuntimeRegistryFailure, StoreRuntimeRegistryFuture,
StoreRuntimeResolver}` under `#[cfg(any(test, feature = "test-transport"))]`.

`Database::publish_fixture_runtime` builds a whole daemon registry inline
(~120 lines) to hand `Database::publish_test_runtime` /
`publish_maintenance_test_runtime` an attachment. Twenty-nine call sites across
fourteen kernel test files depend on those constructors, so deleting or
cfg-gating them would trade one compile error for fourteen dead test files.
It was left intact deliberately.

Two ways out, in order of preference:

1. **Move `daemon/store_runtime/` into this crate.** It is already far more
   coupled to the kernel than to the daemon: 53 references to `crate::db`,
   14 to `crate::storage`, 2 to `crate::sqlite_read_snapshot`, 1 each to
   `crate::runtime_identity` and `crate::errors` — against 34 to sibling
   `crate::daemon` modules. `StoreRuntimeHandle` holds a
   `crate::db::DatabaseAuthority` and implements
   `tracedecay_rusqlite_runtime::RuntimeWriteAuthority` over it, so the
   registry/kernel edge is a genuine cycle that only disappears when the
   registry lands below the daemon. Doing this also retires seam 2.
2. **Add a `FixtureStoreRuntimeFactory` port** next to `StoreRuntimeSource` in
   `ports.rs`, move the registry-construction body to whichever crate owns the
   registry, and register it. This compiles, but turns twenty-nine
   compile-checked tests into runtime failures whenever a test process forgets
   to register — hence the preference for (1).

### 2. `StoreRuntimeSource` port wiring (root side)

`crates/tracedecay-runtime-core/src/ports.rs` defines `StoreRuntimeSource` and
`StoreRuntimeSourceHandle = Arc<dyn StoreRuntimeSource>`. The kernel now
retains that trait object instead of the concrete
`daemon::store_runtime::registry::StoreRuntimeHandle` at:

- `db/connection.rs` — `Database::publish_runtime`, `Database::retained_runtime`,
  `DatabaseInner::_runtime`
- `db/connection/registry.rs` — `DatabaseInner::publish`
- `db/maintenance.rs` — `storage_page_counts`, `run_incremental_vacuum`
- `store/memory/runtime.rs` — the fact-store read/write dispatch

**Owed by the root:** `impl StoreRuntimeSource for StoreRuntimeHandle` and
`Arc::new(handle)` at every `Database::publish_runtime` call. The trait covers
every method the kernel used: `opened_file_identity`, `canonical_path`
(was `locator().path()`), `verified_locator` (was `locator().verified()`),
`binding`, `schema_migrated`, `writer_present` (was
`physical_snapshot().writer_present`), `validate_registered_read`,
`telemetry_read_handle`, `authorized_migration_sql_handle`,
`database_authority`, `storage_page_counts`,
`run_bounded_incremental_compaction`, `run_checkpoint`, `snapshot_to`,
`dispatch_submit_authorized`, `dispatch_read`, `runtime_identity`.

`StoreRuntimeRegistryFailure` does not cross the boundary — every kernel call
site only `Debug`-formats it, so the port returns `String`. Async methods
return `ports::StoreRuntimeFuture<'_, T>` because the port is a trait object.

Root callers of `Database::retained_runtime()` that want concrete
`StoreRuntimeHandle` methods not on the port will need either a port method or
a downcast.

### 3. `global_db` / `sessions` store adapters stayed in the root

`src/store/{git_correlation,global_db,observation,session,vector_generations,
workflow}.rs` and `src/store/mod.rs` were moved into the kernel and then moved
back: they are adapters *over* `global_db`, `sessions`, and `semantic_code`,
all of which sit above the kernel. Only `store::memory` (the fact store, ~16K
lines, zero upward references) stayed. `src/store/mod.rs` re-exports it with
`pub use tracedecay_runtime_core::store::memory;`, so `crate::store::memory::…`
resolves unchanged.

**Owed by the lead:** when `tracedecay-global-db` and the full `tracedecay-
sessions` land, these adapters should follow them, not the kernel.

### 4. Daemon session registry — `memory::user::open_user_memory_db`

The five-line opener borrows
`daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1`, so
it stayed in the root `memory::user` shim
(`src/memory.rs`), which re-exports the kernel's `memory::user` alongside it.
Callers (`automation::memory_curator`, `automation::runner::session_reflector`,
`mcp::tools::handlers::memory`) are unchanged.

### 5. Branch-admin recovery gate needs registering

`branch::try_acquire_branch_add_lock` moved into the kernel, but its
pending-recovery check reads `branch::admin::transaction`'s journal, which
stayed in the root. The kernel calls
`ports::branch_admin_recovery::ensure_no_pending_recovery`, which is a **no-op
until the root registers**. The root exposes
`crate::branch::register_branch_admin_recovery_gate()`.

**Owed by the lead:** call it from every process entry point that can take a
branch lock (daemon startup, CLI `main`, and the test harnesses that exercise
branch admin). Until then, locking still serializes correctly; it just does not
refuse a lock while a branch-admin mutation is unfinished.

### 6. Test coverage parked behind cfgs

Two `cfg` flags were added (declared in the crate's `[lints.rust]
unexpected_cfgs` check-cfg list) because the types they need live above the
kernel. Nothing sets them, so these tests do not run anywhere right now:

- `tracedecay_graph_query_tests` — `db/coverage.rs`
  `temp_table_lifecycle_uses_the_database_writer` needs
  `graph::queries::GraphQueryManager`.
- `tracedecay_memory_application_tests` — `store/memory/memory_cutover_test.rs`
  `cutover_preserves_legacy_usage_telemetry_and_search_ranking` and
  `dashboard_vector_points_report_v1_entity_link_connections` need
  `application::memory::MemoryApplication` / `MemoryOperationContext`.

Both should be re-homed into the crate that owns the missing type.

### 7. Visibility promotion is now the kernel's public surface

439 `pub(crate)` items in the moved files were promoted to `pub` so the root
shims' `pub use …::*;` globs can still reach them. `#![allow(unreachable_pub)]`
documents that this surface is an artifact of the split, not new API. If the
lead wants a tighter surface later, the promotion is mechanical to reverse
item by item once the root's real needs are known.

`db::engine::params!` needed `#[macro_export]` (a bare `macro_rules!` cannot be
re-exported across a crate boundary) and is re-exported at
`db::engine::params` so `src/global_db/**`'s SQL sites keep compiling.

---

## Resolved seams (mechanism applied, nothing owed)

| Edge | Mechanism |
|---|---|
| `crate::tracedecay::current_timestamp` (11 kernel sites) | The 6-line fn moved into `runtime_core::tracedecay`; root `src/tracedecay.rs` re-exports it. |
| `crate::config::{DB_FILENAME, TRACEDECAY_DIR, USER_DATA_DIR_ENV, db_filename, get_tracedecay_dir, active_data_dir_name, get_project_db_path, has_project_database, user_data_dir, discover_project_root}` | Moved into `runtime_core::config` (plus the private `nextest_isolated_user_data_dir`, `canonicalize_data_dir`, `canonicalize_path_or_existing_parent`, `paths_same` helpers); root `src/config.rs` re-exports the public ten. Kernel paths spell `crate::config::…` unchanged. |
| `crate::branch::{current_branch, acquire_branch_lock_blocking, try_acquire_branch_add_lock}` | Moved into `runtime_core::branch` together with `GixHead`/`current_branch_gix`/`current_branch_git`, the raw lock, the blocking-retry helper, and `BRANCH_LOCK_RETRY_*`. Root `src/branch.rs` re-exports all of it; the duplicated helpers were deleted from `src/branch/admin.rs`. |
| `crate::project_registry::primary_checkout_root` (worktree.rs) | Pure path logic, moved into `runtime_core::project_registry`; root re-exports. Preferred over the injected-fn-parameter option because there is no registry state involved. |
| `crate::application::host_admission::{sync_directory, DirectorySyncPolicy}` (db/access/owner_io.rs) | Repointed at the canonical `tracedecay_application::framed_log`. |
| `crate::global_db::{RegisteredGlobalDb, StoreInstanceRecord}` (storage.rs) | `try_classify_project_storage_with_registry` and `classify_registry_storage` lifted to the root `src/storage.rs` shim over a newly `pub` `classify_registry_storage_fields`. No trait object was needed. |
| `include_str!("../tests/fixtures/redundancy_eval_labeled.json")` (redundancy.rs) | Repointed at `../../../tests/fixtures/…`; the fixture stays in the repo-root `tests/`. |

## Feature map

| Root feature | Kernel | Notes |
|---|---|---|
| `test-transport` | forwarded to `tracedecay-runtime-core/test-transport`, which forwards to `tracedecay-rusqlite-runtime/test-transport` | 39 `cfg(feature = "test-transport")` sites in the moved files. Blocked by seam 1. |
| `production`, `lite`, `full`, `medium`, `lang-*`, `token-counting`, `semantic-fastembed`, `rusqlite-parity-helper` | not forwarded | No moved file references them. |

Platform cfgs travel with the code: `cfg(windows)` (`lifecycle_lease`,
`os_str_bytes`, `windows_file`, `db/access/owner_io`), `cfg(unix)`
(`os_str_bytes`, `branch_meta`), `cfg(target_os = "linux")` /
`cfg(target_os = "macos")` (`open_store_holders`). The matching
`[target.'cfg(…)'.dependencies]` blocks (`xattr`, `libc`, `fsys`,
`windows-sys`) are mirrored in the crate manifest.
