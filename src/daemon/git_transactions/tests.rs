use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use std::collections::BTreeSet;
use std::sync::Mutex;

use tracedecay_application::{
    CancellationStage, GitIndexApplyRequestV1, GitIndexTransactionPort,
    GitIndexTransactionPortError, OperationTermination,
};
use tracedecay_domain::{
    GitIndexIdempotencyKey, GitIndexJournalPhaseV1, GitIndexPreviewId, GitIndexPreviewV1,
    GitIndexReceiptOutcomeV1, GitIndexTransactionId, GitIndexTransactionJournalV1,
    GitIndexTransactionReceiptV1, GitOperationStateV1, ProjectId, RepositoryId,
    RepositoryIndexStateV1, UtcMicros,
};
use tracedecay_policy::{GitConflictRiskV1, GitEffectClassifierV1};
use tracedecay_store::{
    GitIndexTransactionBeginRequestV1, GitIndexTransactionBeginResultV1, GitIndexTransactionStore,
    GitIndexTransactionStoreError, GitIndexTransactionStoreResult,
    GitIndexTransactionTerminalWriteV1,
};
use tracedecay_tool_catalog::CapabilityId;

use super::owner::{DaemonGitAuthoritySource, DaemonGitIndexPolicyRecheck, preview_conflict_risk};
use super::queue::{RepositoryMutationQueue, RepositoryMutationQueueError};
use super::service::GitIndexPolicyRecheckPort;
use super::store::DaemonGitIndexTransactionStore;
use super::test_support::{
    FakeNative, NativeMode, RecoveryMode, TestHarness, TestPolicy, apply_request, digest, id,
    preview, preview_with_expiry, receipt_for, test_store, transaction_id_for,
};
use super::{
    DaemonGitAuthorityStateV1, DaemonGitIndexTransactionPort, DaemonGitIndexTransactionService,
    DaemonGitIndexTransactionServiceRegistry,
};

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
async fn daemon_owner_isolates_worktrees_sharing_a_project_database() {
    let directory = tempfile::tempdir().expect("project directory");
    let alternate = tempfile::tempdir().expect("alternate worktree directory");
    let database_path = directory.path().join("canonical-project.db");
    rusqlite::Connection::open(&database_path).expect("canonical project database");
    let registry = DaemonGitIndexTransactionServiceRegistry::default();
    let project_id = id::<ProjectId>("project.singleton.fixture");
    let primary = registry
        .ensure_engine_test(
            database_path.clone(),
            directory.path().to_path_buf(),
            project_id.clone(),
            UtcMicros(20),
        )
        .await
        .expect("first project service");
    let linked = registry
        .ensure_engine_test(
            database_path,
            alternate.path().to_path_buf(),
            project_id,
            UtcMicros(21),
        )
        .await
        .expect("linked worktree service");

    assert!(
        !Arc::ptr_eq(&primary, &linked),
        "worktrees sharing one store need independent native executors"
    );
    let primary_owner = registry
        .for_repository_root(directory.path())
        .await
        .expect("primary owner lookup")
        .expect("primary owner");
    let linked_owner = registry
        .for_repository_root(alternate.path())
        .await
        .expect("linked owner lookup")
        .expect("linked owner");
    assert!(Arc::ptr_eq(&primary_owner.service, &primary));
    assert!(Arc::ptr_eq(&linked_owner.service, &linked));
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
