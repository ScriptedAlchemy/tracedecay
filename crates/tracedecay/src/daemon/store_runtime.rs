//! Re-export of the kernel store-runtime registry, plus the daemon-owned
//! session registry that cannot move into the kernel.
//!
//! `session_registry` stays here because it stores `RegisteredGlobalDbLeaseV1`
//! on its public surface, and `tracedecay-global-db` already depends on the
//! kernel — taking that edge would be a Cargo cycle. It also reaches
//! `daemon::transport`, `log_daemon_event`, and
//! `tracedecay_daemon_identity::{authority, profile_identity}`.
//! See `tracedecay_runtime_core`'s crate-level doc.

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
        Box::pin(tracedecay_global_db::ensure_registered_schema(connection))
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
            tracedecay_sessions::runtime::USER_SESSIONS_DB_FILENAME,
        );
    }

    #[test]
    fn kernel_user_sessions_path_matches_sessions_crate() {
        let profile_root = std::path::Path::new("/profile");
        assert_eq!(
            tracedecay_runtime_core::store_runtime::profile_paths::user_sessions_db_path(
                profile_root
            ),
            tracedecay_sessions::runtime::user_sessions_db_path(profile_root),
        );
    }
}
