//! Application-port implementation for daemon-owned Git index transactions.

use tracedecay_application::{
    CancellationObservation, CancellationStage, EffectId, EffectTermination,
    GitIndexApplyPortResultV1, GitIndexApplyRequestV1, GitIndexPreviewPortResultV1,
    GitIndexPreviewRequestV1, GitIndexRecoveryRequestV1, GitIndexTransactionPort,
    GitIndexTransactionPortError, OperationBudgetUsage, OperationReceipt, OperationTermination,
    ReconciliationState, RequestAdmission,
};
#[cfg(test)]
use tracedecay_domain::GitIndexIdempotencyKey;
use tracedecay_domain::{
    GitIndexJournalPhaseV1, GitIndexPreviewV1, GitIndexReceiptId, GitIndexReceiptOutcomeV1,
    GitIndexTransactionId, GitIndexTransactionJournalV1, GitIndexTransactionOperationV1,
    GitIndexTransactionReceiptV1, ManifestDigest, canonical_sha256,
};
use tracedecay_policy::{
    GitConflictRiskV1, GitEffectAuthorizationV1, GitEffectClassificationInputV1,
    GitEffectClassifier, GitEffectDispositionV1, GitIndexEffectV1, GitPreviewPreconditionV1,
    GitRepositoryStateFactV1,
};
use tracedecay_store::{
    GitIndexTransactionBeginRequestV1, GitIndexTransactionBeginResultV1, GitIndexTransactionStore,
    GitIndexTransactionStoreError,
};

use super::{
    DurableGitIndexJournal, GitIndexJournalError, GitIndexRecoveryCoordinator,
    GitIndexRecoveryExecutor, RepositoryMutationQueue, RepositoryMutationQueueError,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CurrentGitIndexPolicyStateV1 {
    pub authorization: GitEffectAuthorizationV1,
    pub conflict_risk: GitConflictRiskV1,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub evaluated_at: tracedecay_domain::UtcMicros,
}

/// Current grant, scope, policy, and configuration authority resolved
/// immediately before the durable native effect boundary.
pub(crate) trait GitIndexPolicyRecheckPort {
    fn recheck(
        &self,
        request: &GitIndexApplyRequestV1,
        preview: &GitIndexPreviewV1,
    ) -> Result<CurrentGitIndexPolicyStateV1, GitIndexTransactionPortError>;
}

/// Result of one native apply pass after it has revalidated the exact preview
/// and repository snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeGitIndexApplyResult {
    pub receipt: GitIndexTransactionReceiptV1,
    pub execution: OperationReceipt,
}

/// Classification emitted by the fixed native boundary after admission.
///
/// `ProvenNoMutation` is safe to turn into an `AbortedNoChange` receipt.
/// `CommitBoundaryUnknown` means a durable index/ref boundary may have been
/// crossed, so the coordinator must reconcile once and never replay apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NativeGitIndexApplyOutcomeV1 {
    ProvenNoMutation,
    CommitBoundaryUnknown,
    Completed(Box<NativeGitIndexApplyResult>),
}

/// Fixed native boundary used by the daemon transaction coordinator.
///
/// Implementations may use only the internal PR11 stage/unstage/commit
/// adapters. Every post-admission result is classified so safe failures can
/// receive an abort receipt while unknown commit-boundary state is reconciled
/// exactly once rather than replayed.
pub(crate) trait GitIndexNativeExecutor {
    fn preview(
        &self,
        request: &GitIndexPreviewRequestV1,
    ) -> Result<GitIndexPreviewPortResultV1, GitIndexTransactionPortError>;

    fn apply(
        &self,
        transaction_id: &GitIndexTransactionId,
        preview: &GitIndexPreviewV1,
        request: &GitIndexApplyRequestV1,
    ) -> Result<NativeGitIndexApplyOutcomeV1, GitIndexTransactionPortError>;

