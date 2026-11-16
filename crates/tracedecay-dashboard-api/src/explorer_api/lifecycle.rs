//! Source cancellation and admitted request deadline for Explorer streams.

use std::time::{Duration, Instant};

use tracedecay_contracts::Deadline;
use tracedecay_runtime_core::native_runtime;

use super::{ExplorerSourceIdV1, ExplorerSourceOutcomeV1, ExplorerSourceProgressV1};

impl ExplorerSourceProgressV1 {
    pub(super) fn cancelled(
        source_id: ExplorerSourceIdV1,
        error_code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            outcome: ExplorerSourceOutcomeV1::Cancelled,
            ..Self::unavailable(source_id, error_code, message)
        }
    }
}

pub(super) async fn admitted_deadline_elapsed(deadline: Deadline) {
    let observed_at = tracedecay_session_memory::context::application_observed_at();
    let remaining_micros = deadline.expires_at.0.saturating_sub(observed_at.0).max(0) as u64;
    let due = Instant::now() + Duration::from_micros(remaining_micros);
    // A missing timer fails closed instead of leaving the request unbounded.
    if let Err(error) = native_runtime::sleep_until(due).await {
        tracing::error!(%error, "dashboard explorer deadline timer unavailable");
    }
}
