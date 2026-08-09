//! Transport-neutral orchestration for bounded derived-memory convergence.

use std::future::Future;

/// Truthful state returned after one bounded derived-memory repair pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DerivedMemoryConvergenceState {
    Converged,
    /// More durable repair work remains for the daemon scheduler.
    Pending,
}

/// Store-neutral progress for feedback-history repair.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DerivedMemoryFeedbackHistoryRepair {
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
pub struct DerivedMemoryRepairStats {
    missing_vectors_repaired: u64,
    banks_rebuilt: u64,
    feedback_history_repair: DerivedMemoryFeedbackHistoryRepair,
    saturated: bool,
}

impl DerivedMemoryRepairStats {
    pub const fn new(missing_vectors_repaired: u64, banks_rebuilt: u64, saturated: bool) -> Self {
        Self {
            missing_vectors_repaired,
            banks_rebuilt,
            feedback_history_repair: DerivedMemoryFeedbackHistoryRepair::Unknown,
            saturated,
        }
    }

    pub fn with_feedback_history_repair(
        mut self,
        feedback_history_repair: DerivedMemoryFeedbackHistoryRepair,
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

    pub const fn feedback_history_repair(self) -> DerivedMemoryFeedbackHistoryRepair {
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
    ) -> impl Future<Output = Result<DerivedMemoryRepairStats, Self::Error>> + Send;
}

/// Receipt for one bounded pass plus its truthful convergence state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DerivedMemoryConvergenceReport {
    state: DerivedMemoryConvergenceState,
    stats: DerivedMemoryRepairStats,
}

impl DerivedMemoryConvergenceReport {
    pub const fn state(self) -> DerivedMemoryConvergenceState {
        self.state
    }

    pub const fn is_pending(self) -> bool {
        matches!(self.state, DerivedMemoryConvergenceState::Pending)
    }

    pub const fn stats(self) -> DerivedMemoryRepairStats {
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
) -> Result<DerivedMemoryConvergenceReport, P::Error>
where
    P: DerivedMemoryRepairPort,
{
    let stats = port.repair_derived_memory(action).await?;
    let feedback_pending = matches!(
        stats.feedback_history_repair(),
        DerivedMemoryFeedbackHistoryRepair::Unknown
            | DerivedMemoryFeedbackHistoryRepair::Incomplete { .. }
    );
    let state = if stats.saturated() || feedback_pending {
        DerivedMemoryConvergenceState::Pending
    } else {
        DerivedMemoryConvergenceState::Converged
    };
    Ok(DerivedMemoryConvergenceReport { state, stats })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RepairPort {
        saturated: bool,
        feedback_history_repair: DerivedMemoryFeedbackHistoryRepair,
    }

    impl DerivedMemoryRepairPort for RepairPort {
        type Error = std::convert::Infallible;

        async fn repair_derived_memory(
            &self,
            _action: &str,
        ) -> Result<DerivedMemoryRepairStats, Self::Error> {
            Ok(DerivedMemoryRepairStats::new(2, 1, self.saturated)
                .with_feedback_history_repair(self.feedback_history_repair))
        }
    }

    #[test]
    fn saturated_bounded_pass_reports_pending_for_scheduler() {
        let pending = futures_lite_block_on(converge_derived_memory(
            &RepairPort {
                saturated: true,
                feedback_history_repair: DerivedMemoryFeedbackHistoryRepair::NotRequired,
            },
            "repair",
        ))
        .unwrap();
        assert_eq!(pending.state(), DerivedMemoryConvergenceState::Pending);

        let converged = futures_lite_block_on(converge_derived_memory(
            &RepairPort {
                saturated: false,
                feedback_history_repair: DerivedMemoryFeedbackHistoryRepair::NotRequired,
            },
            "repair",
        ))
        .unwrap();
        assert_eq!(converged.state(), DerivedMemoryConvergenceState::Converged);
    }

    #[test]
    fn convergence_report_preserves_incomplete_feedback_history_repair() {
        let report = futures_lite_block_on(converge_derived_memory(
            &RepairPort {
                saturated: false,
                feedback_history_repair: DerivedMemoryFeedbackHistoryRepair::Incomplete {
                    processed: 3,
                    remaining: Some(7),
                },
            },
            "repair",
        ))
        .unwrap();

        assert_eq!(
            report.stats().feedback_history_repair(),
            DerivedMemoryFeedbackHistoryRepair::Incomplete {
                processed: 3,
                remaining: Some(7),
            }
        );
        assert_eq!(report.state(), DerivedMemoryConvergenceState::Pending);

        let unknown = futures_lite_block_on(converge_derived_memory(
            &RepairPort {
                saturated: false,
                feedback_history_repair: DerivedMemoryFeedbackHistoryRepair::Unknown,
            },
            "repair",
        ))
        .unwrap();
        assert_eq!(unknown.state(), DerivedMemoryConvergenceState::Pending);
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
