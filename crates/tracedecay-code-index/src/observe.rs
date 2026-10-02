//! Observation for the code-index kernel.
//!
//! Labels are static string literals. Timing is file-operation granularity:
//! per-node work is never measured. Tight loops are sampled (1 in 32) so a
//! generation of thousands of files does not flood the profiler. Every helper
//! is `#[inline(always)]` so the measurement seam stays cheap on paths that
//! run per file.

#[cfg(test)]
use std::cell::Cell;
#[cfg(not(test))]
use std::marker::PhantomData;

use std::sync::atomic::AtomicU64;

use std::sync::atomic::{AtomicUsize, Ordering};

use std::time::Instant;

const HOT_LOOP_SAMPLE_PERIOD: u64 = 32;

static PENDING_WORK: AtomicUsize = AtomicUsize::new(0);

static WORKERS_ACTIVE: AtomicUsize = AtomicUsize::new(0);

static WORKERS_POOL_COORDINATION: AtomicUsize = AtomicUsize::new(0);

static HOT_LOOP_SAMPLE: AtomicU64 = AtomicU64::new(0);

#[must_use]
#[inline(always)]
fn is_hot_loop_sample(sequence: u64) -> bool {
    sequence.is_multiple_of(HOT_LOOP_SAMPLE_PERIOD)
}

#[must_use]
#[inline(always)]
pub(crate) fn sample_hot_loop() -> bool {
    is_hot_loop_sample(HOT_LOOP_SAMPLE.fetch_add(1, Ordering::Relaxed))
}

/// Measure one fixed-rate sample from a hot file loop.
///
/// The label must be a literal so callers cannot create path- or
/// generation-shaped cardinality. The disabled path still evaluates only the
/// work expression; its sampler is an inlined constant `false` with no atomic
/// or clock access.
macro_rules! measure_hot_loop {
    ($label:literal, $work:expr) => {{
        if $crate::observe::sample_hot_loop() {
            {
                let _span = tracing::trace_span!($label).entered();
                $work
            }
        } else {
            $work
        }
    }};
}

pub(crate) use measure_hot_loop;

#[inline(always)]
fn decrement_if_positive(counter: &AtomicUsize) -> bool {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_sub(1)
        })
        .is_ok()
}

pub(crate) struct PendingWorkQueue {
    remaining: AtomicUsize,
}

impl PendingWorkQueue {
    #[inline(always)]
    pub(crate) fn new(depth: usize) -> Self {
        {
            PENDING_WORK.fetch_add(depth, Ordering::Relaxed);
            refresh_queue_gauge();
        }
        // Both the gauge above and the `remaining` field below are gated, so a
        // plain production build reads `depth` nowhere and `-D warnings` fails
        // on it. Discard it explicitly rather than renaming the parameter,
        // which would lose the name at the two call sites that do use it.

        Self {
            remaining: AtomicUsize::new(depth),
        }
    }

    #[inline(always)]
    pub(crate) fn start_worker(&self) -> WorkerBusyGuard {
        let started = decrement_if_positive(&self.remaining);

        if started {
            let _ = decrement_if_positive(&PENDING_WORK);
            refresh_queue_gauge();
        }

        WorkerBusyGuard::enter()
    }
}

impl Drop for PendingWorkQueue {
    fn drop(&mut self) {
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

            started: Instant::now(),
        }
    }
}

impl Drop for WorkerBusyGuard {
    fn drop(&mut self) {
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

    started: Instant,
}

impl Drop for WorkerPoolCoordinationGuard<'_> {
    fn drop(&mut self) {
        {
            let _ = decrement_if_positive(&WORKERS_POOL_COORDINATION);
            metrics::gauge!("code_index_worker_pool_coordination_micros").increment(
                (u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX)) as f64,
            );
            refresh_worker_gauges();
        }
        #[cfg(test)]
        self.worker.coordinating.set(false);
    }
}

fn refresh_worker_gauges() {
    let active = WORKERS_ACTIVE.load(Ordering::Relaxed);
    let coordinating = WORKERS_POOL_COORDINATION.load(Ordering::Relaxed);
    let cpu = active.saturating_sub(coordinating);
    let workers = crate::parallelism::indexing_workers();
    metrics::gauge!("code_index_workers_busy").set((active) as f64);
    metrics::gauge!("code_index_workers_cpu").set((cpu) as f64);
    metrics::gauge!("code_index_workers_pool_coordination").set((coordinating) as f64);
    metrics::gauge!("code_index_worker_count").set((workers) as f64);
    let utilization = if workers == 0 {
        0.0
    } else {
        (cpu as f64) * 100.0 / workers as f64
    };
    metrics::gauge!("code_index_worker_utilization_pct").set(utilization);
}

#[inline(always)]
fn refresh_queue_gauge() {
    metrics::gauge!("code_index_queue_depth").set((PENDING_WORK.load(Ordering::Relaxed)) as f64);
}

#[inline(always)]
pub(crate) fn record_files(count: usize) {
    {
        metrics::gauge!("code_index_files").set((count) as f64);
    }
}

/// Every call site computes its byte total inside a metrics block, so this
/// helper carries the total instead of re-walking sources.
#[inline(always)]
pub(crate) fn record_source_bytes(bytes: u64) {
    metrics::gauge!("code_index_source_bytes").set((bytes) as f64);
}

#[inline(always)]
pub(crate) fn add_parse_bytes(bytes: u64) {
    {
        metrics::gauge!("code_index_parse_bytes").increment((bytes) as f64);
    }
}

#[inline(always)]
pub(crate) fn add_reused_parses(count: u64) {
    {
        metrics::gauge!("code_index_reused_parses").increment((count) as f64);
    }
}

/// Gated with its call site, which reads generation statistics only when
/// profiling is on.
#[inline(always)]
pub(crate) fn record_symbols(count: u64) {
    metrics::gauge!("code_index_symbols").set((count) as f64);
}

/// Gated with its call site, which reads generation statistics only when
/// profiling is on.
#[inline(always)]
pub(crate) fn record_relations(count: u64) {
    metrics::gauge!("code_index_relations").set((count) as f64);
}

#[inline(always)]
pub(crate) fn record_pages(count: u64) {
    {
        metrics::gauge!("code_index_pages").set((count) as f64);
    }
}

/// Start of one production-owner generation build. The matching observation
/// ends only after the immutable generation has been published and is
/// queryable through that owner; daemon scheduling/wake latency is measured
/// separately by the reconcile cadence receipt.
pub(crate) struct BuildToQueryableStart(Instant);

#[inline(always)]
pub(crate) fn start_build_to_queryable() -> BuildToQueryableStart {
    BuildToQueryableStart(Instant::now())
}

#[inline(always)]
pub(crate) fn record_build_to_queryable(started: BuildToQueryableStart) {
    {
        metrics::gauge!("code_index_build_to_queryable_micros")
            .set(started.0.elapsed().as_micros() as f64);
    }
}

#[inline(always)]
pub(crate) fn record_generation_state(state: &'static str) {
    {
        tracing::trace!(name: "code_index_generation_state", value = ?state);
    }
}

#[inline(always)]
pub(crate) fn record_rebuild_state(state: &'static str) {
    {
        tracing::trace!(name: "code_index_rebuild_state", value = ?state);
    }
}
