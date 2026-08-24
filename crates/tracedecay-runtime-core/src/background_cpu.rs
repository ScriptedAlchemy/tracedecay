//! Process-wide weighted admission for background CPU work.
//!
//! The authority counts active CPU units rather than owning an executor. Code
//! indexing, semantic native threads, and session preparation can therefore
//! use their existing execution substrates while sharing one hard process
//! ceiling. FIFO waiter order prevents a continuously busy class from starving
//! another class, and RAII releases capacity on success, cancellation, or
//! unwind.

use std::cell::Cell;
use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

#[derive(Debug)]
struct BackgroundCpuWaiterV1 {
    units: usize,
}

#[derive(Default)]
struct BackgroundCpuStateV1 {
    active_units: usize,
    waiters: VecDeque<Arc<BackgroundCpuWaiterV1>>,
}

/// One process-wide background CPU budget shared across subsystems.
pub struct ProcessBackgroundCpuV1 {
    width: NonZeroUsize,
    state: Mutex<BackgroundCpuStateV1>,
    available: Condvar,
}

impl fmt::Debug for ProcessBackgroundCpuV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessBackgroundCpuV1")
            .field("width", &self.width)
            .field("active_units", &self.active_units())
            .finish_non_exhaustive()
    }
}

thread_local! {
    static BACKGROUND_CPU_DEPTH: Cell<usize> = const { Cell::new(0) };
    static BACKGROUND_CPU_UNITS: Cell<usize> = const { Cell::new(0) };
}

struct BackgroundCpuScopeV1;

impl BackgroundCpuScopeV1 {
    fn enter() -> Self {
        BACKGROUND_CPU_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
        Self
    }
}

struct YieldedBackgroundCpuV1<'a> {
    authority: &'a Arc<ProcessBackgroundCpuV1>,
    units: usize,
    depth: usize,
}

impl Drop for YieldedBackgroundCpuV1<'_> {
    fn drop(&mut self) {
        self.authority.admit_units(self.units);
        BACKGROUND_CPU_UNITS.with(|units| units.set(self.units));
        BACKGROUND_CPU_DEPTH.with(|depth| depth.set(self.depth));
    }
}

impl Drop for BackgroundCpuScopeV1 {
    fn drop(&mut self) {
        BACKGROUND_CPU_DEPTH.with(|depth| {
            let remaining = depth.get().saturating_sub(1);
            depth.set(remaining);
            if remaining == 0 {
                BACKGROUND_CPU_UNITS.with(|units| units.set(0));
            }
        });
    }
}

impl ProcessBackgroundCpuV1 {
    fn new(width: NonZeroUsize) -> Self {
        Self {
            width,
            state: Mutex::new(BackgroundCpuStateV1::default()),
            available: Condvar::new(),
        }
    }

    #[must_use]
    pub const fn width(&self) -> NonZeroUsize {
        self.width
    }

    #[must_use]
    pub fn active_units(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active_units
    }

    #[must_use]
    pub fn waiting_work_units(&self) -> usize {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        waiting_units(&state)
    }

    /// Acquire one CPU unit, waiting in FIFO order when the process budget is
    /// full. The returned guard must remain alive for the active work unit.
    pub fn acquire(self: &Arc<Self>) -> BackgroundCpuPermitV1 {
        self.acquire_units(1)
    }

