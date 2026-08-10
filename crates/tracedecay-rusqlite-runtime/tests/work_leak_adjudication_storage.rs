//! Durable Work leak adjudication replay and integrity checks.

mod work_registered_store;

use tracedecay_application::{
    AdjudicateWorkLeakCommandV1, VerifiedWorkLeakEvidenceV1, WorkAttemptStoragePort,
    WorkLeakAdjudicationOutcomeV1, WorkLeakAdjudicationReceiptV1,
    WorkLeakAdjudicationStorageErrorV1, WorkLeakAdjudicationStoragePortV1,
    WorkLeakAdjudicationWriteV1,
};
use tracedecay_domain::{
    ActorId, AttemptId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId,
    CoverageStateV1, LeakOwnerClassV1, ManifestDigest, ProjectId, ProposalId, ProviderId, RefId,
    RepositoryId, RunId, TaskId, UtcMicros, WorkApprovalPolicy, WorkAttemptIdentityV1,
    WorkAttemptProjectionBindingV1, WorkAttemptStateV1, WorkAttemptV1, WorkAuthority,
    WorkCancellationStateV1, WorkCommandId, WorkEffectStateV1, WorkEgressPolicy,
    WorkExecutableReference, WorkExecutionEnvelopeV1, WorkExecutionLeakKindV1,
    WorkExecutionLeakRecoveryV1, WorkExecutionLimits, WorkExecutionSnapshot,
    WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFilesystemPolicy, WorkGraphVersionV1,
    WorkLeaseFenceV1, WorkLeaseId, WorkProductEventSequenceV1, WorkProductSourceWatermarkV1,
    WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteId, WorkProviderRouteV1,
    WorkRecoveryStateV1, WorkSandboxPolicy, WorkTerminalEvidenceV1, WorkflowOperationRef,
    WorktreeId, canonical_sha256,
};

use work_registered_store::RegisteredWorkStore;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn authority() -> WorkAuthority {
    WorkAuthority::new(
        id::<ProjectId>("project.leak-storage"),
        id::<RepositoryId>("repository.leak-storage"),
        id::<WorktreeId>("worktree.leak-storage"),
        id::<ActorId>("actor.leak-storage"),
        digest('a'),
    )
    .unwrap()
}

fn terminal_attempt() -> WorkAttemptV1 {
    let identity = WorkAttemptIdentityV1::new(
        id::<TaskId>("task.leak-storage"),
        id::<RunId>("run.leak-storage"),
        id::<AttemptId>("attempt.leak-storage"),
    )
    .unwrap();
    let binding = WorkAttemptProjectionBindingV1::new(
        WorkGraphVersionV1::new(1).unwrap(),
        WorkProductEventSequenceV1::new(1).unwrap(),
        WorkProductSourceWatermarkV1::new(Default::default()).unwrap(),
        digest('f'),
        id::<ProposalId>("proposal.leak-storage"),
    )
    .unwrap();
    let route = WorkProviderRouteV1::new(
        id::<ProviderId>("provider.work.claude-code-cli"),
        id::<WorkProviderRouteId>("route.leak-storage"),
    )
    .unwrap();
    let snapshot = WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>(
            "configuration-revision.leak-storage",
        ),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>(
            "configuration-snapshot.leak-storage",
        ),
        effective_behavior_digest: digest('b'),
        resolution_provenance_digest: digest('c'),
        route: route.clone(),
        backend: WorkProviderBackendV1::ClaudeCodeCli,
        protocol: WorkProviderProtocol::ClaudeStreamJson,
        model: "claude-test".to_owned(),
        executable: WorkExecutableReference::new("executable.leak-storage".to_owned(), digest('d'))
            .unwrap(),
        sandbox: WorkSandboxPolicy::Required,
        approval: WorkApprovalPolicy::Never,
        filesystem: WorkFilesystemPolicy::WorkspaceWrite,
        egress: WorkEgressPolicy::Deny,
        environment_allowlist: Default::default(),
        credential_references: Default::default(),
        limits: WorkExecutionLimits::new(1024, 1024, 1024, 1024, 1024, 1).unwrap(),
        deadline: UtcMicros(1_000),
        fallback: WorkFallbackTopology::Disabled,
        topology: tracedecay_domain::safe_work_topology_policy_v1(),
    })
    .unwrap();
    let execution = WorkExecutionEnvelopeV1::new(
        identity.clone(),
        binding.clone(),
        id::<WorkflowOperationRef>("operation.leak-storage"),
        snapshot,
        id::<ProjectId>("project.leak-storage"),
        id::<RepositoryId>("repository.leak-storage"),
        id::<WorktreeId>("worktree.leak-storage"),
        "/tmp/leak-storage".to_owned(),
        Some(id::<RefId>("refs/heads/leak-storage")),
        id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        "Execute the admitted provider step.".to_owned(),
        1,
        WorkEffectStateV1::Observational,
    )
    .unwrap();
    let leased = WorkAttemptV1::new(
        identity,
        binding,
        execution,
        WorkLeaseFenceV1::new(
            id::<WorkLeaseId>("lease.leak-storage"),
            tracedecay_domain::WorkFenceEpochV1::new(1).unwrap(),
        )
        .unwrap(),
        WorkAttemptStateV1::Leased,
        None,
        Vec::new(),
        WorkCancellationStateV1::None,
        WorkRecoveryStateV1::Fresh,
        route,
        None,
        None,
    )
    .unwrap();
    let running = leased
        .transition(
            WorkAttemptStateV1::Running,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(leased.requested_route().clone()),
            None,
            leased.lease().clone(),
        )
        .unwrap();
    running
        .transition(
            WorkAttemptStateV1::Failed,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(running.requested_route().clone()),
            Some(WorkTerminalEvidenceV1::failed(digest('f'), UtcMicros(10)).unwrap()),
            running.lease().clone(),
        )
        .unwrap()
}

