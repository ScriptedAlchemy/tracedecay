use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use tracedecay_application::{
    AuthorityReceipt, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, GitIndexApplyRequestV1, GitIndexEffectProofV1, GitIndexOperationBindingV1,
    GitIndexPreviewPortResultV1, GitIndexPreviewRequestV1, GitIndexTransactionPort,
    GitIndexTransactionPortError, IdempotencyKey, OperationBudgetUsage, OperationReceipt,
    OperationTermination, PolicyDecisionRef, RequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, ComponentVersion, GitBlobExpectationV1, GitCommitIdentityV1, GitCoverageV1,
    GitHeadStateV1, GitIndexCommitIntentV1, GitIndexEntryExpectationV1,
    GitIndexPreviewDispositionV1, GitIndexPreviewId, GitIndexPreviewV1, GitIndexReceiptId,
    GitIndexReceiptOutcomeV1, GitIndexSigningPolicyV1, GitIndexTransactionId,
    GitIndexTransactionOperationV1, GitIndexTransactionReceiptV1, GitObjectFormatV1, GitOidV1,
    GitOperationStateV1, HunkDirectionV1, HunkRefV1, ManifestDigest, ProjectId, RefId,
    RepositoryId, RepositoryIndexSnapshotV1, RepositoryIndexStateV1, RepositoryStateSnapshotV1,
    RepositoryWorkingTreeSnapshotV1, RepositoryWorkingTreeStateV1, UtcMicros, WorktreeId,
    canonical_sha256,
};
use tracedecay_policy::{GitConflictRiskV1, GitEffectAuthorizationV1, GitEffectClassifierV1};

use super::DaemonGitIndexTransactionPort;
use super::recovery::{GitIndexRecoveryError, GitIndexRecoveryExecutor};
use super::service::{
    CurrentGitIndexPolicyStateV1, GitIndexNativeExecutor, GitIndexPolicyRecheckPort,
    NativeGitIndexApplyOutcomeV1, NativeGitIndexApplyResult,
};
use super::store::DaemonGitIndexTransactionStore;
use crate::db::engine::TestConnection;

pub(super) type TestPort = DaemonGitIndexTransactionPort<
    DaemonGitIndexTransactionStore,
    FakeNative,
    GitEffectClassifierV1,
    TestPolicy,
>;

pub(super) struct TestHarness {
    pub(super) _directory: TempDir,
    pub(super) port: TestPort,
    pub(super) preview: GitIndexPreviewV1,
    pub(super) request: GitIndexApplyRequestV1,
    pub(super) apply_calls: Arc<AtomicUsize>,
    pub(super) recovery_calls: Arc<AtomicUsize>,
    pub(super) discard_calls: Arc<AtomicUsize>,
    pub(super) entered_native: Arc<AtomicBool>,
    pub(super) allow: Arc<std::sync::atomic::AtomicBool>,
    pub(super) policy_calls: Arc<AtomicUsize>,
    pub(super) policy_evaluated_at: Arc<Mutex<Option<UtcMicros>>>,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum NativeMode {
    ProvenNoMutation,
    CommitBoundaryUnknown,
    Completed(GitIndexReceiptOutcomeV1),
}

#[derive(Clone, Copy, Debug)]
pub(super) enum RecoveryMode {
    AbortedNoChange,
    NeedsInspection,
    Error,
}

pub(super) struct FakeNative {
    preview_results: Mutex<VecDeque<GitIndexPreviewPortResultV1>>,
    apply_modes: Mutex<VecDeque<NativeMode>>,
    recovery_modes: Mutex<VecDeque<RecoveryMode>>,
    pub(super) apply_calls: Arc<AtomicUsize>,
    pub(super) recovery_calls: Arc<AtomicUsize>,
    pub(super) discard_calls: Arc<AtomicUsize>,
    pub(super) entered_native: Arc<AtomicBool>,
}

impl FakeNative {
    pub(super) fn new(
        apply_modes: impl IntoIterator<Item = NativeMode>,
        recovery_modes: impl IntoIterator<Item = RecoveryMode>,
    ) -> Self {
        Self::with_preview(None, apply_modes, recovery_modes)
    }

