//! Canonical derived-memory convergence policy.
//!
//! [`MemoryApplication::dashboard_repair_v1`] runs exactly one bounded,
//! store-owned repair batch and reports whether the store is still
//! `saturated()` — i.e. whether more repair work is known to remain behind
//! that batch's cap. Any caller that needs derived state (missing vectors,
//! HRR banks) to be *fully converged* before it proceeds has to decide how
//! many of those passes to run and when to give up.
//!
//! That decision used to diverge into three separate copies: a hardcoded
//! eight-pass any-progress loop next to the standalone dashboard's startup
//! path, a single uncoverged pass in `memory curate --apply`, and the daemon
//! scheduler's own saturation-driven backoff loop across ticks. This module
//! is the one place that policy lives now: [`Self::converge_derived_memory`]
//! runs repair passes until the store reports it is no longer saturated, or
//! until a wall-clock bound is exhausted, in which case it logs a warning and
//! returns the last-observed (still-saturated) stats instead of looping
//! forever. The dashboard startup path and `memory curate --apply` are both
//! one-shot callers of this single entry point.
//!
//! The daemon repair scheduler's cadence and backoff *between ticks*
//! (`src/daemon/memory_repair_scheduler.rs`) is a distinct, intentionally
//! separate policy for spreading retries across real time when nothing is
//! actively waiting on convergence, and is untouched here: it still calls
//! [`MemoryApplication::dashboard_repair_v1`] directly, once per tick, and
//! decides whether to tick again from its own saturation-aware
//! `MemoryRepairPassDecision`.

use std::time::{Duration, Instant};

use tracedecay_store::{CompatibilityMemoryRepairStatsV1, FactCompatibilityStore};

use super::MemoryApplication;
use super::context::MemoryOperationContext;
use super::error::MemoryApplicationError;

/// Upper bound on how long [`MemoryApplication::converge_derived_memory`]
/// keeps issuing repair passes before giving up and returning the
/// last-observed stats.
const CONVERGE_WALL_CLOCK_BOUND: Duration = Duration::from_secs(10);

/// What a repair pass's outcome means for the convergence loop. A pure,
/// unit-testable decision kept separate from the store-calling loop, in the
/// same spirit as the daemon scheduler's `MemoryRepairPassDecision`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConvergeStep {
    /// The store reports no remaining saturation: derived state is converged.
    Converged,
    /// Still saturated, and time remains within the bound: run another pass.
    Continue,
    /// Still saturated, but the wall-clock bound is exhausted: stop and warn.
    BoundExhausted,
}

fn converge_step(saturated: bool, now: Instant, deadline: Instant) -> ConvergeStep {
    if !saturated {
        ConvergeStep::Converged
    } else if now >= deadline {
        ConvergeStep::BoundExhausted
    } else {
        ConvergeStep::Continue
    }
}

impl<A: FactCompatibilityStore> MemoryApplication<A> {
    /// Runs bounded compatibility-memory repair passes until the store
    /// reports it is no longer saturated, or until
    /// [`CONVERGE_WALL_CLOCK_BOUND`] elapses.
    ///
    /// `action` names the trigger (e.g. `"dashboard-startup-repair"`) used
    /// for each pass's generated operation identity; every pass gets a fresh
    /// generated operation id so repeated batches never collide with an
    /// earlier one.
    ///
    /// Returns the last-observed repair stats. When the bound is exhausted
    /// while the store is still saturated, this logs `tracing::warn!` and
    /// returns those (still-saturated) stats rather than erroring — the
    /// caller serves possibly-stale derived state instead of blocking
    /// indefinitely.
    pub async fn converge_derived_memory(
        &self,
        action: &str,
    ) -> Result<CompatibilityMemoryRepairStatsV1, MemoryApplicationError> {
        let deadline = Instant::now() + CONVERGE_WALL_CLOCK_BOUND;
        loop {
            let context = MemoryOperationContext::generated(&self.owner, action, None)?;
            let stats = self.dashboard_repair_v1(context).await?;
            match converge_step(stats.saturated(), Instant::now(), deadline) {
                ConvergeStep::Converged => return Ok(stats),
                ConvergeStep::BoundExhausted => {
                    let bound_secs = CONVERGE_WALL_CLOCK_BOUND.as_secs();
                    tracing::warn!(
                        "Derived-memory convergence for {action} exhausted its {bound_secs}s \
                         wall-clock bound while the store still reports saturation; serving \
                         possibly-stale derived state"
                    );
                    return Ok(stats);
                }
                ConvergeStep::Continue => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ConvergeStep, Duration, Instant, converge_step};

    #[test]
    fn unsaturated_pass_converges_regardless_of_remaining_time() {
        let now = Instant::now();
        assert_eq!(converge_step(false, now, now), ConvergeStep::Converged);
        assert_eq!(
            converge_step(false, now, now + Duration::from_secs(10)),
            ConvergeStep::Converged
        );
    }

    #[test]
    fn saturated_pass_continues_while_time_remains() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        assert_eq!(converge_step(true, now, deadline), ConvergeStep::Continue);
    }

    #[test]
    fn saturated_pass_stops_once_the_bound_is_exhausted() {
        let now = Instant::now();
        assert_eq!(converge_step(true, now, now), ConvergeStep::BoundExhausted);
        assert_eq!(
            converge_step(true, now + Duration::from_secs(1), now),
            ConvergeStep::BoundExhausted
        );
    }
}