    /// Forget process-local preview material without entering a native Git
    /// boundary. In particular, commit messages, identities, and signing key
    /// references must not outlive a rejected one-shot apply attempt.
    fn discard_preview(&self, preview_id: &tracedecay_domain::GitIndexPreviewId);
}

/// One daemon instance owns the queue, journal transitions, policy recheck,
/// idempotency replay, and recovery quarantine for its Git index operations.
pub(crate) struct DaemonGitIndexTransactionPort<S, N, C, A> {
    store: S,
    native: N,
    classifier: C,
    authorization: A,
    queue: RepositoryMutationQueue,
}

impl<S, N, C, A> DaemonGitIndexTransactionPort<S, N, C, A> {
    pub(crate) fn new(store: S, native: N, classifier: C, authorization: A) -> Self {
        Self {
            store,
            native,
            classifier,
            authorization,
            queue: RepositoryMutationQueue::default(),
        }
    }
}

#[cfg(test)]
impl<S, N, C, A> DaemonGitIndexTransactionPort<S, N, C, A>
where
    S: GitIndexTransactionStore,
{
    #[cfg_attr(not(unix), allow(dead_code))] // exercised only by unix-only daemon tests
    pub(crate) fn quarantine_preview_for_test(
        &self,
        preview: &GitIndexPreviewV1,
        observed_at: tracedecay_domain::UtcMicros,
    ) -> Result<(), GitIndexTransactionPortError> {
        let transaction_id = GitIndexTransactionId::new(format!(
            "git-index-transaction.test-quarantine.{}",
            preview.preview_id.as_str()
        ))
        .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
        self.store
            .begin_or_replay(GitIndexTransactionBeginRequestV1 {
                idempotency_key: GitIndexIdempotencyKey::new(format!(
                    "git-index-idempotency.test-quarantine.{}",
                    preview.preview_id.as_str()
                ))
                .map_err(|_| GitIndexTransactionPortError::StalePreview)?,
                input_digest: canonical_sha256(&(
                    "tracedecay.test.git-index-quarantine.v1",
                    &preview.preview_digest,
                ))
                .map_err(|_| GitIndexTransactionPortError::StalePreview)?,
                preview: preview.clone(),
                journal: GitIndexTransactionJournalV1::prepared(
                    transaction_id.clone(),
                    preview,
                    observed_at,
                )
                .map_err(|_| GitIndexTransactionPortError::StalePreview)?,
            })
            .map_err(map_store_error)?;
        self.store
            .quarantine_repository(&preview.repository_snapshot.repository_id, &transaction_id)
            .map_err(map_store_error)
    }
}

impl<S, N, C, A> GitIndexTransactionPort for DaemonGitIndexTransactionPort<S, N, C, A>
where
    S: GitIndexTransactionStore,
    N: GitIndexNativeExecutor + GitIndexRecoveryExecutor,
    C: GitEffectClassifier,
    A: GitIndexPolicyRecheckPort,
{
    fn preview(
        &self,
        request: &GitIndexPreviewRequestV1,
    ) -> Result<GitIndexPreviewPortResultV1, GitIndexTransactionPortError> {
        request
            .validate()
            .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
        let result = self.native.preview(request)?;
        if result.validate_for(request).is_err() {
            self.native.discard_preview(&result.preview.preview_id);
            return Err(GitIndexTransactionPortError::StalePreview);
        }
        if let Err(error) = self.store.save_preview(result.preview.clone()) {
            self.native.discard_preview(&result.preview.preview_id);
            return Err(map_store_error(error));
        }
        Ok(result)
    }

    fn apply(
        &self,
        request: &GitIndexApplyRequestV1,
    ) -> Result<GitIndexApplyPortResultV1, GitIndexTransactionPortError> {
        self.apply_cancellable(request, || None)
    }

    fn recover(
        &self,
        request: &GitIndexRecoveryRequestV1,
    ) -> Result<GitIndexTransactionReceiptV1, GitIndexTransactionPortError> {
        request
            .repository_id
            .validate()
            .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
        request
            .transaction_id
            .validate()
            .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
        self.queue
            .with_repository(&request.repository_id, || {
                let coordinator = GitIndexRecoveryCoordinator::new(&self.store, &self.native);
                let receipts = coordinator
                    .recover_repository(&request.repository_id, request.observed_at)
                    .map_err(|_| GitIndexTransactionPortError::NeedsInspection)?;
                receipts
                    .into_iter()
                    .find(|receipt| receipt.transaction_id == request.transaction_id)
                    .ok_or(GitIndexTransactionPortError::StalePreview)
            })
            .map_err(map_queue_error)?
    }
}

