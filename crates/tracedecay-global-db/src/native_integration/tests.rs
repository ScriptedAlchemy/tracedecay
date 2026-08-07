use tracedecay_domain::{
    ActorId, CapabilityId, FrozenIndependentBranchSelectionV1, GitHeadStateV1, GitObjectFormatV1,
    GitOidV1, GitOperationStateV1, ManifestDigest, MechanicalIntegrationModeV1,
    NativeIntegrationApprovalId, NativeIntegrationApprovalV1, NativeIntegrationPhaseV1,
    NativeIntegrationPreviewDispositionV1, NativeIntegrationPreviewId, NativeIntegrationPreviewV1,
    NativeIntegrationReceiptV1, NativeIntegrationRepositorySnapshotV1, NativeIntegrationSelectionV1,
    NativeIntegrationTerminalOutcomeV1, NativeIntegrationTransactionId,
    NativeIntegrationTransactionStatusV1, ProjectId, RefId, RepositoryId, UtcMicros,
    WorktreeInventoryEpoch, WorktreeInventorySnapshotId,
};
use tracedecay_runtime_core::db::engine::{TestConnection, TransactionBehavior};
use tracedecay_store::{
    NativeIntegrationBeginResultV1, NativeIntegrationRecordV1, NativeIntegrationStoreError,
};

use super::store::GlobalDbNativeIntegrationStore;

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("fixture digest")
}

fn oid(byte: char) -> GitOidV1 {
    GitOidV1::new(byte.to_string().repeat(40)).expect("fixture object id")
}

fn preview_fixture(preview_id: &str) -> NativeIntegrationPreviewV1 {
    let project_id = ProjectId::new("project.native-integration.fixture").expect("project id");
    let repository_id =
        RepositoryId::new("repository.native-integration.fixture").expect("repository id");
    let source_ref = RefId::new("refs/heads/feature").expect("source ref");
    let destination_ref = RefId::new("refs/heads/main").expect("destination ref");
    let selection = FrozenIndependentBranchSelectionV1::new(
        project_id.clone(),
        repository_id.clone(),
        WorktreeInventorySnapshotId::new("inventory.fixture").expect("inventory snapshot"),
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
    .expect("frozen selection");
    let snapshot = NativeIntegrationRepositorySnapshotV1 {
        project_id,
        repository_id,
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
        adapter_revision: "gix-fixture".to_owned(),
        captured_at: UtcMicros(6),
        digest: digest('b'),
    }
    .seal()
    .expect("sealed snapshot");
    NativeIntegrationPreviewV1 {
        preview_id: NativeIntegrationPreviewId::new(preview_id).expect("preview id"),
        selection: NativeIntegrationSelectionV1::IndependentBranch(selection),
        repository_snapshot: snapshot,
        grant_digest: digest('c'),
        policy_digest: digest('d'),
        graph_revision_digest: digest('e'),
        test_revision_digest: digest('f'),
        schema_revision_digest: digest('0'),
        migration_revision_digest: digest('1'),
        disposition: NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(
            MechanicalIntegrationModeV1::FastForward,
        ),
        candidate_tree: Some(oid('4')),
        ordered_commits: vec![oid('1')],
        created_at: UtcMicros(10),
        expires_at: UtcMicros(1_000_000),
        preview_digest: digest('2'),
    }
    .seal()
    .expect("sealed preview")
}

fn approval_fixture(approval_id: &str, preview: &NativeIntegrationPreviewV1) -> NativeIntegrationApprovalV1 {
    NativeIntegrationApprovalV1 {
        approval_id: NativeIntegrationApprovalId::new(approval_id).expect("approval id"),
        preview_id: preview.preview_id.clone(),
        preview_digest: preview.preview_digest.clone(),
        principal: ActorId::new("actor.approver").expect("principal"),
        delegated_agent: None,
        capability: CapabilityId::new("capability.git.native-integration-apply")
            .expect("capability"),
        grant_digest: preview.grant_digest.clone(),
        issued_at: UtcMicros(11),
        expires_at: UtcMicros(2_000_000),
        approval_digest: digest('3'),
    }
    .seal()
    .expect("sealed approval")
}

fn prepared_record(
    transaction_id: &str,
    preview: &NativeIntegrationPreviewV1,
    approval: &NativeIntegrationApprovalV1,
) -> NativeIntegrationRecordV1 {
    let status = NativeIntegrationTransactionStatusV1 {
        transaction_id: NativeIntegrationTransactionId::new(transaction_id)
            .expect("transaction id"),
        preview_id: preview.preview_id.clone(),
        preview_digest: preview.preview_digest.clone(),
        approval_id: approval.approval_id.clone(),
        repository_id: preview.repository_snapshot.repository_id.clone(),
        destination_ref: preview.repository_snapshot.destination_ref.clone(),
        expected_destination_tip: preview.repository_snapshot.destination_tip.clone(),
        candidate_tip: None,
        phase: NativeIntegrationPhaseV1::Prepared,
        phase_revision: 1,
        cancellation_requested: false,
        terminal_outcome: None,
        updated_at: UtcMicros(12),
    };
    NativeIntegrationRecordV1 {
        preview: preview.clone(),
        approval: approval.clone(),
        status,
        terminal_receipt: None,
    }
}

fn terminal_receipt(
    record: &NativeIntegrationRecordV1,
    outcome: NativeIntegrationTerminalOutcomeV1,
) -> NativeIntegrationReceiptV1 {
    let mut status = record.status.clone();
    status.phase = NativeIntegrationPhaseV1::Terminal;
    status.phase_revision = status.phase_revision.saturating_add(1);
    status.terminal_outcome = Some(outcome);
    status.updated_at = UtcMicros(20);
    NativeIntegrationReceiptV1 {
        status,
        final_ref_tip: record.preview.repository_snapshot.destination_tip.clone(),
        final_tree: record.preview.repository_snapshot.destination_tree.clone(),
        final_index_digest: record.preview.repository_snapshot.index_digest.clone(),
        final_worktree_digest: record.preview.repository_snapshot.worktree_digest.clone(),
        completed_at: UtcMicros(20),
        receipt_digest: digest('4'),
    }
    .seal()
    .expect("sealed receipt")
}

async fn open_database() -> (tempfile::TempDir, std::path::PathBuf, TestConnection) {
    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("project-sessions.db");
    let database = TestConnection::open(&path);
    let transaction = database
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .expect("begin schema transaction");
    super::ensure_native_integration_schema(&transaction)
        .await
        .expect("ensure native integration schema");
    transaction
        .commit()
        .await
        .expect("commit native integration schema");
    (directory, path, database)
}

fn test_store(database: &TestConnection) -> GlobalDbNativeIntegrationStore<'_> {
    GlobalDbNativeIntegrationStore::for_engine_test(database)
}

