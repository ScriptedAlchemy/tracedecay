//! The clock behind every exact SQL deadline: statement execution and the
//! transaction idle and absolute leases.
//!
//! Production reads the monotonic wall clock. Under `cfg(test)` every thread
//! reads a [`FakeClock`] that stands still until a test moves it: expiry tests
//! drive the production limits deterministically, and no other test can
//! expire because the machine running it was loaded.

#[cfg(not(test))]
pub(super) fn now() -> std::time::Instant {
    std::time::Instant::now()
}

#[cfg(test)]
pub(super) use fake::now;

#[cfg(test)]
pub(super) mod fake {
    use std::{
        cell::RefCell,
        sync::{Arc, Mutex, PoisonError},
        time::{Duration, Instant},
    };

    thread_local! {
        static INSTALLED: RefCell<Option<Arc<FakeClock>>> = const { RefCell::new(None) };
    }

    /// This thread's installed clock. A thread with none gets a frozen one,
    /// so no test's exact SQL deadline passes because the machine was slow.
    pub(in crate::exact_sql) fn now() -> Instant {
        INSTALLED.with(|installed| {
            installed
                .borrow_mut()
                .get_or_insert_with(FakeClock::frozen)
                .read()
        })
    }

    /// A shared clock that moves only when a test moves it, or by a fixed
    /// tick on every read.
    pub(in crate::exact_sql) struct FakeClock {
        started: Instant,
        now: Mutex<Instant>,
        tick: Duration,
    }

    impl FakeClock {
        pub(in crate::exact_sql) fn frozen() -> Arc<Self> {
            Self::ticking(Duration::ZERO)
        }

        /// Advances by `tick` after every read, so a statement's progress
        /// callbacks move it with no other thread involved.
        pub(in crate::exact_sql) fn ticking(tick: Duration) -> Arc<Self> {
            let started = Instant::now();
            Arc::new(Self {
                started,
                now: Mutex::new(started),
                tick,
            })
        }

        /// Makes every exact SQL deadline computed or checked on the calling
        /// thread from here on read this clock.
        pub(in crate::exact_sql) fn install(self: &Arc<Self>) {
            INSTALLED.with(|installed| *installed.borrow_mut() = Some(Arc::clone(self)));
        }

        pub(in crate::exact_sql) fn advance(&self, by: Duration) {
            *self.now.lock().unwrap_or_else(PoisonError::into_inner) += by;
        }

        pub(in crate::exact_sql) fn elapsed(&self) -> Duration {
            self.now
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .duration_since(self.started)
        }

        fn read(&self) -> Instant {
            let mut now = self.now.lock().unwrap_or_else(PoisonError::into_inner);
            let read = *now;
            *now += self.tick;
            read
        }
    }
}
