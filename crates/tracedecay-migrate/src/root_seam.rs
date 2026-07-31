//! The runtime kernel seam this package still depends on.
//!
//! The migration subsystem was moved out of the root `tracedecay` crate before
//! the runtime kernel it sits on (`db`, `storage`, `errors`,
//! `sqlite_read_snapshot`, `lifecycle_lease`, `global_db`, `sessions`,
//! `config`, `daemon`, `branch`, `branch_meta`, `memory`, `worktree`, `git`,
//! `application`, `agents`, `open_store_holders`, `tracedecay`) finished its
//! own extraction into `tracedecay-runtime-core`. Every moved reference to one
//! of those root modules was rewritten to `crate::root_seam::<module>` so the
//! whole coupling surface is one greppable name.
//!
//! At integration the lead repoints this module at the kernel crate — the
//! intended shape is a set of re-exports such as:
//!
//! ```ignore
//! pub use tracedecay_runtime_core::{
//!     agents, application, branch, branch_meta, config, daemon, db, errors, git, global_db,
//!     lifecycle_lease, memory, open_store_holders, sessions, sqlite_read_snapshot, storage,
//!     tracedecay, worktree,
//! };
//! ```
//!
//! Until that lands the seam is deliberately empty, so `cargo check` reports
//! exactly which kernel items the migration subsystem needs. `SEAMS.md` in this
//! crate catalogs them by file and line.
