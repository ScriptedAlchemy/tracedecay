//! Exact terminal transitions for admitted Explorer query runs.

use std::time::Duration;

use tracedecay_application::Deadline;

use super::{
    ExplorerFinalityV1, ExplorerQueryRunV1, ExplorerRunStateV1, ExplorerSourceIdV1,
    ExplorerSourceOutcomeV1, ExplorerSourcePhaseV1, ExplorerSourceProgressV1, now_micros,
};

impl ExplorerSourceProgressV1 {
    pub(super) fn cancelled(
        source_id: ExplorerSourceIdV1,
        error_code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            phase: ExplorerSourcePhaseV1::Cancelled,
            outcome: ExplorerSourceOutcomeV1::Cancelled,
            ..Self::unavailable(source_id, error_code, message)
        }
    }
}

pub(super) async fn admitted_deadline_elapsed(deadline: Deadline) {
    let observed_at = crate::application::context::application_observed_at();
    let remaining_micros = deadline.expires_at.0.saturating_sub(observed_at.0).max(0) as u64;
    tokio::time::sleep(Duration::from_micros(remaining_micros)).await;
}

pub(super) fn mark_cancelled(run: &mut ExplorerQueryRunV1) {
    let completed_at = now_micros();
    run.state = ExplorerRunStateV1::Cancelled;
    run.finality = ExplorerFinalityV1::Cancelled;
    run.completed_at_micros = Some(completed_at);
    run.elapsed_micros = completed_at.saturating_sub(run.submitted_at_micros);
    for source in &mut run.sources {
        if source.outcome == ExplorerSourceOutcomeV1::Pending {
            source.phase = ExplorerSourcePhaseV1::Cancelled;
            source.outcome = ExplorerSourceOutcomeV1::Cancelled;
            source.error_code = Some("request_cancelled");
            source.message = Some("dashboard request was cancelled".to_owned());
        }
    }
}

pub(super) fn mark_timed_out(run: &mut ExplorerQueryRunV1) {
    let completed_at = now_micros();
    run.state = ExplorerRunStateV1::TimedOut;
    run.finality = ExplorerFinalityV1::TimedOut;
    run.completed_at_micros = Some(completed_at);
    run.elapsed_micros = completed_at.saturating_sub(run.submitted_at_micros);
    for source in &mut run.sources {
        if source.outcome == ExplorerSourceOutcomeV1::Pending {
            source.phase = ExplorerSourcePhaseV1::Completed;
            source.outcome = ExplorerSourceOutcomeV1::Error;
            source.error_code = Some("request_deadline_elapsed");
            source.message = Some("dashboard request deadline elapsed".to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer_api::{ExplorerQueryRequestV1, initial_run};

    fn pending_run() -> ExplorerQueryRunV1 {
        initial_run(
            "explorer-run.fixture".to_owned(),
            ExplorerQueryRequestV1 {
                query: "cache".to_owned(),
                limit: 1,
                offset: 0,
            },
        )
    }

    #[test]
    fn cancellation_is_terminal_for_run_and_pending_sources() {
        let mut run = pending_run();
        mark_cancelled(&mut run);

        assert_eq!(run.state, ExplorerRunStateV1::Cancelled);
        assert_eq!(run.finality, ExplorerFinalityV1::Cancelled);
        assert!(run.sources.iter().all(|source| {
            source.outcome == ExplorerSourceOutcomeV1::Cancelled
                && source.error_code == Some("request_cancelled")
        }));
    }

    #[test]
    fn deadline_is_terminal_for_run_and_pending_sources() {
        let mut run = pending_run();
        mark_timed_out(&mut run);

        assert_eq!(run.state, ExplorerRunStateV1::TimedOut);
        assert_eq!(run.finality, ExplorerFinalityV1::TimedOut);
        assert!(run.sources.iter().all(|source| {
            source.outcome == ExplorerSourceOutcomeV1::Error
                && source.error_code == Some("request_deadline_elapsed")
        }));
    }
}
