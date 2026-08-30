//! Construction ports that need MCP-adjacent types the application crate
//! cannot name.
//!
//! Profile identity, refresh wake, remote status, and hook-orchestration
//! admission live in `tracedecay-application`. Lifecycle observation lives
//! in [`crate::lifecycle`]. Invocation execution is
//! `tracedecay_application::ApplicationInvocationExecutor` (protocol
//! extension: `tracedecay_daemon_protocol::DaemonInvocationExecutor`).
//! The composition root implements every port over live daemon authorities.

use tracedecay_domain::ManifestDigest;
use tracedecay_usecases::context::ResolvedSessionIdentity;

/// Durable lease identity for one authenticated profile-retained connection.
///
/// Handlers that also need `DaemonSessionRuntimeRegistryV1` stay in the
/// composition root behind the dispatch seam and read these fields through
/// this port rather than naming the daemon lease type.
pub trait ProfileRetainedLeasePort: Send + Sync {
    fn session_identity(&self) -> &ResolvedSessionIdentity;
    fn configuration_digest(&self) -> &ManifestDigest;
}
