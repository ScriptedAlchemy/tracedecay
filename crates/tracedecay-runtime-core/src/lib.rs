//! `TraceDecay` runtime kernel.
//!
//! This crate owns the load-bearing runtime substrate that every other
//! `TraceDecay` subsystem sits on: cancellation and task ownership, process
//! and profile leases, resource accounting (resident memory, background CPU),
//! the storage layout and store-identity resolver with the repository and
//! worktree reads it depends on, the `SQLite` database facade with its
//! migrations and read snapshots, and the per-shard runtime registry.
//!
//! ## What sits above this kernel
//!
//! Product runtimes live with their vertical owners, not here: the fact store
//! and memory model in `tracedecay-session-memory`, privacy detection and
//! sanitization in `tracedecay-privacy`, Work/Workflow topology projections in
//! `tracedecay-application`, and the token-savings monitor ring in
//! `tracedecay-session-memory`. Branch tracking (`branch`, `branch_meta`)
//! remains here only because its consumers (`tracedecay-global-db`,
//! `tracedecay-maintenance`, the CLI) sit below every crate that could own it,
//! and [`worktree`] identity reads depend on [`branch::current_branch`].
//!
//! [`shard_runtime`] is the per-shard runtime and registry that `db::Database`
//! is built on. The store runtime that decides which shards a daemon opens and
//! how they converge, retire, and shut down is `tracedecay-store-runtime`; it
//! stores `RegisteredGlobalDbLeaseV1` in its public surface, and
//! `tracedecay-global-db` depends on this kernel — so the kernel taking that
//! edge back would be a Cargo cycle. `global_db`, sessions, and semantic
//! projection are above this crate for the same reason.
//!
//! [`ports::registered_schema`] is the port a freshly opened profile- or
//! session-scoped shard uses to install the registered global/session schema
//! (owned by `tracedecay-global-db`, which this crate cannot name). It
//! **fails closed**: an unregistered installer refuses the open rather than
//! publishing an uninitialised store. `tracedecay-store-runtime` registers it
//! from `register_registered_schema_installer()`, called at the top of
//! `DaemonSessionRuntimeRegistryV1::open()` — the sole constructor of the
//! production registry.
//!
//! `test-transport` forwards to `tracedecay-rusqlite-runtime/test-transport`.
//! Platform cfgs travel with the code that needs them: `cfg(windows)`
//! (`lifecycle_lease`, `os_str_bytes`, `db/access/owner_io`),
//! `cfg(unix)` (`os_str_bytes`, `branch_meta`), `cfg(target_os = "linux")` /
//! `cfg(target_os = "macos")` (`open_store_holders`), with matching
//! `[target.'cfg(…)'.dependencies]` blocks (`xattr`, `libc`, `fsys`,
//! `windows-sys`) in the crate manifest.

#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::similar_names)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::trivially_copy_pass_by_ref)]
#![allow(clippy::unused_self)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::struct_field_names)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::option_option)]
#![allow(clippy::manual_let_else)]
#![allow(clippy::ref_option)]
#![allow(clippy::zero_sized_map_values)]
#![allow(clippy::used_underscore_binding)]
#![allow(clippy::manual_async_fn)]
#![allow(clippy::unused_async)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::if_not_else)]
#![allow(clippy::fn_params_excessive_bools)]
#![allow(clippy::case_sensitive_file_extension_comparisons)]
#![allow(clippy::missing_fields_in_debug)]
#![allow(clippy::single_match_else)]
#![allow(clippy::large_futures)]
// The kernel was extracted from a single crate where every intra-crate item
// was reachable through crate-restricted visibility. Crossing the crate
// boundary promoted those to `pub`; the root shims re-export them, so this
// public surface is an artifact of the split, not new API.
#![allow(unreachable_pub)]
#![allow(rustdoc::broken_intra_doc_links)]
#![allow(rustdoc::private_intra_doc_links)]

/// Upper bound for daemon-owned shutdown persistence work.
pub const DAEMON_SHUTDOWN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(45);
/// Grace retained for forced task abort and join during daemon shutdown.
pub const DAEMON_TASK_ABORT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

pub mod ast_grep;
pub mod background_cpu;
pub mod branch;
pub mod branch_meta;
pub mod cancellation;
pub mod config;
pub mod db;
pub mod git;
pub mod git_discovery;
pub mod git_repository;
pub mod lifecycle_lease;
pub mod logging;
pub mod operation_task_owner;
pub mod os_str_bytes;
pub mod path_safety;
pub mod path_scope;
mod profiled_lock;
pub mod resident_memory;
pub mod runtime_identity;
pub mod shard_runtime;
pub mod sqlite_read_snapshot;
pub mod storage;
pub mod store_telemetry;
pub mod sync;
pub mod text;
pub mod timeutil;
pub mod tracedecay;
pub mod weak_registry;
pub use operation_task_owner::RuntimeOperationTaskOwnerV1;
#[cfg(windows)]
pub use tracedecay_private_fs::windows as windows_security;
pub mod worktree;

/// Ports the kernel exposes so the root crate can inject subsystems that stay
/// above it (daemon store runtimes, the registered global database, the
/// session registry).
pub mod ports;
