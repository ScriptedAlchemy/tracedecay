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
    /// A session target joins only that session; `None` joins a batch source
    /// scan across the store.
    fn wake_and_wait_for_projection(
        &self,
        session_id: Option<SessionId>,
        timeout: Duration,
    ) -> SessionTemporalRefreshWakeFuture<'_>;
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
        _session_id: Option<SessionId>,
        _timeout: Duration,
    ) -> SessionTemporalRefreshWakeFuture<'_> {
        Box::pin(async { false })
    }
}
