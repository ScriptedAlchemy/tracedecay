//! Root shim for the kernel `memory` module.
//!
//! The implementation moved to `tracedecay_runtime_core::memory` in the one-shot
//! crate split. This glob keeps every historical `crate::memory::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::memory::*;

/// Profile-level durable memory for conversations without a code project.
///
/// Everything except the registry opener moved into the kernel; that one
/// adapter borrows `daemon::store_runtime::session_registry`, which sits above
/// the kernel, so it stays here and shadows the kernel's `user` module.
pub mod user {
    pub use tracedecay_runtime_core::memory::user::*;

    use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
    use crate::db::Database;
    use crate::errors::Result;

    pub(crate) async fn open_user_memory_db(
        registry: &DaemonSessionRuntimeRegistryV1,
    ) -> Result<Database> {
        registry
            .profile_memory()
            .await
            .map(|database| database.as_ref().clone())
    }
}
