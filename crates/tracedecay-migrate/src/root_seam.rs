//! The upward seam this package still depends on.
//!
//! The migration subsystem was moved out of the root `tracedecay` crate before
//! the subsystems it sits on finished their own extraction. Every moved
//! reference to one of those root modules was rewritten to
//! `crate::root_seam::<module>` so the whole coupling surface is one greppable
//! name.
//!
//! **The kernel half is closed.** `storage`, `db`, `sqlite_read_snapshot`,
//! `errors`, `lifecycle_lease`, `config`, `branch`, `branch_meta`, `memory`,
//! `open_store_holders`, `worktree`, `git`, and `tracedecay::current_timestamp`
//! now resolve directly against `tracedecay_runtime_core`, so they no longer
//! come through here.
//!
//! Four modules remain — `sessions`, `daemon`, `agents`, and `application` —
//! and **none of them can be closed from inside this crate**:
//!
//! - `sessions` / `agents` — `tracedecay-sessions` and
//!   `tracedecay-agent-hosts` have not repointed at the kernel yet, so they do
//!   not compile and cannot be depended on.
//! - `daemon` — `daemon/store_runtime` has no owning crate at all, and the
//!   items needed here are types held in this crate's public API, which a
//!   registered port cannot supply.
//! - `application::host_admission` — still lives in the root crate, not in
//!   `tracedecay-application`.
//!
//! The seam is therefore left deliberately empty for exactly these four, so
//! `cargo check` keeps naming the blockers instead of hiding them behind ports
//! that nothing can implement. `SEAMS.md` next to this crate's manifest
//! catalogs all 94 remaining references by module, item, and file:line, and
//! records the recommended mechanism and owner for each.

/// The global-db slice found its owning crate: `tracedecay-global-db` no
/// longer depends on this crate (the `durability` edge moved into the
/// kernel), so the reverse dependency stopped being a cycle and the seam
/// closes against the crate's public surface. `tests::harness` rides in via
/// the `test-helpers` opt-in on the dev-dependency.
pub mod global_db {
    pub use tracedecay_global_db::*;
}

/// The daemon slice that found an owning crate: `store_runtime` now lives in
/// the kernel. `session_registry` and `profile_identity` remain root-owned
/// (their dependency closures still reach root modules), so their references
/// still fail here by design.
pub mod daemon {
    pub use tracedecay_runtime_core::store_runtime;
}

/// The sessions slice found its owning crate: the moved root tree lives at
/// `tracedecay_sessions::runtime`.
pub mod sessions {
    pub use tracedecay_sessions::runtime::{git_correlation, hermes, lcm, workflow_index};
}

/// Root-owned storage adapters (typed wrappers over the kernel's
/// `classify_registry_storage_fields`) and the root graph engine. Both stay
/// seams until their owners extract.
pub mod tracedecay_root {}
pub mod storage_adapters {}
