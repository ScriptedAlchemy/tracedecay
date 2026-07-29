//! Code-index scheduler cadence telemetry and event-to-ready receipts.
//!
//! Hints, mount wakes, and query-admission freshness checks are wake-up signals
//! only. Every receipt records the scheduled-arrival-to-terminal latency for one
//! completed reconcile (publish or no-op) so operators can prove cadence instead
//! of inferring it from sealed-generation age.

use std::path::PathBuf;

use tracedecay_domain::{CodeGenerationId, ContentDigest};

/// Why the scheduler was asked to reconcile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CodeIndexCadenceTriggerV1 {
    /// Worktree mount scheduled an initial/verification reconcile.
    Mount,
    /// Host after-file-edit (or equivalent) hint paths arrived.
    HookHint,
    /// Hint overflow / dropped-event reconciliation.
    Overflow,
    /// Query-admission freshness ladder required truth.
    QueryAdmission,
    /// Follow-up wake after a busy serve-prior-generation admission.
    BusyFollowUp,
}

/// Terminal outcome of one cadence-driven reconcile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CodeIndexCadenceOutcomeV1 {
    Published {
        generation_id: CodeGenerationId,
        reextracted_files: usize,
        changed_chunks: usize,
        reused_chunks: usize,
    },
    Noop {
        snapshot_content_identity: ContentDigest,
    },
}

/// One completed event-to-ready measurement for a mounted worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodeIndexEventToReadyReceiptV1 {
    pub project_root: PathBuf,
    pub trigger: CodeIndexCadenceTriggerV1,
    /// When the wake/hint was accepted (Unix micros).
    pub wake_micros: i64,
    /// When the reconcile reached a terminal publish/no-op (Unix micros).
    pub ready_micros: i64,
    /// `ready_micros - wake_micros`, saturating at zero.
    pub queue_delay_micros: i64,
    pub outcome: CodeIndexCadenceOutcomeV1,
    pub overflow_reconciled: bool,
}

impl CodeIndexEventToReadyReceiptV1 {
    pub(crate) fn new(
        project_root: PathBuf,
        trigger: CodeIndexCadenceTriggerV1,
        wake_micros: i64,
        ready_micros: i64,
        outcome: CodeIndexCadenceOutcomeV1,
        overflow_reconciled: bool,
    ) -> Self {
        Self {
            project_root,
            trigger,
            wake_micros,
            ready_micros,
            queue_delay_micros: ready_micros.saturating_sub(wake_micros),
            outcome,
            overflow_reconciled,
        }
    }

    pub(crate) fn is_noop(&self) -> bool {
        matches!(self.outcome, CodeIndexCadenceOutcomeV1::Noop { .. })
    }
}

/// Bounded ring of recent event-to-ready receipts for tests and read models.
#[derive(Debug, Default)]
pub(crate) struct CodeIndexCadenceTelemetryV1 {
    receipts: Vec<CodeIndexEventToReadyReceiptV1>,
}

impl CodeIndexCadenceTelemetryV1 {
    pub(crate) const CAPACITY: usize = 64;

    pub(crate) fn record(&mut self, receipt: CodeIndexEventToReadyReceiptV1) {
        if self.receipts.len() >= Self::CAPACITY {
            self.receipts.remove(0);
        }
        self.receipts.push(receipt);
    }

    pub(crate) fn latest(&self) -> Option<&CodeIndexEventToReadyReceiptV1> {
        self.receipts.last()
    }

    pub(crate) fn receipts(&self) -> &[CodeIndexEventToReadyReceiptV1] {
        &self.receipts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tracedecay_domain::ContentDigest;

    #[test]
    fn event_to_ready_queue_delay_saturates_at_zero() {
        let receipt = CodeIndexEventToReadyReceiptV1::new(
            PathBuf::from("/tmp/project"),
            CodeIndexCadenceTriggerV1::HookHint,
            100,
            90,
            CodeIndexCadenceOutcomeV1::Noop {
                snapshot_content_identity: ContentDigest::new(format!("sha256:{}", "a".repeat(64)))
                    .expect("digest"),
            },
            false,
        );
        assert_eq!(receipt.queue_delay_micros, 0);
        assert!(receipt.is_noop());
    }

    #[test]
    fn telemetry_ring_evicts_oldest_past_capacity() {
        let mut telemetry = CodeIndexCadenceTelemetryV1::default();
        for index in 0..(CodeIndexCadenceTelemetryV1::CAPACITY + 3) {
            telemetry.record(CodeIndexEventToReadyReceiptV1::new(
                PathBuf::from("/tmp/project"),
                CodeIndexCadenceTriggerV1::Mount,
                i64::try_from(index).unwrap_or(0),
                i64::try_from(index).unwrap_or(0) + 1,
                CodeIndexCadenceOutcomeV1::Noop {
                    snapshot_content_identity: ContentDigest::new(format!(
                        "sha256:{}",
                        "b".repeat(64)
                    ))
                    .expect("digest"),
                },
                false,
            ));
        }
        assert_eq!(
            telemetry.receipts().len(),
            CodeIndexCadenceTelemetryV1::CAPACITY
        );
        assert_eq!(telemetry.receipts()[0].wake_micros, 3);
    }
}