#[tokio::test]
async fn preview_commitments_are_immutable_and_conflicts_are_rejected() {
    let (_directory, _path, database) = open_database().await;
    let store = test_store(&database);
    let preview = preview_fixture("preview.native.one");

    store
        .save_preview(preview.clone())
        .await
        .expect("first save");
    // Idempotent for the identical commitment.
    store
        .save_preview(preview.clone())
        .await
        .expect("identical save");
    // A different preview under the same identity is a conflict.
    let mut different = preview.clone();
    different.expires_at = UtcMicros(2_000_000);
    let different = different.seal().expect("resealed preview");
    assert_eq!(
        store.save_preview(different).await,
        Err(NativeIntegrationStoreError::PreviewConflict)
    );
    assert_eq!(
        store
            .read_preview(&preview.preview_id)
            .await
            .expect("read preview"),
        Some(preview)
    );
}

#[tokio::test]
async fn issued_approvals_persist_and_conflicting_reissue_is_rejected() {
    let (_directory, _path, database) = open_database().await;
    let store = test_store(&database);
    let preview = preview_fixture("preview.native.approvals");
    let approval = approval_fixture("approval.native.one", &preview);

    store
        .save_approval(approval.clone())
        .await
        .expect("first issuance");
    store
        .save_approval(approval.clone())
        .await
        .expect("identical issuance is idempotent");
    let mut conflicting = approval.clone();
    conflicting.expires_at = UtcMicros(3_000_000);
    let conflicting = conflicting.seal().expect("resealed approval");
    assert_eq!(
        store.save_approval(conflicting).await,
        Err(NativeIntegrationStoreError::ApprovalConflict)
    );
    assert_eq!(
        store
            .read_approval(&approval.approval_id)
            .await
            .expect("read approval"),
        Some(approval.clone())
    );
    assert!(
        !store
            .approval_consumed(&approval.approval_id)
            .await
            .expect("unconsumed approval")
    );
}

