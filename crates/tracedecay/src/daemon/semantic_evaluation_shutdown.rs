//! Boundary type for semantic-evaluation worker shutdown receipts.
//!
//! `SemanticEvaluationShutdownReceiptV1` used to live inside
//! `semantic_evaluation.rs` (slice 10 extract). Shutdown orchestration stays
//! in the root crate and must collect the receipt without importing the
//! worker owner. This module is that surface:
//! - the extracted crate will produce the receipt (and can take this file)
//! - root `shutdown_orchestration` collects through
//!   [`collect_semantic_evaluation_shutdown`]
//!
//! The counts and [`SemanticEvaluationShutdownReceiptV1::is_clean`] contract
//! are unchanged. Until slice 10, the producer is still
//! `DaemonSemanticEvaluationWorkerOwnerV1` in this crate.

use std::future::Future;
use std::pin::Pin;

use super::shutdown_coordination::ShutdownStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SemanticEvaluationShutdownReceiptV1 {
    pub(crate) joined_workers: usize,
    /// Workers whose join surfaced a panic or abort instead of a cooperative
    /// exit. They are no longer running but did not shut down cleanly.
    pub(crate) failed_workers: usize,
    pub(crate) remaining_workers: usize,
}

impl SemanticEvaluationShutdownReceiptV1 {
    pub(crate) fn is_clean(self) -> bool {
        self.remaining_workers == 0 && self.failed_workers == 0
    }
}

impl From<SemanticEvaluationShutdownReceiptV1> for ShutdownStatus {
    fn from(receipt: SemanticEvaluationShutdownReceiptV1) -> Self {
        if receipt.remaining_workers > 0 {
            Self::TimedOut
        } else if receipt.is_clean() {
            Self::Clean
        } else {
            Self::Failed(format!(
                "semantic evaluation workers failed to join cleanly: failed={}",
                receipt.failed_workers
            ))
        }
    }
}

/// Typed join surface the future extracted crate implements so root
/// orchestration can collect receipts without the worker-owner type.
pub(crate) trait SemanticEvaluationShutdownJoinV1: Send + Sync {
    fn cancel_and_join_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> Pin<Box<dyn Future<Output = SemanticEvaluationShutdownReceiptV1> + Send + '_>>;
}

/// Collect one semantic-evaluation shutdown receipt through the typed join
/// surface. Shutdown orchestration and project-runtime drain share this so
/// neither path names the worker-owner type.
pub(crate) async fn collect_semantic_evaluation_shutdown(
    owner: &dyn SemanticEvaluationShutdownJoinV1,
    deadline: tokio::time::Instant,
) -> SemanticEvaluationShutdownReceiptV1 {
    owner.cancel_and_join_until(deadline).await
}
