//! Transport-neutral orchestration for bounded derived-memory convergence.

use std::future::Future;

/// Truthful state returned after one bounded derived-memory repair pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DerivedMemoryConvergenceStateV1 {
    Converged,
    /// More durable repair work remains for the daemon scheduler.
    Pending,
}

/// Store-neutral progress for compatibility feedback-history repair.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DerivedMemoryFeedbackHistoryRepairV1 {
    /// The repair authority did not report a progress state.
    #[default]
    Unknown,
    /// No feedback-history repair is needed.
    NotRequired,
    /// Repair completed during the observed pass.
    Complete { processed: u64 },
    /// Repair advanced a bounded batch with durable work remaining.
    Incomplete {
        processed: u64,
        remaining: Option<u64>,
    },
}

/// Store-neutral projection of one bounded repair pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DerivedMemoryRepairStatsV1 {
    missing_vectors_repaired: u64,
    banks_rebuilt: u64,
    feedback_history_repair: DerivedMemoryFeedbackHistoryRepairV1,
    saturated: bool,
}

impl DerivedMemoryRepairStatsV1 {
    pub const fn new(
        missing_vectors_repaired: u64,
        banks_rebuilt: u64,
        saturated: bool,
    ) -> Self {
        Self {
            missing_vectors_repaired,
            banks_rebuilt,
            feedback_history_repair: DerivedMemoryFeedbackHistoryRepairV1::Unknown,
            saturated,
        }
    }

    pub fn with_feedback_history_repair(
        mut self,
        feedback_history_repair: DerivedMemoryFeedbackHistoryRepairV1,
    ) -> Self {
        self.feedback_history_repair = feedback_history_repair;
        self
    }

    pub const fn missing_vectors_repaired(self) -> u64 {
        self.missing_vectors_repaired
    }

    pub const fn banks_rebuilt(self) -> u64 {
        self.banks_rebuilt
    }

    pub const fn feedback_history_repair(self) -> DerivedMemoryFeedbackHistoryRepairV1 {
        self.feedback_history_repair
    }

    pub const fn saturated(self) -> bool {
        self.saturated
    }
}

/// Application-facing port for exactly one bounded derived-memory repair pass.
pub trait DerivedMemoryRepairPort: Send + Sync {
    type Error;

    fn repair_derived_memory(
        &self,
        action: &str,
    ) -> impl Future<Output = Result<DerivedMemoryRepairStatsV1, Self::Error>> + Send;
}

/// Receipt for one bounded pass plus its truthful convergence state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DerivedMemoryConvergenceReportV1 {
    state: DerivedMemoryConvergenceStateV1,
    stats: DerivedMemoryRepairStatsV1,
}

impl DerivedMemoryConvergenceReportV1 {
    pub const fn state(self) -> DerivedMemoryConvergenceStateV1 {
        self.state
    }

    pub const fn is_pending(self) -> bool {
        matches!(self.state, DerivedMemoryConvergenceStateV1::Pending)
    }

    pub const fn stats(self) -> DerivedMemoryRepairStatsV1 {
        self.stats
    }

    pub const fn missing_vectors_repaired(self) -> u64 {
        self.stats.missing_vectors_repaired()
    }

    pub const fn banks_rebuilt(self) -> u64 {
        self.stats.banks_rebuilt()
    }
}

/// Runs exactly one bounded repair pass and classifies remaining work.
pub async fn converge_derived_memory<P>(
    port: &P,
    action: &str,
) -> Result<DerivedMemoryConvergenceReportV1, P::Error>
where
    P: DerivedMemoryRepairPort,
{
    let stats = port.repair_derived_memory(action).await?;
    let state = if stats.saturated() {
        DerivedMemoryConvergenceStateV1::Pending
    } else {
        DerivedMemoryConvergenceStateV1::Converged
    };
    Ok(DerivedMemoryConvergenceReportV1 { state, stats })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RepairPort {
        saturated: bool,
        feedback_history_repair: DerivedMemoryFeedbackHistoryRepairV1,
    }

    impl DerivedMemoryRepairPort for RepairPort {
        type Error = std::convert::Infallible;

        async fn repair_derived_memory(
            &self,
            _action: &str,
        ) -> Result<DerivedMemoryRepairStatsV1, Self::Error> {
            Ok(
                DerivedMemoryRepairStatsV1::new(2, 1, self.saturated)
                    .with_feedback_history_repair(self.feedback_history_repair),
            )
        }
    }

    #[test]
    fn saturated_bounded_pass_reports_pending_for_scheduler() {
        let pending = futures_lite_block_on(converge_derived_memory(
            &RepairPort {
                saturated: true,
                feedback_history_repair: DerivedMemoryFeedbackHistoryRepairV1::NotRequired,
            },
            "repair",
        ))
        .unwrap();
        assert_eq!(pending.state(), DerivedMemoryConvergenceStateV1::Pending);

        let converged = futures_lite_block_on(converge_derived_memory(
            &RepairPort {
                saturated: false,
                feedback_history_repair: DerivedMemoryFeedbackHistoryRepairV1::NotRequired,
            },
            "repair",
        ))
        .unwrap();
        assert_eq!(
            converged.state(),
            DerivedMemoryConvergenceStateV1::Converged
        );
    }

    #[test]
    fn convergence_report_preserves_incomplete_feedback_history_repair() {
        let report = futures_lite_block_on(converge_derived_memory(
            &RepairPort {
                saturated: false,
                feedback_history_repair: DerivedMemoryFeedbackHistoryRepairV1::Incomplete {
                    processed: 3,
                    remaining: Some(7),
                },
            },
            "repair",
        ))
        .unwrap();

        assert_eq!(
            report.stats().feedback_history_repair(),
            DerivedMemoryFeedbackHistoryRepairV1::Incomplete {
                processed: 3,
                remaining: Some(7),
            }
        );
    }

    fn futures_lite_block_on<F: Future>(future: F) -> F::Output {
        use std::task::{Context, Poll, Waker};

        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = std::pin::pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }
}
