//! Read-only native-integration status notifications for LSP clients.
//!
//! The daemon transaction coordinator remains the sole mutation authority;
//! this module carries only the bounded application status projection to an
//! already-authorized session as a server-to-client notification. No client
//! method is admitted here: the gateway cannot preflight, approve, apply, or
//! cancel a native integration, apply edits, or mutate Git through this path.

use tracedecay_application::NativeIntegrationStatusProjectionV1;

pub const TRACEDECAY_NATIVE_INTEGRATION_STATUS_METHOD: &str = "tracedecay/nativeIntegrationStatus";

/// The most recent status projections one session flush may forward.
pub(crate) const MAX_NATIVE_INTEGRATION_STATUS_PER_POLL: usize = 16;

/// Bytes reserved on the outbound queue before a status flush runs.
pub(crate) const MAX_NATIVE_INTEGRATION_STATUS_BYTES: usize = 16 * 1024;

/// Daemon-owned read of recently observed native-integration transaction
/// statuses. Implementations return current bounded projections; each session
/// dedupes what it already forwarded, so re-returning an unchanged status is
/// harmless and never re-notifies.
pub trait NativeIntegrationStatusPort: Send + Sync {
    fn poll_status(&self, maximum: usize) -> Vec<NativeIntegrationStatusProjectionV1>;
}