impl<S, N, C, A> DaemonGitIndexTransactionPort<S, N, C, A>
where
    S: GitIndexTransactionStore,
    N: GitIndexNativeExecutor + GitIndexRecoveryExecutor,
    C: GitEffectClassifier,
    A: GitIndexPolicyRecheckPort,
{
    pub(crate) fn apply_cancellable(
        &self,
        request: &GitIndexApplyRequestV1,
        cancellation_requested: impl Fn() -> Option<tracedecay_domain::UtcMicros>,
    ) -> Result<GitIndexApplyPortResultV1, GitIndexTransactionPortError> {
        request
            .validate()
            .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
        let preview = match self.store.read_preview(&request.preview_id) {
            Ok(Some(preview)) => preview,
            Ok(None) => {
                self.native.discard_preview(&request.preview_id);
                return Err(GitIndexTransactionPortError::StalePreview);
            }
            Err(error) => {
                self.native.discard_preview(&request.preview_id);
                return Err(map_store_error(error));
            }
        };
        if preview.validate().is_err() {
            self.native.discard_preview(&request.preview_id);
            return Err(GitIndexTransactionPortError::StalePreview);
        }
        let repository_id = preview.repository_snapshot.repository_id.clone();
        let result = self
            .queue
            .with_repository_cancellable(
                &repository_id,
                &cancellation_requested,
                |cancellation_observed| {
                    self.apply_serialized(
                        request,
                        &preview,
                        cancellation_observed,
                        &cancellation_requested,
                    )
                },
            )
            .map_err(map_queue_error);
        if result.is_err() {
            self.native.discard_preview(&request.preview_id);
        }
        result?
    }
}