#[tokio::test]
async fn begin_consumes_the_approval_exactly_once() {
    let (_directory, _path, database) = open_database().await;
    let store = test_store(&database);
    let preview = preview_fixture("preview.native.begin");
    let approval = approval_fixture("approval.native.begin", &preview);
    let record = prepared_record("transaction.native.begin", &preview, &approval);

    match store
        .begin_or_replay(record.clone())
        .await
        .expect("begin starts")
    {
        NativeIntegrationBeginResultV1::Started(started) => assert_eq!(*started, record),
        other => panic!("unexpected begin result: {other:?}"),
    }
    assert!(
        store
            .approval_consumed(&approval.approval_id)
            .await
            .expect("consumed approval")
    );

    // The same approval can never start a second transaction.
    let second = prepared_record("transaction.native.second", &preview, &approval);
    assert_eq!(
        store.begin_or_replay(second).await,
        Err(NativeIntegrationStoreError::ApprovalConflict)
    );

    // The same transaction with different input is a conflict, not a replay.
    let other_preview = preview_with_expiry("preview.native.begin-b", UtcMicros(3_000_000));
    let other_approval = approval_fixture("approval.native.begin-b", &other_preview);
    let mut conflicting =
        prepared_record("transaction.native.begin", &other_preview, &other_approval);
    conflicting.status.updated_at = UtcMicros(13);
    assert_eq!(
        store.begin_or_replay(conflicting).await,
        Err(NativeIntegrationStoreError::TransactionConflict)
    );

    // Identical re-submission before terminal state requires recovery.
    match store
        .begin_or_replay(record.clone())
        .await
        .expect("identical resubmission")
    {
        NativeIntegrationBeginResultV1::RecoveryRequired(pending) => {
            assert_eq!(pending.status, record.status);
        }
        other => panic!("unexpected resubmission result: {other:?}"),
    }
}

fn preview_with_expiry(preview_id: &str, expires_at: UtcMicros) -> NativeIntegrationPreviewV1 {
    let mut value = preview_fixture(preview_id);
    value.expires_at = expires_at;
    value.seal().expect("resealed preview")
}

#[tokio::test]
async fn status_compare_and_swap_rejects_stale_revisions_and_identity_rebinds() {
    let (_directory, _path, database) = open_database().await;
    let store = test_store(&database);
    let preview = preview_fixture("preview.native.cas");
    let approval = approval_fixture("approval.native.cas", &preview);
    let record = prepared_record("transaction.native.cas", &preview, &approval);
    store
        .begin_or_replay(record.clone())
        .await
        .expect("begin starts");

    let mut advanced = record.status.clone();
    advanced.phase = NativeIntegrationPhaseV1::CandidateVerified;
    advanced.phase_revision = 2;
    advanced.updated_at = UtcMicros(14);
    let stored = store
        .compare_and_swap_status(&record.status.transaction_id, 1, advanced.clone())
        .await
        .expect("first CAS");
    assert_eq!(stored, advanced);

    // A stale expected revision must fail.
    let mut stale = record.status.clone();
    stale.phase = NativeIntegrationPhaseV1::RefCommitStarted;
    stale.phase_revision = 2;
    stale.updated_at = UtcMicros(15);
    assert_eq!(
        store
            .compare_and_swap_status(&record.status.transaction_id, 1, stale)
            .await,
        Err(NativeIntegrationStoreError::StatusConflict)
    );

    // CAS can never rebind the transaction identity.
    let mut rebind = advanced.clone();
    rebind.phase_revision = 3;
    rebind.approval_id =
        NativeIntegrationApprovalId::new("approval.native.other").expect("approval id");
    rebind.updated_at = UtcMicros(16);
    assert_eq!(
        store
            .compare_and_swap_status(&record.status.transaction_id, 2, rebind)
            .await,
        Err(NativeIntegrationStoreError::StatusConflict)
    );

    // Terminal phases are unreachable through CAS.
    let mut terminal = advanced.clone();
    terminal.phase = NativeIntegrationPhaseV1::Terminal;
    terminal.phase_revision = 3;
    terminal.terminal_outcome = Some(NativeIntegrationTerminalOutcomeV1::AbortedNoChange);
    terminal.updated_at = UtcMicros(17);
    assert_eq!(
        store
            .compare_and_swap_status(&record.status.transaction_id, 2, terminal)
            .await,
        Err(NativeIntegrationStoreError::StatusConflict)
    );

    // An unknown transaction is a typed conflict, not an empty success.
    let unknown = NativeIntegrationTransactionId::new("transaction.native.unknown")
        .expect("transaction id");
    let mut orphan = record.status.clone();
    orphan.transaction_id = unknown.clone();
    orphan.phase_revision = 2;
    assert_eq!(
        store.compare_and_swap_status(&unknown, 1, orphan).await,
        Err(NativeIntegrationStoreError::StatusConflict)
    );
}

