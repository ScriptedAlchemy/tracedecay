#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    CancellationSignal, NativeIntegrationApplyRequestV1, NativeIntegrationCancelDispositionV1,
    NativeIntegrationCancelRequestV1, NativeIntegrationPort, NativeIntegrationPortError,
    NativeIntegrationPreflightRequestV1, NativeIntegrationStackResolutionOutcomeV1,
    NativeIntegrationStackResolutionPort, NativeIntegrationStatusRequestV1,
};
use tracedecay_domain::{
    GitOidV1, ManifestDigest, NativeIntegrationApprovalId, NativeIntegrationPhaseV1,
    NativeIntegrationPreviewId, NativeIntegrationSelectionV1, NativeIntegrationTerminalOutcomeV1,
    NativeIntegrationTransactionId, NativeIntegrationTransactionStatusV1, RefId, RepositoryId,
    UtcMicros,
};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_store::{
    NativeIntegrationBeginResultV1, NativeIntegrationRecordV1, NativeIntegrationStore,
    NativeIntegrationStoreError, NativeIntegrationStoreResult,
};

use super::{
    NativeApplyEffectV1, NativeIntegrationAuthorizationOutcomeV1,
    NativeIntegrationAuthorizationPort, NativeIntegrationMechanics, NativeIntegrationProbeV1,
    NativeIntegrationTransactionCoordinator,
};

#[derive(Default)]
struct StatusStore {
    statuses: Mutex<BTreeMap<NativeIntegrationTransactionId, NativeIntegrationTransactionStatusV1>>,
}

impl NativeIntegrationStore for StatusStore {
    fn save_preview(
        &self,
        _preview: tracedecay_domain::NativeIntegrationPreviewV1,
    ) -> NativeIntegrationStoreResult<()> {
        Err(NativeIntegrationStoreError::Unavailable)
    }

    fn read_preview(
        &self,
        _preview_id: &NativeIntegrationPreviewId,
    ) -> NativeIntegrationStoreResult<Option<tracedecay_domain::NativeIntegrationPreviewV1>> {
        Ok(None)
    }

