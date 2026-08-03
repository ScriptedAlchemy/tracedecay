# tracedecay-runtime-core seams

Written by the kernel mover during the one-shot crate split
(`docs/superpowers/plans/2026-07-31-one-shot-crate-split.md`). Everything here
is an edge that crossed the new crate boundary. Resolved edges are recorded so
the lead knows the mechanism; open edges are what the fix-to-green campaign
still owes.

Status:

- `cargo check -p tracedecay-runtime-core` — **green**
- `cargo check -p tracedecay-runtime-core --features test-transport` — **green**
  (was 1 error; seam 1 is closed)
- `cargo check -p tracedecay-runtime-core --all-targets --features test-transport`
  — only the pre-existing `db/retrieval_anchor_schema.rs`
  `TestConnection: Executor` failures remain; everything under
  `store_runtime/` compiles, tests included.
- `cargo check -p tracedecay` — red from other movers' lanes, not from this
  crate. This file only measures the kernel's own landing.

---

## Open seams

### 1. Fixture store runtimes — **CLOSED**

Resolved by option (1) below: `src/daemon/store_runtime/` moved into
`crate::store_runtime`, so `db/connection.rs`'s
`#[cfg(any(test, feature = "test-transport"))]` fixture block now imports
`crate::store_runtime::registry::{…}` and the `test-transport` build is green.
The root keeps `src/daemon/store_runtime.rs` as a
`pub(crate) use tracedecay_runtime_core::store_runtime::*;` shim, so every
historical `crate::daemon::store_runtime::…` path still resolves.

Recorded rationale, since the measurement is what justified the move: the tree
referenced `crate::db` 59 times, `crate::storage` 14, `crate::memory` 3,
`crate::sqlite_read_snapshot` 2, and `crate::runtime_identity`/`crate::errors`
once each — against 25 genuine `crate::daemon` references, 24 of which were in
`session_registry.rs` alone. `StoreRuntimeHandle` holds a
`crate::db::DatabaseAuthority` and implements
`tracedecay_rusqlite_runtime::RuntimeWriteAuthority` over it.

What moved: `graph_metadata`, `registry` (+ `attachment`, `capacity`, `close`,
`leases`, `open`, `ports`, `tests`), `resolver`, `rusqlite_parity`, `shard`,
`telemetry` — 11.1K of the 12.9K lines. See seam 9 for the part that stayed.

`store_runtime::rusqlite_parity` is `#![cfg(test)]` and speaks the parity
helper's wire protocol, so `tracedecay-sqlite-parity-protocol` was added as a
**dev-dependency**. It is a leaf crate (hex/serde/serde_json/sha2), so it adds
no edge to the non-test crate graph.

### 9. `session_registry` could not follow the registry down

`src/daemon/store_runtime/session_registry.rs` (1.8K lines) stayed in the root
and is declared as a submodule of the shim. Blockers, in order of severity:

| Root reference | Count | Why it blocks the move |
|---|---|---|
| `global_db::RegisteredGlobalDb` | 3 (+ ~12 uses) | Held in the public surface: `profile_database()`, `profile_sessions()`, `mounted_session_databases()`, `project_sessions()` all return `Arc<RegisteredGlobalDb>`, and 15 root files consume them. `tracedecay-global-db` → `tracedecay-migrate` → `tracedecay-runtime-core`, so the kernel taking that edge is a **Cargo cycle**. A port would have to erase the type and force every root caller to downcast. |
| `daemon::profile_identity::LocalProfileIdentityAuthorityV1` | 12 (10 test) | The registry stores it by value and `open()` takes it. `src/daemon/profile_identity.rs` is itself kernel-pure (`crate::{db, errors, storage}` only), so moving *it* down is the cheap unblock — but it is named by 15 root files, so it belongs to a separate pass. |
| `daemon::log_daemon_event` | 4 | Production. Daemon event log; needs a port or a `tracing` repoint. |
| `daemon::code_index_scheduler::identity::{IndexingIdentityV1, repository_id_for, worktree_id_for}` | 4 | Production. Also kernel-pure (`crate::worktree` only) — same class as `profile_identity`. |
| `daemon::authority::{current_record, DaemonAuthority::acquire}` | 3 (2 test) | `current_record` is production, in `runtime_incarnation`. `src/daemon/authority.rs` is kernel-pure too (`crate::{db, errors, runtime_identity, storage}`). |
| `daemon::transport::{DaemonEndpoint, default_loopback_endpoint}` | 2 | Test-only. |
| `sessions::user_sessions_db_path` | 1 | Test-only; see seam 11. |