    fn with_preview(
        preview: Option<GitIndexPreviewV1>,
        apply_modes: impl IntoIterator<Item = NativeMode>,
        recovery_modes: impl IntoIterator<Item = RecoveryMode>,
    ) -> Self {
        let preview_results = preview
            .map(|preview| {
                VecDeque::from([GitIndexPreviewPortResultV1 {
                    execution: OperationReceipt::completed(
                        preview.created_at,
                        preview.created_at,
                        Deadline::new(preview.expires_at).expect("preview deadline"),
                        OperationBudgetUsage::default(),
                    )
                    .expect("preview execution"),
                    preview,
                }])
            })
            .unwrap_or_default();
        Self {
            preview_results: Mutex::new(preview_results),
            apply_modes: Mutex::new(apply_modes.into_iter().collect()),
            recovery_modes: Mutex::new(recovery_modes.into_iter().collect()),
            apply_calls: Arc::new(AtomicUsize::new(0)),
            recovery_calls: Arc::new(AtomicUsize::new(0)),
            discard_calls: Arc::new(AtomicUsize::new(0)),
            entered_native: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl GitIndexNativeExecutor for FakeNative {
    fn preview(
        &self,
        _request: &GitIndexPreviewRequestV1,
    ) -> Result<GitIndexPreviewPortResultV1, GitIndexTransactionPortError> {
        self.preview_results
            .lock()
            .expect("preview results")
            .pop_front()
            .ok_or(GitIndexTransactionPortError::StalePreview)
    }

    fn apply(
        &self,
        transaction_id: &GitIndexTransactionId,
        preview: &GitIndexPreviewV1,
        request: &GitIndexApplyRequestV1,
    ) -> Result<NativeGitIndexApplyOutcomeV1, GitIndexTransactionPortError> {
        self.entered_native.store(true, Ordering::SeqCst);
        self.apply_calls.fetch_add(1, Ordering::SeqCst);
        let mode = self
            .apply_modes
            .lock()
            .expect("apply modes")
            .pop_front()
            .unwrap_or(NativeMode::ProvenNoMutation);
        match mode {
            NativeMode::ProvenNoMutation => Ok(NativeGitIndexApplyOutcomeV1::ProvenNoMutation),
            NativeMode::CommitBoundaryUnknown => {
                Ok(NativeGitIndexApplyOutcomeV1::CommitBoundaryUnknown)
            }
            NativeMode::Completed(outcome) => Ok(NativeGitIndexApplyOutcomeV1::Completed(
                Box::new(NativeGitIndexApplyResult {
                    receipt: receipt_for(transaction_id, preview, outcome),
                    execution: execution_for(request, outcome),
                }),
            )),
        }
    }

    fn discard_preview(&self, _preview_id: &GitIndexPreviewId) {
        self.discard_calls.fetch_add(1, Ordering::SeqCst);
    }
}

impl GitIndexRecoveryExecutor for FakeNative {
    fn reconcile(
        &self,
        record: &tracedecay_store::GitIndexTransactionRecordV1,
    ) -> Result<GitIndexTransactionReceiptV1, GitIndexRecoveryError> {
        self.recovery_calls.fetch_add(1, Ordering::SeqCst);
        let mode = self
            .recovery_modes
            .lock()
            .expect("recovery modes")
            .pop_front()
            .unwrap_or(RecoveryMode::Error);
        let outcome = match mode {
            RecoveryMode::AbortedNoChange => GitIndexReceiptOutcomeV1::AbortedNoChange,
            RecoveryMode::NeedsInspection => GitIndexReceiptOutcomeV1::NeedsInspection,
            RecoveryMode::Error => return Err(GitIndexRecoveryError::Indeterminate),
        };
        Ok(receipt_for(
            &record.journal.transaction_id,
            &record.preview,
            outcome,
        ))
    }
}

pub(super) struct TestPolicy {
    pub(super) allow: Arc<std::sync::atomic::AtomicBool>,
    pub(super) calls: Arc<AtomicUsize>,
    pub(super) evaluated_at: Arc<Mutex<Option<UtcMicros>>>,
}

impl TestPolicy {
    pub(super) fn allowing() -> Self {
        Self {
            allow: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            calls: Arc::new(AtomicUsize::new(0)),
            evaluated_at: Arc::new(Mutex::new(None)),
        }
    }
}

impl GitIndexPolicyRecheckPort for TestPolicy {
    fn recheck(
        &self,
        request: &GitIndexApplyRequestV1,
        _preview: &GitIndexPreviewV1,
    ) -> Result<CurrentGitIndexPolicyStateV1, GitIndexTransactionPortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.allow.load(Ordering::SeqCst) {
            return Err(GitIndexTransactionPortError::PolicyDenied);
        }
        Ok(CurrentGitIndexPolicyStateV1 {
            authorization: GitEffectAuthorizationV1 {
                capability_granted: true,
                owner_scope_matches: true,
            },
            conflict_risk: GitConflictRiskV1::NoneKnown,
            policy_revision: request.authority.policy.revision,
            policy_digest: request.proof.policy_digest.clone(),
            configuration_digest: request.proof.configuration_digest.clone(),
            evaluated_at: self
                .evaluated_at
                .lock()
                .expect("policy evaluated-at")
                .unwrap_or(request.observed_at),
        })
    }
}

pub(super) fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("fixture identity")
}

pub(super) fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("fixture digest")
}

pub(super) fn oid(byte: char) -> GitOidV1 {
    GitOidV1::new(byte.to_string().repeat(40)).expect("fixture object id")
}

pub(super) fn snapshot() -> RepositoryStateSnapshotV1 {
    RepositoryStateSnapshotV1::new(
        id::<ProjectId>("project.fixture"),
        id::<RepositoryId>("repository.fixture"),
        Some(id::<WorktreeId>("worktree.fixture")),
        1,
        GitObjectFormatV1::Sha1,
        GitHeadStateV1::Attached {
            branch: "refs/heads/main".to_owned(),
            commit: oid('a'),
        },
        RepositoryIndexSnapshotV1 {
            checksum: digest('b'),
            tree_id: Some(oid('c')),
            state: RepositoryIndexStateV1::Clean,
            unmerged_stage_digest: None,
        },
        RepositoryWorkingTreeSnapshotV1 {
            state: RepositoryWorkingTreeStateV1::Clean,
            tracked_digest: digest('d'),
            untracked_name_digest: None,
            ignored_collision_digest: None,
        },
        GitOperationStateV1::None,
        Some(digest('1')),
        Some(digest('2')),
        Some(digest('3')),
        Some(digest('4')),
        UtcMicros(1),
        GitCoverageV1::complete(),
    )
    .expect("snapshot")
    .with_native_identity(
        "git version fixture".to_owned(),
        "tracedecay.git-index-adapter.v1".to_owned(),
        digest('5'),
    )
    .expect("native identity")
}

pub(super) fn commit_intent() -> GitIndexCommitIntentV1 {
    let identity = GitCommitIdentityV1 {
        name: "TraceDecay Test".to_owned(),
        email: "tracedecay@example.com".to_owned(),
        at: UtcMicros(1_000_000),
    };
    GitIndexCommitIntentV1::new(
        "transaction fixture\n".to_owned(),
        identity.clone(),
        identity,
        GitIndexSigningPolicyV1::UnsignedPermitted,
    )
    .expect("intent")
}

fn fixture_hunk(
    operation: GitIndexTransactionOperationV1,
    preview_id: &GitIndexPreviewId,
    snapshot_digest: &ManifestDigest,
) -> HunkRefV1 {
    HunkRefV1 {
        repository: id::<RepositoryId>("repository.fixture"),
        worktree: id::<WorktreeId>("worktree.fixture"),
        direction: operation.hunk_direction().expect("hunk operation"),
        path: "packet.txt".to_owned(),
        original_path: None,
        expected_base_blob: GitBlobExpectationV1::AbsentFile,
        expected_index_entry: GitIndexEntryExpectationV1 {
            blob: GitBlobExpectationV1::AbsentFile,
            mode: None,
            unmerged_stage: None,
        },
        expected_worktree_blob: match operation.hunk_direction() {
            Some(HunkDirectionV1::WorkingTreeToIndex) => Some(GitBlobExpectationV1::AbsentFile),
            Some(HunkDirectionV1::IndexToHead) => None,
            None => None,
        },
        expected_worktree_mode: None,
        hunk_header: "@@ -1 +1 @@".to_owned(),
        context_digest: digest('b'),
        patch_digest: digest('c'),
        selected_line_bitmap: vec![1],
        attributes_digest: None,
        preview_id: preview_id.as_str().to_owned(),
        schema_version: "tracedecay.git-hunk-ref.v1".to_owned(),
        snapshot_digest: snapshot_digest.clone(),
    }
}

fn preview_for_operation(
    operation: GitIndexTransactionOperationV1,
    expires_at: UtcMicros,
) -> GitIndexPreviewV1 {
    let snapshot = snapshot();
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&snapshot).expect("snapshot digest");
    let preview_id = GitIndexPreviewId::new(match operation {
        GitIndexTransactionOperationV1::StageHunks => "preview.transport.stage",
        GitIndexTransactionOperationV1::UnstageHunks => "preview.transport.unstage",
        GitIndexTransactionOperationV1::CommitIndex => "preview.transport.commit",
    })
    .expect("preview id");
    let (selected_hunks, candidate_index_tree, intent) = match operation {
        GitIndexTransactionOperationV1::CommitIndex => (
            Vec::new(),
            snapshot.index.tree_id.clone(),
            Some(commit_intent()),
        ),
        GitIndexTransactionOperationV1::StageHunks
        | GitIndexTransactionOperationV1::UnstageHunks => (
            vec![fixture_hunk(operation, &preview_id, &snapshot_digest)],
            Some(oid('e')),
            None,
        ),
    };
    GitIndexPreviewV1::new_with_commit_intent(
        preview_id,
        operation,
        snapshot,
        snapshot_digest,
        selected_hunks,
        candidate_index_tree,
        intent.as_ref(),
        GitIndexPreviewDispositionV1::Applicable,
        UtcMicros(10),
        expires_at,
    )
    .expect("preview")
}

