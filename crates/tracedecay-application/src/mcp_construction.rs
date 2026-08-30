//! Ports consumed by MCP server construction.
//!
//! These traits name the daemon-private capabilities the MCP server and
//! handlers read without importing root daemon types. The composition root
//! implements each port over its live authorities.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use tracedecay_domain::{BrainId, UserProfileId};

/// Durable profile identity the MCP server reads for route and hook binding.
pub trait ProfileIdentityReadPort: Send + Sync {
    fn profile_root(&self) -> &Path;
    fn brain_id(&self) -> &BrainId;
    fn profile_id(&self) -> &UserProfileId;
}

pub type SessionTemporalRefreshWakeFuture<'a> = Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

/// Wake and observe the session-temporal refresh worker bound to one store.
///
/// `wake` returns `false` when the route has no live target. `is_unavailable`
/// is the typed missing-worker / stalled-worker state, never an empty success.
pub trait SessionTemporalRefreshWakePort: Send + Sync {
    fn wake(&self) -> bool;
    fn is_unavailable(&self) -> bool;
    fn wake_and_wait_until_idle(&self, timeout: Duration) -> SessionTemporalRefreshWakeFuture<'_>;
}

/// Refresh wake that never has a live worker. Tests and unmounted servers
/// install this instead of fabricating a daemon scheduler.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableSessionTemporalRefreshWake;

impl SessionTemporalRefreshWakePort for UnavailableSessionTemporalRefreshWake {
    fn wake(&self) -> bool {
        false
    }

    fn is_unavailable(&self) -> bool {
        true
    }

    fn wake_and_wait_until_idle(&self, _timeout: Duration) -> SessionTemporalRefreshWakeFuture<'_> {
        Box::pin(async { false })
    }
}
