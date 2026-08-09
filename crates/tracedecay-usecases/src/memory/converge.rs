//! Canonical derived-memory convergence policy.
//!
//! [`MemoryApplication::dashboard_repair`] runs one store-bounded repair
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
    DerivedMemoryRepairPort, DerivedMemoryRepairStats, converge_derived_memory,
};
use tracedecay_store::{ProjectMemoryFactStore, ProjectMemoryFeedbackRepairProgressV1};

use super::MemoryApplication;
use super::context::MemoryOperationContext;
use super::error::MemoryApplicationError;

pub use tracedecay_application::{
    DerivedMemoryConvergenceReport, DerivedMemoryFeedbackHistoryRepair,
};

const fn feedback_history_repair(
    progress: ProjectMemoryFeedbackRepairProgressV1,
) -> DerivedMemoryFeedbackHistoryRepair {
    match progress {
        ProjectMemoryFeedbackRepairProgressV1::Unknown => {
            DerivedMemoryFeedbackHistoryRepair::Unknown
        }
        ProjectMemoryFeedbackRepairProgressV1::NotRequired => {
            DerivedMemoryFeedbackHistoryRepair::NotRequired
        }
        ProjectMemoryFeedbackRepairProgressV1::Complete { processed } => {
            DerivedMemoryFeedbackHistoryRepair::Complete { processed }
        }
        ProjectMemoryFeedbackRepairProgressV1::Incomplete {
            processed,
            remaining,
        } => DerivedMemoryFeedbackHistoryRepair::Incomplete {
            processed,
            remaining,
        },
    }
}

impl<A: ProjectMemoryFactStore> DerivedMemoryRepairPort for MemoryApplication<A> {
    type Error = MemoryApplicationError;

    async fn repair_derived_memory(
        &self,
        action: &str,
    ) -> Result<DerivedMemoryRepairStats, Self::Error> {
        let context = MemoryOperationContext::generated(&self.owner, action, None)?;
        let stats = self.dashboard_repair(context).await?;
        Ok(DerivedMemoryRepairStats::new(
            stats.missing_vectors_repaired(),
            stats.banks_rebuilt(),
            stats.saturated(),
        )
        .with_feedback_history_repair(feedback_history_repair(stats.feedback_history_repair())))
    }
}

impl<A: ProjectMemoryFactStore> MemoryApplication<A> {
    /// Runs exactly one bounded derived-memory repair pass.
    ///
    /// `action` names the trigger (e.g. `"dashboard-startup-repair"`) used
    /// for the pass's generated operation identity. Saturation is reported as
    /// [`tracedecay_application::DerivedMemoryConvergenceState::Pending`];
    /// the caller proceeds while the existing daemon scheduler owns the
    /// remaining durable backlog.
    pub async fn converge_derived_memory(
        &self,
        action: &str,
    ) -> Result<DerivedMemoryConvergenceReport, MemoryApplicationError> {
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

#[cfg(test)]
mod tests {
    use tracedecay_application::DerivedMemoryFeedbackHistoryRepair;
    use tracedecay_store::ProjectMemoryFeedbackRepairProgressV1;

    use super::feedback_history_repair;

    #[test]
    fn feedback_history_repair_projection_retains_each_authoritative_state() {
        let cases = [
            (
                ProjectMemoryFeedbackRepairProgressV1::Unknown,
                DerivedMemoryFeedbackHistoryRepair::Unknown,
            ),
            (
                ProjectMemoryFeedbackRepairProgressV1::NotRequired,
                DerivedMemoryFeedbackHistoryRepair::NotRequired,
            ),
            (
                ProjectMemoryFeedbackRepairProgressV1::Complete { processed: 2 },
                DerivedMemoryFeedbackHistoryRepair::Complete { processed: 2 },
            ),
            (
                ProjectMemoryFeedbackRepairProgressV1::Incomplete {
                    processed: 3,
                    remaining: Some(7),
                },
                DerivedMemoryFeedbackHistoryRepair::Incomplete {
                    processed: 3,
                    remaining: Some(7),
                },
            ),
        ];

        for (authoritative, expected) in cases {
            assert_eq!(feedback_history_repair(authoritative), expected);
        }
    }
}
