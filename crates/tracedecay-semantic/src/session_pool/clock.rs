use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Monotonic time source for reaping, acquisition, and model-load deadlines.
pub trait MonotonicClock: Send + Sync {
    fn now(&self) -> Duration;
}

/// Wall-clock driver for production wiring.
#[derive(Debug)]
pub struct SystemMonotonicClock {
    start: Instant,
}

impl Default for SystemMonotonicClock {
    fn default() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl MonotonicClock for SystemMonotonicClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
}

/// Deterministic test clock; advances only when told to.
#[derive(Debug, Default)]
pub struct ManualClock {
    micros: AtomicU64,
}

impl ManualClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn advance(&self, delta: Duration) {
        self.micros
            .fetch_add(delta.as_micros() as u64, Ordering::SeqCst);
    }
}

impl MonotonicClock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_micros(self.micros.load(Ordering::SeqCst))
    }
}

impl<C> MonotonicClock for Arc<C>
where
    C: MonotonicClock + ?Sized,
{
    fn now(&self) -> Duration {
        self.as_ref().now()
    }
}