    /// Acquire one CPU unit only when no earlier waiter exists and capacity is
    /// immediately available.
    pub fn try_acquire(self: &Arc<Self>) -> Option<BackgroundCpuPermitV1> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.waiters.is_empty() || state.active_units >= self.width.get() {
            return None;
        }
        state.active_units += 1;
        record_state(&state, self.width);
        Some(BackgroundCpuPermitV1 {
            authority: Arc::clone(self),
            units: 1,
        })
    }

    /// Run one active work unit under the process budget. Nested work on the
    /// same thread reuses a sufficient parent admission instead of waiting on
    /// itself.
    pub fn with_permit<R>(self: &Arc<Self>, operation: impl FnOnce() -> R) -> R {
        self.with_permits(1, operation)
    }

    /// Run a weighted work unit, clamped to the entire process width. Semantic
    /// inference uses its native intra-op thread count as the weight; ordinary
    /// index/session preparation uses one.
    pub fn with_permits<R>(
        self: &Arc<Self>,
        requested_units: usize,
        operation: impl FnOnce() -> R,
    ) -> R {
        let units = requested_units.max(1).min(self.width.get());
        let active_units = BACKGROUND_CPU_UNITS.with(Cell::get);
        if active_units >= units {
            return operation();
        }
        if active_units > 0 {
            let depth = BACKGROUND_CPU_DEPTH.with(Cell::get);
            self.release(active_units);
            BACKGROUND_CPU_UNITS.with(|active| active.set(0));
            BACKGROUND_CPU_DEPTH.with(|active| active.set(0));
            let _restore = YieldedBackgroundCpuV1 {
                authority: self,
                units: active_units,
                depth,
            };
            let _permit = self.acquire_units(units);
            let _scope = BackgroundCpuScopeV1::enter();
            BACKGROUND_CPU_UNITS.with(|active| active.set(units));
            return operation();
        }
        let _permit = self.acquire_units(units);
        let _scope = BackgroundCpuScopeV1::enter();
        BACKGROUND_CPU_UNITS.with(|active| active.set(units));
        operation()
    }

    /// Temporarily yield the caller's active units while a nested executor
    /// fans out independently admitted leaf work. This prevents a parent
    /// Rayon worker from holding capacity while it waits for child workers,
    /// including a full-width weighted child. Capacity is reacquired before
    /// the parent resumes, including during unwind.
    pub fn with_yielded_permits<R>(self: &Arc<Self>, operation: impl FnOnce() -> R) -> R {
        let units = BACKGROUND_CPU_UNITS.with(Cell::get);
        if units == 0 {
            return operation();
        }
        let depth = BACKGROUND_CPU_DEPTH.with(Cell::get);
        self.release(units);
        BACKGROUND_CPU_UNITS.with(|active| active.set(0));
        BACKGROUND_CPU_DEPTH.with(|active| active.set(0));
        let _restore = YieldedBackgroundCpuV1 {
            authority: self,
            units,
            depth,
        };
        operation()
    }

    fn acquire_units(self: &Arc<Self>, units: usize) -> BackgroundCpuPermitV1 {
        self.admit_units(units);
        BackgroundCpuPermitV1 {
            authority: Arc::clone(self),
            units,
        }
    }

    fn admit_units(&self, units: usize) {
        let waiter = Arc::new(BackgroundCpuWaiterV1 { units });
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.waiters.push_back(Arc::clone(&waiter));
        record_state(&state, self.width);
        loop {
            let is_front = state
                .waiters
                .front()
                .is_some_and(|front| Arc::ptr_eq(front, &waiter));
            if is_front && state.active_units.saturating_add(waiter.units) <= self.width.get() {
                state.waiters.pop_front();
                state.active_units += waiter.units;
                record_state(&state, self.width);
                self.available.notify_all();
                return;
            }
            state = self
                .available
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn release(&self, units: usize) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(state.active_units >= units);
        state.active_units = state.active_units.saturating_sub(units);
        record_state(&state, self.width);
        self.available.notify_all();
    }
}

fn record_state(state: &BackgroundCpuStateV1, width: NonZeroUsize) {
    hotpath::gauge!("runtime_core.background_cpu.width").set(width.get());
    hotpath::gauge!("runtime_core.background_cpu.active_units").set(state.active_units);
    hotpath::gauge!("runtime_core.background_cpu.waiting_work_units").set(waiting_units(state));
}

fn waiting_units(state: &BackgroundCpuStateV1) -> usize {
    state
        .waiters
        .iter()
        .fold(0usize, |total, waiter| total.saturating_add(waiter.units))
}

/// RAII ownership of active CPU capacity. Dropping it is cancellation-safe and
/// releases the exact acquired weight.
pub struct BackgroundCpuPermitV1 {
    authority: Arc<ProcessBackgroundCpuV1>,
    units: usize,
}

impl fmt::Debug for BackgroundCpuPermitV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackgroundCpuPermitV1")
            .field("units", &self.units)
            .finish_non_exhaustive()
    }
}

impl Drop for BackgroundCpuPermitV1 {
    fn drop(&mut self) {
        self.authority.release(self.units);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BackgroundCpuInstallErrorV1 {
    #[error(
        "background CPU authority is already installed at width {installed_width}, not requested width {requested_width}"
    )]
    ConflictingWidth {
        installed_width: usize,
        requested_width: usize,
    },
    #[error("background CPU authority installation did not settle")]
    InstallationDidNotSettle,
}

static PROCESS_BACKGROUND_CPU: OnceLock<Arc<ProcessBackgroundCpuV1>> = OnceLock::new();

/// Install or idempotently reuse the one process background CPU authority.
pub fn install_process_background_cpu(
    width: NonZeroUsize,
) -> Result<Arc<ProcessBackgroundCpuV1>, BackgroundCpuInstallErrorV1> {
    if let Some(installed) = PROCESS_BACKGROUND_CPU.get() {
        return compare_installed_width(installed, width);
    }
    let requested = Arc::new(ProcessBackgroundCpuV1::new(width));
    match PROCESS_BACKGROUND_CPU.set(Arc::clone(&requested)) {
        Ok(()) => Ok(requested),
        Err(_) => PROCESS_BACKGROUND_CPU.get().map_or_else(
            || Err(BackgroundCpuInstallErrorV1::InstallationDidNotSettle),
            |installed| compare_installed_width(installed, width),
        ),
    }
}

