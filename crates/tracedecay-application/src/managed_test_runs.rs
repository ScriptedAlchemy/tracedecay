//! Canonical reader of a project root's newest managed test run.
//!
//! The project sessions store is the authority for which run is newest, the
//! source identity it ran against, and its terminal outcome, so a run stays
//! readable across daemon restarts and live-stream eviction. The operation
//! event stream contributes only what exists while a run executes: its
//! progress and deadline.

use std::collections::BTreeMap;

use tokio::sync::TryLockError;
use tracedecay_contracts::feedback::TestResultProjectionV1;
use tracedecay_contracts::{Deadline, OperationTermination};
use tracedecay_domain::{CodeGenerationId, CommitId, ContentDigest};
use tracedecay_global_db::{
    ManagedTestRunOutcomeV1, ManagedTestRunRecordV1, ManagedTestRunStartV1,
    RegisteredGlobalDbLeaseV1,
};

use crate::operation_stream::{
    ManagedTestRunActivity, ManagedTestRunProgress, OperationEventAuthority,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagedTestRunSnapshot {
    pub(crate) operation_id: String,
    pub(crate) head_commit_id: Option<CommitId>,
    pub(crate) code_generation_id: Option<CodeGenerationId>,
    /// Saved content digest of each document the run covers, keyed by its
    /// canonical `file:` URI.
    pub(crate) document_content_digests: BTreeMap<String, ContentDigest>,
    pub(crate) deadline: Deadline,
    /// Every pass/fail result the run recorded, in the order libtest reported
    /// them. A run still executing has recorded none yet.
    pub(crate) results: Vec<TestResultProjectionV1>,
    /// Tests that reported an outcome, ignored ones included.
    pub(crate) completed: u64,
    pub(crate) total: Option<u64>,
    pub(crate) termination: Option<OperationTermination>,
}

impl ManagedTestRunSnapshot {
    fn settled(start: ManagedTestRunStartV1, outcome: ManagedTestRunOutcomeV1) -> Self {
        Self {
            operation_id: start.operation_id,
            head_commit_id: start.head_commit_id,
            code_generation_id: start.code_generation_id,
            document_content_digests: start.document_content_digests,
            deadline: outcome.receipt.effective_deadline,
            completed: (outcome.results.len() as u64).saturating_add(outcome.ignored),
            results: outcome.results,
            total: Some(start.requested_tests),
            termination: Some(outcome.receipt.termination),
        }
    }

    fn executing(start: ManagedTestRunStartV1, progress: ManagedTestRunProgress) -> Self {
        Self {
            operation_id: start.operation_id,
            head_commit_id: start.head_commit_id,
            code_generation_id: start.code_generation_id,
            document_content_digests: start.document_content_digests,
            deadline: progress.deadline,
            results: Vec::new(),
            completed: progress.completed,
            total: Some(start.requested_tests),
            termination: None,
        }
    }

    /// Differs between any two snapshots that carry different evidence about
    /// the run. A settled run's recorded outcome never changes.
    pub(crate) fn revision(&self) -> String {
        format!(
            "{}:{}:{:?}:{:?}",
            self.operation_id, self.completed, self.total, self.termination
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagedTestRunCurrentScope {
    pub(crate) root_uri: String,
    pub(crate) head_commit_id: Option<CommitId>,
    pub(crate) code_generation_id: Option<CodeGenerationId>,
    /// The identity a managed run recorded the document's digest under: the
    /// canonical project root joined with the project-relative path, never a
    /// client's alias spelling of the same file.
    pub(crate) document_uri: Option<String>,
    pub(crate) document_content_digest: Option<ContentDigest>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ManagedTestRunUnavailableReason {
    /// The project root has never recorded a managed run.
    Unrecorded,
    /// The newest recorded run never settled and no producer still executes
    /// it: the daemon stopped mid-run or its producer failed to settle it.
    Abandoned,
    CurrentHeadUnbound,
    CurrentCodeGenerationUnbound,
    RetainedHeadUnbound,
    RetainedCodeGenerationUnbound,
    CurrentDocumentUnbound,
    RetainedDocumentUnbound,
    AuthorityFailure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ManagedTestRunStaleReason {
    SourceIdentity,
    DocumentContent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ManagedTestRunReadOutcome {
    Current(ManagedTestRunSnapshot),
    Stale(ManagedTestRunStaleReason),
    Unavailable(ManagedTestRunUnavailableReason),
}

/// Canonical generation- and content-bound reader for a project's newest
/// managed test run. Adapters project its outcome; they do not read the
/// durable record or the event authority, or decide current identity,
/// independently.
#[derive(Clone)]
pub(crate) struct CanonicalManagedTestRunReader {
    store: RegisteredGlobalDbLeaseV1,
    events: OperationEventAuthority,
}

impl CanonicalManagedTestRunReader {
    pub(crate) fn new(store: RegisteredGlobalDbLeaseV1, events: OperationEventAuthority) -> Self {
        Self { store, events }
    }

    #[hotpath::measure(label = "usecases.managed_test_runs.read_latest", future = true)]
    pub(crate) async fn latest_current(
        &self,
        current: &ManagedTestRunCurrentScope,
    ) -> ManagedTestRunReadOutcome {
        match self.latest(&current.root_uri).await {
            Ok(snapshot) => current_managed_test_run(snapshot, current),
            Err(reason) => ManagedTestRunReadOutcome::Unavailable(reason),
        }
    }

    /// The live stream's activity for `root_uri` without waiting on it. A
    /// recorded run changes only while its live stream publishes, so an
    /// unchanged activity means there is nothing new to read back.
    pub(crate) fn try_activity(
        &self,
        root_uri: &str,
    ) -> Result<Option<ManagedTestRunActivity>, TryLockError> {
        self.events.try_managed_test_run_activity(root_uri)
    }

    async fn latest(
        &self,
        root_uri: &str,
    ) -> Result<ManagedTestRunSnapshot, ManagedTestRunUnavailableReason> {
        let record = self.recorded(root_uri).await?;
        let operation_id = record.start.operation_id.clone();
        if let Some(snapshot) = self.executing_or_settled(root_uri, record).await {
            return Ok(snapshot);
        }
        // A producer records its outcome before it publishes the terminal
        // receipt, so a run the live stream no longer executes has either
        // settled since the first read or was abandoned.
        let reread = self.recorded(root_uri).await?;
        if reread.start.operation_id == operation_id && reread.outcome.is_none() {
            return Err(ManagedTestRunUnavailableReason::Abandoned);
        }
        self.executing_or_settled(root_uri, reread)
            .await
            .ok_or(ManagedTestRunUnavailableReason::Abandoned)
    }

    /// The run as settled or still executing; `None` when it is unsettled and
    /// the live stream no longer executes it.
    async fn executing_or_settled(
        &self,
        root_uri: &str,
        record: ManagedTestRunRecordV1,
    ) -> Option<ManagedTestRunSnapshot> {
        let ManagedTestRunRecordV1 { start, outcome } = record;
        if let Some(outcome) = outcome {
            return Some(ManagedTestRunSnapshot::settled(start, outcome));
        }
        self.events
            .managed_test_run_progress(root_uri, &start.operation_id)
            .await
            .map(|progress| ManagedTestRunSnapshot::executing(start, progress))
    }

    async fn recorded(
        &self,
        root_uri: &str,
    ) -> Result<ManagedTestRunRecordV1, ManagedTestRunUnavailableReason> {
        match self.store.latest_managed_test_run(root_uri).await {
            Ok(Some(record)) => Ok(record),
            Ok(None) => Err(ManagedTestRunUnavailableReason::Unrecorded),
            Err(error) => {
                tracing::warn!(%error, "managed test-run record is unreadable");
                Err(ManagedTestRunUnavailableReason::AuthorityFailure)
            }
        }
    }
}

/// The refusal outcome when a retained run's head or code generation is
/// unbound or differs from the current one; `None` when both are current.
pub(crate) fn managed_test_run_source_refusal(
    retained_head: Option<&CommitId>,
    retained_generation: Option<&CodeGenerationId>,
    current: &ManagedTestRunCurrentScope,
) -> Option<ManagedTestRunReadOutcome> {
    let unavailable = |reason| Some(ManagedTestRunReadOutcome::Unavailable(reason));
    let Some(current_head) = current.head_commit_id.as_ref() else {
        return unavailable(ManagedTestRunUnavailableReason::CurrentHeadUnbound);
    };
    let Some(current_generation) = current.code_generation_id.as_ref() else {
        return unavailable(ManagedTestRunUnavailableReason::CurrentCodeGenerationUnbound);
    };
    let Some(retained_head) = retained_head else {
        return unavailable(ManagedTestRunUnavailableReason::RetainedHeadUnbound);
    };
    let Some(retained_generation) = retained_generation else {
        return unavailable(ManagedTestRunUnavailableReason::RetainedCodeGenerationUnbound);
    };
    (retained_head != current_head || retained_generation != current_generation).then_some(
        ManagedTestRunReadOutcome::Stale(ManagedTestRunStaleReason::SourceIdentity),
    )
}

fn current_managed_test_run(
    snapshot: ManagedTestRunSnapshot,
    current: &ManagedTestRunCurrentScope,
) -> ManagedTestRunReadOutcome {
    if let Some(outcome) = managed_test_run_source_refusal(
        snapshot.head_commit_id.as_ref(),
        snapshot.code_generation_id.as_ref(),
        current,
    ) {
        return outcome;
    }
    match (
        current.document_uri.as_ref(),
        current.document_content_digest.as_ref(),
    ) {
        (None, None) => {}
        (Some(document_uri), Some(current_digest)) => {
            let Some(retained_digest) =
                snapshot.document_content_digests.get(document_uri.as_str())
            else {
                return ManagedTestRunReadOutcome::Unavailable(
                    ManagedTestRunUnavailableReason::RetainedDocumentUnbound,
                );
            };
            if retained_digest != current_digest {
                return ManagedTestRunReadOutcome::Stale(
                    ManagedTestRunStaleReason::DocumentContent,
                );
            }
        }
        _ => {
            return ManagedTestRunReadOutcome::Unavailable(
                ManagedTestRunUnavailableReason::CurrentDocumentUnbound,
            );
        }
    }
    ManagedTestRunReadOutcome::Current(snapshot)
}

#[cfg(test)]
#[path = "managed_test_runs_tests.rs"]
mod tests;
