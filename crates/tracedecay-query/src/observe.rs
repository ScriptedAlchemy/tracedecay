//! Query-kernel span labels and sampled gauges.
//!
//! Keys are static product-capability names. Never pass query text, user
//! text, paths, or identifiers.

use std::cell::Cell;

use tracedecay_domain::{RetrieverBatch, RetrieverOutcome};

/// Closed residency vocabulary recorded as tracing fields.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Residency {
    Cold,
    Warm,
    Rebuilding,
}

impl Residency {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Warm => "warm",
            Self::Rebuilding => "rebuilding",
        }
    }

    #[inline(always)]
    pub(crate) fn record(self, scope: &'static str) {
        tracing::trace!(scope = scope, value = ?self.as_str());
    }
}

/// No metrics recorder is installed outside profiling sessions, so samplers
/// and metric-only walks share TRACE as their measurement switch.
#[inline(always)]
pub(crate) fn observing() -> bool {
    tracing::level_enabled!(tracing::Level::TRACE)
}

/// Sample 1-in-16 of frequent inner scopes (per-row scoring).
#[inline]
pub(crate) fn sample_frequent() -> bool {
    if !observing() {
        return false;
    }
    thread_local! {
        static TICK: Cell<u32> = const { Cell::new(0) };
    }
    TICK.with(|tick| {
        let next = tick.get().wrapping_add(1);
        tick.set(next);
        next.is_multiple_of(16)
    })
}

/// Time a frequent inner scope only when sampled. The body always runs.
#[inline]
pub(crate) fn measure_frequent<T>(label: &'static str, body: impl FnOnce() -> T) -> T {
    {
        if sample_frequent() {
            {
                let _span = tracing::trace_span!("query.measure_frequent", label = label).entered();
                body()
            }
        } else {
            body()
        }
    }
}

pub(crate) fn record_lane<E>(
    candidates: &'static str,
    examined: &'static str,
    results: &'static str,
    residency: &'static str,
    outcome: &RetrieverOutcome<RetrieverBatch<E>>,
) {
    match outcome {
        RetrieverOutcome::Complete(batch) | RetrieverOutcome::Partial { value: batch, .. } => {
            Residency::Warm.record(residency);
        }
        RetrieverOutcome::Stale(_) => Residency::Rebuilding.record(residency),
        RetrieverOutcome::Cancelled => {}
        _ => {}
    }
}