fn compare_installed_width(
    installed: &Arc<ProcessBackgroundCpuV1>,
    requested: NonZeroUsize,
) -> Result<Arc<ProcessBackgroundCpuV1>, BackgroundCpuInstallErrorV1> {
    if installed.width == requested {
        Ok(Arc::clone(installed))
    } else {
        Err(BackgroundCpuInstallErrorV1::ConflictingWidth {
            installed_width: installed.width.get(),
            requested_width: requested.get(),
        })
    }
}

/// Installed process authority, or `None` before daemon worker-plan admission.
#[must_use]
pub fn process_background_cpu() -> Option<Arc<ProcessBackgroundCpuV1>> {
    PROCESS_BACKGROUND_CPU.get().map(Arc::clone)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    use super::*;

    #[test]
    fn combined_classes_never_exceed_width_and_both_progress() {
        let authority = Arc::new(ProcessBackgroundCpuV1::new(
            NonZeroUsize::new(4).expect("nonzero width"),
        ));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let index_completed = Arc::new(AtomicUsize::new(0));
        let session_completed = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(17));
        let mut workers = Vec::new();
        for ordinal in 0..16 {
            let authority = Arc::clone(&authority);
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            let index_completed = Arc::clone(&index_completed);
            let session_completed = Arc::clone(&session_completed);
            let start = Arc::clone(&start);
            workers.push(std::thread::spawn(move || {
                start.wait();
                authority.with_permit(|| {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(current, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(5));
                    active.fetch_sub(1, Ordering::SeqCst);
                    if ordinal % 2 == 0 {
                        index_completed.fetch_add(1, Ordering::SeqCst);
                    } else {
                        session_completed.fetch_add(1, Ordering::SeqCst);
                    }
                });
            }));
        }
        start.wait();
        for worker in workers {
            worker.join().expect("background worker");
        }

        assert!(maximum.load(Ordering::SeqCst) <= 4);
        assert_eq!(index_completed.load(Ordering::SeqCst), 8);
        assert_eq!(session_completed.load(Ordering::SeqCst), 8);
    }

    #[test]
    fn waiting_demand_sums_weighted_work_units() {
        let state = BackgroundCpuStateV1 {
            active_units: 4,
            waiters: VecDeque::from([
                Arc::new(BackgroundCpuWaiterV1 { units: 4 }),
                Arc::new(BackgroundCpuWaiterV1 { units: 1 }),
            ]),
        };

        assert_eq!(waiting_units(&state), 5);
    }

    #[test]
    fn weighted_units_and_nested_work_share_one_width() {
        let authority = Arc::new(ProcessBackgroundCpuV1::new(
            NonZeroUsize::new(4).expect("nonzero width"),
        ));
        authority.with_permits(4, || {
            assert_eq!(authority.active_units(), 4);
            authority.with_permit(|| assert_eq!(authority.active_units(), 4));
        });
        authority.with_permit(|| {
            assert_eq!(authority.active_units(), 1);
            authority.with_permits(4, || assert_eq!(authority.active_units(), 4));
            assert_eq!(authority.active_units(), 1);
        });
        assert_eq!(authority.active_units(), 0);
    }

    #[test]
    fn nested_executor_yields_parent_units_and_reacquires_them() {
        let authority = Arc::new(ProcessBackgroundCpuV1::new(
            NonZeroUsize::new(4).expect("nonzero width"),
        ));
        authority.with_permit(|| {
            assert_eq!(authority.active_units(), 1);
            authority.with_yielded_permits(|| {
                assert_eq!(authority.active_units(), 0);
                authority.with_permits(4, || assert_eq!(authority.active_units(), 4));
            });
            assert_eq!(authority.active_units(), 1);
        });
        assert_eq!(authority.active_units(), 0);
    }

    #[test]
    fn panic_and_cancellation_drop_release_every_unit() {
        let authority = Arc::new(ProcessBackgroundCpuV1::new(
            NonZeroUsize::new(2).expect("nonzero width"),
        ));
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            authority.with_permits(2, || panic!("injected background panic"));
        }));
        assert!(panic.is_err());
        assert_eq!(authority.active_units(), 0);

        let nested_panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            authority.with_permit(|| {
                authority.with_yielded_permits(|| panic!("injected nested executor panic"));
            });
        }));
        assert!(nested_panic.is_err());
        assert_eq!(authority.active_units(), 0);

        let cancelled = authority.acquire();
        assert_eq!(authority.active_units(), 1);
        drop(cancelled);
        assert_eq!(authority.active_units(), 0);
        assert!(authority.try_acquire().is_some());
    }
}
