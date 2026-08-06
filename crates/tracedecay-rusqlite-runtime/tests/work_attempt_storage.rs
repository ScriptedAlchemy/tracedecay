//! Durable Work attempt storage contract: fence monotonicity, idempotent
//! admission, fenced compare-and-swap transitions, authority isolation, and
//! restart durability over the registered exact-SQL channel.

mod work_registered_store;

use std::collections::BTreeSet;

use tracedecay_application::{
    WorkAttemptEvidenceRecordV1, WorkAttemptInsertOutcome, WorkAttemptProviderOutcomeV1,
    WorkAttemptStorageError, WorkAttemptStoragePort,
};
use tracedecay_domain::{
    ActorId, AttemptId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId, ManifestDigest,
    ProjectId, ProjectionGenerationId, ProposalId, ProviderId, RefId, RepositoryId, RunId, TaskId,
    UtcMicros, WorkApprovalPolicy, WorkAttemptIdentityV1, WorkAttemptProjectionBindingV1,
    WorkAttemptStateV1, WorkAttemptV1, WorkAuthority, WorkCancellationStateV1, WorkEffectStateV1,
    WorkEgressPolicy, WorkExecutableReference, WorkExecutionEnvelopeV1, WorkExecutionLimits,
    WorkExecutionSnapshot, WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFenceEpochV1,
    WorkFilesystemPolicy, WorkLeaseFenceV1, WorkLeaseId, WorkProviderBackendV1,
    WorkProviderProtocol, WorkProviderRouteId, WorkProviderRouteV1, WorkRecoveryStateV1,
    WorkSandboxPolicy, WorkTerminalEvidenceV1, WorkVersion, WorkflowOperationRef, WorktreeId,
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

fn authority(actor: &str) -> WorkAuthority {
    WorkAuthority::new(
        id::<ProjectId>("project.attempt.storage"),
        id::<RepositoryId>("repository.attempt.storage"),
        id::<WorktreeId>("worktree.attempt.storage"),
        id::<ActorId>(actor),
        digest('a'),
    )
    .unwrap()
}

fn identity(attempt: &str) -> WorkAttemptIdentityV1 {
    WorkAttemptIdentityV1::new(
        id::<TaskId>("task.attempt.storage"),
        id::<RunId>("run.attempt.storage"),
        id::<AttemptId>(attempt),
    )
    .unwrap()
}

fn lease(epoch: u64) -> WorkLeaseFenceV1 {
    WorkLeaseFenceV1::new(
        id::<WorkLeaseId>("lease.attempt.storage"),
        WorkFenceEpochV1::new(epoch).unwrap(),
    )
    .unwrap()
}

fn requested_route() -> WorkProviderRouteV1 {
    WorkProviderRouteV1::new(
        id::<ProviderId>("provider.work.claude-code-cli"),
        id::<WorkProviderRouteId>("route.attempt.claude-code.v1"),
    )
    .unwrap()
}

fn execution_snapshot() -> WorkExecutionSnapshot {
    WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>("configuration-revision.att.1"),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>("configuration-snapshot.att.1"),
        effective_behavior_digest: digest('c'),
        resolution_provenance_digest: digest('d'),
        route: requested_route(),
        backend: WorkProviderBackendV1::ClaudeCodeCli,
        protocol: WorkProviderProtocol::ClaudeStreamJson,
        model: "claude-test".to_owned(),
        executable: WorkExecutableReference::new(
            "executable.claude.code-cli".to_owned(),
            digest('e'),
        )
        .unwrap(),
        sandbox: WorkSandboxPolicy::Required,
        approval: WorkApprovalPolicy::Never,
        filesystem: WorkFilesystemPolicy::WorkspaceWrite,
        egress: WorkEgressPolicy::Deny,
        environment_allowlist: BTreeSet::new(),
        credential_references: BTreeSet::new(),
        limits: WorkExecutionLimits::new(128_000, 8_192, 16_384, 16_384, 65_536, 1).unwrap(),
        deadline: UtcMicros(1_000_000),
        fallback: WorkFallbackTopology::Disabled,
        topology_policy_digest: digest('f'),
    })
    .unwrap()
}

