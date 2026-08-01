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
//! Five modules remain — `global_db`, `sessions`, `daemon`, `agents`, and
//! `application` — and **none of them can be closed from inside this crate**:
//!
//! - `global_db` — `tracedecay-global-db` already depends on this crate, so
//!   taking the reverse dependency is a Cargo cycle.
//! - `sessions` / `agents` — `tracedecay-sessions` and
//!   `tracedecay-agent-hosts` have not repointed at the kernel yet, so they do
//!   not compile and cannot be depended on.
//! - `daemon` — `daemon/store_runtime` has no owning crate at all, and the
//!   items needed here are types held in this crate's public API, which a
//!   registered port cannot supply.
//! - `application::host_admission` — still lives in the root crate, not in
//!   `tracedecay-application`.
//!
//! The seam is therefore left deliberately empty for exactly these five, so
//! `cargo check` keeps naming the blockers instead of hiding them behind ports
//! that nothing can implement. `SEAMS.md` next to this crate's manifest
//! catalogs all 94 remaining references by module, item, and file:line, and
//! records the recommended mechanism and owner for each.
