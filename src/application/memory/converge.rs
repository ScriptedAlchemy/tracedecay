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

use tracedecay_application::{
    DerivedMemoryRepairPort, DerivedMemoryRepairStatsV1, converge_derived_memory,
};
use tracedecay_store::FactCompatibilityStore;

use super::MemoryApplication;
use super::context::MemoryOperationContext;
use super::error::MemoryApplicationError;

pub use tracedecay_application::{
    DerivedMemoryConvergenceReportV1, DerivedMemoryConvergenceStateV1,
};

impl<A: FactCompatibilityStore> DerivedMemoryRepairPort for MemoryApplication<A> {
    type Error = MemoryApplicationError;

    async fn repair_derived_memory(
        &self,
        action: &str,
    ) -> Result<DerivedMemoryRepairStatsV1, Self::Error> {
        let context = MemoryOperationContext::generated(&self.owner, action, None)?;
        let stats = self.dashboard_repair_v1(context).await?;
        Ok(DerivedMemoryRepairStatsV1::new(
            stats.missing_vectors_repaired(),
            stats.banks_rebuilt(),
            stats.saturated(),
        ))
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
        let report = converge_derived_memory(self, action).await?;
        if report.is_pending() {
            tracing::warn!(
                "Derived-memory convergence for {action} remains pending after one bounded pass; \
                 serving possibly-stale derived state while the daemon repair scheduler owns \
                 remaining work"
            );
        }
        Ok(report)
    }
}