    fn begin_or_replay(
        &self,
        _record: NativeIntegrationRecordV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationBeginResultV1> {
        Err(NativeIntegrationStoreError::Unavailable)
    }

    fn read_status(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationTransactionStatusV1>> {
        Ok(self.statuses.lock().unwrap().get(transaction_id).cloned())
    }

    fn read_record(
        &self,
        _transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationRecordV1>> {
        Ok(None)
    }

    fn read_receipt(
        &self,
        _transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<tracedecay_domain::NativeIntegrationReceiptV1>> {
        Ok(None)
    }

    fn compare_and_swap_status(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
        expected_phase_revision: u64,
        replacement: NativeIntegrationTransactionStatusV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationTransactionStatusV1> {
        let mut statuses = self.statuses.lock().unwrap();
        let current = statuses
            .get(transaction_id)
            .ok_or(NativeIntegrationStoreError::StatusConflict)?;
        if current.phase_revision != expected_phase_revision
            || replacement.transaction_id != *transaction_id
            || replacement.phase_revision != expected_phase_revision.saturating_add(1)
        {
            return Err(NativeIntegrationStoreError::StatusConflict);
        }
        replacement.validate()?;
        statuses.insert(transaction_id.clone(), replacement.clone());
        Ok(replacement)
    }

    fn write_terminal(
        &self,
        _transaction_id: &NativeIntegrationTransactionId,
        _expected_phase_revision: u64,
        _receipt: tracedecay_domain::NativeIntegrationReceiptV1,
    ) -> NativeIntegrationStoreResult<tracedecay_domain::NativeIntegrationReceiptV1> {
        Err(NativeIntegrationStoreError::Unavailable)
    }

    fn pending_transactions(
        &self,
        _repository_id: Option<&RepositoryId>,
    ) -> NativeIntegrationStoreResult<Vec<NativeIntegrationRecordV1>> {
        Ok(Vec::new())
    }

    fn approval_consumed(
        &self,
        _approval_id: &NativeIntegrationApprovalId,
    ) -> NativeIntegrationStoreResult<bool> {
        Ok(false)
    }

    fn quarantine_repository(
        &self,
        _repository_id: &RepositoryId,
        _transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<()> {
        Err(NativeIntegrationStoreError::Unavailable)
    }

    fn begin_worktree_cleanup(
        &self,
        _transaction: tracedecay_domain::NativeWorktreeCleanupTransactionV1,
    ) -> NativeIntegrationStoreResult<tracedecay_store::NativeWorktreeCleanupBeginResultV1> {
        Err(NativeIntegrationStoreError::Unavailable)
    }

    fn read_worktree_cleanup(
        &self,
        _confirmation_digest: &ManifestDigest,
    ) -> NativeIntegrationStoreResult<Option<tracedecay_domain::NativeWorktreeCleanupTransactionV1>>
    {
        Ok(None)
    }

    fn compare_and_swap_worktree_cleanup(
        &self,
        _confirmation_digest: &ManifestDigest,
        _expected_phase_revision: u64,
        _replacement: tracedecay_domain::NativeWorktreeCleanupTransactionV1,
    ) -> NativeIntegrationStoreResult<tracedecay_domain::NativeWorktreeCleanupTransactionV1> {
        Err(NativeIntegrationStoreError::Unavailable)
    }

    fn write_worktree_cleanup_terminal(
        &self,
        _confirmation_digest: &ManifestDigest,
        _expected_phase_revision: u64,
        _receipt: tracedecay_domain::NativeWorktreeCleanupReceiptV1,
    ) -> NativeIntegrationStoreResult<tracedecay_domain::NativeWorktreeCleanupReceiptV1> {
        Err(NativeIntegrationStoreError::Unavailable)
    }
}

struct UnusedTopology;

impl NativeIntegrationStackResolutionPort for UnusedTopology {
    fn resolve(
        &self,
        _request: &tracedecay_application::NativeIntegrationStackResolutionRequestV1,
        _cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationStackResolutionOutcomeV1, NativeIntegrationPortError> {
        Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable)
    }
}

struct UnusedMechanics;

impl NativeIntegrationMechanics for UnusedMechanics {
    fn preflight(
        &self,
        _selection: &NativeIntegrationSelectionV1,
        _request: &NativeIntegrationPreflightRequestV1,
        _cancellation: &CancellationToken,
    ) -> Result<tracedecay_domain::NativeIntegrationPreviewV1, NativeIntegrationPortError> {
        Err(NativeIntegrationPortError::Unavailable)
    }

    fn apply(
        &self,
        _preview: &tracedecay_domain::NativeIntegrationPreviewV1,
        _cancellation: &CancellationToken,
    ) -> Result<NativeApplyEffectV1, NativeIntegrationPortError> {
        Err(NativeIntegrationPortError::Unavailable)
    }

    fn probe(
        &self,
        _record: &NativeIntegrationRecordV1,
    ) -> Result<NativeIntegrationProbeV1, NativeIntegrationPortError> {
        Err(NativeIntegrationPortError::Unavailable)
    }

    fn rollback(
        &self,
        _record: &NativeIntegrationRecordV1,
        _committed_tip: &GitOidV1,
    ) -> Result<NativeIntegrationProbeV1, NativeIntegrationPortError> {
        Err(NativeIntegrationPortError::Unavailable)
    }
}

struct UnusedAuthorization;

impl NativeIntegrationAuthorizationPort for UnusedAuthorization {
    fn authorize_preflight(
        &self,
        _request: &NativeIntegrationPreflightRequestV1,
    ) -> NativeIntegrationAuthorizationOutcomeV1 {
        NativeIntegrationAuthorizationOutcomeV1::Unavailable
    }

    fn authorize_apply(
        &self,
        _request: &NativeIntegrationApplyRequestV1,
        _before_ref_commit: bool,
    ) -> NativeIntegrationAuthorizationOutcomeV1 {
        NativeIntegrationAuthorizationOutcomeV1::Unavailable
    }
}

fn status(
    transaction: &str,
    phase: NativeIntegrationPhaseV1,
    terminal_outcome: Option<NativeIntegrationTerminalOutcomeV1>,
) -> NativeIntegrationTransactionStatusV1 {
    NativeIntegrationTransactionStatusV1 {
        transaction_id: NativeIntegrationTransactionId::new(transaction).unwrap(),
        preview_id: NativeIntegrationPreviewId::new(format!("preview.{transaction}")).unwrap(),
        preview_digest: ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        approval_id: NativeIntegrationApprovalId::new(format!("approval.{transaction}")).unwrap(),
        repository_id: RepositoryId::new("repository.cancel").unwrap(),
        destination_ref: RefId::new("refs/heads/main").unwrap(),
        expected_destination_tip: GitOidV1::new("a".repeat(40)).unwrap(),
        candidate_tip: None,
        phase,
        phase_revision: 1,
        cancellation_requested: false,
        terminal_outcome,
        updated_at: UtcMicros(1),
    }
}

#[test]
fn cancellation_is_durable_only_before_the_ref_commit_boundary() {
    let store = Arc::new(StatusStore::default());
    for status in [
        status(
            "transaction.prepared",
            NativeIntegrationPhaseV1::Prepared,
            None,
        ),
        status(
            "transaction.commit-point",
            NativeIntegrationPhaseV1::RefCommitStarted,
            None,
        ),
        status(
            "transaction.terminal",
            NativeIntegrationPhaseV1::Terminal,
            Some(NativeIntegrationTerminalOutcomeV1::Committed),
        ),
    ] {
        store
            .statuses
            .lock()
            .unwrap()
            .insert(status.transaction_id.clone(), status);
    }
    let coordinator = NativeIntegrationTransactionCoordinator::new(
        store.clone(),
        Arc::new(UnusedTopology),
        Arc::new(UnusedMechanics),
        Arc::new(UnusedAuthorization),
    );

    let prepared = NativeIntegrationTransactionId::new("transaction.prepared").unwrap();
    assert_eq!(
        coordinator
            .cancel(&NativeIntegrationCancelRequestV1 {
                transaction_id: prepared.clone(),
                requested_at: UtcMicros(2),
            })
            .unwrap(),
        NativeIntegrationCancelDispositionV1::CancellationRequested
    );
    let durable = coordinator
        .status(&NativeIntegrationStatusRequestV1 {
            transaction_id: prepared,
        })
        .unwrap()
        .unwrap();
    assert!(durable.cancellation_requested);
    assert_eq!(durable.phase_revision, 2);

    assert_eq!(
        coordinator
            .cancel(&NativeIntegrationCancelRequestV1 {
                transaction_id: NativeIntegrationTransactionId::new("transaction.commit-point")
                    .unwrap(),
                requested_at: UtcMicros(2),
            })
            .unwrap(),
        NativeIntegrationCancelDispositionV1::CommitPointPassed
    );
    assert_eq!(
        coordinator
            .cancel(&NativeIntegrationCancelRequestV1 {
                transaction_id: NativeIntegrationTransactionId::new("transaction.terminal")
                    .unwrap(),
                requested_at: UtcMicros(2),
            })
            .unwrap(),
        NativeIntegrationCancelDispositionV1::AlreadyTerminal(
            NativeIntegrationTerminalOutcomeV1::Committed
        )
    );
    assert_eq!(
        coordinator
            .cancel(&NativeIntegrationCancelRequestV1 {
                transaction_id: NativeIntegrationTransactionId::new("transaction.unknown").unwrap(),
                requested_at: UtcMicros(2),
            })
            .unwrap(),
        NativeIntegrationCancelDispositionV1::UnknownTransaction
    );
}
