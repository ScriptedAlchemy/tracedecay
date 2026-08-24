//! Opt-in Hotpath observation for the code-index kernel.
//!
//! Labels are static string literals. Timing is file-operation granularity:
//! per-node work is never measured. Tight loops are sampled (1 in 32) so a
//! generation of thousands of files does not flood the profiler. Every helper
//! is `#[inline(always)]` and compiles to a no-op when the `hotpath` feature
//! is off — atomics, clocks, and census walks must not run on the default path.

#[cfg(test)]
use std::cell::Cell;
#[cfg(not(test))]
use std::marker::PhantomData;
#[cfg(feature = "hotpath")]
use std::sync::atomic::AtomicU64;
#[cfg(any(feature = "hotpath", test))]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(feature = "hotpath")]
use std::time::Instant;

#[cfg(feature = "hotpath")]
const HOT_LOOP_SAMPLE_PERIOD: u64 = 32;

#[cfg(feature = "hotpath")]
static PENDING_WORK: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "hotpath")]
static WORKERS_ACTIVE: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "hotpath")]
static WORKERS_POOL_COORDINATION: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "hotpath")]
static GREP_FILE_SAMPLE: AtomicU64 = AtomicU64::new(0);

#[must_use]
#[inline(always)]
pub(crate) fn sample_hot_loop() -> bool {
    #[cfg(feature = "hotpath")]
    {
        GREP_FILE_SAMPLE
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(HOT_LOOP_SAMPLE_PERIOD)
    }
    #[cfg(not(feature = "hotpath"))]
    {
        false
    }
}

#[cfg(any(feature = "hotpath", test))]
#[inline(always)]
fn decrement_if_positive(counter: &AtomicUsize) -> bool {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_sub(1)
        })
        .is_ok()
}

pub(crate) struct PendingWorkQueue {
    #[cfg(any(feature = "hotpath", test))]
    remaining: AtomicUsize,
}

impl PendingWorkQueue {
    #[inline(always)]
    pub(crate) fn new(depth: usize) -> Self {
        #[cfg(feature = "hotpath")]
        {
            PENDING_WORK.fetch_add(depth, Ordering::Relaxed);
            refresh_queue_gauge();
        }
        // Both the gauge above and the `remaining` field below are gated, so a
        // plain production build reads `depth` nowhere and `-D warnings` fails
        // on it. Discard it explicitly rather than renaming the parameter,
        // which would lose the name at the two call sites that do use it.
        #[cfg(not(any(feature = "hotpath", test)))]
        let _ = depth;
        Self {
            #[cfg(any(feature = "hotpath", test))]
            remaining: AtomicUsize::new(depth),
        }
    }

    #[inline(always)]
    pub(crate) fn start_worker(&self) -> WorkerBusyGuard {
        #[cfg(any(feature = "hotpath", test))]
        let started = decrement_if_positive(&self.remaining);
        #[cfg(feature = "hotpath")]
        if started {
            let _ = decrement_if_positive(&PENDING_WORK);
            refresh_queue_gauge();
        }
        #[cfg(all(test, not(feature = "hotpath")))]
        let _ = started;
        WorkerBusyGuard::enter()
    }

    #[cfg(test)]
    fn pending_for_test(&self) -> usize {
        self.remaining.load(Ordering::Relaxed)
    }
}

impl Drop for PendingWorkQueue {
    fn drop(&mut self) {
        #[cfg(feature = "hotpath")]
        {
            let abandoned = self.remaining.swap(0, Ordering::Relaxed);
            let _ = PENDING_WORK.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_sub(abandoned))
            });
            refresh_queue_gauge();
        }
    }
}

pub(crate) struct WorkerBusyGuard {
    #[cfg(test)]
    coordinating: Cell<bool>,
}

impl WorkerBusyGuard {
    #[inline(always)]
    pub(crate) fn enter() -> Self {
        #[cfg(feature = "hotpath")]
        {
            WORKERS_ACTIVE.fetch_add(1, Ordering::Relaxed);
            refresh_worker_gauges();
        }
        Self {
            #[cfg(test)]
            coordinating: Cell::new(false),
        }
    }

    #[inline(always)]
    pub(crate) fn pool_coordination(&self) -> WorkerPoolCoordinationGuard<'_> {
        #[cfg(feature = "hotpath")]
        {
            WORKERS_POOL_COORDINATION.fetch_add(1, Ordering::Relaxed);
            refresh_worker_gauges();
        }
        #[cfg(test)]
        self.coordinating.set(true);
        WorkerPoolCoordinationGuard {
            #[cfg(not(test))]
            _worker: PhantomData,
            #[cfg(test)]
            worker: self,
            #[cfg(feature = "hotpath")]
            started: Instant::now(),
        }
    }

    #[cfg(test)]
    fn is_coordinating_for_test(&self) -> bool {
        self.coordinating.get()
    }
}

impl Drop for WorkerBusyGuard {
    fn drop(&mut self) {
        #[cfg(feature = "hotpath")]
        {
            let _ = decrement_if_positive(&WORKERS_ACTIVE);
            refresh_worker_gauges();
        }
    }
}