impl<S, N, C, A> DaemonGitIndexTransactionPort<S, N, C, A>
where
    S: GitIndexTransactionStore,
    N: GitIndexNativeExecutor + GitIndexRecoveryExecutor,
    C: GitEffectClassifier,
    A: GitIndexPolicyRecheckPort,
{
    fn apply_serialized(
        &self,
        request: &GitIndexApplyRequestV1,
        preview: &GitIndexPreviewV1,
        cancellation_observed_while_queued: Option<tracedecay_domain::UtcMicros>,
        cancellation_requested: &impl Fn() -> Option<tracedecay_domain::UtcMicros>,
    ) -> Result<GitIndexApplyPortResultV1, GitIndexTransactionPortError> {
        let idempotency_key = request
            .native_idempotency_key()
            .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
        let transaction_id = transaction_id(request, &preview.preview_digest)?;
        let journal = GitIndexTransactionJournalV1::prepared(
            transaction_id.clone(),
            preview,
            request.observed_at,
        )
        .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
        let begin = GitIndexTransactionBeginRequestV1 {
            idempotency_key: idempotency_key.clone(),
            input_digest: request
                .input_digest()
                .map_err(|_| GitIndexTransactionPortError::StalePreview)?,
            preview: preview.clone(),
            journal,
        };
        let durable = DurableGitIndexJournal::new(&self.store);
        let begin = match durable.begin_or_replay(begin) {
            Ok(begin) => begin,
            Err(error) => {
                self.native.discard_preview(&preview.preview_id);
                return Err(map_journal_error(error));
            }
        };
        let record = match begin {
            GitIndexTransactionBeginResultV1::Replay(receipt) => {
                self.native.discard_preview(&preview.preview_id);
                return replay_result(request, &receipt);
            }
            GitIndexTransactionBeginResultV1::RecoveryRequired(_) => {
                self.native.discard_preview(&preview.preview_id);
                return Err(GitIndexTransactionPortError::RecoveryRequired);
            }
            GitIndexTransactionBeginResultV1::Started(record) => record,
        };
        if let Some(cancelled_at) =
            cancellation_observed_while_queued.or_else(cancellation_requested)
        {
            self.native.discard_preview(&preview.preview_id);
            return finish_aborted_no_change(
                &self.store,
                &durable,
                &idempotency_key,
                &record.journal,
                &transaction_id,
                preview,
                request,
                EffectTermination::Cancelled,
                Some(cancelled_at),
            );
        }
        // Terminal replay has already returned above. All remaining paths
        // have admitted a new durable record and therefore must end in an
        // atomic terminal receipt or a durable quarantine.
        if request.validate_for_preview(preview).is_err()
            || self.recheck_policy(request, preview).is_err()
        {
            self.native.discard_preview(&preview.preview_id);
            return finish_aborted_no_change(
                &self.store,
                &durable,
                &idempotency_key,
                &record.journal,
                &transaction_id,
                preview,
                request,
                EffectTermination::Failed,
                None,
            );
        }
        if let Some(cancelled_at) = cancellation_requested() {
            self.native.discard_preview(&preview.preview_id);
            return finish_aborted_no_change(
                &self.store,
                &durable,
                &idempotency_key,
                &record.journal,
                &transaction_id,
                preview,
                request,
                EffectTermination::Cancelled,
                Some(cancelled_at),
            );
        }
        let Ok(started) = durable.advance(
            &idempotency_key,
            &record.journal,
            GitIndexJournalPhaseV1::NativeApplyStarted,
            request.observed_at,
        ) else {
            // No native boundary was entered. Prefer a durable no-change
            // terminal receipt; quarantine only if that safe terminal
            // write cannot be proven.
            self.native.discard_preview(&preview.preview_id);
            return finish_aborted_no_change(
                &self.store,
                &durable,
                &idempotency_key,
                &record.journal,
                &transaction_id,
                preview,
                request,
                EffectTermination::Failed,
                None,
            )
            .inspect_err(|_| {
                let _ = quarantine_after_admission(&self.store, preview, &transaction_id);
            });
        };
        if let Some(cancelled_at) = cancellation_requested() {
            self.native.discard_preview(&preview.preview_id);
            return finish_aborted_no_change(
                &self.store,
                &durable,
                &idempotency_key,
                &started,
                &transaction_id,
                preview,
                request,
                EffectTermination::Cancelled,
                Some(cancelled_at),
            );
        }
        match self.native.apply(&transaction_id, preview, request) {
            Ok(NativeGitIndexApplyOutcomeV1::ProvenNoMutation) => finish_aborted_no_change(
                &self.store,
                &durable,
                &idempotency_key,
                &started,
                &transaction_id,
                preview,
                request,
                EffectTermination::Failed,
                None,
            ),
            Ok(NativeGitIndexApplyOutcomeV1::Completed(native))
                if native_result_binds_record(&native, &record, &transaction_id) =>
            {
                let receipt = terminalize_admitted_receipt(
                    &self.store,
                    &durable,
                    &idempotency_key,
                    &started,
                    preview,
                    &transaction_id,
                    native.receipt,
                    request.observed_at,
                )?;
                result_from_receipt(
                    request,
                    deterministic_effect_id(&transaction_id)?,
                    receipt,
                    native.execution,
                )
            }
            Ok(NativeGitIndexApplyOutcomeV1::Completed(_)) => {
                quarantine_after_admission(&self.store, preview, &transaction_id)?;
                Err(GitIndexTransactionPortError::NeedsInspection)
            }
            Ok(NativeGitIndexApplyOutcomeV1::CommitBoundaryUnknown) | Err(_) => {
                let mut recovery_record = (*record).clone();
                recovery_record.journal = started;
                let receipt = GitIndexRecoveryCoordinator::new(&self.store, &self.native)
                    .recover_record(&recovery_record, request.observed_at)
                    .map_err(|_| GitIndexTransactionPortError::NeedsInspection)?;
                let termination = match receipt.outcome {
                    GitIndexReceiptOutcomeV1::Committed => EffectTermination::Completed,
                    GitIndexReceiptOutcomeV1::AbortedNoChange => EffectTermination::Failed,
                    GitIndexReceiptOutcomeV1::NeedsInspection => EffectTermination::EffectUnknown,
                };
                result_from_receipt(
                    request,
                    deterministic_effect_id(&transaction_id)?,
                    receipt,
                    terminal_execution(request, termination, None),
                )
            }
        }
    }

    fn recheck_policy(
        &self,
        request: &GitIndexApplyRequestV1,
        preview: &GitIndexPreviewV1,
    ) -> Result<(), GitIndexTransactionPortError> {
        let effect = match request.binding.operation {
            GitIndexTransactionOperationV1::StageHunks => GitIndexEffectV1::StageHunks,
            GitIndexTransactionOperationV1::UnstageHunks => GitIndexEffectV1::UnstageHunks,
            GitIndexTransactionOperationV1::CommitIndex => GitIndexEffectV1::CommitIndex,
        };
        let current = self.authorization.recheck(request, preview)?;
        if request.context.admission_at(current.evaluated_at) != RequestAdmission::Admitted
            || current.policy_digest != request.proof.policy_digest
            || current.configuration_digest != request.proof.configuration_digest
            || current.policy_revision != request.authority.policy.revision
        {
            return Err(GitIndexTransactionPortError::PolicyDenied);
        }
        let decision = self.classifier.evaluate(&GitEffectClassificationInputV1 {
            effect,
            authorization: current.authorization,
            repository_state: GitRepositoryStateFactV1::from_snapshot(&preview.repository_snapshot),
            expected_preview_digest: Some(request.preview_digest.clone()),
            preview: Some(GitPreviewPreconditionV1 {
                preview_digest: preview.preview_digest.clone(),
                repository_state_id: preview.repository_snapshot.snapshot_id().clone(),
            }),
            conflict_risk: current.conflict_risk,
            policy_revision: current.policy_revision,
            policy_digest: current.policy_digest,
            configuration_digest: current.configuration_digest,
            evaluated_at: current.evaluated_at,
        });
        if decision.disposition == GitEffectDispositionV1::Allow {
            Ok(())
        } else {
            Err(GitIndexTransactionPortError::PolicyDenied)
        }
    }
}