pub(super) fn preview() -> GitIndexPreviewV1 {
    preview_with_expiry(UtcMicros(100))
}

pub(super) fn preview_with_expiry(expires_at: UtcMicros) -> GitIndexPreviewV1 {
    preview_for_operation(GitIndexTransactionOperationV1::CommitIndex, expires_at)
}

pub(super) fn apply_request(preview: &GitIndexPreviewV1, key: &str) -> GitIndexApplyRequestV1 {
    let binding =
        GitIndexOperationBindingV1::for_operation(preview.operation).expect("operation binding");
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.fixture"),
        id::<RepositoryId>("repository.fixture"),
        id::<WorktreeId>("worktree.fixture"),
        Some(id::<RefId>("refs/heads/main")),
    )
    .expect("scope");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.fixture").expect("grant id"),
        1,
        digest('6'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(1_000),
        scope.clone(),
        BTreeSet::from([binding.capability_id.clone()]),
        BTreeSet::from([binding.use_case_id.clone()]),
        DisclosureClass::Sensitive,
    )
    .expect("grant");
    let context = RequestContext::new(
        id::<ActorId>("actor.requester"),
        scope,
        grant,
        RequestId::new("request.fixture").expect("request id"),
        Deadline::new(UtcMicros(500)).expect("deadline"),
        CancellationContext::active("cancel.fixture").expect("cancellation"),
    )
    .expect("context");
    let authority = AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.fixture",
            1,
            digest('7'),
            ComponentVersion::new("policy.evaluator.v1").expect("policy version"),
        )
        .expect("policy"),
        UtcMicros(2),
    )
    .expect("authority");
    GitIndexApplyRequestV1 {
        context,
        authority: authority.clone(),
        binding,
        preview_id: preview.preview_id.clone(),
        preview_digest: preview.preview_digest.clone(),
        idempotency_key: IdempotencyKey::new(key).expect("idempotency key"),
        proof: GitIndexEffectProofV1 {
            policy_digest: authority.policy.digest,
            configuration_digest: digest('8'),
            catalog_digest: digest('9'),
            privacy_digest: digest('a'),
            external_proof: None,
        },
        observed_at: UtcMicros(15),
    }
}