fn attempt(attempt_id: &str, epoch: u64) -> WorkAttemptV1 {
    let identity = identity(attempt_id);
    let binding = WorkAttemptProjectionBindingV1::new(
        id::<ProjectionGenerationId>("generation.attempt.storage"),
        tracedecay_domain::WorkProjectionSequenceV1::new(7),
        WorkVersion::new(3).unwrap(),
        id::<ProposalId>("proposal.attempt.storage"),
    )
    .unwrap();
    let envelope = WorkExecutionEnvelopeV1::new(
        identity.clone(),
        binding.clone(),
        id::<WorkflowOperationRef>("operation.attempt.execute-provider"),
        execution_snapshot(),
        id::<ProjectId>("project.attempt.storage"),
        id::<RepositoryId>("repository.attempt.storage"),
        id::<WorktreeId>("worktree.attempt.storage"),
        "/tmp/attempt-storage".to_owned(),
        Some(id::<RefId>("refs/heads/attempt-storage")),
        id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        "Execute the admitted provider step.".to_owned(),
        1,
        WorkEffectStateV1::Observational,
    )
    .unwrap();
    WorkAttemptV1::new(
        identity,
        binding,
        envelope,
        lease(epoch),
        WorkAttemptStateV1::Leased,
        None,
        Vec::new(),
        WorkCancellationStateV1::None,
        WorkRecoveryStateV1::Fresh,
        requested_route(),
        None,
        None,
    )
    .unwrap()
}

fn running(attempt: &WorkAttemptV1) -> WorkAttemptV1 {
    attempt
        .transition(
            WorkAttemptStateV1::Running,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(requested_route()),
            None,
            attempt.lease().clone(),
        )
        .unwrap()
}

fn evidence(attempt: &WorkAttemptV1) -> WorkAttemptEvidenceRecordV1 {
    WorkAttemptEvidenceRecordV1 {
        identity: attempt.identity().clone(),
        requested_route: attempt.requested_route().clone(),
        actual_route: Some(requested_route()),
        outcome: WorkAttemptProviderOutcomeV1::Exited { code: 0 },
        stdout: None,
        stderr: None,
        observed_at: UtcMicros(500),
    }
}

fn succeeded(attempt: &WorkAttemptV1) -> WorkAttemptV1 {
    let terminal = WorkTerminalEvidenceV1::succeeded(digest('9'), UtcMicros(500)).unwrap();
    attempt
        .transition(
            WorkAttemptStateV1::Succeeded,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(requested_route()),
            Some(terminal),
            attempt.lease().clone(),
        )
        .unwrap()
}

#[test]
fn fence_epochs_are_monotonic_and_isolated_per_authority() {
    let store = RegisteredWorkStore::start("attempt-fences");
    let mine = authority("actor.attempt.mine");
    let peer = authority("actor.attempt.peer");
    assert_eq!(store.storage().next_fence_epoch(&mine).unwrap(), 1);
    assert_eq!(store.storage().next_fence_epoch(&mine).unwrap(), 2);
    assert_eq!(store.storage().next_fence_epoch(&mine).unwrap(), 3);
    // A different actor's fence sequence starts fresh: epochs never leak
    // across authorities.
    assert_eq!(store.storage().next_fence_epoch(&peer).unwrap(), 1);
}

#[test]
fn insert_replays_identical_admissions_and_refuses_divergent_ones() {
    let store = RegisteredWorkStore::start("attempt-insert");
    let authority = authority("actor.attempt.insert");
    let first = attempt("attempt.storage.1", 1);
    assert_eq!(
        store.storage().insert(&authority, &first).unwrap(),
        WorkAttemptInsertOutcome::Inserted
    );
    // Byte-identical admission replays without a second row.
    assert_eq!(
        store.storage().insert(&authority, &first).unwrap(),
        WorkAttemptInsertOutcome::Replayed(first.clone())
    );
    assert_eq!(store.count("work_attempts_v1"), 1);
    // The same identity with different content is a conflict, not a refresh.
    let divergent = attempt("attempt.storage.1", 2);
    assert_eq!(
        store.storage().insert(&authority, &divergent).unwrap_err(),
        WorkAttemptStorageError::AttemptConflict
    );
    assert_eq!(store.count("work_attempts_v1"), 1);
}