#[tokio::test]
async fn terminal_receipts_replay_and_survive_restart() {
    let (_directory, path, database) = open_database().await;
    let store = test_store(&database);
    let preview = preview_fixture("preview.native.terminal");
    let approval = approval_fixture("approval.native.terminal", &preview);
    let record = prepared_record("transaction.native.terminal", &preview, &approval);
    store
        .begin_or_replay(record.clone())
        .await
        .expect("begin starts");
    let receipt = terminal_receipt(&record, NativeIntegrationTerminalOutcomeV1::Committed);

    let published = store
        .write_terminal(&record.status.transaction_id, 1, receipt.clone())
        .await
        .expect("terminal publish");
    assert_eq!(published, receipt);
    // Publishing the identical receipt again replays it.
    assert_eq!(
        store
            .write_terminal(&record.status.transaction_id, 1, receipt.clone())
            .await
            .expect("terminal replay"),
        receipt
    );
    // A different receipt for the same transaction is a conflict.
    let conflicting = terminal_receipt(&record, NativeIntegrationTerminalOutcomeV1::RolledBack);
    assert_eq!(
        store
            .write_terminal(&record.status.transaction_id, 1, conflicting)
            .await,
        Err(NativeIntegrationStoreError::ReceiptConflict)
    );
    // Identical begin input now replays the terminal receipt.
    match store
        .begin_or_replay(record.clone())
        .await
        .expect("terminal replay via begin")
    {
        NativeIntegrationBeginResultV1::Replay(replayed) => assert_eq!(*replayed, receipt),
        other => panic!("unexpected replay result: {other:?}"),
    }

    drop(store);
    drop(database);
    let reopened = TestConnection::open(&path);
    let store = test_store(&reopened);
    let restored = store
        .read_record(&record.status.transaction_id)
        .await
        .expect("read record after restart")
        .expect("record survives restart");
    assert_eq!(restored.terminal_receipt, Some(receipt.clone()));
    assert_eq!(
        store
            .read_receipt(&record.status.transaction_id)
            .await
            .expect("read receipt after restart"),
        Some(receipt)
    );
    assert_eq!(
        store
            .pending_transactions(None)
            .await
            .expect("no pending work after terminal receipt"),
        Vec::new()
    );
}

#[tokio::test]
async fn needs_inspection_quarantines_the_repository_until_recovery() {
    let (_directory, path, database) = open_database().await;
    let store = test_store(&database);
    let preview = preview_fixture("preview.native.inspect");
    let approval = approval_fixture("approval.native.inspect", &preview);
    let record = prepared_record("transaction.native.inspect", &preview, &approval);
    store
        .begin_or_replay(record.clone())
        .await
        .expect("begin starts");
    let receipt = terminal_receipt(&record, NativeIntegrationTerminalOutcomeV1::NeedsInspection);
    store
        .write_terminal(&record.status.transaction_id, 1, receipt)
        .await
        .expect("inspection receipt");

    // The durable fence refuses new transactions for the repository.
    let next_preview = preview_with_expiry("preview.native.inspect-b", UtcMicros(3_000_000));
    let next_approval = approval_fixture("approval.native.inspect-b", &next_preview);
    let next = prepared_record("transaction.native.inspect-b", &next_preview, &next_approval);
    assert_eq!(
        store.begin_or_replay(next.clone()).await,
        Err(NativeIntegrationStoreError::RepositoryQuarantined)
    );

    // The fence survives restart.
    drop(store);
    drop(database);
    let reopened = TestConnection::open(&path);
    let store = test_store(&reopened);
    assert_eq!(
        store.begin_or_replay(next).await,
        Err(NativeIntegrationStoreError::RepositoryQuarantined)
    );
}

#[tokio::test]
async fn pending_transactions_expose_unfinished_work_for_restart_recovery() {
    let (_directory, path, database) = open_database().await;
    let store = test_store(&database);
    let preview = preview_fixture("preview.native.pending");
    let approval = approval_fixture("approval.native.pending", &preview);
    let record = prepared_record("transaction.native.pending", &preview, &approval);
    store
        .begin_or_replay(record.clone())
        .await
        .expect("begin starts");

    drop(store);
    drop(database);
    let reopened = TestConnection::open(&path);
    let store = test_store(&reopened);
    let pending = store
        .pending_transactions(None)
        .await
        .expect("pending transactions");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status, record.status);
    let scoped = store
        .pending_transactions(Some(&record.status.repository_id))
        .await
        .expect("repository-scoped pending transactions");
    assert_eq!(scoped.len(), 1);
    let other_repository = RepositoryId::new("repository.native.other").expect("repository id");
    assert_eq!(
        store
            .pending_transactions(Some(&other_repository))
            .await
            .expect("unrelated repository has no pending work"),
        Vec::new()
    );
    assert_eq!(
        store
            .read_status(&record.status.transaction_id)
            .await
            .expect("status read"),
        Some(record.status)
    );
}
