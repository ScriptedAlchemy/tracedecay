//! Opt-in Hotpath observation for the code-index kernel.
//!
//! Labels are static string literals. Timing is file-operation granularity:
//! per-node work is never measured. Tight loops are sampled (1 in 32) so a
//! generation of thousands of files does not flood the profiler. Every helper
//! is `#[inline(always)]` and compiles to a no-op when the `hotpath` feature
//! is off — atomics, clocks, and census walks must not run on the default path.

#[cfg(feature = "hotpath")]
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
#[cfg(feature = "hotpath")]
use std::time::Instant;

#[cfg(feature = "hotpath")]
const HOT_LOOP_SAMPLE_PERIOD: u64 = 32;

#[cfg(feature = "hotpath")]
static WORKERS_BUSY: AtomicUsize = AtomicUsize::new(0);
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

pub(crate) struct WorkerBusyGuard;

impl WorkerBusyGuard {
    #[inline(always)]
    pub(crate) fn enter() -> Self {
        #[cfg(feature = "hotpath")]
        {
            let busy = WORKERS_BUSY
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1);
            refresh_worker_gauges(busy);
        }
        Self
    }
}

#[cfg(feature = "hotpath")]
impl Drop for WorkerBusyGuard {
    fn drop(&mut self) {
        let previous = WORKERS_BUSY.fetch_sub(1, Ordering::Relaxed);
        refresh_worker_gauges(previous.saturating_sub(1));
    }
}

#[cfg(feature = "hotpath")]
fn refresh_worker_gauges(busy: usize) {
    let workers = crate::parallelism::indexing_workers();
    hotpath::gauge!("code_index_workers_busy").set(busy);
    hotpath::gauge!("code_index_worker_count").set(workers);
    let utilization = if workers == 0 {
        0.0
    } else {
        (busy as f64) * 100.0 / workers as f64
    };
    hotpath::gauge!("code_index_worker_utilization_pct").set(utilization);
}

#[inline(always)]
pub(crate) fn record_queue_depth(depth: usize) {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("code_index_queue_depth").set(depth);
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = depth;
    }
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
