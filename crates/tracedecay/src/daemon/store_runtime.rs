//! Kernel store-runtime re-export kept at the composition root.
//!
//! The session registry now lives in `tracedecay-store-runtime`. This module
//! still forwards the kernel `store_runtime` surface (`registry`, `resolver`,
//! `telemetry`, `profile_paths`) so existing daemon callers of
//! `crate::daemon::store_runtime::registry` keep a single path. See
//! `tracedecay_runtime_core`'s crate-level doc.

pub(crate) use tracedecay_runtime_core::store_runtime::*;

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
