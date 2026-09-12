#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tracedecay_contracts::{
    CancellationContext, CancellationSignal, CapabilityGrantId, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, NativeIntegrationApplyRequestV1, NativeIntegrationCancelDispositionV1,
    NativeIntegrationCancelRequestV1, NativeIntegrationPort, NativeIntegrationPortError,
    NativeIntegrationPreflightRequestV1, NativeIntegrationStackResolutionOutcomeV1,
    NativeIntegrationStackResolutionPort, NativeIntegrationStatusRequestV1, RequestContext,
    RequestId, ResolvedScope, native_integration_surface_operation,
};
use tracedecay_domain::{
    ActorId, CapabilityId, CodeGenerationId, ContentDigest, FrozenIndependentBranchSelectionV1,
    GitHeadStateV1, GitObjectFormatV1, GitOidV1, GitOperationStateV1, ManifestDigest,
    MechanicalIntegrationModeV1, NativeIntegrationAnalysisCoverageV1,
    NativeIntegrationAnalysisLaneV1, NativeIntegrationAnalysisReportV1,
    NativeIntegrationApprovalId, NativeIntegrationApprovalV1, NativeIntegrationGenerationBindingV1,
    NativeIntegrationPhaseV1, NativeIntegrationPreviewDispositionV1, NativeIntegrationPreviewId,
    NativeIntegrationPreviewV1, NativeIntegrationRepositorySnapshotV1,
    NativeIntegrationSelectionV1, NativeIntegrationTerminalOutcomeV1,
    NativeIntegrationTransactionId, NativeIntegrationTransactionStatusV1, ProjectId, RefId,
    RepositoryId, UtcMicros, WorktreeId, WorktreeInventoryEpoch, WorktreeInventorySnapshotId,
    canonical_sha256,
};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_store::{
    NativeIntegrationBeginResultV1, NativeIntegrationRecordV1, NativeIntegrationStore,
    NativeIntegrationStoreError, NativeIntegrationStoreResult,
};

use super::{
    NativeApplyEffectV1, NativeIntegrationAnalysisRevalidationV1,
    NativeIntegrationAuthorizationOutcomeV1, NativeIntegrationAuthorizationPort,
    NativeIntegrationMechanics, NativeIntegrationProbeV1, NativeIntegrationTransactionCoordinator,
};

#[derive(Default)]
struct StatusStore {
    statuses: Mutex<BTreeMap<NativeIntegrationTransactionId, NativeIntegrationTransactionStatusV1>>,
    records: Mutex<BTreeMap<NativeIntegrationTransactionId, NativeIntegrationRecordV1>>,
    receipts: Mutex<
        BTreeMap<NativeIntegrationTransactionId, tracedecay_domain::NativeIntegrationReceiptV1>,
    >,
    quarantined: Mutex<Vec<NativeIntegrationTransactionId>>,
}

impl NativeIntegrationStore for StatusStore {
    fn save_preview(
        &self,
        _preview: tracedecay_domain::NativeIntegrationPreviewV1,
    ) -> NativeIntegrationStoreResult<()> {
        Err(NativeIntegrationStoreError::Unavailable(
            "fixture store is unavailable".to_owned(),
        ))
    }

    fn read_preview(
        &self,
        _preview_id: &NativeIntegrationPreviewId,
    ) -> NativeIntegrationStoreResult<Option<tracedecay_domain::NativeIntegrationPreviewV1>> {
        Ok(None)
    }

