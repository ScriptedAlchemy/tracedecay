//! Installs the registered global/session schema into the kernel store-runtime
//! registry.
//!
//! The schema lives in `tracedecay-global-db`, which already depends on the
//! kernel — so the kernel reaches it through
//! `tracedecay_runtime_core::ports::registered_schema` instead. The
//! port fails closed, so every path that can initialise a profile- or
//! session-scoped shard must call this first. Idempotent.

/// Installs the registered global/session schema installer into the kernel's
/// store-runtime registry.
pub fn register_registered_schema_installer() {
    tracedecay_runtime_core::ports::registered_schema::register(|connection| {
        Box::pin(tracedecay_global_db::ensure_registered_schema(connection))
    });
}
