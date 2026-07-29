use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use std::collections::{BTreeSet, VecDeque};
use std::sync::Mutex;

use tracedecay_application::{
    AuthorityReceipt, CancellationContext, CancellationStage, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, GitIndexApplyRequestV1,
    GitIndexEffectProofV1, GitIndexOperationBindingV1, GitIndexPreviewPortResultV1,
    GitIndexPreviewRequestV1, GitIndexTransactionPort, GitIndexTransactionPortError,
    IdempotencyKey, OperationBudgetUsage, OperationReceipt, OperationTermination,
    PolicyDecisionRef, RequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, ComponentVersion, GitCommitIdentityV1, GitCoverageV1, GitHeadStateV1,
    GitIndexCommitIntentV1, GitIndexIdempotencyKey, GitIndexJournalPhaseV1,
    GitIndexPreviewDispositionV1, GitIndexPreviewId, GitIndexPreviewV1, GitIndexReceiptId,
    GitIndexReceiptOutcomeV1, GitIndexSigningPolicyV1, GitIndexTransactionId,
    GitIndexTransactionJournalV1, GitIndexTransactionOperationV1, GitIndexTransactionReceiptV1,
    GitObjectFormatV1, GitOidV1, GitOperationStateV1, ManifestDigest, ProjectId, RefId,
    RepositoryId, RepositoryIndexSnapshotV1, RepositoryIndexStateV1, RepositoryStateSnapshotV1,
    RepositoryWorkingTreeSnapshotV1, RepositoryWorkingTreeStateV1, UtcMicros, WorktreeId,
    canonical_sha256,
};
use tracedecay_policy::{GitConflictRiskV1, GitEffectAuthorizationV1, GitEffectClassifierV1};
use tracedecay_store::{
    GitIndexTransactionBeginRequestV1, GitIndexTransactionBeginResultV1, GitIndexTransactionStore,
    GitIndexTransactionStoreError, GitIndexTransactionStoreResult,
    GitIndexTransactionTerminalWriteV1,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::owner::{DaemonGitAuthoritySource, DaemonGitIndexPolicyRecheck, preview_conflict_risk};
use super::queue::{RepositoryMutationQueue, RepositoryMutationQueueError};
use super::recovery::{GitIndexRecoveryError, GitIndexRecoveryExecutor};
use super::service::{
    CurrentGitIndexPolicyStateV1, GitIndexNativeExecutor, GitIndexPolicyRecheckPort,
    NativeGitIndexApplyOutcomeV1, NativeGitIndexApplyResult,
};
use super::store::DaemonGitIndexTransactionStore;
use super::{
    DaemonGitAuthorityStateV1, DaemonGitIndexTransactionPort, DaemonGitIndexTransactionService,
    DaemonGitIndexTransactionServiceRegistry,
};
use crate::db::engine::TestConnection;

#[test]
fn same_repository_mutations_never_enter_the_native_section_together() {
    let queue = Arc::new(RepositoryMutationQueue::default());
    let repository = RepositoryId::new("repository.fixture").expect("repository id");
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let workers: Vec<_> = (0..4)
        .map(|_| {
            let queue = Arc::clone(&queue);
            let repository = repository.clone();
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            thread::spawn(move || {
                queue
                    .with_repository(&repository, || {
                        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(current, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(5));
                        active.fetch_sub(1, Ordering::SeqCst);
                    })
                    .expect("queue is available");
            })
        })
        .collect();
    for worker in workers {
        worker.join().expect("worker joins");
    }

    assert_eq!(peak.load(Ordering::SeqCst), 1);
}

#[test]
fn repository_mutation_capacity_fails_closed_before_waiting() {
    let queue = RepositoryMutationQueue::with_capacity_for_test(1);
    let repository = RepositoryId::new("repository.capacity").expect("repository id");
    let nested = queue
        .with_repository(&repository, || queue.with_repository(&repository, || ()))
        .expect("outer mutation admitted");

    assert!(matches!(
        nested,
        Err(RepositoryMutationQueueError::Saturated)
    ));
    assert!(
        queue.with_repository(&repository, || ()).is_ok(),
        "capacity is released when an operation exits"
    );
}

#[test]
fn queued_repository_mutation_observes_live_cancellation() {
    let queue = Arc::new(RepositoryMutationQueue::default());
    let repository = RepositoryId::new("repository.cancellation").expect("repository id");
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder_queue = Arc::clone(&queue);
    let holder_repository = repository.clone();
    let holder = thread::spawn(move || {
        holder_queue
            .with_repository(&holder_repository, || {
                entered_tx.send(()).expect("signal queue holder");
                release_rx.recv().expect("release queue holder");
            })
            .expect("holder queue");
    });
    entered_rx.recv().expect("queue holder entered");

    let cancellation = Arc::new(AtomicBool::new(false));
    let checks = Arc::new(AtomicUsize::new(0));
    let waiter_queue = Arc::clone(&queue);
    let waiter_cancellation = Arc::clone(&cancellation);
    let waiter_checks = Arc::clone(&checks);
    let waiter = thread::spawn(move || {
        waiter_queue
            .with_repository_cancellable(
                &repository,
                || {
                    waiter_checks.fetch_add(1, Ordering::SeqCst);
                    waiter_cancellation
                        .load(Ordering::SeqCst)
                        .then_some(UtcMicros(25))
                },
                |observed| observed.is_some(),
            )
            .expect("waiter queue")
    });

    while checks.load(Ordering::SeqCst) < 2 {
        thread::yield_now();
    }
    cancellation.store(true, Ordering::SeqCst);
    let checks_before_cancellation = checks.load(Ordering::SeqCst);
    while checks.load(Ordering::SeqCst) == checks_before_cancellation {
        thread::yield_now();
    }
    release_tx.send(()).expect("release holder");

    holder.join().expect("holder joins");
    assert!(waiter.join().expect("waiter joins"));
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("fixture identity")
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("fixture digest")
}

fn oid(byte: char) -> GitOidV1 {
    GitOidV1::new(byte.to_string().repeat(40)).expect("fixture object id")
}

fn snapshot() -> RepositoryStateSnapshotV1 {
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

fn commit_intent() -> GitIndexCommitIntentV1 {
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

fn preview() -> GitIndexPreviewV1 {
    preview_with_expiry(UtcMicros(100))
}

fn preview_with_expiry(expires_at: UtcMicros) -> GitIndexPreviewV1 {
    let snapshot = snapshot();
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&snapshot).expect("snapshot digest");
    GitIndexPreviewV1::new_with_commit_intent(
        GitIndexPreviewId::new("preview.fixture").expect("preview id"),
        GitIndexTransactionOperationV1::CommitIndex,
        snapshot.clone(),
        snapshot_digest,
        Vec::new(),
        snapshot.index.tree_id.clone(),
        Some(&commit_intent()),
        GitIndexPreviewDispositionV1::Applicable,
        UtcMicros(10),
        expires_at,
    )
    .expect("preview")
}

#[test]
fn policy_recheck_uses_previewed_conflict_evidence() {
    let clean = preview();
    assert_eq!(preview_conflict_risk(&clean), GitConflictRiskV1::NoneKnown);

    let mut possible = clean.clone();
    possible.repository_snapshot.operation_state = GitOperationStateV1::Merge;
    assert_eq!(
        preview_conflict_risk(&possible),
        GitConflictRiskV1::Possible
    );

    let mut confirmed = clean;
    confirmed.repository_snapshot.index.state = RepositoryIndexStateV1::Unmerged;
    assert_eq!(
        preview_conflict_risk(&confirmed),
        GitConflictRiskV1::Confirmed
    );
}

fn apply_request(preview: &GitIndexPreviewV1, key: &str) -> GitIndexApplyRequestV1 {
    let capability_id = CapabilityId::new("capability.git.commit-index").expect("capability");
    let use_case_id = UseCaseId::new("use-case.git.commit-index").expect("use case");
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
        BTreeSet::from([capability_id.clone()]),
        BTreeSet::from([use_case_id.clone()]),
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
        binding: GitIndexOperationBindingV1 {
            capability_id,
            use_case_id,
            operation: GitIndexTransactionOperationV1::CommitIndex,
        },
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

fn transaction_id_for(
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

fn receipt_for(
    transaction_id: &GitIndexTransactionId,
    preview: &GitIndexPreviewV1,
    outcome: GitIndexReceiptOutcomeV1,
) -> GitIndexTransactionReceiptV1 {
    let (final_snapshot, new_index_tree, new_head, created_commit) = match outcome {
        GitIndexReceiptOutcomeV1::Committed => {
            (digest('f'), Some(oid('c')), Some(oid('d')), Some(oid('d')))
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

fn execution_for(
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

#[derive(Clone, Copy, Debug)]
enum NativeMode {
    ProvenNoMutation,
    CommitBoundaryUnknown,
    Completed(GitIndexReceiptOutcomeV1),
}

#[derive(Clone, Copy, Debug)]
enum RecoveryMode {
    AbortedNoChange,
    NeedsInspection,
    Error,
}

struct FakeNative {
    apply_modes: Mutex<VecDeque<NativeMode>>,
    recovery_modes: Mutex<VecDeque<RecoveryMode>>,
    apply_calls: Arc<AtomicUsize>,
    recovery_calls: Arc<AtomicUsize>,
    discard_calls: Arc<AtomicUsize>,
    entered_native: Arc<AtomicBool>,
}

impl FakeNative {
    fn new(
        apply_modes: impl IntoIterator<Item = NativeMode>,
        recovery_modes: impl IntoIterator<Item = RecoveryMode>,
    ) -> Self {
        Self {
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
        Err(GitIndexTransactionPortError::StalePreview)
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

struct TestPolicy {
    allow: Arc<std::sync::atomic::AtomicBool>,
    calls: Arc<AtomicUsize>,
    evaluated_at: Arc<Mutex<Option<UtcMicros>>>,
}

impl TestPolicy {
    fn allowing() -> Self {
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

struct MutableDaemonGitAuthority {
    current: Mutex<DaemonGitAuthorityStateV1>,
}

impl DaemonGitAuthoritySource for MutableDaemonGitAuthority {
    fn current_capability(
        &self,
        _capability_id: &CapabilityId,
    ) -> Result<DaemonGitAuthorityStateV1, GitIndexTransactionPortError> {
        Ok(self.current.lock().expect("authority state").clone())
    }
}

#[test]
fn daemon_policy_recheck_rejects_a_capability_revoked_after_preview() {
    let preview = preview();
    let request = apply_request(&preview, "idempotency.revoked-authority");
    let source = Arc::new(MutableDaemonGitAuthority {
        current: Mutex::new(DaemonGitAuthorityStateV1 {
            scope: request.context.scope().clone(),
            requester: request.context.actor().clone(),
            effective_capabilities: BTreeSet::from([request.binding.capability_id.clone()]),
            grant_expires_at: request.context.grant().expires_at,
            policy_revision: request.authority.policy.revision,
            policy_digest: request.proof.policy_digest.clone(),
            configuration_digest: request.proof.configuration_digest.clone(),
            catalog_digest: request.proof.catalog_digest.clone(),
            privacy_digest: request.proof.privacy_digest.clone(),
            evaluated_at: request.observed_at,
        }),
    });
    let recheck = DaemonGitIndexPolicyRecheck::new(source.clone());

    recheck
        .recheck(&request, &preview)
        .expect("current authenticated authority admits apply");
    source
        .current
        .lock()
        .expect("authority state")
        .effective_capabilities
        .clear();

    assert_eq!(
        recheck.recheck(&request, &preview),
        Err(GitIndexTransactionPortError::PolicyDenied)
    );
}

type TestPort = DaemonGitIndexTransactionPort<
    DaemonGitIndexTransactionStore,
    FakeNative,
    GitEffectClassifierV1,
    TestPolicy,
>;

struct TestHarness {
    _directory: tempfile::TempDir,
    port: TestPort,
    preview: GitIndexPreviewV1,
    request: GitIndexApplyRequestV1,
    apply_calls: Arc<AtomicUsize>,
    recovery_calls: Arc<AtomicUsize>,
    discard_calls: Arc<AtomicUsize>,
    entered_native: Arc<AtomicBool>,
    allow: Arc<std::sync::atomic::AtomicBool>,
    policy_calls: Arc<AtomicUsize>,
    policy_evaluated_at: Arc<Mutex<Option<UtcMicros>>>,
}

fn test_store(directory: &tempfile::TempDir) -> DaemonGitIndexTransactionStore {
    let path = directory.path().join("canonical-project.db");
    DaemonGitIndexTransactionStore::open_engine_test(TestConnection::open(&path))
        .expect("canonical store actor")
}

struct StartupUnavailableStore(DaemonGitIndexTransactionStore);

impl GitIndexTransactionStore for StartupUnavailableStore {
    fn save_preview(&self, preview: GitIndexPreviewV1) -> GitIndexTransactionStoreResult<()> {
        self.0.save_preview(preview)
    }

    fn read_preview(
        &self,
        preview_id: &GitIndexPreviewId,
    ) -> GitIndexTransactionStoreResult<Option<GitIndexPreviewV1>> {
        self.0.read_preview(preview_id)
    }

    fn begin_or_replay(
        &self,
        request: GitIndexTransactionBeginRequestV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionBeginResultV1> {
        self.0.begin_or_replay(request)
    }

    fn compare_and_swap_journal(
        &self,
        idempotency_key: &GitIndexIdempotencyKey,
        expected_phase_epoch: u64,
        replacement: GitIndexTransactionJournalV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionJournalV1> {
        self.0
            .compare_and_swap_journal(idempotency_key, expected_phase_epoch, replacement)
    }

    fn write_terminal(
        &self,
        write: GitIndexTransactionTerminalWriteV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionReceiptV1> {
        self.0.write_terminal(write)
    }

    fn recovery_candidates(
        &self,
        repository_id: &RepositoryId,
    ) -> GitIndexTransactionStoreResult<Vec<tracedecay_store::GitIndexTransactionRecordV1>> {
        self.0.recovery_candidates(repository_id)
    }

    fn recovery_repositories(&self) -> GitIndexTransactionStoreResult<Vec<RepositoryId>> {
        Err(GitIndexTransactionStoreError::Unavailable)
    }

    fn quarantine_repository(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &GitIndexTransactionId,
    ) -> GitIndexTransactionStoreResult<()> {
        self.0.quarantine_repository(repository_id, transaction_id)
    }

    fn clear_repository_quarantine(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &GitIndexTransactionId,
        recovery_receipt: GitIndexTransactionReceiptV1,
    ) -> GitIndexTransactionStoreResult<()> {
        self.0
            .clear_repository_quarantine(repository_id, transaction_id, recovery_receipt)
    }
}

fn test_port(
    native_modes: impl IntoIterator<Item = NativeMode>,
    recovery_modes: impl IntoIterator<Item = RecoveryMode>,
) -> TestHarness {
    let directory = tempfile::tempdir().expect("store directory");
    let store = test_store(&directory);
    let preview = preview();
    store.save_preview(preview.clone()).expect("save preview");
    let request = apply_request(&preview, "idempotency.fixture");
    let native = FakeNative::new(native_modes, recovery_modes);
    let apply_calls = Arc::clone(&native.apply_calls);
    let recovery_calls = Arc::clone(&native.recovery_calls);
    let discard_calls = Arc::clone(&native.discard_calls);
    let entered_native = Arc::clone(&native.entered_native);
    let policy = TestPolicy::allowing();
    let allow = Arc::clone(&policy.allow);
    let policy_calls = Arc::clone(&policy.calls);
    let policy_evaluated_at = Arc::clone(&policy.evaluated_at);
    TestHarness {
        _directory: directory,
        port: DaemonGitIndexTransactionPort::new(
            store,
            native,
            GitEffectClassifierV1::default(),
            policy,
        ),
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

fn seed_prepared_transaction(
    store: &DaemonGitIndexTransactionStore,
    preview: &GitIndexPreviewV1,
    request: &GitIndexApplyRequestV1,
) {
    let transaction_id = transaction_id_for(request, preview);
    let begin = GitIndexTransactionBeginRequestV1 {
        idempotency_key: GitIndexIdempotencyKey::new(request.idempotency_key.as_str().to_owned())
            .expect("native idempotency key"),
        input_digest: request.input_digest().expect("input digest"),
        preview: preview.clone(),
        journal: GitIndexTransactionJournalV1::prepared(
            transaction_id,
            preview,
            request.observed_at,
        )
        .expect("prepared journal"),
    };
    assert!(matches!(
        store.begin_or_replay(begin),
        Ok(GitIndexTransactionBeginResultV1::Started(_))
    ));
}

fn seed_terminal_abort(
    store: &DaemonGitIndexTransactionStore,
    preview: &GitIndexPreviewV1,
    request: &GitIndexApplyRequestV1,
) {
    seed_prepared_transaction(store, preview, request);
    let transaction_id = transaction_id_for(request, preview);
    let mut journal = GitIndexTransactionJournalV1::prepared(
        transaction_id.clone(),
        preview,
        request.observed_at,
    )
    .expect("prepared journal");
    journal
        .advance(GitIndexJournalPhaseV1::AbortedNoChange, request.observed_at)
        .expect("abort transition");
    store
        .write_terminal(tracedecay_store::GitIndexTransactionTerminalWriteV1 {
            idempotency_key: GitIndexIdempotencyKey::new(
                request.idempotency_key.as_str().to_owned(),
            )
            .expect("native idempotency key"),
            expected_phase_epoch: journal.phase_epoch,
            journal,
            receipt: receipt_for(
                &transaction_id,
                preview,
                GitIndexReceiptOutcomeV1::AbortedNoChange,
            ),
        })
        .expect("terminal receipt");
}

#[test]
fn safe_native_failure_receives_terminal_abort_and_replays_without_native_work() {
    let harness = test_port([NativeMode::ProvenNoMutation], []);

    let first = harness
        .port
        .apply(&harness.request)
        .expect("safe failure is receipted");
    assert_eq!(
        first.receipt.outcome,
        GitIndexReceiptOutcomeV1::AbortedNoChange
    );
    assert!(!first.receipt.final_snapshot_captured);
    let replay = harness
        .port
        .apply(&harness.request)
        .expect("terminal replay");
    assert_eq!(replay.receipt, first.receipt);
    assert_eq!(harness.apply_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.recovery_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.policy_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn terminal_replay_bypasses_revalidated_policy_but_conflicting_effect_input_rejects() {
    let harness = test_port(
        [NativeMode::Completed(GitIndexReceiptOutcomeV1::Committed)],
        [],
    );

    let first = harness
        .port
        .apply(&harness.request)
        .expect("complete transaction");
    harness.allow.store(false, Ordering::SeqCst);
    let replay = harness
        .port
        .apply(&harness.request)
        .expect("replay ignores later policy denial");
    assert_eq!(replay.receipt, first.receipt);
    assert_eq!(harness.apply_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.policy_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.discard_calls.load(Ordering::SeqCst), 1);

    let mut revalidated = harness.request.clone();
    revalidated.proof.configuration_digest = digest('e');
    let replay = harness
        .port
        .apply(&revalidated)
        .expect("authorization evidence does not change semantic effect identity");
    assert_eq!(replay.receipt, first.receipt);

    let mut conflicting = harness.request.clone();
    conflicting.preview_digest = digest('f');
    assert_eq!(
        harness.port.apply(&conflicting),
        Err(GitIndexTransactionPortError::IdempotencyConflict)
    );
    assert_eq!(harness.apply_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn rejected_admitted_apply_discards_ephemeral_preview_material() {
    let harness = test_port([], []);
    harness.allow.store(false, Ordering::SeqCst);

    let result = harness.port.apply(&harness.request);

    assert!(
        result.is_ok(),
        "a pre-native denial receives a no-change receipt"
    );
    assert_eq!(harness.apply_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.discard_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn deadline_elapsed_during_policy_recheck_never_reaches_native_git() {
    let harness = test_port([], []);
    *harness
        .policy_evaluated_at
        .lock()
        .expect("policy evaluated-at") = Some(harness.request.context.deadline().expires_at);

    let result = harness
        .port
        .apply(&harness.request)
        .expect("elapsed deadline is a durable no-change result");

    assert_eq!(
        result.receipt.outcome,
        GitIndexReceiptOutcomeV1::AbortedNoChange
    );
    assert_eq!(harness.apply_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.discard_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn cancellation_at_the_last_pre_native_boundary_returns_a_cancelled_receipt() {
    let harness = test_port([], []);
    let cancellation_checks = AtomicUsize::new(0);

    let result = harness
        .port
        .apply_cancellable(&harness.request, || {
            (cancellation_checks.fetch_add(1, Ordering::SeqCst) + 1 >= 5).then_some(UtcMicros(25))
        })
        .expect("pre-native cancellation is a durable no-change result");

    assert_eq!(
        result.receipt.outcome,
        GitIndexReceiptOutcomeV1::AbortedNoChange
    );
    assert_eq!(
        result.execution.termination,
        OperationTermination::Cancelled
    );
    let cancellation = result
        .execution
        .cancellation
        .as_ref()
        .expect("canonical cancellation observation");
    assert_eq!(cancellation.stage, CancellationStage::BeforeEffect);
    assert_eq!(cancellation.observed_at, UtcMicros(25));
    assert_eq!(harness.apply_calls.load(Ordering::SeqCst), 0);
    result
        .validate_for(&harness.request)
        .expect("cancelled apply result contract");
}

#[test]
fn cancellation_after_native_entry_does_not_rewrite_the_terminal_outcome() {
    let harness = test_port([NativeMode::ProvenNoMutation], []);

    let result = harness
        .port
        .apply_cancellable(&harness.request, || {
            harness
                .entered_native
                .load(Ordering::SeqCst)
                .then_some(UtcMicros(25))
        })
        .expect("native outcome remains authoritative");

    assert_eq!(harness.apply_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        result.receipt.outcome,
        GitIndexReceiptOutcomeV1::AbortedNoChange
    );
    assert_eq!(result.execution.termination, OperationTermination::Failed);
    assert!(result.execution.cancellation.is_none());
}

#[test]
fn terminal_replay_bypasses_preview_expiry_before_policy_or_native_execution() {
    let directory = tempfile::tempdir().expect("store directory");
    let store = test_store(&directory);
    let preview = preview_with_expiry(UtcMicros(14));
    let request = apply_request(&preview, "idempotency.expired-replay");
    seed_terminal_abort(&store, &preview, &request);
    let native = FakeNative::new([], []);
    let apply_calls = Arc::clone(&native.apply_calls);
    let policy = TestPolicy::allowing();
    policy.allow.store(false, Ordering::SeqCst);
    let policy_calls = Arc::clone(&policy.calls);
    let port =
        DaemonGitIndexTransactionPort::new(store, native, GitEffectClassifierV1::default(), policy);

    let replay = port.apply(&request).expect("expired terminal replay");
    assert_eq!(
        replay.receipt.outcome,
        GitIndexReceiptOutcomeV1::AbortedNoChange
    );
    assert_eq!(apply_calls.load(Ordering::SeqCst), 0);
    assert_eq!(policy_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn ambiguous_boundary_reconciles_once_without_replaying_native_apply() {
    let harness = test_port(
        [NativeMode::CommitBoundaryUnknown],
        [RecoveryMode::AbortedNoChange],
    );

    let result = harness
        .port
        .apply(&harness.request)
        .expect("reconciliation result");
    assert_eq!(
        result.receipt.outcome,
        GitIndexReceiptOutcomeV1::AbortedNoChange
    );
    assert_eq!(harness.apply_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.recovery_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn needs_inspection_quarantines_new_keys_but_allows_terminal_replay() {
    let harness = test_port(
        [NativeMode::CommitBoundaryUnknown],
        [RecoveryMode::NeedsInspection],
    );

    let first = harness
        .port
        .apply(&harness.request)
        .expect("inspection receipt");
    assert_eq!(
        first.receipt.outcome,
        GitIndexReceiptOutcomeV1::NeedsInspection
    );
    let replay = harness
        .port
        .apply(&harness.request)
        .expect("terminal replay remains available");
    assert_eq!(replay.receipt, first.receipt);
    let new_key = apply_request(&harness.preview, "idempotency.new-key");
    assert_eq!(
        harness.port.apply(&new_key),
        Err(GitIndexTransactionPortError::RecoveryRequired)
    );
    assert_eq!(harness.apply_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.recovery_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn indeterminate_recovery_keeps_durable_quarantine_without_native_replay() {
    let harness = test_port([NativeMode::CommitBoundaryUnknown], [RecoveryMode::Error]);

    let first = harness
        .port
        .apply(&harness.request)
        .expect("unobservable recovery terminalizes inspection");
    assert_eq!(
        first.receipt.outcome,
        GitIndexReceiptOutcomeV1::NeedsInspection
    );
    assert!(!first.receipt.final_snapshot_captured);
    let replay = harness
        .port
        .apply(&harness.request)
        .expect("inspection receipt replays");
    assert_eq!(replay.receipt, first.receipt);
    let new_key = apply_request(&harness.preview, "idempotency.indeterminate-new-key");
    assert_eq!(
        harness.port.apply(&new_key),
        Err(GitIndexTransactionPortError::RecoveryRequired)
    );
    assert_eq!(harness.apply_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.recovery_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn nonterminal_key_is_recovery_only_and_startup_recovery_is_idempotent() {
    let directory = tempfile::tempdir().expect("store directory");
    let store = test_store(&directory);
    let preview = preview();
    let request = apply_request(&preview, "idempotency.fixture");
    seed_prepared_transaction(&store, &preview, &request);
    let native = FakeNative::new([], [RecoveryMode::AbortedNoChange]);
    let apply_calls = Arc::clone(&native.apply_calls);
    let recovery_calls = Arc::clone(&native.recovery_calls);
    let policy = TestPolicy::allowing();
    let port =
        DaemonGitIndexTransactionPort::new(store, native, GitEffectClassifierV1::default(), policy);

    assert_eq!(
        port.apply(&request),
        Err(GitIndexTransactionPortError::RecoveryRequired)
    );
    assert_eq!(apply_calls.load(Ordering::SeqCst), 0);
    let recovered = port
        .recover_startup(UtcMicros(20))
        .expect("startup recovery");
    assert_eq!(recovered.len(), 1);
    assert_eq!(
        recovered[0].outcome,
        GitIndexReceiptOutcomeV1::AbortedNoChange
    );
    assert!(
        port.recover_startup(UtcMicros(21))
            .expect("idempotent startup recovery")
            .is_empty()
    );
    assert_eq!(recovery_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn startup_service_recovers_before_exposing_the_mutation_port() {
    let directory = tempfile::tempdir().expect("store directory");
    let store = test_store(&directory);
    let preview = preview();
    let request = apply_request(&preview, "idempotency.startup-service");
    seed_prepared_transaction(&store, &preview, &request);
    let native = FakeNative::new([], [RecoveryMode::AbortedNoChange]);
    let apply_calls = Arc::clone(&native.apply_calls);
    let recovery_calls = Arc::clone(&native.recovery_calls);

    let service = DaemonGitIndexTransactionService::start(
        store,
        native,
        GitEffectClassifierV1::default(),
        TestPolicy::allowing(),
        UtcMicros(20),
    )
    .expect("startup recovery completes before service publication");
    assert_eq!(recovery_calls.load(Ordering::SeqCst), 1);

    let replay = service
        .apply(&request)
        .expect("recovered terminal receipt is immediately replayable");
    assert_eq!(
        replay.receipt.outcome,
        GitIndexReceiptOutcomeV1::AbortedNoChange
    );
    assert_eq!(apply_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn startup_recovery_failure_returns_no_mutation_service() {
    let directory = tempfile::tempdir().expect("store directory");
    let store = StartupUnavailableStore(test_store(&directory));
    let native = FakeNative::new([], []);
    let apply_calls = Arc::clone(&native.apply_calls);

    let result = DaemonGitIndexTransactionService::start(
        store,
        native,
        GitEffectClassifierV1::default(),
        TestPolicy::allowing(),
        UtcMicros(20),
    );

    assert!(matches!(
        result,
        Err(GitIndexTransactionPortError::DaemonUnavailable)
    ));
    assert_eq!(apply_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn quarantine_clears_only_after_a_proven_recovery_receipt() {
    let harness = test_port(
        [NativeMode::CommitBoundaryUnknown],
        [RecoveryMode::NeedsInspection, RecoveryMode::AbortedNoChange],
    );
    let receipt = harness
        .port
        .apply(&harness.request)
        .expect("inspection receipt")
        .receipt;
    assert_eq!(receipt.outcome, GitIndexReceiptOutcomeV1::NeedsInspection);

    let new_key = apply_request(&harness.preview, "idempotency.blocked");
    assert_eq!(
        harness.port.apply(&new_key),
        Err(GitIndexTransactionPortError::RecoveryRequired)
    );

    let cleared = harness
        .port
        .recover_startup(UtcMicros(20))
        .expect("proven clear");
    assert_eq!(cleared.len(), 1);
    assert_eq!(
        cleared[0].outcome,
        GitIndexReceiptOutcomeV1::AbortedNoChange
    );
    let admitted = harness
        .port
        .apply(&new_key)
        .expect("new key after proven clear");
    assert_eq!(
        admitted.receipt.outcome,
        GitIndexReceiptOutcomeV1::AbortedNoChange
    );
    assert_eq!(harness.recovery_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn daemon_owner_reuses_one_service_for_the_same_project_database() {
    let directory = tempfile::tempdir().expect("project directory");
    let database_path = directory.path().join("canonical-project.db");
    rusqlite::Connection::open(&database_path).expect("canonical project database");
    let registry = DaemonGitIndexTransactionServiceRegistry::default();
    let project_id = id::<ProjectId>("project.singleton.fixture");

    let first = registry
        .ensure_engine_test(
            database_path.clone(),
            directory.path().to_path_buf(),
            project_id.clone(),
            UtcMicros(20),
        )
        .await
        .expect("first project service");
    let second = registry
        .ensure_engine_test(
            database_path,
            directory.path().to_path_buf(),
            project_id,
            UtcMicros(21),
        )
        .await
        .expect("reused project service");

    assert!(Arc::ptr_eq(&first, &second));
}

#[tokio::test]
async fn daemon_owner_rejects_rebinding_a_project_database_to_another_worktree_root() {
    let directory = tempfile::tempdir().expect("project directory");
    let alternate = tempfile::tempdir().expect("alternate worktree directory");
    let database_path = directory.path().join("canonical-project.db");
    rusqlite::Connection::open(&database_path).expect("canonical project database");
    let registry = DaemonGitIndexTransactionServiceRegistry::default();
    let project_id = id::<ProjectId>("project.singleton.fixture");
    registry
        .ensure_engine_test(
            database_path.clone(),
            directory.path().to_path_buf(),
            project_id.clone(),
            UtcMicros(20),
        )
        .await
        .expect("first project service");

    assert_eq!(
        registry
            .ensure_engine_test(
                database_path,
                alternate.path().to_path_buf(),
                project_id,
                UtcMicros(21),
            )
            .await
            .map(|_| ()),
        Err(GitIndexTransactionPortError::PolicyDenied),
        "one database must not silently reuse a native executor rooted at another worktree"
    );
}

/// Symlink alias of an already-mounted root must resolve to the same owner
/// entry. Distinct filesystem identities remain rejected (see rebinding test).
#[cfg(unix)]
#[tokio::test]
async fn daemon_owner_resolves_symlink_alias_to_the_canonical_mounted_root() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("project directory");
    let alias_parent = tempfile::tempdir().expect("alias parent");
    let alias = alias_parent.path().join("worktree-alias");
    symlink(directory.path(), &alias).expect("worktree root symlink alias");
    let database_path = directory.path().join("canonical-project.db");
    rusqlite::Connection::open(&database_path).expect("canonical project database");
    let registry = DaemonGitIndexTransactionServiceRegistry::default();
    let project_id = id::<ProjectId>("project.alias.fixture");
    let first = registry
        .ensure_engine_test(
            database_path.clone(),
            directory.path().to_path_buf(),
            project_id.clone(),
            UtcMicros(30),
        )
        .await
        .expect("mount through real root");
    let second = registry
        .ensure_engine_test(database_path, alias, project_id, UtcMicros(31))
        .await
        .expect("reuse through symlink alias");
    assert!(
        Arc::ptr_eq(&first, &second),
        "symlink alias must resolve to the same mounted owner as the canonical root"
    );
}
