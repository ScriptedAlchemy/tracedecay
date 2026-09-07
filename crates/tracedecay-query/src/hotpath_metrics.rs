//! Query-kernel hotpath labels and sampled gauges.
//!
//! Keys are static product-capability names. Never pass query text, user
//! text, paths, or identifiers.

#[cfg(feature = "hotpath")]
use std::cell::Cell;

use tracedecay_domain::{RetrieverBatch, RetrieverOutcome};

/// Closed residency vocabulary recorded with `hotpath::val!`.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Residency {
    Cold,
    Warm,
    Rebuilding,
}

impl Residency {
    #[cfg(feature = "hotpath")]
    #[hotpath::skip]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Warm => "warm",
            Self::Rebuilding => "rebuilding",
        }
    }

    #[inline(always)]
    pub(crate) fn record(self, scope: &'static str) {
        #[cfg(feature = "hotpath")]
        hotpath::val!(scope).set(&self.as_str());
        #[cfg(not(feature = "hotpath"))]
        let _ = (self, scope);
    }
}

/// Sample 1-in-16 of frequent inner scopes (per-row scoring).
#[inline]
#[cfg(feature = "hotpath")]
pub(crate) fn sample_frequent() -> bool {
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
    #[cfg(feature = "hotpath")]
    {
        if sample_frequent() {
            hotpath::measure_block!(label, body())
        } else {
            body()
        }
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = label;
        body()
    }
}

pub(crate) fn record_lane<E>(
    candidates: &'static str,
    examined: &'static str,
    results: &'static str,
    residency: &'static str,
    outcome: &RetrieverOutcome<RetrieverBatch<E>>,
) {
    #[cfg(feature = "hotpath")]
    {
        match outcome {
            RetrieverOutcome::Complete(batch) | RetrieverOutcome::Partial { value: batch, .. } => {
                hotpath::gauge!(candidates).set(batch.candidates.len());
                hotpath::gauge!(examined).set(batch.coverage.examined);
                hotpath::gauge!(results).set(batch.candidates.len());
                Residency::Warm.record(residency);
            }
            RetrieverOutcome::Stale(_) => Residency::Rebuilding.record(residency),
            RetrieverOutcome::Cancelled => {
                hotpath::gauge!("query.cancel.count").inc(1u32);
            }
            _ => {}
        }
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = (candidates, examined, results, residency, outcome);
}