#[test]
fn foreign_authorities_cannot_observe_or_advance_an_attempt() {
    let store = RegisteredWorkStore::start("attempt-isolation");
    let owner = authority("actor.attempt.owner");
    let stranger = authority("actor.attempt.stranger");
    let leased = attempt("attempt.storage.1", 1);
    store.storage().insert(&owner, &leased).unwrap();
    // Absence and denial are indistinguishable for a foreign authority.
    assert_eq!(
        store
            .storage()
            .load(&stranger, leased.identity())
            .unwrap_err(),
        WorkAttemptStorageError::NotFoundOrNotAuthorized
    );
    assert!(store.storage().open_attempts(&stranger).unwrap().is_empty());
    let advanced = running(&leased);
    assert_eq!(
        store
            .storage()
            .update(
                &stranger,
                leased.lease(),
                WorkAttemptStateV1::Leased,
                &advanced,
                None,
            )
            .unwrap_err(),
        WorkAttemptStorageError::NotFoundOrNotAuthorized
    );
    // The owner's row is unchanged after the denied write.
    let loaded = store.storage().load(&owner, leased.identity()).unwrap();
    assert_eq!(loaded.state(), WorkAttemptStateV1::Leased);
}

#[test]
fn stale_fences_and_states_cannot_advance_an_attempt() {
    let store = RegisteredWorkStore::start("attempt-cas");
    let authority = authority("actor.attempt.cas");
    let leased = attempt("attempt.storage.1", 1);
    store.storage().insert(&authority, &leased).unwrap();
    let advanced = running(&leased);
    // Wrong expected state: the row stays exactly as persisted.
    assert_eq!(
        store
            .storage()
            .update(
                &authority,
                leased.lease(),
                WorkAttemptStateV1::Running,
                &advanced,
                None,
            )
            .unwrap_err(),
        WorkAttemptStorageError::FenceConflict
    );
    // Wrong expected fence epoch: also refused.
    assert_eq!(
        store
            .storage()
            .update(
                &authority,
                &lease(9),
                WorkAttemptStateV1::Leased,
                &advanced,
                None,
            )
            .unwrap_err(),
        WorkAttemptStorageError::FenceConflict
    );
    let unchanged = store.storage().load(&authority, leased.identity()).unwrap();
    assert_eq!(unchanged, leased);
    // The exact expected fence and state advance the row.
    store
        .storage()
        .update(
            &authority,
            leased.lease(),
            WorkAttemptStateV1::Leased,
            &advanced,
            None,
        )
        .unwrap();
    let loaded = store.storage().load(&authority, leased.identity()).unwrap();
    assert_eq!(loaded.state(), WorkAttemptStateV1::Running);
}

#[test]
fn terminal_attempts_leave_the_open_set_and_survive_restart() {
    let store = RegisteredWorkStore::start("attempt-restart");
    let authority = authority("actor.attempt.restart");
    let open = attempt("attempt.storage.open", 1);
    let closing = attempt("attempt.storage.done", 1);
    store.storage().insert(&authority, &open).unwrap();
    store.storage().insert(&authority, &closing).unwrap();
    let closing_running = running(&closing);
    store
        .storage()
        .update(
            &authority,
            closing.lease(),
            WorkAttemptStateV1::Leased,
            &closing_running,
            None,
        )
        .unwrap();
    let closed = succeeded(&closing_running);
    store
        .storage()
        .update(
            &authority,
            closing_running.lease(),
            WorkAttemptStateV1::Running,
            &closed,
            Some(&evidence(&closing_running)),
        )
        .unwrap();
    let open_now = store.storage().open_attempts(&authority).unwrap();
    assert_eq!(open_now.len(), 1);
    assert_eq!(open_now[0].identity(), open.identity());
    // Restart rebinds the registered channel to the same persisted rows.
    let store = store.restart("attempt-restart");
    let after = store.storage().open_attempts(&authority).unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].identity(), open.identity());
    let closed_after = store.storage().load(&authority, closed.identity()).unwrap();
    assert_eq!(closed_after.state(), WorkAttemptStateV1::Succeeded);
    assert!(closed_after.is_terminal());
}
