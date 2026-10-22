use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tracedecay_domain::SessionId;

pub type SessionTemporalRefreshWakeFuture<'a> = Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

/// Wake and observe the session-temporal refresh worker bound to one store.
pub trait SessionTemporalRefreshWakePort: Send + Sync {
    fn wake(&self) -> bool;
    fn is_unavailable(&self) -> bool;
    /// Join temporal publication caused by this wake. Background history and
    /// summary convergence may continue after the projection has committed.
    fn wake_and_wait_for_projection(
        &self,
        timeout: Duration,
    ) -> SessionTemporalRefreshWakeFuture<'_>;

    /// Join the named session's committed temporal publication. Owners that
    /// cannot prove a session frontier retain the stricter store-wide join.
    fn wake_and_wait_for_session_projection<'a>(
        &'a self,
        _session_id: &'a SessionId,
        timeout: Duration,
    ) -> SessionTemporalRefreshWakeFuture<'a> {
        self.wake_and_wait_for_projection(timeout)
    }
}

/// Typed missing-worker implementation for unmounted servers and tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableSessionTemporalRefreshWake;

impl SessionTemporalRefreshWakePort for UnavailableSessionTemporalRefreshWake {
    fn wake(&self) -> bool {
        false
    }

    fn is_unavailable(&self) -> bool {
        true
    }

    fn wake_and_wait_for_projection(
        &self,
        _timeout: Duration,
    ) -> SessionTemporalRefreshWakeFuture<'_> {
        Box::pin(async { false })
    }
}
