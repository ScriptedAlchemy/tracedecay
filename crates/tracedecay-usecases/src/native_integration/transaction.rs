use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    CancellationSignal, NativeIntegrationApplyRequestV1, NativeIntegrationCancelDispositionV1,
    NativeIntegrationCancelRequestV1, NativeIntegrationPort, NativeIntegrationPortError,
    NativeIntegrationPreflightOutcomeV1, NativeIntegrationPreflightRequestV1,
    NativeIntegrationRecoveryRequestV1, NativeIntegrationStackResolutionOutcomeV1,
    NativeIntegrationStackResolutionPort, NativeIntegrationStatusRequestV1,
};
use tracedecay_domain::{
    GitOidV1, ManifestDigest, NativeIntegrationPhaseV1, NativeIntegrationPreviewV1,
    NativeIntegrationReceiptV1, NativeIntegrationSelectionV1, NativeIntegrationTerminalOutcomeV1,
    NativeIntegrationTransactionId, NativeIntegrationTransactionStatusV1, RepositoryId, UtcMicros,
};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_store::{
    NativeIntegrationBeginResultV1, NativeIntegrationRecordV1, NativeIntegrationStore,
    NativeIntegrationStoreError,
};

/// Authorization is checked before preflight enqueue, before apply admission,
/// and immediately before the native ref commit boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeIntegrationAuthorizationOutcomeV1 {
    Authorized,
    Denied,
    Stale,
    Unavailable,
}

pub trait NativeIntegrationAuthorizationPort: Send + Sync {
    fn authorize_preflight(
        &self,
        request: &NativeIntegrationPreflightRequestV1,
    ) -> NativeIntegrationAuthorizationOutcomeV1;

    fn authorize_apply(
        &self,
        request: &NativeIntegrationApplyRequestV1,
        before_ref_commit: bool,
    ) -> NativeIntegrationAuthorizationOutcomeV1;
}

