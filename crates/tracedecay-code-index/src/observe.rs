//! Spans and trace events for the code-index kernel.
//!
//! Span names are static string literals. Timing is file-operation
//! granularity: per-node work is never measured. Tight loops are sampled
//! (1 in 32) so a generation of thousands of files does not flood the trace.
//! The sampler touches a shared atomic, so it runs only while TRACE is
//! enabled; otherwise it is a level check.

use std::sync::atomic::{AtomicU64, Ordering};

const HOT_LOOP_SAMPLE_PERIOD: u64 = 32;

static HOT_LOOP_SAMPLE: AtomicU64 = AtomicU64::new(0);

#[must_use]
#[inline(always)]
pub(crate) fn sample_hot_loop() -> bool {
    tracing::level_enabled!(tracing::Level::TRACE)
        && HOT_LOOP_SAMPLE
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(HOT_LOOP_SAMPLE_PERIOD)
}

/// Measure one fixed-rate sample from a hot file loop.
///
/// The label must be a literal so callers cannot create path- or
/// generation-shaped cardinality. With TRACE disabled the sampler is a level
/// check with no atomic or clock access.
macro_rules! measure_hot_loop {
    ($label:literal, $work:expr) => {{
        if $crate::observe::sample_hot_loop() {
            let _span = tracing::trace_span!($label).entered();
            $work
        } else {
            $work
        }
    }};
}

pub(crate) use measure_hot_loop;

#[inline(always)]
pub(crate) fn record_generation_state(state: &'static str) {
    tracing::trace!(name: "code_index_generation_state", value = ?state);
}

#[inline(always)]
pub(crate) fn record_rebuild_state(state: &'static str) {
    tracing::trace!(name: "code_index_rebuild_state", value = ?state);
}