**Recommended order for whoever closes this:** move `daemon/profile_identity`,
`daemon/authority`, and `daemon/code_index_scheduler/identity` into the kernel
first (all three are pure and their only kernel-external users are root shims),
then port `log_daemon_event`, and only then decide whether `RegisteredGlobalDb`
is worth erasing — it may be cheaper to wait for `tracedecay-global-db` to stop
depending on `tracedecay-migrate`.

### 10. `ports::registered_schema` needs registering (fails closed)

`store_runtime/registry/ports.rs` initialises a freshly created profile- or
session-scoped shard by installing the registered global/session schema. That
schema is `tracedecay_global_db::ensure_registered_schema`, which the kernel
cannot name (same cycle as seam 9), so it now goes through
`ports::registered_schema`.

Unlike `branch_admin_recovery`, this port **fails closed**: an unregistered
installer refuses the open rather than publishing an uninitialised store. The
root registers it from
`daemon::store_runtime::register_registered_schema_installer()`, called at the
top of `DaemonSessionRuntimeRegistryV1::open()` — the sole constructor of the
production registry.

**Owed:** `tracedecay-migrate`'s `consolidate/runtime.rs` builds its own
`StoreRuntimeRegistry` and never calls the root, so a migration that initialises
a session-scoped artifact will hit the fail-closed error. It needs its own
registration once that crate compiles. Kernel unit tests are unaffected: they
use a fake `ShardRuntimePublisher` that never reaches `initialize_schema`.

### 11. `USER_SESSIONS_DB_FILENAME` is restated in the kernel

`store_runtime::resolver` needs the profile-scoped session filename, but it is
owned by `tracedecay_sessions::runtime::ingest::user`, which the kernel cannot
depend on. `store_runtime::profile_paths` restates the constant and the
one-line join.

The duplication is pinned rather than trusted: `src/daemon/store_runtime.rs`
carries two `#[cfg(test)]` assertions that the kernel's value equals
`crate::sessions::`'s, so a divergence fails the root suite. When
`tracedecay-sessions` repoints at the kernel, it should re-export
`profile_paths::USER_SESSIONS_DB_FILENAME` and the assertions can go.

### 8. `impl From<db::engine::Error> for LcmError` violates the orphan rule

`src/sessions/lcm/types.rs:20` — the only remaining root error. `LcmError` is
defined in `crates/tracedecay-sessions/src/lcm/contracts.rs` and
`db::engine::Error` now lives in `tracedecay-runtime-core`, so neither type is
local to the root crate any more.

Deleting the impl is not an option: it carries 691 `?` sites across the LCM
read paths (measured). Two viable fixes, both outside this mover's scope
because they touch the sessions mover's crate:

1. **Move `db::engine::Error`/`Result` down.** It is 117 lines with no
   dependency beyond `std` (`crates/tracedecay-runtime-core/src/db/engine/
   error.rs`). Both `tracedecay-runtime-core` and `tracedecay-sessions`
   already depend on `tracedecay-domain`, so moving it there (or to
   `tracedecay-store`, if sessions gains that dep) lets the `From` impl live
   beside `LcmError` with no new heavy edge. **Recommended.**
2. **Add `tracedecay-runtime-core` as a dependency of `tracedecay-sessions`**
   and put the impl in `lcm/contracts.rs`. One line, but it hangs the whole
   kernel off the sessions crate's compile graph for a single conversion.

### 2. `StoreRuntimeSource` port — **COLLAPSED**

