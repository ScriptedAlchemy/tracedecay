//! Root shim for the store-runtime registry that moved into the kernel.
//!
//! `src/daemon/store_runtime/` was 12.9K lines that referenced `crate::db` 59
//! times and `crate::storage` 14 times against 25 genuine `crate::daemon`
//! references, and `StoreRuntimeHandle` holds a `db::DatabaseAuthority`. It now
//! lives in `tracedecay_runtime_core::store_runtime`; this glob keeps every
//! historical `crate::daemon::store_runtime::…` path resolving.
//!
//! `session_registry` did **not** follow. It stores `Arc<RegisteredGlobalDb>`
//! in its public surface, and `tracedecay-global-db` depends on the kernel —
//! so the kernel taking that edge is a Cargo cycle. It also reaches
//! `daemon::{authority,
//! code_index_scheduler, profile_identity, transport}` and `log_daemon_event`.
//! `crates/tracedecay-runtime-core/SEAMS.md` catalogs the remainder.

pub(crate) use tracedecay_runtime_core::store_runtime::*;

pub(crate) mod session_registry;

/// Installs the root-owned registered global/session schema installer into the
/// kernel's store-runtime registry.
///
/// The schema lives in `tracedecay-global-db`, which already depends on the
/// kernel — so the kernel reaches it through
/// `tracedecay_runtime_core::ports::registered_schema` instead. The
/// port fails closed, so every path that can initialise a profile- or
/// session-scoped shard must call this first. Idempotent.
pub(crate) fn register_registered_schema_installer() {
    tracedecay_runtime_core::ports::registered_schema::register(|connection| {
        Box::pin(crate::global_db::ensure_registered_schema(connection))
    });
}

#[cfg(test)]
mod profile_paths_parity {
    /// The kernel restates the user-session filename because it cannot depend
    /// on `tracedecay-sessions` without a Cargo cycle. The root sees both, so
    /// it is the only place the two definitions can be pinned together.
    #[test]
    fn kernel_user_sessions_filename_matches_sessions_crate() {
        assert_eq!(
            tracedecay_runtime_core::store_runtime::profile_paths::USER_SESSIONS_DB_FILENAME,
            crate::sessions::USER_SESSIONS_DB_FILENAME,
        );
    }

    #[test]
    fn kernel_user_sessions_path_matches_sessions_crate() {
        let profile_root = std::path::Path::new("/profile");
        assert_eq!(
            tracedecay_runtime_core::store_runtime::profile_paths::user_sessions_db_path(
                profile_root
            ),
            crate::sessions::user_sessions_db_path(profile_root),
        );
    }
}
