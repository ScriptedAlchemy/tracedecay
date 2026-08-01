//! Shared directory-fsync durability primitive for host-admission spools.
//!
//! The implementation lives in `tracedecay_application::framed_log` so hook and
//! host-admission spools share one crash-safe kernel.

pub(crate) use tracedecay_application::framed_log::{DirectorySyncPolicy, sync_directory};
