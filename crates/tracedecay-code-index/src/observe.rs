//! Observation for the code-index kernel.
//!
//! Labels are static string literals. Timing is file-operation granularity:
//! per-node work is never measured. Tight loops are sampled (1 in 32) so a
//! generation of thousands of files does not flood the profiler. The sampler,
//! worker gauges, and queue gauge touch shared atomics on every file, so they
//! run only while TRACE is enabled; otherwise each helper is a level check.

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

/// No metrics recorder is installed outside profiling sessions, so gauges
/// that cost per-file atomics or a walk to compute share TRACE as their
/// measurement switch.
#[must_use]
#[inline(always)]
pub(crate) fn observing() -> bool {
    tracing::level_enabled!(tracing::Level::TRACE)
}

#[must_use]
#[inline(always)]
fn is_hot_loop_sample(sequence: u64) -> bool {
    sequence.is_multiple_of(HOT_LOOP_SAMPLE_PERIOD)
}

#[must_use]
#[inline(always)]
pub(crate) fn sample_hot_loop() -> bool {
    observing() && is_hot_loop_sample(HOT_LOOP_SAMPLE.fetch_add(1, Ordering::Relaxed))
}

/// Measure one fixed-rate sample from a hot file loop.
///
/// The label must be a literal so callers cannot create path- or
/// generation-shaped cardinality. With TRACE disabled the sampler is a level
/// check with no atomic or clock access.
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

/// Gauge accounting for one fan-out. Whether it is observed is fixed at
/// construction so the shared counters stay balanced if the level changes
/// mid-fan-out.
pub(crate) struct PendingWorkQueue {
    observed: bool,
    remaining: AtomicUsize,
}

impl PendingWorkQueue {
    #[inline(always)]
    pub(crate) fn new(depth: usize) -> Self {
        let observed = observing();
        if observed {
            PENDING_WORK.fetch_add(depth, Ordering::Relaxed);
            refresh_queue_gauge();
        }
        Self {
            observed,
            remaining: AtomicUsize::new(depth),
        }
    }

    #[inline(always)]
    pub(crate) fn start_worker(&self) -> WorkerBusyGuard {
        if self.observed && decrement_if_positive(&self.remaining) {
            let _ = decrement_if_positive(&PENDING_WORK);
            refresh_queue_gauge();
        }
        WorkerBusyGuard::enter(self.observed)
    }
}

impl Drop for PendingWorkQueue {
    fn drop(&mut self) {
        if self.observed {
            let abandoned = self.remaining.swap(0, Ordering::Relaxed);
            let _ = PENDING_WORK.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_sub(abandoned))
            });
            refresh_queue_gauge();
        }
    }
}

pub(crate) struct WorkerBusyGuard {
    observed: bool,
    #[cfg(test)]
    coordinating: Cell<bool>,
}

impl WorkerBusyGuard {
    #[inline(always)]
    fn enter(observed: bool) -> Self {
        if observed {
            WORKERS_ACTIVE.fetch_add(1, Ordering::Relaxed);
            refresh_worker_gauges();
        }
        Self {
            observed,
            #[cfg(test)]
            coordinating: Cell::new(false),
        }
    }

    #[inline(always)]
    pub(crate) fn pool_coordination(&self) -> WorkerPoolCoordinationGuard<'_> {
        let started = self.observed.then(|| {
            WORKERS_POOL_COORDINATION.fetch_add(1, Ordering::Relaxed);
            refresh_worker_gauges();
            Instant::now()
        });
        #[cfg(test)]
        self.coordinating.set(true);
        WorkerPoolCoordinationGuard {
            #[cfg(not(test))]
            _worker: PhantomData,
            #[cfg(test)]
            worker: self,

            started,
        }
    }
}

impl Drop for WorkerBusyGuard {
    fn drop(&mut self) {
        if self.observed {
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

    started: Option<Instant>,
}

impl Drop for WorkerPoolCoordinationGuard<'_> {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            let _ = decrement_if_positive(&WORKERS_POOL_COORDINATION);
            metrics::gauge!("code_index_worker_pool_coordination_micros").increment(
                (u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)) as f64,
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
    metrics::gauge!("code_index_workers_busy").set(active as f64);
    metrics::gauge!("code_index_workers_cpu").set(cpu as f64);
    metrics::gauge!("code_index_workers_pool_coordination").set(coordinating as f64);
    metrics::gauge!("code_index_worker_count").set(workers as f64);
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
        metrics::gauge!("code_index_files").set(count as f64);
    }
}

/// Carries a caller-computed total so this helper never re-walks sources.
#[inline(always)]
pub(crate) fn record_source_bytes(bytes: u64) {
    metrics::gauge!("code_index_source_bytes").set(bytes as f64);
}

#[inline(always)]
pub(crate) fn add_parse_bytes(bytes: u64) {
    {
        metrics::gauge!("code_index_parse_bytes").increment(bytes as f64);
    }
}

#[inline(always)]
pub(crate) fn add_reused_parses(count: u64) {
    {
        metrics::gauge!("code_index_reused_parses").increment(count as f64);
    }
}

#[inline(always)]
pub(crate) fn record_symbols(count: u64) {
    metrics::gauge!("code_index_symbols").set(count as f64);
}

#[inline(always)]
pub(crate) fn record_relations(count: u64) {
    metrics::gauge!("code_index_relations").set(count as f64);
}

#[inline(always)]
pub(crate) fn record_pages(count: u64) {
    {
        metrics::gauge!("code_index_pages").set(count as f64);
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