/// Native commit result. `UnknownAfterCommitPoint` carries the candidate tip
/// only as recovery evidence; it is never reported as success.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeApplyEffectV1 {
    Committed {
        new_tip: GitOidV1,
        final_tree: GitOidV1,
        final_index_digest: ManifestDigest,
        final_worktree_digest: ManifestDigest,
    },
    FailedNoChange,
    UnknownAfterCommitPoint {
        candidate_tip: Option<GitOidV1>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeIntegrationProbeV1 {
    OldState {
        tip: GitOidV1,
        tree: GitOidV1,
        index_digest: ManifestDigest,
        worktree_digest: ManifestDigest,
    },
    CommittedState {
        tip: GitOidV1,
        tree: GitOidV1,
        index_digest: ManifestDigest,
        worktree_digest: ManifestDigest,
    },
    Diverged,
    Unavailable,
}

/// Fixed native mechanics. Implementations are constructed with an enrolled
/// repository authority; no method accepts a path or generic Git input.
pub trait NativeIntegrationMechanics: Send + Sync {
    fn preflight(
        &self,
        selection: &NativeIntegrationSelectionV1,
        request: &NativeIntegrationPreflightRequestV1,
        cancellation: &CancellationToken,
    ) -> Result<NativeIntegrationPreviewV1, NativeIntegrationPortError>;

    fn apply(
        &self,
        preview: &NativeIntegrationPreviewV1,
        cancellation: &CancellationToken,
    ) -> Result<NativeApplyEffectV1, NativeIntegrationPortError>;

    fn probe(
        &self,
        record: &NativeIntegrationRecordV1,
    ) -> Result<NativeIntegrationProbeV1, NativeIntegrationPortError>;

    fn rollback(
        &self,
        record: &NativeIntegrationRecordV1,
        committed_tip: &GitOidV1,
    ) -> Result<NativeIntegrationProbeV1, NativeIntegrationPortError>;
}

/// One transaction kernel. The store owns durable CAS and one-use approval;
/// this coordinator owns repository serialization and live cancellation.
pub struct NativeIntegrationTransactionCoordinator<S, T, N, A> {
    store: Arc<S>,
    topology: Arc<T>,
    native: Arc<N>,
    authorization: Arc<A>,
    repository_locks: Mutex<BTreeMap<RepositoryId, Arc<Mutex<()>>>>,
    cancellations: Mutex<BTreeMap<NativeIntegrationTransactionId, CancellationToken>>,
}

impl<S, T, N, A> NativeIntegrationTransactionCoordinator<S, T, N, A> {
    pub fn new(store: Arc<S>, topology: Arc<T>, native: Arc<N>, authorization: Arc<A>) -> Self {
        Self {
            store,
            topology,
            native,
            authorization,
            repository_locks: Mutex::new(BTreeMap::new()),
            cancellations: Mutex::new(BTreeMap::new()),
        }
    }
}

impl<S, T, N, A> NativeIntegrationPort for NativeIntegrationTransactionCoordinator<S, T, N, A>
where
    S: NativeIntegrationStore,
    T: NativeIntegrationStackResolutionPort,
    N: NativeIntegrationMechanics,
    A: NativeIntegrationAuthorizationPort,
{
    fn preflight(
        &self,
        request: &NativeIntegrationPreflightRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationPreflightOutcomeV1, NativeIntegrationPortError> {
        if cancellation.is_cancelled() {
            return Ok(NativeIntegrationPreflightOutcomeV1::Cancelled);
        }
        match self.authorization.authorize_preflight(request) {
            NativeIntegrationAuthorizationOutcomeV1::Authorized => {}
            NativeIntegrationAuthorizationOutcomeV1::Denied => {
                return Ok(NativeIntegrationPreflightOutcomeV1::Denied);
            }
            NativeIntegrationAuthorizationOutcomeV1::Stale => {
                return Ok(NativeIntegrationPreflightOutcomeV1::Stale);
            }
            NativeIntegrationAuthorizationOutcomeV1::Unavailable => {
                return Ok(NativeIntegrationPreflightOutcomeV1::Unavailable);
            }
        }
        let selection = match self.topology.resolve(&request.topology, cancellation)? {
            NativeIntegrationStackResolutionOutcomeV1::Complete(selection) => *selection,
            NativeIntegrationStackResolutionOutcomeV1::Partial => {
                return Ok(NativeIntegrationPreflightOutcomeV1::Partial);
            }
            NativeIntegrationStackResolutionOutcomeV1::Stale => {
                return Ok(NativeIntegrationPreflightOutcomeV1::Stale);
            }
            NativeIntegrationStackResolutionOutcomeV1::Denied => {
                return Ok(NativeIntegrationPreflightOutcomeV1::Denied);
            }
            NativeIntegrationStackResolutionOutcomeV1::Unavailable => {
                return Ok(NativeIntegrationPreflightOutcomeV1::Unavailable);
            }
            NativeIntegrationStackResolutionOutcomeV1::ResetRequired => {
                return Ok(NativeIntegrationPreflightOutcomeV1::ResetRequired);
            }
            NativeIntegrationStackResolutionOutcomeV1::DurabilityUncertain => {
                return Ok(NativeIntegrationPreflightOutcomeV1::DurabilityUncertain);
            }
        };
        let native_cancellation =
            CancellationToken::for_application_request(request.context.request_id().as_str());
        if cancellation.is_cancelled() {
            native_cancellation.cancel();
        }
        let preview = self
            .native
            .preflight(&selection, request, &native_cancellation)?;
        self.store
            .save_preview(preview.clone())
            .map_err(map_store_error)?;
        Ok(NativeIntegrationPreflightOutcomeV1::Preview(Box::new(
            preview,
        )))
    }

    fn apply(
        &self,
        request: &NativeIntegrationApplyRequestV1,
        external_cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationReceiptV1, NativeIntegrationPortError> {
        let repository_id = request.preview.repository_snapshot.repository_id.clone();
        let repository_lock = {
            let mut locks = self.repository_locks.lock().map_err(lock_error)?;
            locks
                .entry(repository_id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = repository_lock.lock().map_err(lock_error)?;
        if let Some(receipt) = self
            .store
            .read_receipt(&request.transaction_id)
            .map_err(map_store_error)?
        {
            return Ok(receipt);
        }
        if self.authorization.authorize_apply(request, false)
            != NativeIntegrationAuthorizationOutcomeV1::Authorized
        {
            return Err(NativeIntegrationPortError::Denied);
        }
        let cancellation =
            CancellationToken::for_application_request(request.context.request_id().as_str());
        if external_cancellation.is_cancelled() {
            cancellation.cancel();
        }
        self.cancellations
            .lock()
            .map_err(lock_error)?
            .insert(request.transaction_id.clone(), cancellation.clone());

        let status = NativeIntegrationTransactionStatusV1 {
            transaction_id: request.transaction_id.clone(),
            preview_id: request.preview.preview_id.clone(),
            preview_digest: request.preview.preview_digest.clone(),
            approval_id: request.approval.approval_id.clone(),
            repository_id: repository_id.clone(),
            destination_ref: request.preview.repository_snapshot.destination_ref.clone(),
            expected_destination_tip: request.preview.repository_snapshot.destination_tip.clone(),
            candidate_tip: None,
            phase: NativeIntegrationPhaseV1::Prepared,
            phase_revision: 1,
            cancellation_requested: cancellation.is_cancelled(),
            terminal_outcome: None,
            updated_at: request.observed_at,
        };
        let record = NativeIntegrationRecordV1 {
            preview: request.preview.clone(),
            approval: request.approval.clone(),
            status,
            terminal_receipt: None,
        };
        let begin = self.store.begin_or_replay(record).map_err(map_store_error);
        let record = match begin {
            Ok(NativeIntegrationBeginResultV1::Started(record)) => *record,
            Ok(NativeIntegrationBeginResultV1::Replay(receipt)) => {
                self.clear_cancellation(&request.transaction_id)?;
                return Ok(*receipt);
            }
            Ok(NativeIntegrationBeginResultV1::RecoveryRequired(_)) => {
                self.clear_cancellation(&request.transaction_id)?;
                return Err(NativeIntegrationPortError::RecoveryRequired);
            }
            Err(error) => {
                self.clear_cancellation(&request.transaction_id)?;
                return Err(error);
            }
        };

        if cancellation.is_cancelled() {
            let receipt = self.finish_from_probe(
                &record,
                NativeIntegrationProbeV1::OldState {
                    tip: record.preview.repository_snapshot.destination_tip.clone(),
                    tree: record.preview.repository_snapshot.destination_tree.clone(),
                    index_digest: record.preview.repository_snapshot.index_digest.clone(),
                    worktree_digest: record.preview.repository_snapshot.worktree_digest.clone(),
                },
                request.observed_at,
            );
            self.clear_cancellation(&request.transaction_id)?;
            return receipt;
        }
        let candidate_verified = advance_status(
            self.store.as_ref(),
            &record.status,
            NativeIntegrationPhaseV1::CandidateVerified,
            false,
            request.observed_at,
        )?;
        if self.authorization.authorize_apply(request, true)
            != NativeIntegrationAuthorizationOutcomeV1::Authorized
        {
            let receipt = self.finish_from_probe(
                &NativeIntegrationRecordV1 {
                    status: candidate_verified,
                    ..record
                },
                NativeIntegrationProbeV1::OldState {
                    tip: request.preview.repository_snapshot.destination_tip.clone(),
                    tree: request.preview.repository_snapshot.destination_tree.clone(),
                    index_digest: request.preview.repository_snapshot.index_digest.clone(),
                    worktree_digest: request.preview.repository_snapshot.worktree_digest.clone(),
                },
                request.observed_at,
            );
            self.clear_cancellation(&request.transaction_id)?;
            return receipt;
        }
        if cancellation.is_cancelled() {
            let receipt = self.finish_from_probe(
                &NativeIntegrationRecordV1 {
                    status: candidate_verified,
                    ..record
                },
                NativeIntegrationProbeV1::OldState {
                    tip: request.preview.repository_snapshot.destination_tip.clone(),
                    tree: request.preview.repository_snapshot.destination_tree.clone(),
                    index_digest: request.preview.repository_snapshot.index_digest.clone(),
                    worktree_digest: request.preview.repository_snapshot.worktree_digest.clone(),
                },
                request.observed_at,
            );
            self.clear_cancellation(&request.transaction_id)?;
            return receipt;
        }
        let commit_started = advance_status(
            self.store.as_ref(),
            &candidate_verified,
            NativeIntegrationPhaseV1::RefCommitStarted,
            false,
            request.observed_at,
        )?;
        let record = NativeIntegrationRecordV1 {
            status: commit_started,
            ..record
        };
        let effect = self.native.apply(&record.preview, &cancellation);
        let receipt = match effect {
            Err(error) => Err(error),
            Ok(NativeApplyEffectV1::Committed {
                new_tip,
                final_tree,
                final_index_digest,
                final_worktree_digest,
            }) => self.finish_from_probe(
                &record,
                NativeIntegrationProbeV1::CommittedState {
                    tip: new_tip,
                    tree: final_tree,
                    index_digest: final_index_digest,
                    worktree_digest: final_worktree_digest,
                },
                request.observed_at,
            ),
            Ok(NativeApplyEffectV1::FailedNoChange) => {
                self.finish_from_probe(&record, self.native.probe(&record)?, request.observed_at)
            }
            Ok(NativeApplyEffectV1::UnknownAfterCommitPoint { candidate_tip }) => {
                let probe = self.native.probe(&record)?;
                if matches!(probe, NativeIntegrationProbeV1::Diverged)
                    && let Some(candidate_tip) = candidate_tip
                {
                    let rolled_back = self.native.rollback(&record, &candidate_tip)?;
                    self.finish_rolled_back_or_inspect(&record, rolled_back, request.observed_at)
                } else {
                    self.finish_from_probe(&record, probe, request.observed_at)
                }
            }
        };
        self.clear_cancellation(&request.transaction_id)?;
        receipt
    }

    fn status(
        &self,
        request: &NativeIntegrationStatusRequestV1,
    ) -> Result<Option<NativeIntegrationTransactionStatusV1>, NativeIntegrationPortError> {
        self.store
            .read_status(&request.transaction_id)
            .map_err(map_store_error)
    }

    fn cancel(
        &self,
        request: &NativeIntegrationCancelRequestV1,
    ) -> Result<NativeIntegrationCancelDispositionV1, NativeIntegrationPortError> {
        let Some(status) = self
            .store
            .read_status(&request.transaction_id)
            .map_err(map_store_error)?
        else {
            return Ok(NativeIntegrationCancelDispositionV1::UnknownTransaction);
        };
        if let Some(outcome) = status.terminal_outcome {
            return Ok(NativeIntegrationCancelDispositionV1::AlreadyTerminal(
                outcome,
            ));
        }
        if status.phase >= NativeIntegrationPhaseV1::RefCommitStarted {
            return Ok(NativeIntegrationCancelDispositionV1::CommitPointPassed);
        }
        if let Some(cancellation) = self
            .cancellations
            .lock()
            .map_err(lock_error)?
            .get(&request.transaction_id)
        {
            cancellation.cancel();
        }
        let mut replacement = status.clone();
        replacement.phase_revision = replacement.phase_revision.saturating_add(1);
        replacement.cancellation_requested = true;
        replacement.updated_at = request.requested_at;
        self.store
            .compare_and_swap_status(&request.transaction_id, status.phase_revision, replacement)
            .map_err(map_store_error)?;
        Ok(NativeIntegrationCancelDispositionV1::CancellationRequested)
    }

    fn recover(
        &self,
        request: &NativeIntegrationRecoveryRequestV1,
    ) -> Result<NativeIntegrationReceiptV1, NativeIntegrationPortError> {
        if let Some(receipt) = self
            .store
            .read_receipt(&request.transaction_id)
            .map_err(map_store_error)?
        {
            return Ok(receipt);
        }
        let record = self
            .store
            .read_record(&request.transaction_id)
            .map_err(map_store_error)?
            .ok_or(NativeIntegrationPortError::Stale)?;
        let probe = self.native.probe(&record)?;
        self.finish_from_probe(&record, probe, request.observed_at)
    }
}

impl<S, T, N, A> NativeIntegrationTransactionCoordinator<S, T, N, A>
where
    S: NativeIntegrationStore,
    N: NativeIntegrationMechanics,
{
    fn clear_cancellation(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> Result<(), NativeIntegrationPortError> {
        self.cancellations
            .lock()
            .map_err(lock_error)?
            .remove(transaction_id);
        Ok(())
    }

    fn finish_rolled_back_or_inspect(
        &self,
        record: &NativeIntegrationRecordV1,
        probe: NativeIntegrationProbeV1,
        observed_at: UtcMicros,
    ) -> Result<NativeIntegrationReceiptV1, NativeIntegrationPortError> {
        match probe {
            NativeIntegrationProbeV1::OldState {
                tip,
                tree,
                index_digest,
                worktree_digest,
            } => self.write_terminal(
                record,
                NativeIntegrationTerminalOutcomeV1::RolledBack,
                tip,
                tree,
                index_digest,
                worktree_digest,
                observed_at,
            ),
            _ => self.needs_inspection(record, observed_at),
        }
    }

    fn finish_from_probe(
        &self,
        record: &NativeIntegrationRecordV1,
        probe: NativeIntegrationProbeV1,
        observed_at: UtcMicros,
    ) -> Result<NativeIntegrationReceiptV1, NativeIntegrationPortError> {
        match probe {
            NativeIntegrationProbeV1::OldState {
                tip,
                tree,
                index_digest,
                worktree_digest,
            } => self.write_terminal(
                record,
                NativeIntegrationTerminalOutcomeV1::AbortedNoChange,
                tip,
                tree,
                index_digest,
                worktree_digest,
                observed_at,
            ),
            NativeIntegrationProbeV1::CommittedState {
                tip,
                tree,
                index_digest,
                worktree_digest,
            } => self.write_terminal(
                record,
                NativeIntegrationTerminalOutcomeV1::Committed,
                tip,
                tree,
                index_digest,
                worktree_digest,
                observed_at,
            ),
            NativeIntegrationProbeV1::Diverged | NativeIntegrationProbeV1::Unavailable => {
                self.needs_inspection(record, observed_at)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_terminal(
        &self,
        record: &NativeIntegrationRecordV1,
        outcome: NativeIntegrationTerminalOutcomeV1,
        tip: GitOidV1,
        tree: GitOidV1,
        index_digest: ManifestDigest,
        worktree_digest: ManifestDigest,
        observed_at: UtcMicros,
    ) -> Result<NativeIntegrationReceiptV1, NativeIntegrationPortError> {
        let mut status = if record.status.phase >= NativeIntegrationPhaseV1::RefCommitStarted
            && record.status.phase < NativeIntegrationPhaseV1::FinalStateVerification
        {
            advance_status(
                self.store.as_ref(),
                &record.status,
                NativeIntegrationPhaseV1::FinalStateVerification,
                record.status.cancellation_requested,
                observed_at,
            )?
        } else {
            record.status.clone()
        };
        let expected_phase_revision = status.phase_revision;
        status.phase = NativeIntegrationPhaseV1::Terminal;
        status.phase_revision = status.phase_revision.saturating_add(1);
        status.terminal_outcome = Some(outcome);
        status.updated_at = observed_at;
        let receipt = NativeIntegrationReceiptV1 {
            status,
            final_ref_tip: tip,
            final_tree: tree,
            final_index_digest: index_digest,
            final_worktree_digest: worktree_digest,
            completed_at: observed_at,
            receipt_digest: placeholder_digest()?,
        }
        .seal()
        .map_err(domain_error)?;
        self.store
            .write_terminal(
                &record.status.transaction_id,
                expected_phase_revision,
                receipt,
            )
            .map_err(map_store_error)
    }

    fn needs_inspection(
        &self,
        record: &NativeIntegrationRecordV1,
        observed_at: UtcMicros,
    ) -> Result<NativeIntegrationReceiptV1, NativeIntegrationPortError> {
        self.store
            .quarantine_repository(&record.status.repository_id, &record.status.transaction_id)
            .map_err(map_store_error)?;
        self.write_terminal(
            record,
            NativeIntegrationTerminalOutcomeV1::NeedsInspection,
            record.status.expected_destination_tip.clone(),
            record.preview.repository_snapshot.destination_tree.clone(),
            record.preview.repository_snapshot.index_digest.clone(),
            record.preview.repository_snapshot.worktree_digest.clone(),
            observed_at,
        )
    }
}

fn advance_status<S: NativeIntegrationStore>(
    store: &S,
    current: &NativeIntegrationTransactionStatusV1,
    phase: NativeIntegrationPhaseV1,
    cancellation_requested: bool,
    observed_at: UtcMicros,
) -> Result<NativeIntegrationTransactionStatusV1, NativeIntegrationPortError> {
    let mut replacement = current.clone();
    replacement.phase = phase;
    replacement.phase_revision = replacement.phase_revision.saturating_add(1);
    replacement.cancellation_requested |= cancellation_requested;
    replacement.updated_at = observed_at;
    store
        .compare_and_swap_status(&current.transaction_id, current.phase_revision, replacement)
        .map_err(map_store_error)
}

fn map_store_error(error: NativeIntegrationStoreError) -> NativeIntegrationPortError {
    match error {
        NativeIntegrationStoreError::PreviewConflict
        | NativeIntegrationStoreError::TransactionConflict
        | NativeIntegrationStoreError::StatusConflict
        | NativeIntegrationStoreError::ReceiptConflict => {
            NativeIntegrationPortError::TransactionConflict
        }
        NativeIntegrationStoreError::ApprovalConflict => {
            NativeIntegrationPortError::ApprovalConflict
        }
        NativeIntegrationStoreError::RepositoryQuarantined => {
            NativeIntegrationPortError::NeedsInspection
        }
        NativeIntegrationStoreError::Unavailable => NativeIntegrationPortError::Unavailable,
        NativeIntegrationStoreError::ResetRequired => NativeIntegrationPortError::ResetRequired,
        NativeIntegrationStoreError::DurabilityUncertain => {
            NativeIntegrationPortError::DurabilityUncertain
        }
        NativeIntegrationStoreError::InvalidData(detail) => {
            NativeIntegrationPortError::Native(detail)
        }
    }
}

fn lock_error<T>(error: std::sync::PoisonError<T>) -> NativeIntegrationPortError {
    NativeIntegrationPortError::Native(format!("native integration lock poisoned: {error}"))
}

fn domain_error(error: tracedecay_domain::DomainError) -> NativeIntegrationPortError {
    NativeIntegrationPortError::Native(error.to_string())
}

fn placeholder_digest() -> Result<ManifestDigest, NativeIntegrationPortError> {
    tracedecay_domain::canonical_sha256(&"pending native integration receipt").map_err(domain_error)
}
