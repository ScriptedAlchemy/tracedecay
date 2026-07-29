//! Transport-neutral orchestration for bounded derived-memory convergence.

use std::future::Future;

/// Truthful state returned after one bounded derived-memory repair pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DerivedMemoryConvergenceStateV1 {
    Converged,
    /// More durable repair work remains for the daemon scheduler.
    Pending,
}

/// Store-neutral projection of one bounded repair pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DerivedMemoryRepairStatsV1 {
    missing_vectors_repaired: u64,
    banks_rebuilt: u64,
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
            saturated,
        }
    }

    pub const fn missing_vectors_repaired(self) -> u64 {
        self.missing_vectors_repaired
    }

    pub const fn banks_rebuilt(self) -> u64 {
        self.banks_rebuilt
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
    }

    impl DerivedMemoryRepairPort for RepairPort {
        type Error = std::convert::Infallible;

        async fn repair_derived_memory(
            &self,
            _action: &str,
        ) -> Result<DerivedMemoryRepairStatsV1, Self::Error> {
            Ok(DerivedMemoryRepairStatsV1::new(2, 1, self.saturated))
        }
    }

    #[test]
    fn saturated_bounded_pass_reports_pending_for_scheduler() {
        let pending = futures_lite_block_on(converge_derived_memory(
            &RepairPort { saturated: true },
            "repair",
        ))
        .unwrap();
        assert_eq!(pending.state(), DerivedMemoryConvergenceStateV1::Pending);

        let converged = futures_lite_block_on(converge_derived_memory(
            &RepairPort { saturated: false },
            "repair",
        ))
        .unwrap();
        assert_eq!(
            converged.state(),
            DerivedMemoryConvergenceStateV1::Converged
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