The port existed only because the registry had stayed in the root. With seam 1
closed, `StoreRuntimeHandle` is crate-local to the kernel, so the trait, the
`StoreRuntimeFuture` alias, the `Arc<dyn …>` handle alias, and the ~130-line
`impl` in `registry.rs` were all deleted. `db/connection.rs`,
`db/connection/registry.rs`, `db/maintenance.rs`, and `store/memory/runtime.rs`
now name the concrete `store_runtime::registry::StoreRuntimeHandle`.

The four adapters the port had supplied became inherent methods on the handle:
`canonical_path` (was `locator().path()`), `verified_locator` (was
`locator().verified()`), `writer_present` (was
`physical_snapshot().writer_present`), and `runtime_identity`. Everything else
the kernel used was already inherent.

Consequences, all applied: `Database::publish_runtime` takes the handle by
value, so the `Arc::new(runtime)` wraps were dropped at the six
`session_registry.rs` sites and the one in `application/evidence_assembly.rs`.
`tracedecay-migrate`'s two sites in `consolidate/runtime.rs` still wrap and are
owed by that crate's owner (it is not compiling yet for unrelated reasons).
Registry failures no longer have to cross as `String`, and root callers of
`retained_runtime()` can reach every concrete method again — `publication()` and
`runtime()`, which the port did not expose, were already being called.

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

One `cfg` flag was added (declared in the crate's `[lints.rust]
unexpected_cfgs` check-cfg list) because the types it needs live above the
kernel. Nothing sets it, so this test does not run anywhere right now:

- `tracedecay_graph_query_tests` — `db/coverage.rs`
  `temp_table_lifecycle_uses_the_database_writer` needs
  `graph::queries::GraphQueryManager`.

It should be re-homed into the crate that owns the missing type.

(A second flag, `tracedecay_memory_application_tests`, gated
`store/memory/memory_cutover_test.rs`; that file went away with the
legacy-memory cutover removal, and the flag with it.)

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
| `impl ExactSqlWriteAuthority for DatabaseAuthority` (`src/db/access.rs`) | The trait belongs to `tracedecay_rusqlite_runtime`, so the orphan rule allows the implementation only beside `DatabaseAuthority`. |
| `daemon::store_runtime::{graph_metadata, registry, resolver, rusqlite_parity, shard, telemetry}` | Moved into `runtime_core::store_runtime` (11.1K lines); root `src/daemon/store_runtime.rs` glob-re-exports. Closes seams 1 and 2. |
| `branch::{sanitize_branch_name, detect_default_branch, resolve_branch_db_path}` | Moved into `runtime_core::branch`; root re-exports. All three are pure over `gix`, `branch::current_branch`, and `branch_meta::BranchMeta`. `tracedecay-migrate` consumes all three and could not reach the root. |
| `config::{GENERATED_DIR_SEGMENTS, is_generated_dir_segment}` | Moved into `runtime_core::config`; root re-exports. Same reason — `tracedecay_migrate::inventory` prunes directories with it. |
| `application::context::{CancellationToken, MonotonicDeadline}` | Moved into the new `runtime_core::cancellation`; `src/application/context.rs` re-exports. `store_runtime::rusqlite_parity` bounds every parity probe with them. `is_same_token` widened from `pub(crate)` to `pub` for `src/application/session/types.rs`. |

## Feature map

| Root feature | Kernel | Notes |
|---|---|---|
| `test-transport` | forwarded to `tracedecay-runtime-core/test-transport`, which forwards to `tracedecay-rusqlite-runtime/test-transport` | 39 `cfg(feature = "test-transport")` sites in the moved files. **Unblocked** — seam 1 is closed and the feature build is green. |
| `production`, `lite`, `full`, `medium`, `lang-*`, `token-counting`, `semantic-fastembed`, `rusqlite-parity-helper` | not forwarded | No moved file references them. |

Platform cfgs travel with the code: `cfg(windows)` (`lifecycle_lease`,
`os_str_bytes`, `windows_file`, `db/access/owner_io`), `cfg(unix)`
(`os_str_bytes`, `branch_meta`), `cfg(target_os = "linux")` /
`cfg(target_os = "macos")` (`open_store_holders`). The matching
`[target.'cfg(…)'.dependencies]` blocks (`xattr`, `libc`, `fsys`,
`windows-sys`) are mirrored in the crate manifest.