fn preview_request(
    preview: &GitIndexPreviewV1,
    request: &GitIndexApplyRequestV1,
) -> GitIndexPreviewRequestV1 {
    GitIndexPreviewRequestV1 {
        context: request.context.clone(),
        authority: request.authority.clone(),
        binding: request.binding.clone(),
        preview_id: preview.preview_id.clone(),
        repository_snapshot: preview.repository_snapshot.clone(),
        selected_hunks: preview.selected_hunks.clone(),
        commit_intent: (preview.operation == GitIndexTransactionOperationV1::CommitIndex)
            .then(commit_intent),
        observed_at: request.observed_at,
    }
}

pub(super) fn transaction_id_for(
    request: &GitIndexApplyRequestV1,
    preview: &GitIndexPreviewV1,
) -> GitIndexTransactionId {
    let input_digest = request.input_digest().expect("input digest");
    let digest = canonical_sha256(&(
        "tracedecay.daemon.git-index-transaction.v1",
        request.idempotency_key.as_str(),
        input_digest,
        &preview.preview_digest,
    ))
    .expect("transaction digest");
    GitIndexTransactionId::new(format!(
        "git-index-transaction.v1.{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
    .expect("transaction id")
}

pub(super) fn receipt_for(
    transaction_id: &GitIndexTransactionId,
    preview: &GitIndexPreviewV1,
    outcome: GitIndexReceiptOutcomeV1,
) -> GitIndexTransactionReceiptV1 {
    let (final_snapshot, new_index_tree, new_head, created_commit) = match outcome {
        GitIndexReceiptOutcomeV1::Committed => {
            let is_commit = preview.operation == GitIndexTransactionOperationV1::CommitIndex;
            (
                digest('f'),
                Some(oid('c')),
                is_commit
                    .then(|| oid('d'))
                    .or_else(|| preview.repository_snapshot.head.commit().cloned()),
                is_commit.then(|| oid('d')),
            )
        }
        GitIndexReceiptOutcomeV1::AbortedNoChange | GitIndexReceiptOutcomeV1::NeedsInspection => (
            preview.repository_snapshot_digest.clone(),
            preview.repository_snapshot.index.tree_id.clone(),
            preview.repository_snapshot.head.commit().cloned(),
            None,
        ),
    };
    GitIndexTransactionReceiptV1::new(
        GitIndexReceiptId::new(format!("git-index-receipt.v1.{}", transaction_id.as_str()))
            .expect("receipt id"),
        transaction_id.clone(),
        preview,
        final_snapshot,
        new_index_tree,
        new_head,
        created_commit,
        outcome,
        UtcMicros(15),
    )
    .expect("receipt")
}

pub(super) fn execution_for(
    request: &GitIndexApplyRequestV1,
    outcome: GitIndexReceiptOutcomeV1,
) -> OperationReceipt {
    let termination = match outcome {
        GitIndexReceiptOutcomeV1::Committed => OperationTermination::Completed,
        GitIndexReceiptOutcomeV1::AbortedNoChange => OperationTermination::Failed,
        GitIndexReceiptOutcomeV1::NeedsInspection => OperationTermination::EffectUnknown,
    };
    OperationReceipt {
        started_at: request.observed_at,
        ended_at: request.observed_at,
        effective_deadline: request.context.deadline().clone(),
        cancellation: None,
        budget: OperationBudgetUsage::default(),
        termination,
    }
}

fn transaction_store(directory: &TempDir) -> DaemonGitIndexTransactionStore {
    let path = directory.path().join("canonical-project.db");
    DaemonGitIndexTransactionStore::open_engine_test(TestConnection::open(&path))
        .expect("canonical store actor")
}

pub(super) fn test_store(directory: &TempDir) -> DaemonGitIndexTransactionStore {
    transaction_store(directory)
}

pub(super) fn test_port_from_preview(
    operation: GitIndexTransactionOperationV1,
    native_modes: impl IntoIterator<Item = NativeMode>,
    recovery_modes: impl IntoIterator<Item = RecoveryMode>,
) -> TestHarness {
    let directory = tempfile::tempdir().expect("store directory");
    let store = transaction_store(&directory);
    let preview_template = preview_for_operation(operation, UtcMicros(100));
    let authority_request = apply_request(&preview_template, "idempotency.transport");
    let preview_request = preview_request(&preview_template, &authority_request);
    let native =
        FakeNative::with_preview(Some(preview_template.clone()), native_modes, recovery_modes);
    let apply_calls = Arc::clone(&native.apply_calls);
    let recovery_calls = Arc::clone(&native.recovery_calls);
    let discard_calls = Arc::clone(&native.discard_calls);
    let entered_native = Arc::clone(&native.entered_native);
    let policy = TestPolicy::allowing();
    let allow = Arc::clone(&policy.allow);
    let policy_calls = Arc::clone(&policy.calls);
    let policy_evaluated_at = Arc::clone(&policy.evaluated_at);
    let port =
        DaemonGitIndexTransactionPort::new(store, native, GitEffectClassifierV1::default(), policy);
    let preview = port
        .preview(&preview_request)
        .expect("preview reaches the native owner")
        .preview;
    let request = apply_request(&preview, "idempotency.transport");
    TestHarness {
        _directory: directory,
        port,
        preview,
        request,
        apply_calls,
        recovery_calls,
        discard_calls,
        entered_native,
        allow,
        policy_calls,
        policy_evaluated_at,
    }
}