impl<S, N, C, A> DaemonGitIndexTransactionPort<S, N, C, A>
where
    S: GitIndexTransactionStore,
    N: GitIndexRecoveryExecutor,
{
    /// Reconcile every unresolved durable record and active quarantine before
    /// the daemon admits any new transaction for an affected repository.
    pub(crate) fn recover_startup(
        &self,
        observed_at: tracedecay_domain::UtcMicros,
    ) -> Result<Vec<GitIndexTransactionReceiptV1>, GitIndexTransactionPortError> {
        let mut receipts = Vec::new();
        for repository_id in self
            .store
            .recovery_repositories()
            .map_err(map_store_error)?
        {
            let recovered = self
                .queue
                .with_repository(&repository_id, || {
                    GitIndexRecoveryCoordinator::new(&self.store, &self.native)
                        .recover_repository(&repository_id, observed_at)
                        .map_err(|_| GitIndexTransactionPortError::NeedsInspection)
                })
                .map_err(map_queue_error)??;
            receipts.extend(recovered);
        }
        Ok(receipts)
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_aborted_no_change<S>(
    store: &S,
    durable: &DurableGitIndexJournal<'_, S>,
    idempotency_key: &tracedecay_domain::GitIndexIdempotencyKey,
    journal: &GitIndexTransactionJournalV1,
    transaction_id: &GitIndexTransactionId,
    preview: &GitIndexPreviewV1,
    request: &GitIndexApplyRequestV1,
    termination: EffectTermination,
    cancelled_at: Option<tracedecay_domain::UtcMicros>,
) -> Result<GitIndexApplyPortResultV1, GitIndexTransactionPortError>
where
    S: GitIndexTransactionStore,
{
    let receipt = GitIndexTransactionReceiptV1::new_with_final_snapshot(
        receipt_id(transaction_id)?,
        transaction_id.clone(),
        preview,
        None,
        preview.repository_snapshot.index.tree_id.clone(),
        preview.repository_snapshot.head.commit().cloned(),
        None,
        GitIndexReceiptOutcomeV1::AbortedNoChange,
        request.observed_at,
    )
    .map_err(|_| GitIndexTransactionPortError::NeedsInspection)?;
    let receipt = terminalize_admitted_receipt(
        store,
        durable,
        idempotency_key,
        journal,
        preview,
        transaction_id,
        receipt,
        request.observed_at,
    )?;
    result_from_receipt(
        request,
        deterministic_effect_id(transaction_id)?,
        receipt,
        terminal_execution(request, termination, cancelled_at),
    )
}

#[allow(clippy::too_many_arguments)]
fn terminalize_admitted_receipt<S>(
    store: &S,
    durable: &DurableGitIndexJournal<'_, S>,
    idempotency_key: &tracedecay_domain::GitIndexIdempotencyKey,
    journal: &GitIndexTransactionJournalV1,
    preview: &GitIndexPreviewV1,
    transaction_id: &GitIndexTransactionId,
    receipt: GitIndexTransactionReceiptV1,
    observed_at: tracedecay_domain::UtcMicros,
) -> Result<GitIndexTransactionReceiptV1, GitIndexTransactionPortError>
where
    S: GitIndexTransactionStore,
{
    if receipt.outcome == GitIndexReceiptOutcomeV1::NeedsInspection {
        quarantine_after_admission(store, preview, transaction_id)?;
    }
    let Ok(final_journal) = advance_for_native_outcome(
        durable,
        idempotency_key,
        journal,
        receipt.outcome,
        observed_at,
    ) else {
        quarantine_after_admission(store, preview, transaction_id)?;
        return Err(GitIndexTransactionPortError::NeedsInspection);
    };
    if let Ok(receipt) =
        durable.write_terminal(idempotency_key, &final_journal, receipt, observed_at)
    {
        Ok(receipt)
    } else {
        quarantine_after_admission(store, preview, transaction_id)?;
        Err(GitIndexTransactionPortError::NeedsInspection)
    }
}

fn native_result_binds_record(
    native: &NativeGitIndexApplyResult,
    record: &tracedecay_store::GitIndexTransactionRecordV1,
    transaction_id: &GitIndexTransactionId,
) -> bool {
    let receipt = &native.receipt;
    let expected_termination = match receipt.outcome {
        GitIndexReceiptOutcomeV1::Committed => EffectTermination::Completed,
        GitIndexReceiptOutcomeV1::AbortedNoChange => EffectTermination::Failed,
        GitIndexReceiptOutcomeV1::NeedsInspection => EffectTermination::EffectUnknown,
    };
    native.execution.validate().is_ok()
        && native.execution.termination == operation_termination(expected_termination)
        && receipt.transaction_id == *transaction_id
        && record.receipt_binds_preview(receipt)
}

fn quarantine_after_admission<S>(
    store: &S,
    preview: &GitIndexPreviewV1,
    transaction_id: &GitIndexTransactionId,
) -> Result<(), GitIndexTransactionPortError>
where
    S: GitIndexTransactionStore,
{
    store
        .quarantine_repository(&preview.repository_snapshot.repository_id, transaction_id)
        .map_err(|_| GitIndexTransactionPortError::NeedsInspection)
}

fn transaction_id(
    request: &GitIndexApplyRequestV1,
    preview_digest: &ManifestDigest,
) -> Result<GitIndexTransactionId, GitIndexTransactionPortError> {
    let input_digest = request
        .input_digest()
        .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
    let digest = canonical_sha256(&(
        "tracedecay.daemon.git-index-transaction.v1",
        request.idempotency_key.as_str(),
        input_digest,
        preview_digest,
    ))
    .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
    let encoded = digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(GitIndexTransactionPortError::StalePreview)?;
    GitIndexTransactionId::new(format!("git-index-transaction.v1.{encoded}"))
        .map_err(|_| GitIndexTransactionPortError::StalePreview)
}

fn deterministic_effect_id(
    transaction_id: &GitIndexTransactionId,
) -> Result<EffectId, GitIndexTransactionPortError> {
    EffectId::new(format!("git-index-effect.v1.{}", transaction_id.as_str()))
        .map_err(|_| GitIndexTransactionPortError::StalePreview)
}

fn receipt_id(
    transaction_id: &GitIndexTransactionId,
) -> Result<GitIndexReceiptId, GitIndexTransactionPortError> {
    GitIndexReceiptId::new(format!("git-index-receipt.v1.{}", transaction_id.as_str()))
        .map_err(|_| GitIndexTransactionPortError::NeedsInspection)
}

fn advance_for_native_outcome<S>(
    durable: &DurableGitIndexJournal<'_, S>,
    idempotency_key: &tracedecay_domain::GitIndexIdempotencyKey,
    journal: &GitIndexTransactionJournalV1,
    outcome: GitIndexReceiptOutcomeV1,
    observed_at: tracedecay_domain::UtcMicros,
) -> Result<GitIndexTransactionJournalV1, GitIndexJournalError>
where
    S: GitIndexTransactionStore,
{
    let mut journal = journal.clone();
    let phases: &[GitIndexJournalPhaseV1] = match outcome {
        GitIndexReceiptOutcomeV1::AbortedNoChange | GitIndexReceiptOutcomeV1::NeedsInspection => {
            &[]
        }
        GitIndexReceiptOutcomeV1::Committed => {
            if journal.operation == GitIndexTransactionOperationV1::CommitIndex {
                &[
                    GitIndexJournalPhaseV1::IndexCommitted,
                    GitIndexJournalPhaseV1::RefCommitted,
                    GitIndexJournalPhaseV1::Verifying,
                ]
            } else {
                &[
                    GitIndexJournalPhaseV1::IndexCommitted,
                    GitIndexJournalPhaseV1::Verifying,
                ]
            }
        }
    };
    for phase in phases {
        journal = durable.advance(idempotency_key, &journal, *phase, observed_at)?;
    }
    Ok(journal)
}

fn replay_result(
    request: &GitIndexApplyRequestV1,
    receipt: &GitIndexTransactionReceiptV1,
) -> Result<GitIndexApplyPortResultV1, GitIndexTransactionPortError> {
    result_from_receipt(
        request,
        deterministic_effect_id(&receipt.transaction_id)?,
        receipt.clone(),
        terminal_execution(
            request,
            match receipt.outcome {
                GitIndexReceiptOutcomeV1::Committed => EffectTermination::Completed,
                GitIndexReceiptOutcomeV1::AbortedNoChange => EffectTermination::Failed,
                GitIndexReceiptOutcomeV1::NeedsInspection => EffectTermination::EffectUnknown,
            },
            None,
        ),
    )
}

fn result_from_receipt(
    request: &GitIndexApplyRequestV1,
    effect_id: EffectId,
    receipt: GitIndexTransactionReceiptV1,
    execution: OperationReceipt,
) -> Result<GitIndexApplyPortResultV1, GitIndexTransactionPortError> {
    let (termination, reconciliation) = match receipt.outcome {
        GitIndexReceiptOutcomeV1::Committed => (
            EffectTermination::Completed,
            ReconciliationState::Reconciled,
        ),
        GitIndexReceiptOutcomeV1::AbortedNoChange => match execution.termination {
            OperationTermination::Cancelled => (
                EffectTermination::Cancelled,
                ReconciliationState::Reconciled,
            ),
            OperationTermination::TimedOut => {
                (EffectTermination::TimedOut, ReconciliationState::Reconciled)
            }
            _ => (EffectTermination::Failed, ReconciliationState::Reconciled),
        },
        GitIndexReceiptOutcomeV1::NeedsInspection => (
            EffectTermination::EffectUnknown,
            ReconciliationState::Pending,
        ),
    };
    if execution.termination != operation_termination(termination) {
        return Err(GitIndexTransactionPortError::NeedsInspection);
    }
    Ok(GitIndexApplyPortResultV1 {
        effect_id,
        idempotency_key: request.idempotency_key.clone(),
        preview_digest: request.preview_digest.clone(),
        receipt,
        execution,
        reconciliation,
    })
}

fn terminal_execution(
    request: &GitIndexApplyRequestV1,
    termination: EffectTermination,
    cancelled_at: Option<tracedecay_domain::UtcMicros>,
) -> OperationReceipt {
    let ended_at = cancelled_at.unwrap_or(request.observed_at);
    OperationReceipt {
        started_at: request.observed_at,
        ended_at,
        effective_deadline: request.context.deadline().clone(),
        cancellation: cancelled_at.map(|observed_at| CancellationObservation {
            stage: CancellationStage::BeforeEffect,
            observed_at,
        }),
        budget: OperationBudgetUsage::default(),
        termination: operation_termination(termination),
    }
}

const fn operation_termination(termination: EffectTermination) -> OperationTermination {
    match termination {
        EffectTermination::Completed => OperationTermination::Completed,
        EffectTermination::Cancelled => OperationTermination::Cancelled,
        EffectTermination::TimedOut => OperationTermination::TimedOut,
        EffectTermination::Failed => OperationTermination::Failed,
        EffectTermination::Partial => OperationTermination::Partial,
        EffectTermination::EffectUnknown => OperationTermination::EffectUnknown,
    }
}

#[allow(clippy::needless_pass_by_value)]
fn map_store_error(error: GitIndexTransactionStoreError) -> GitIndexTransactionPortError {
    match error {
        GitIndexTransactionStoreError::IdempotencyConflict => {
            GitIndexTransactionPortError::IdempotencyConflict
        }
        GitIndexTransactionStoreError::RepositoryQuarantined => {
            GitIndexTransactionPortError::RecoveryRequired
        }
        GitIndexTransactionStoreError::Unavailable => {
            GitIndexTransactionPortError::DaemonUnavailable
        }
        GitIndexTransactionStoreError::PreviewConflict
        | GitIndexTransactionStoreError::JournalConflict
        | GitIndexTransactionStoreError::ReceiptConflict
        | GitIndexTransactionStoreError::InvalidData(_) => {
            GitIndexTransactionPortError::StalePreview
        }
    }
}

fn map_journal_error(error: GitIndexJournalError) -> GitIndexTransactionPortError {
    match error {
        GitIndexJournalError::Store(error) => map_store_error(error),
        GitIndexJournalError::Domain(_) => GitIndexTransactionPortError::NeedsInspection,
    }
}

#[allow(clippy::needless_pass_by_value)]
fn map_queue_error(error: RepositoryMutationQueueError) -> GitIndexTransactionPortError {
    match error {
        RepositoryMutationQueueError::Unavailable | RepositoryMutationQueueError::Saturated => {
            GitIndexTransactionPortError::DaemonUnavailable
        }
    }
}
