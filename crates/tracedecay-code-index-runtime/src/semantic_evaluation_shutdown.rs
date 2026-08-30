//! Boundary type for semantic-evaluation worker shutdown receipts.
//!
//! Root shutdown orchestration collects through
//! [`SemanticEvaluationShutdownJoinV1`] and maps the receipt onto its own
//! `ShutdownStatus` vocabulary.

use std::future::Future;
use std::pin::Pin;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticEvaluationShutdownReceiptV1 {
    pub joined_workers: usize,
    /// Workers whose join surfaced a panic or abort instead of a cooperative
    /// exit. They are no longer running but did not shut down cleanly.
    pub failed_workers: usize,
    pub remaining_workers: usize,
}

impl SemanticEvaluationShutdownReceiptV1 {
    pub fn is_clean(self) -> bool {
        self.remaining_workers == 0 && self.failed_workers == 0
    }
}

/// Typed join surface the worker owner implements so root orchestration can
/// collect receipts without the worker-owner type.
pub trait SemanticEvaluationShutdownJoinV1: Send + Sync {
    fn cancel_and_join_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> Pin<Box<dyn Future<Output = SemanticEvaluationShutdownReceiptV1> + Send + '_>>;
}

/// Collect one semantic-evaluation shutdown receipt through the typed join
/// surface.
pub async fn collect_semantic_evaluation_shutdown(
    owner: &dyn SemanticEvaluationShutdownJoinV1,
    deadline: tokio::time::Instant,
) -> SemanticEvaluationShutdownReceiptV1 {
    owner.cancel_and_join_until(deadline).await
}