    fn begin_or_replay(
        &self,
        record: NativeIntegrationRecordV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationBeginResultV1> {
        let transaction_id = record.status.transaction_id.clone();
        self.statuses
            .lock()
            .unwrap()
            .insert(transaction_id.clone(), record.status.clone());
        self.records
            .lock()
            .unwrap()
            .insert(transaction_id, record.clone());
        Ok(NativeIntegrationBeginResultV1::Started(Box::new(record)))
    }

    fn read_status(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationTransactionStatusV1>> {
        Ok(self.statuses.lock().unwrap().get(transaction_id).cloned())
    }

    fn read_record(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationRecordV1>> {
        Ok(self.records.lock().unwrap().get(transaction_id).cloned())
    }

    fn read_receipt(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<tracedecay_domain::NativeIntegrationReceiptV1>> {
        Ok(self.receipts.lock().unwrap().get(transaction_id).cloned())
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
        if let Some(record) = self.records.lock().unwrap().get_mut(transaction_id) {
            record.status = replacement.clone();
        }
        Ok(replacement)
    }

    fn write_terminal(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
        expected_phase_revision: u64,
        receipt: tracedecay_domain::NativeIntegrationReceiptV1,
    ) -> NativeIntegrationStoreResult<tracedecay_domain::NativeIntegrationReceiptV1> {
        let mut statuses = self.statuses.lock().unwrap();
        let status = statuses
            .get(transaction_id)
            .ok_or(NativeIntegrationStoreError::StatusConflict)?;
        if status.phase_revision != expected_phase_revision {
            return Err(NativeIntegrationStoreError::StatusConflict);
        }
        statuses.insert(transaction_id.clone(), receipt.status.clone());
        let mut records = self.records.lock().unwrap();
        let record = records
            .get_mut(transaction_id)
            .ok_or(NativeIntegrationStoreError::StatusConflict)?;
        record.status = receipt.status.clone();
        record.terminal_receipt = Some(receipt.clone());
        self.receipts
            .lock()
            .unwrap()
            .insert(transaction_id.clone(), receipt.clone());
        Ok(receipt)
    }

    fn pending_transactions(
        &self,
        _repository_id: Option<&RepositoryId>,
    ) -> NativeIntegrationStoreResult<Vec<NativeIntegrationRecordV1>> {
        Ok(Vec::new())
    }

    fn live_candidate_generation_bindings(
        &self,
        _repository_id: &RepositoryId,
        _observed_at: UtcMicros,
    ) -> NativeIntegrationStoreResult<Vec<tracedecay_domain::CodeGenerationId>> {
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
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<()> {
        self.quarantined
            .lock()
            .unwrap()
            .push(transaction_id.clone());
        Ok(())
    }

    fn begin_worktree_cleanup(
        &self,
        _transaction: tracedecay_domain::NativeWorktreeCleanupTransactionV1,
    ) -> NativeIntegrationStoreResult<tracedecay_store::NativeWorktreeCleanupBeginResultV1> {
        Err(NativeIntegrationStoreError::Unavailable(
            "fixture store is unavailable".to_owned(),
        ))
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
        Err(NativeIntegrationStoreError::Unavailable(
            "fixture store is unavailable".to_owned(),
        ))
    }

    fn write_worktree_cleanup_terminal(
        &self,
        _confirmation_digest: &ManifestDigest,
        _expected_phase_revision: u64,
        _receipt: tracedecay_domain::NativeWorktreeCleanupReceiptV1,
    ) -> NativeIntegrationStoreResult<tracedecay_domain::NativeWorktreeCleanupReceiptV1> {
        Err(NativeIntegrationStoreError::Unavailable(
            "fixture store is unavailable".to_owned(),
        ))
    }
}

struct UnusedTopology;

impl NativeIntegrationStackResolutionPort for UnusedTopology {
    fn resolve(
        &self,
        _request: &tracedecay_contracts::NativeIntegrationStackResolutionRequestV1,
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
        _cancellation_signal: &CancellationSignal,
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

    fn revalidate_analysis(
        &self,
        _preview: &tracedecay_domain::NativeIntegrationPreviewV1,
        _deadline: &tracedecay_contracts::Deadline,
        _cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationAnalysisRevalidationV1, NativeIntegrationPortError> {
        Ok(NativeIntegrationAnalysisRevalidationV1::Current)
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

struct Authorized;

impl NativeIntegrationAuthorizationPort for Authorized {
    fn authorize_preflight(
        &self,
        _request: &NativeIntegrationPreflightRequestV1,
    ) -> NativeIntegrationAuthorizationOutcomeV1 {
        NativeIntegrationAuthorizationOutcomeV1::Authorized
    }

    fn authorize_apply(
        &self,
        _request: &NativeIntegrationApplyRequestV1,
        _before_ref_commit: bool,
    ) -> NativeIntegrationAuthorizationOutcomeV1 {
        NativeIntegrationAuthorizationOutcomeV1::Authorized
    }
}

struct ControlledMechanics {
    probe: Result<NativeIntegrationProbeV1, NativeIntegrationPortError>,
    apply: Result<NativeApplyEffectV1, NativeIntegrationPortError>,
}

struct DurableCancelDuringRevalidation {
    store: Arc<StatusStore>,
    revalidations: AtomicUsize,
    probe: NativeIntegrationProbeV1,
}

impl NativeIntegrationMechanics for DurableCancelDuringRevalidation {
    fn preflight(
        &self,
        _selection: &NativeIntegrationSelectionV1,
        _request: &NativeIntegrationPreflightRequestV1,
        _cancellation_signal: &CancellationSignal,
        _cancellation: &CancellationToken,
    ) -> Result<NativeIntegrationPreviewV1, NativeIntegrationPortError> {
        Err(NativeIntegrationPortError::Unavailable)
    }

    fn apply(
        &self,
        _preview: &NativeIntegrationPreviewV1,
        _cancellation: &CancellationToken,
    ) -> Result<NativeApplyEffectV1, NativeIntegrationPortError> {
        Err(NativeIntegrationPortError::Unavailable)
    }

    fn revalidate_analysis(
        &self,
        _preview: &NativeIntegrationPreviewV1,
        _deadline: &Deadline,
        _cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationAnalysisRevalidationV1, NativeIntegrationPortError> {
        if self.revalidations.fetch_add(1, Ordering::SeqCst) == 1 {
            let transaction_id = self
                .store
                .statuses
                .lock()
                .unwrap()
                .keys()
                .next()
                .cloned()
                .expect("durable transaction");
            let status = self
                .store
                .read_status(&transaction_id)
                .unwrap()
                .expect("durable status");
            let mut cancelled = status.clone();
            cancelled.phase_revision = cancelled.phase_revision.saturating_add(1);
            cancelled.cancellation_requested = true;
            self.store
                .compare_and_swap_status(&transaction_id, status.phase_revision, cancelled)
                .expect("concurrent durable cancellation");
            return Ok(NativeIntegrationAnalysisRevalidationV1::Stale);
        }
        Ok(NativeIntegrationAnalysisRevalidationV1::Current)
    }

    fn probe(
        &self,
        _record: &NativeIntegrationRecordV1,
    ) -> Result<NativeIntegrationProbeV1, NativeIntegrationPortError> {
        Ok(self.probe.clone())
    }

    fn rollback(
        &self,
        _record: &NativeIntegrationRecordV1,
        _committed_tip: &GitOidV1,
    ) -> Result<NativeIntegrationProbeV1, NativeIntegrationPortError> {
        Err(NativeIntegrationPortError::Unavailable)
    }
}

impl NativeIntegrationMechanics for ControlledMechanics {
    fn preflight(
        &self,
        _selection: &NativeIntegrationSelectionV1,
        _request: &NativeIntegrationPreflightRequestV1,
        _cancellation_signal: &CancellationSignal,
        _cancellation: &CancellationToken,
    ) -> Result<NativeIntegrationPreviewV1, NativeIntegrationPortError> {
        Err(NativeIntegrationPortError::Unavailable)
    }

    fn apply(
        &self,
        _preview: &NativeIntegrationPreviewV1,
        _cancellation: &CancellationToken,
    ) -> Result<NativeApplyEffectV1, NativeIntegrationPortError> {
        self.apply.clone()
    }

    fn revalidate_analysis(
        &self,
        _preview: &NativeIntegrationPreviewV1,
        _deadline: &Deadline,
        _cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationAnalysisRevalidationV1, NativeIntegrationPortError> {
        Ok(NativeIntegrationAnalysisRevalidationV1::Current)
    }

    fn probe(
        &self,
        _record: &NativeIntegrationRecordV1,
    ) -> Result<NativeIntegrationProbeV1, NativeIntegrationPortError> {
        self.probe.clone()
    }

    fn rollback(
        &self,
        _record: &NativeIntegrationRecordV1,
        _committed_tip: &GitOidV1,
    ) -> Result<NativeIntegrationProbeV1, NativeIntegrationPortError> {
        Err(NativeIntegrationPortError::Unavailable)
    }
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
}

fn oid(byte: char) -> GitOidV1 {
    GitOidV1::new(byte.to_string().repeat(40)).expect("object id")
}

fn apply_fixture(
    transaction: &str,
) -> (
    NativeIntegrationApplyRequestV1,
    NativeIntegrationTransactionId,
) {
    let project_id = ProjectId::new("project.native.transaction").expect("project id");
    let repository_id = RepositoryId::new("repository.native.transaction").expect("repository id");
    let source_ref = RefId::new("refs/heads/source").expect("source ref");
    let destination_ref = RefId::new("refs/heads/destination").expect("destination ref");
    let scope = ResolvedScope::new(
        project_id.clone(),
        repository_id.clone(),
        WorktreeId::new("worktree.native.transaction").expect("worktree id"),
        Some(destination_ref.clone()),
    )
    .expect("scope");
    let operation = native_integration_surface_operation(
        tracedecay_contracts::NATIVE_INTEGRATION_APPLY_OPERATION,
    )
    .expect("operation lookup")
    .expect("declared operation");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.native.transaction").expect("grant id"),
        1,
        digest('a'),
        ActorId::new("actor.native.transaction").expect("actor"),
        UtcMicros(1),
        UtcMicros(i64::MAX),
        scope.clone(),
        std::collections::BTreeSet::from([operation.capability_id().clone()]),
        std::collections::BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Sensitive,
    )
    .expect("grant");
    let context = RequestContext::new(
        ActorId::new("actor.native.transaction").expect("actor"),
        scope,
        grant,
        RequestId::new(format!("request.{transaction}")).expect("request id"),
        Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        CancellationContext::active(format!("cancel.{transaction}")).expect("cancellation"),
    )
    .expect("context");
    let selection = FrozenIndependentBranchSelectionV1::new(
        project_id.clone(),
        repository_id.clone(),
        WorktreeInventorySnapshotId::new("inventory.native.transaction").expect("inventory"),
        WorktreeInventoryEpoch::new(1).expect("inventory epoch"),
        None,
        None,
        source_ref.clone(),
        destination_ref.clone(),
        oid('1'),
        oid('2'),
        digest('3'),
        UtcMicros(5),
    )
    .expect("selection");
    let snapshot = NativeIntegrationRepositorySnapshotV1 {
        project_id: project_id.clone(),
        repository_id: repository_id.clone(),
        source_worktree_id: None,
        destination_worktree_id: None,
        source_ref,
        destination_ref,
        source_tip: oid('1'),
        destination_tip: oid('2'),
        source_tree: oid('4'),
        destination_tree: oid('5'),
        merge_base: oid('6'),
        dependency_commits: vec![oid('1')],
        destination_head: GitHeadStateV1::Detached { commit: oid('2') },
        refs_digest: digest('7'),
        index_digest: digest('8'),
        worktree_digest: digest('9'),
        attributes_digest: digest('a'),
        operation_state: GitOperationStateV1::None,
        clean: true,
        object_format: GitObjectFormatV1::Sha1,
        adapter_revision: "transaction-test.v1".to_owned(),
        captured_at: UtcMicros(6),
        digest: digest('b'),
    }
    .seal()
    .expect("snapshot");
    let binding = |name: &str, source_revision: Option<GitOidV1>, source_tree: GitOidV1| {
        NativeIntegrationGenerationBindingV1 {
            generation_id: CodeGenerationId::new(format!("generation.{name}"))
                .expect("generation id"),
            project_id: project_id.clone(),
            repository_id: repository_id.clone(),
            worktree_id: None,
            reference: Some(if matches!(name, "base" | "source") {
                snapshot.source_ref.clone()
            } else {
                snapshot.destination_ref.clone()
            }),
            snapshot_digest: digest('c'),
            content_identity: ContentDigest::new(digest('d').as_str().to_owned())
                .expect("content identity"),
            source_revision,
            source_tree,
            seal_digest: digest('e'),
        }
    };
    let complete = NativeIntegrationAnalysisLaneV1 {
        coverage: NativeIntegrationAnalysisCoverageV1::Complete,
        gaps: Vec::new(),
    };
    let analysis = NativeIntegrationAnalysisReportV1 {
        merge_base: binding("base", Some(snapshot.merge_base.clone()), oid('6')),
        source: binding("source", Some(snapshot.source_tip.clone()), oid('4')),
        destination: binding(
            "destination",
            Some(snapshot.destination_tip.clone()),
            oid('5'),
        ),
        candidate: binding("candidate", None, oid('4')),
        graph: complete.clone(),
        tests: complete.clone(),
        schema: complete.clone(),
        migrations: complete,
        conflicts: Vec::new(),
        analyzer_revision: "transaction-test.v1".to_owned(),
        digest: digest('f'),
    }
    .seal()
    .expect("analysis");
    let preview = NativeIntegrationPreviewV1 {
        preview_id: NativeIntegrationPreviewId::new(format!("preview.{transaction}"))
            .expect("preview id"),
        selection: NativeIntegrationSelectionV1::IndependentBranch(selection),
        repository_snapshot: snapshot,
        grant_digest: digest('a'),
        policy_digest: digest('f'),
        analysis: Some(analysis),
        disposition: NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(
            MechanicalIntegrationModeV1::FastForward,
        ),
        candidate_tree: Some(oid('4')),
        ordered_commits: vec![oid('1')],
        created_at: UtcMicros(10),
        expires_at: UtcMicros(i64::MAX),
        preview_digest: digest('0'),
    }
    .seal()
    .expect("preview");
    let approval = NativeIntegrationApprovalV1 {
        approval_id: NativeIntegrationApprovalId::new(format!("approval.{transaction}"))
            .expect("approval id"),
        preview_id: preview.preview_id.clone(),
        preview_digest: preview.preview_digest.clone(),
        principal: ActorId::new("actor.native.transaction").expect("principal"),
        delegated_agent: None,
        capability: CapabilityId::new(operation.capability_id().as_str().to_owned())
            .expect("capability"),
        grant_digest: preview.grant_digest.clone(),
        issued_at: UtcMicros(11),
        expires_at: UtcMicros(i64::MAX),
        approval_digest: canonical_sha256(&"transaction approval").expect("approval digest"),
    }
    .seal()
    .expect("approval");
    let transaction_id = NativeIntegrationTransactionId::new(transaction).expect("transaction id");
    (
        NativeIntegrationApplyRequestV1 {
            context,
            transaction_id: transaction_id.clone(),
            preview,
            approval,
            observed_at: UtcMicros(12),
        },
        transaction_id,
    )
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

#[test]
fn cancellation_after_begin_records_live_diverged_state() {
    let store = Arc::new(StatusStore::default());
    let diverged = NativeIntegrationProbeV1::Diverged {
        tip: oid('a'),
        tree: oid('b'),
        index_digest: digest('c'),
        worktree_digest: digest('d'),
    };
    let coordinator = NativeIntegrationTransactionCoordinator::new(
        store,
        Arc::new(UnusedTopology),
        Arc::new(ControlledMechanics {
            probe: Ok(diverged),
            apply: Err(NativeIntegrationPortError::Unavailable),
        }),
        Arc::new(Authorized),
    );
    let (request, _) = apply_fixture("transaction.cancelled-live-probe");
    let cancellation =
        CancellationSignal::active("cancel.transaction.live-probe").expect("cancellation");
    cancellation.cancel(UtcMicros(11));

    let receipt = coordinator
        .apply(&request, &cancellation)
        .expect("cancelled transaction must terminate from live state");

    assert_eq!(
        receipt.status.terminal_outcome,
        Some(NativeIntegrationTerminalOutcomeV1::AbortedNoChange)
    );
    assert_eq!(receipt.final_ref_tip, oid('a'));
    assert_eq!(receipt.final_tree, oid('b'));
    assert_eq!(receipt.final_index_digest, digest('c'));
    assert_eq!(receipt.final_worktree_digest, digest('d'));
}

#[test]
fn durable_cancellation_revision_is_settled_after_candidate_verification() {
    let store = Arc::new(StatusStore::default());
    let coordinator = NativeIntegrationTransactionCoordinator::new(
        store.clone(),
        Arc::new(UnusedTopology),
        Arc::new(DurableCancelDuringRevalidation {
            store: store.clone(),
            revalidations: AtomicUsize::new(0),
            probe: NativeIntegrationProbeV1::OldState {
                tip: oid('a'),
                tree: oid('b'),
                index_digest: digest('c'),
                worktree_digest: digest('d'),
            },
        }),
        Arc::new(Authorized),
    );
    let (request, transaction_id) = apply_fixture("transaction.concurrent-cancel");
    let cancellation =
        CancellationSignal::active("external.concurrent-cancel").expect("external cancellation");

    let receipt = coordinator
        .apply(&request, &cancellation)
        .expect("durably cancelled apply must settle");

    assert_eq!(
        receipt.status.terminal_outcome,
        Some(NativeIntegrationTerminalOutcomeV1::AbortedNoChange)
    );
    assert!(receipt.status.cancellation_requested);
    assert_eq!(receipt.status.phase_revision, 4);
    assert_eq!(
        store
            .read_receipt(&transaction_id)
            .unwrap()
            .expect("terminal receipt"),
        receipt
    );
}

#[test]
fn apply_error_after_commit_start_writes_inspection_receipt() {
    let store = Arc::new(StatusStore::default());
    let coordinator = NativeIntegrationTransactionCoordinator::new(
        store.clone(),
        Arc::new(UnusedTopology),
        Arc::new(ControlledMechanics {
            probe: Ok(NativeIntegrationProbeV1::OldState {
                tip: oid('2'),
                tree: oid('5'),
                index_digest: digest('8'),
                worktree_digest: digest('9'),
            }),
            apply: Err(NativeIntegrationPortError::Native(
                "injected apply failure".to_owned(),
            )),
        }),
        Arc::new(Authorized),
    );
    let (request, transaction_id) = apply_fixture("transaction.apply-error-terminal");
    let cancellation =
        CancellationSignal::active("cancel.transaction.apply-error").expect("cancellation");

    let receipt = coordinator
        .apply(&request, &cancellation)
        .expect("post-begin apply error must produce an inspection receipt");

    assert_eq!(
        receipt.status.terminal_outcome,
        Some(NativeIntegrationTerminalOutcomeV1::NeedsInspection)
    );
    assert_eq!(
        store
            .read_receipt(&transaction_id)
            .expect("receipt read")
            .as_ref(),
        Some(&receipt)
    );
    assert_eq!(
        store.quarantined.lock().unwrap().as_slice(),
        &[transaction_id]
    );
}

#[test]
fn probe_error_after_begin_writes_inspection_receipt() {
    let store = Arc::new(StatusStore::default());
    let coordinator = NativeIntegrationTransactionCoordinator::new(
        store.clone(),
        Arc::new(UnusedTopology),
        Arc::new(ControlledMechanics {
            probe: Err(NativeIntegrationPortError::Native(
                "injected probe failure".to_owned(),
            )),
            apply: Ok(NativeApplyEffectV1::FailedNoChange),
        }),
        Arc::new(Authorized),
    );
    let (request, transaction_id) = apply_fixture("transaction.probe-error-terminal");
    let cancellation =
        CancellationSignal::active("cancel.transaction.probe-error").expect("cancellation");

    let receipt = coordinator
        .apply(&request, &cancellation)
        .expect("post-begin probe error must produce an inspection receipt");

    assert_eq!(
        receipt.status.terminal_outcome,
        Some(NativeIntegrationTerminalOutcomeV1::NeedsInspection)
    );
    assert!(
        store
            .read_receipt(&transaction_id)
            .expect("receipt read")
            .is_some()
    );
    assert_eq!(
        store.quarantined.lock().unwrap().as_slice(),
        &[transaction_id]
    );
}