pub(crate) struct WorkerPoolCoordinationGuard<'a> {
    #[cfg(not(test))]
    _worker: PhantomData<&'a WorkerBusyGuard>,
    #[cfg(test)]
    worker: &'a WorkerBusyGuard,
    #[cfg(feature = "hotpath")]
    started: Instant,
}

impl Drop for WorkerPoolCoordinationGuard<'_> {
    fn drop(&mut self) {
        #[cfg(feature = "hotpath")]
        {
            let _ = decrement_if_positive(&WORKERS_POOL_COORDINATION);
            hotpath::gauge!("code_index_worker_pool_coordination_micros")
                .inc(u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX));
            refresh_worker_gauges();
        }
        #[cfg(test)]
        self.worker.coordinating.set(false);
    }
}

#[cfg(feature = "hotpath")]
fn refresh_worker_gauges() {
    let active = WORKERS_ACTIVE.load(Ordering::Relaxed);
    let coordinating = WORKERS_POOL_COORDINATION.load(Ordering::Relaxed);
    let cpu = active.saturating_sub(coordinating);
    let workers = crate::parallelism::indexing_workers();
    hotpath::gauge!("code_index_workers_busy").set(active);
    hotpath::gauge!("code_index_workers_cpu").set(cpu);
    hotpath::gauge!("code_index_workers_pool_coordination").set(coordinating);
    hotpath::gauge!("code_index_worker_count").set(workers);
    let utilization = if workers == 0 {
        0.0
    } else {
        (cpu as f64) * 100.0 / workers as f64
    };
    hotpath::gauge!("code_index_worker_utilization_pct").set(utilization);
}

#[cfg(feature = "hotpath")]
#[inline(always)]
fn refresh_queue_gauge() {
    hotpath::gauge!("code_index_queue_depth").set(PENDING_WORK.load(Ordering::Relaxed));
}

#[inline(always)]
pub(crate) fn record_files(count: usize) {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("code_index_files").set(count);
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = count;
    }
}

/// Every call site computes its byte total inside a `hotpath`-gated block, so
/// this carries the same gate instead of a body that can never run.
#[cfg(feature = "hotpath")]
#[inline(always)]
pub(crate) fn record_source_bytes(bytes: u64) {
    hotpath::gauge!("code_index_source_bytes").set(bytes);
}

#[inline(always)]
pub(crate) fn add_parse_bytes(bytes: u64) {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("code_index_parse_bytes").inc(bytes);
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = bytes;
    }
}

#[inline(always)]
pub(crate) fn add_reused_parses(count: u64) {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("code_index_reused_parses").inc(count);
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = count;
    }
}

/// Gated with its call site, which reads generation statistics only when
/// profiling is on.
#[cfg(feature = "hotpath")]
#[inline(always)]
pub(crate) fn record_symbols(count: u64) {
    hotpath::gauge!("code_index_symbols").set(count);
}

/// Gated with its call site, which reads generation statistics only when
/// profiling is on.
#[cfg(feature = "hotpath")]
#[inline(always)]
pub(crate) fn record_relations(count: u64) {
    hotpath::gauge!("code_index_relations").set(count);
}

#[inline(always)]
pub(crate) fn record_pages(count: u64) {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("code_index_pages").set(count);
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = count;
    }
}

#[inline(always)]
pub(crate) fn record_seal_bytes(bytes: u64) {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("code_index_seal_bytes").set(bytes);
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = bytes;
    }
}

pub(crate) struct TtfqStart(#[cfg(feature = "hotpath")] Instant);

#[inline(always)]
pub(crate) fn start_ttfq() -> TtfqStart {
    TtfqStart(
        #[cfg(feature = "hotpath")]
        Instant::now(),
    )
}

#[inline(always)]
pub(crate) fn record_ttfq(started: TtfqStart) {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("code_index_ttfq_micros").set(started.0.elapsed().as_micros() as f64);
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = started;
    }
}

#[inline(always)]
pub(crate) fn record_generation_state(state: &'static str) {
    #[cfg(feature = "hotpath")]
    {
        hotpath::val!("code_index_generation_state").set(&state);
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = state;
    }
}

#[inline(always)]
pub(crate) fn record_rebuild_state(state: &'static str) {
    #[cfg(feature = "hotpath")]
    {
        hotpath::val!("code_index_rebuild_state").set(&state);
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = state;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_queue_decrements_when_each_worker_starts() {
        let queue = PendingWorkQueue::new(3);
        assert_eq!(queue.pending_for_test(), 3);

        let first = queue.start_worker();
        assert_eq!(queue.pending_for_test(), 2);
        drop(first);

        let second = queue.start_worker();
        assert_eq!(queue.pending_for_test(), 1);
        drop(second);

        let third = queue.start_worker();
        assert_eq!(queue.pending_for_test(), 0);
        drop(third);
    }

    #[test]
    fn pool_coordination_leaves_cpu_stage_until_the_guard_finishes() {
        let queue = PendingWorkQueue::new(1);
        let worker = queue.start_worker();
        assert!(!worker.is_coordinating_for_test());

        let coordinating = worker.pool_coordination();
        assert!(worker.is_coordinating_for_test());

        drop(coordinating);
        assert!(!worker.is_coordinating_for_test());
    }
}
