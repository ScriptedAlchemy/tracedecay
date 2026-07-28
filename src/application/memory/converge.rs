//! Canonical derived-memory convergence policy.
//!
//! [`MemoryApplication::dashboard_repair_v1`] runs one store-bounded repair
//! batch and reports whether more work remains behind the batch cap. This
//! module preserves that bound: [`MemoryApplication::converge_derived_memory`]
//! performs exactly one pass and returns a typed pending state when the store
//! remains saturated.
//!
//! Saturated backlog stays durable for the daemon-owned memory-repair
//! scheduler, whose cadence and backoff live in
//! `src/daemon/memory_repair_scheduler.rs`. Admission and ordinary retrieval
//! never spin waiting for that background convergence.

use tracedecay_store::{CompatibilityMemoryRepairStatsV1, FactCompatibilityStore};

use super::MemoryApplication;
use super::context::MemoryOperationContext;
use super::error::MemoryApplicationError;

/// Truthful state returned after one bounded derived-memory repair pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DerivedMemoryConvergenceStateV1 {
    Converged,
    /// More durable repair work remains for the daemon scheduler.
    Pending,
}

fn convergence_state(saturated: bool) -> DerivedMemoryConvergenceStateV1 {
    if saturated {
        DerivedMemoryConvergenceStateV1::Pending
    } else {
        DerivedMemoryConvergenceStateV1::Converged
    }
}

/// Receipt for one bounded pass plus its truthful convergence state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DerivedMemoryConvergenceReportV1 {
    state: DerivedMemoryConvergenceStateV1,
    stats: CompatibilityMemoryRepairStatsV1,
}

impl DerivedMemoryConvergenceReportV1 {
    pub fn state(&self) -> DerivedMemoryConvergenceStateV1 {
        self.state
    }

    pub fn is_pending(&self) -> bool {
        self.state == DerivedMemoryConvergenceStateV1::Pending
    }

    pub fn stats(&self) -> &CompatibilityMemoryRepairStatsV1 {
        &self.stats
    }

    pub fn missing_vectors_repaired(&self) -> u64 {
        self.stats.missing_vectors_repaired()
    }

    pub fn banks_rebuilt(&self) -> u64 {
        self.stats.banks_rebuilt()
    }
}

impl<A: FactCompatibilityStore> MemoryApplication<A> {
    /// Runs exactly one bounded compatibility-memory repair pass.
    ///
    /// `action` names the trigger (e.g. `"dashboard-startup-repair"`) used
    /// for the pass's generated operation identity. Saturation is reported as
    /// [`DerivedMemoryConvergenceStateV1::Pending`]; the caller proceeds while
    /// the existing daemon scheduler owns the remaining durable backlog.
    pub async fn converge_derived_memory(
        &self,
        action: &str,
    ) -> Result<DerivedMemoryConvergenceReportV1, MemoryApplicationError> {
        let context = MemoryOperationContext::generated(&self.owner, action, None)?;
        let stats = self.dashboard_repair_v1(context).await?;
        let state = convergence_state(stats.saturated());
        if state == DerivedMemoryConvergenceStateV1::Pending {
            tracing::warn!(
                "Derived-memory convergence for {action} remains pending after one bounded pass; \
                 serving possibly-stale derived state while the daemon repair scheduler owns \
                 remaining work"
            );
        }
        Ok(DerivedMemoryConvergenceReportV1 { state, stats })
    }
}

#[cfg(test)]
mod tests {
    use super::{DerivedMemoryConvergenceStateV1, convergence_state};

    #[test]
    fn saturated_bounded_pass_reports_pending_for_scheduler() {
        assert_eq!(
            convergence_state(true),
            DerivedMemoryConvergenceStateV1::Pending
        );
        assert_eq!(
            convergence_state(false),
            DerivedMemoryConvergenceStateV1::Converged
        );
    }
}