fn leak_receipt(attempt: &WorkAttemptV1) -> WorkLeakAdjudicationReceiptV1 {
    let command = AdjudicateWorkLeakCommandV1 {
        adjudication_id: "adjudication.leak-storage".to_owned(),
        expected_revision: None,
        attempt: attempt.identity().clone(),
        detection_horizon_micros: 1_000,
        command_id: id::<WorkCommandId>("command.leak-storage"),
    };
    let evidence = VerifiedWorkLeakEvidenceV1 {
        attempt: attempt.identity().clone(),
        kind: WorkExecutionLeakKindV1::AttemptWithoutLiveOwner,
        recovery: WorkExecutionLeakRecoveryV1::Pending,
        owner_class: LeakOwnerClassV1::Work,
        coverage: CoverageStateV1::Known,
        detection_horizon_micros: command.detection_horizon_micros,
        scan_started_at: UtcMicros(20),
        scan_completed_at: UtcMicros(21),
        evidence_refs: vec!["work-leak:attempt-without-live-owner:canonical".to_owned()],
    };
    let scan_deadline = UtcMicros(30);
    let canonical_input_digest = canonical_sha256(&(
        "tracedecay.application.work-leak-adjudication.v1",
        &command,
        &evidence,
        scan_deadline,
    ))
    .unwrap();
    WorkLeakAdjudicationReceiptV1 {
        command,
        revision: 1,
        evidence,
        scan_deadline,
        canonical_input_digest,
    }
}

#[test]
fn leak_adjudication_replays_exact_receipt_across_restart() {
    let mut store = RegisteredWorkStore::start("leak-adjudication-replay");
    let authority = authority();
    let attempt = terminal_attempt();
    store.storage().insert(&authority, &attempt).unwrap();
    let receipt = leak_receipt(&attempt);
    let outcome = store
        .storage()
        .compare_and_record_leak(
            &authority,
            &WorkLeakAdjudicationWriteV1 {
                receipt: receipt.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        outcome,
        WorkLeakAdjudicationOutcomeV1::Appended(receipt.clone())
    );
    assert_eq!(store.count("work_leak_adjudications_v1"), 1);

    store = store.restart("leak-adjudication-replay");
    assert_eq!(
        store
            .storage()
            .leak_by_command(&authority, &receipt.command.command_id)
            .unwrap(),
        Some(receipt),
    );
}

#[test]
fn leak_adjudication_replay_rejects_corrupted_scalar_truth() {
    let store =
        RegisteredWorkStore::start_with_setup("leak-adjudication-corrupt-replay", |connection| {
            connection
                .execute_batch(
                    "CREATE TRIGGER corrupt_work_leak_observed_at
                     AFTER INSERT ON work_leak_adjudications_v1
                     BEGIN
                       UPDATE work_leak_adjudications_v1
                       SET observed_at = observed_at + 1
                       WHERE command_id = NEW.command_id;
                     END;",
                )
                .unwrap();
        });
    let authority = authority();
    let attempt = terminal_attempt();
    store.storage().insert(&authority, &attempt).unwrap();
    let receipt = leak_receipt(&attempt);
    store
        .storage()
        .compare_and_record_leak(
            &authority,
            &WorkLeakAdjudicationWriteV1 {
                receipt: receipt.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        store
            .storage()
            .leak_by_command(&authority, &receipt.command.command_id),
        Err(WorkLeakAdjudicationStorageErrorV1::Unavailable),
    );
}
