use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

pub type SessionTemporalRefreshWakeFuture<'a> = Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

/// Wake and observe the session-temporal refresh worker bound to one store.
pub trait SessionTemporalRefreshWakePort: Send + Sync {
    fn wake(&self) -> bool;
    fn is_unavailable(&self) -> bool;
    fn wake_and_wait_until_idle(&self, timeout: Duration) -> SessionTemporalRefreshWakeFuture<'_>;
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

    fn wake_and_wait_until_idle(&self, _timeout: Duration) -> SessionTemporalRefreshWakeFuture<'_> {
        Box::pin(async { false })
    }
}
