//! Durable Work attempt storage contract: fence monotonicity, idempotent
//! admission, fenced compare-and-swap transitions, authority isolation, and
//! restart durability over the registered exact-SQL channel.

mod common;
mod work_registered_store;

use std::{
    collections::BTreeSet,
    num::NonZeroU16,
    sync::{Arc, Barrier},
    thread,
};

use tracedecay_application::{
    VerifiedWorkRetryFailureV1, WorkAttemptAdmissionKind, WorkAttemptCapacityScopeV1,
    WorkAttemptCapacityVerdictV1, WorkAttemptEffectDispatchOutcomeV1, WorkAttemptEffectHolderV1,
    WorkAttemptEffectResolutionV1, WorkAttemptEffectStorageErrorV1, WorkAttemptEffectStoragePortV1,
    WorkAttemptEvidenceReadPort, WorkAttemptEvidenceRecordV1, WorkAttemptInsertOutcome,
    WorkAttemptProviderOutcomeV1, WorkAttemptReceiptReadPortV1, WorkAttemptStorageError,
    WorkAttemptStoragePort, WorkOwnerObservationMarkOutcomeV1, WorkOwnerObservationStoragePortV1,
    WorkRetryCauseV1, WorkRetryFailureSelectorV1, WorkRetryReceiptV1, WorkRetrySourceV1,
    WorkRetryStoragePortV1, WorkRetryWriteV1, WorkRunControlStoragePort,
    WorkSynthesisAdmissionRecordV1, WorkSynthesisAdmissionStoragePort, WorkSynthesisAdmissionV1,
    WorkSynthesisEvidenceGroupV1, WorkSynthesisInsertOutcome, WorkSynthesisSourceEnvelopeV1,
    WorkSynthesisSourceOutcomeV1, WorkSynthesisSourceSetV1, WorkflowSynthesisDraft,
};
use tracedecay_domain::configuration::TopologyConcurrencyPolicyV1;
use tracedecay_domain::{
    ActorId, AttemptId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId, ManifestDigest,
    ObservationSourceIdentityV1, ProjectId, ProposalId, ProviderId, RefId, RepositoryId, RunId,
    SessionId, TaskId, UtcMicros, WorkApprovalPolicy, WorkArtifactRefV1, WorkAttemptIdentityV1,
    WorkAttemptProjectionBindingV1, WorkAttemptStateV1, WorkAttemptV1, WorkAuthority,
    WorkCancellationStateV1, WorkEffectStateV1, WorkEgressPolicy, WorkExecutableReference,
    WorkExecutionEnvelopeV1, WorkExecutionLimits, WorkExecutionSnapshot,
    WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFenceEpochV1, WorkFilesystemPolicy,
    WorkGraphVersionV1, WorkLeaseFenceV1, WorkLeaseId, WorkProductEventSequenceV1,
    WorkProductSourceWatermarkV1, WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteId,
    WorkProviderRouteV1, WorkRecoveryStateV1, WorkRestartReasonV1, WorkRunControlReasonV1,
    WorkRunControlV1, WorkSandboxPolicy, WorkTerminalEvidenceV1, WorkflowOperationRef,
    WorkflowOutputName, WorktreeId,
};

use common::fixture_abs_root;
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
    authority_in_worktree(actor, "worktree.attempt.storage")
}

fn authority_in_worktree(actor: &str, worktree: &str) -> WorkAuthority {
    authority_in_scope(
        "project.attempt.storage",
        "repository.attempt.storage",
        actor,
        worktree,
    )
}

fn authority_in_scope(
    project: &str,
    repository: &str,
    actor: &str,
    worktree: &str,
) -> WorkAuthority {
    WorkAuthority::new(
        id::<ProjectId>(project),
        id::<RepositoryId>(repository),
        id::<WorktreeId>(worktree),
        id::<ActorId>(actor),
        digest('a'),
    )
    .unwrap()
}

fn authority_in_scope_with_policy(
    project: &str,
    repository: &str,
    actor: &str,
    worktree: &str,
    policy: char,
) -> WorkAuthority {
    WorkAuthority::new(
        id::<ProjectId>(project),
        id::<RepositoryId>(repository),
        id::<WorktreeId>(worktree),
        id::<ActorId>(actor),
        digest(policy),
    )
    .unwrap()
}

#[test]
fn exact_scope_holder_census_sees_old_policy_and_other_actor_attempts() {
    let store = RegisteredWorkStore::start("attempt-cleanup-holder-scope");
    let old_policy = authority_in_scope_with_policy(
        "project.attempt.cleanup",
        "repository.attempt.cleanup",
        "actor.attempt.current",
        "worktree.attempt.old-policy",
        '9',
    );
    let other_actor = authority_in_scope_with_policy(
        "project.attempt.cleanup",
        "repository.attempt.cleanup",
        "actor.attempt.delegated",
        "worktree.attempt.other-actor",
        'a',
    );
    store
        .storage()
        .insert(
            &old_policy,
            &attempt_at(
                "task.attempt.old-policy",
                "run.attempt.old-policy",
                "attempt.old-policy",
            ),
        )
        .unwrap();
    store
        .storage()
        .insert(
            &other_actor,
            &attempt_at(
                "task.attempt.other-actor",
                "run.attempt.other-actor",
                "attempt.other-actor",
            ),
        )
        .unwrap();

    for authority in [&old_policy, &other_actor] {
        assert!(
            store
                .storage()
                .has_open_attempts_in_exact_scope(
                    authority.project_id(),
                    authority.repository_id(),
                    authority.worktree_id(),
                )
                .unwrap(),
            "cleanup must see holders outside its current actor/policy lineage"
        );
    }
    assert!(
        !store
            .storage()
            .has_open_attempts_in_exact_scope(
                old_policy.project_id(),
                old_policy.repository_id(),
                &id::<WorktreeId>("worktree.attempt.unrelated"),
            )
            .unwrap()
    );
}

fn concurrency(global: u16, repository: u16, task: u16) -> TopologyConcurrencyPolicyV1 {
    TopologyConcurrencyPolicyV1 {
        maximum_global_active: NonZeroU16::new(global).unwrap(),
        maximum_active_per_repository: NonZeroU16::new(repository).unwrap(),
        maximum_parallel_per_task: NonZeroU16::new(task).unwrap(),
        maximum_stack_depth: NonZeroU16::new(1).unwrap(),
    }
}

#[test]
fn bounded_insert_and_read_only_verdict_share_project_global_capacity() {
    let store = RegisteredWorkStore::start("attempt-project-global-capacity");
    let first_authority = authority_in_scope(
        "project.attempt.global",
        "repository.attempt.global.first",
        "actor.attempt.global.first",
        "worktree.attempt.global.first",
    );
    let peer_authority = authority_in_scope(
        "project.attempt.global",
        "repository.attempt.global.peer",
        "actor.attempt.global.peer",
        "worktree.attempt.global.peer",
    );
    let other_project = authority_in_scope(
        "project.attempt.global.other",
        "repository.attempt.global.other",
        "actor.attempt.global.other",
        "worktree.attempt.global.other",
    );
    let policy = concurrency(1, 1, 1);
    let first = attempt_at(
        "task.attempt.global.first",
        "run.attempt.global.first",
        "attempt.global.first",
    );
    let peer = attempt_at(
        "task.attempt.global.peer",
        "run.attempt.global.peer",
        "attempt.global.peer",
    );

    assert_eq!(
        store
            .storage()
            .insert_bounded(&first_authority, &first, &policy)
            .unwrap(),
        WorkAttemptInsertOutcome::Inserted
    );
    let capacities = store
        .storage()
        .admission_capacities(
            &peer_authority,
            std::slice::from_ref(peer.identity().task_id()),
            &policy,
        )
        .unwrap();
    let capacity = &capacities[peer.identity().task_id()];
    assert_eq!(capacity.global_active(), 1);
    assert_eq!(capacity.repository_active(), 0);
    assert_eq!(capacity.task_active(), 0);
    assert_eq!(
        capacity.verdict(),
        WorkAttemptCapacityVerdictV1::Exhausted(BTreeSet::from([
            WorkAttemptCapacityScopeV1::Global,
        ]))
    );
    assert_eq!(
        store
            .storage()
            .insert_bounded(&peer_authority, &peer, &policy)
            .unwrap_err(),
        WorkAttemptStorageError::CapacityExceeded
    );
    assert_eq!(store.count("work_attempts_v1"), 1);

    let other_capacities = store
        .storage()
        .admission_capacities(
            &other_project,
            std::slice::from_ref(peer.identity().task_id()),
            &policy,
        )
        .unwrap();
    let other_capacity = &other_capacities[peer.identity().task_id()];
    assert_eq!(
        other_capacity.verdict(),
        WorkAttemptCapacityVerdictV1::Available
    );
}

#[test]
fn bounded_insert_counts_open_attempts_across_repository_worktrees() {
    let store = RegisteredWorkStore::start("attempt-bounded-insert");
    let root = authority_in_worktree("actor.attempt.bounded", "worktree.attempt.root");
    let linked = authority_in_worktree("actor.attempt.bounded.peer", "worktree.attempt.linked");
    let policy = concurrency(2, 2, 1);
    let first = attempt_at(
        "task.attempt.bounded.shared",
        "run.attempt.bounded.1",
        "attempt.bounded.1",
    );
    assert_eq!(
        store
            .storage()
            .insert_bounded(&root, &first, &policy)
            .unwrap(),
        WorkAttemptInsertOutcome::Inserted
    );
    assert_eq!(
        store
            .storage()
            .insert_bounded(&root, &first, &policy)
            .unwrap(),
        WorkAttemptInsertOutcome::Replayed(Box::new(first.clone()))
    );

    let same_task = attempt_at(
        "task.attempt.bounded.shared",
        "run.attempt.bounded.2",
        "attempt.bounded.2",
    );
    assert_eq!(
        store
            .storage()
            .insert_bounded(&linked, &same_task, &policy)
            .unwrap_err(),
        WorkAttemptStorageError::CapacityExceeded
    );

    let second = attempt_at(
        "task.attempt.bounded.second",
        "run.attempt.bounded.3",
        "attempt.bounded.3",
    );
    assert_eq!(
        store
            .storage()
            .insert_bounded(&linked, &second, &policy)
            .unwrap(),
        WorkAttemptInsertOutcome::Inserted
    );
    let repository_full = attempt_at(
        "task.attempt.bounded.third",
        "run.attempt.bounded.4",
        "attempt.bounded.4",
    );
    assert_eq!(
        store
            .storage()
            .insert_bounded(&linked, &repository_full, &policy)
            .unwrap_err(),
        WorkAttemptStorageError::CapacityExceeded
    );
    assert_eq!(store.count("work_attempts_v1"), 2);
}

#[test]
fn concurrent_bounded_inserts_across_repositories_cannot_overbook_project_global_capacity() {
    let store = RegisteredWorkStore::start("attempt-concurrent-capacity");
    let policy = concurrency(1, 1, 1);
    let barrier = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();
    for ordinal in 1..=2 {
        let storage = store.storage().clone();
        let authority = authority_in_scope(
            "project.attempt.concurrent-capacity",
            &format!("repository.attempt.concurrent.{ordinal}"),
            &format!("actor.attempt.concurrent.{ordinal}"),
            &format!("worktree.attempt.concurrent.{ordinal}"),
        );
        let barrier = Arc::clone(&barrier);
        let policy = policy.clone();
        workers.push(thread::spawn(move || {
            let candidate = attempt_at(
                &format!("task.attempt.concurrent.{ordinal}"),
                &format!("run.attempt.concurrent.{ordinal}"),
                &format!("attempt.concurrent.{ordinal}"),
            );
            barrier.wait();
            storage.insert_bounded(&authority, &candidate, &policy)
        }));
    }

    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Ok(WorkAttemptInsertOutcome::Inserted)))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(WorkAttemptStorageError::CapacityExceeded)))
            .count(),
        1
    );
    assert_eq!(store.count("work_attempts_v1"), 1);
}

#[test]
fn retry_reservation_cannot_overbook_project_global_capacity() {
    let store = RegisteredWorkStore::start("retry-project-global-capacity");
    let retry_authority = authority_in_scope(
        "project.retry.global",
        "repository.retry.global.original",
        "actor.retry.global.original",
        "worktree.retry.global.original",
    );
    let peer_authority = authority_in_scope(
        "project.retry.global",
        "repository.retry.global.peer",
        "actor.retry.global.peer",
        "worktree.retry.global.peer",
    );
    let original = failed(&attempt_at(
        "task.retry.global.original",
        "run.retry.global.original",
        "attempt.retry.global.original",
    ));
    store.storage().insert(&retry_authority, &original).unwrap();
    let occupied = attempt_at(
        "task.retry.global.occupied",
        "run.retry.global.occupied",
        "attempt.retry.global.occupied",
    );
    store.storage().insert(&peer_authority, &occupied).unwrap();

    let write = retry_write(&original);
    assert_eq!(
        store
            .storage()
            .insert_retry_bounded(&retry_authority, &write, &concurrency(1, 1, 1))
            .unwrap_err(),
        WorkAttemptStorageError::CapacityExceeded
    );
    assert_eq!(store.count("work_attempts_v1"), 2);
    assert_eq!(store.count("work_retry_receipts_v1"), 0);
}

#[test]
fn successful_retry_remains_pending_until_exact_durable_marker_cas() {
    let store = RegisteredWorkStore::start("retry-observation-marker");
    let authority = authority_in_scope(
        "project.retry.marker",
        "repository.retry.marker",
        "actor.retry.marker",
        "worktree.retry.marker",
    );
    let original = failed(&attempt_at(
        "task.retry.marker",
        "run.retry.marker",
        "attempt.retry.marker.original",
    ));
    store.storage().insert(&authority, &original).unwrap();
    let write = retry_write(&original);
    store
        .storage()
        .insert_retry_bounded(&authority, &write, &concurrency(1, 1, 1))
        .unwrap();

    let pending = store
        .storage()
        .pending_owner_observations(None, NonZeroU16::new(8).unwrap())
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].validate());
    assert_eq!(
        store
            .storage()
            .mark_owner_observation_durable(&pending[0].marker)
            .unwrap(),
        WorkOwnerObservationMarkOutcomeV1::Marked
    );
    assert_eq!(
        store
            .storage()
            .mark_owner_observation_durable(&pending[0].marker)
            .unwrap(),
        WorkOwnerObservationMarkOutcomeV1::Replayed
    );
    assert!(
        store
            .storage()
            .pending_owner_observations(None, NonZeroU16::new(8).unwrap())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn batch_capacity_read_is_coherent_while_an_admission_commits() {
    let store = RegisteredWorkStore::start("attempt-capacity-snapshot");
    let policy = concurrency(2, 2, 2);
    for ordinal in 1..=16 {
        let authority = authority_in_scope(
            &format!("project.attempt.capacity-snapshot.{ordinal}"),
            "repository.attempt.capacity-snapshot",
            "actor.attempt.capacity-snapshot",
            "worktree.attempt.capacity-snapshot",
        );
        let candidate = attempt_at(
            &format!("task.attempt.capacity-snapshot.a.{ordinal}"),
            &format!("run.attempt.capacity-snapshot.{ordinal}"),
            &format!("attempt.capacity-snapshot.{ordinal}"),
        );
        let peer_task = id::<TaskId>(&format!("task.attempt.capacity-snapshot.b.{ordinal}"));
        let task_ids = [candidate.identity().task_id().clone(), peer_task.clone()];
        let barrier = Arc::new(Barrier::new(2));
        let writer = {
            let storage = store.storage().clone();
            let authority = authority.clone();
            let policy = policy.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                storage.insert_bounded(&authority, &candidate, &policy)
            })
        };

        barrier.wait();
        let capacities = store
            .storage()
            .admission_capacities(&authority, &task_ids, &policy)
            .unwrap();
        writer.join().unwrap().unwrap();
        let candidate_capacity = &capacities[&task_ids[0]];
        let peer_capacity = &capacities[&peer_task];
        assert_eq!(
            candidate_capacity.global_active(),
            candidate_capacity.repository_active()
        );
        assert_eq!(
            candidate_capacity.global_active(),
            candidate_capacity.task_active() + peer_capacity.task_active()
        );
        assert_eq!(
            candidate_capacity.global_active(),
            peer_capacity.global_active()
        );
        assert_eq!(
            candidate_capacity.repository_active(),
            peer_capacity.repository_active()
        );
    }
}

#[test]
fn paused_run_fences_new_attempt_inside_the_insert_transaction() {
    let store = RegisteredWorkStore::start("attempt-paused-reservation");
    let authority = authority("actor.attempt.paused-reservation");
    let first = attempt_at(
        "task.attempt.paused-reservation",
        "run.attempt.paused-reservation",
        "attempt.paused-reservation.1",
    );
    store.storage().insert(&authority, &first).unwrap();
    let paused = WorkRunControlV1::admitted(
        first.identity().task_id().clone(),
        first.identity().run_id().clone(),
        first.execution().deadline(),
        UtcMicros(10),
    )
    .unwrap()
    .pause(
        WorkRunControlReasonV1::OperatorRequest,
        UtcMicros(20),
        vec![first.identity().attempt_id().clone()],
    )
    .unwrap();
    store
        .storage()
        .publish_run_control(&authority, None, &paused, &[])
        .unwrap();

    let next = attempt_at(
        "task.attempt.paused-reservation",
        "run.attempt.paused-reservation",
        "attempt.paused-reservation.2",
    );
    let policy = concurrency(3, 3, 3);
    assert_eq!(
        store
            .storage()
            .insert_bounded(&authority, &next, &policy)
            .unwrap_err(),
        WorkAttemptStorageError::ReservationFenced
    );
    assert_eq!(store.count("work_attempts_v1"), 1);
}

#[test]
fn effect_holder_is_exact_replayable_and_reconciles_unknown_across_restart() {
    let store = RegisteredWorkStore::start("attempt-effect-holder");
    let exact_authority = authority("actor.attempt.effect-holder");
    let attempt = attempt_with_effect(
        identity("attempt.effect-holder"),
        1,
        WorkEffectStateV1::Intercepted,
    );
    store.storage().insert(&exact_authority, &attempt).unwrap();
    let first = WorkAttemptEffectHolderV1::dispatched(
        attempt.identity().clone(),
        WorkEffectStateV1::Intercepted,
        UtcMicros(10),
        UtcMicros(100),
    )
    .unwrap();
    assert_eq!(
        store
            .storage()
            .begin_effect_dispatch(&exact_authority, &first)
            .unwrap(),
        WorkAttemptEffectDispatchOutcomeV1::Recorded(first.clone())
    );
    let replay = WorkAttemptEffectHolderV1::dispatched(
        attempt.identity().clone(),
        WorkEffectStateV1::Intercepted,
        UtcMicros(10),
        UtcMicros(100),
    )
    .unwrap();
    assert_eq!(
        store
            .storage()
            .begin_effect_dispatch(&exact_authority, &replay)
            .unwrap(),
        WorkAttemptEffectDispatchOutcomeV1::Replayed(first),
        "an exact receipt replay never authorizes a second provider dispatch"
    );
    let conflicting_deadline = WorkAttemptEffectHolderV1::dispatched(
        attempt.identity().clone(),
        WorkEffectStateV1::Intercepted,
        UtcMicros(12),
        UtcMicros(101),
    )
    .unwrap();
    assert_eq!(
        store
            .storage()
            .begin_effect_dispatch(&exact_authority, &conflicting_deadline)
            .unwrap_err(),
        WorkAttemptEffectStorageErrorV1::Conflict
    );
    let unknown = store
        .storage()
        .settle_effect_dispatch(
            &exact_authority,
            attempt.identity(),
            WorkAttemptEffectResolutionV1::Unknown,
            UtcMicros(50),
        )
        .unwrap();
    assert_eq!(
        unknown.resolution(),
        Some(WorkAttemptEffectResolutionV1::Unknown)
    );

    let restarted = store.restart("attempt-effect-holder");
    let no_effect = restarted
        .storage()
        .settle_effect_dispatch(
            &exact_authority,
            attempt.identity(),
            WorkAttemptEffectResolutionV1::NoEffect,
            UtcMicros(60),
        )
        .unwrap();
    assert_eq!(
        no_effect.resolution(),
        Some(WorkAttemptEffectResolutionV1::NoEffect)
    );
    assert_eq!(
        restarted
            .storage()
            .load_effect_dispatch(
                &authority("actor.attempt.effect-holder.other"),
                attempt.identity(),
            )
            .unwrap(),
        None,
        "a foreign exact Work authority cannot read the holder"
    );
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
        topology: tracedecay_domain::safe_work_topology_policy_v1(),
    })
    .unwrap()
}

fn attempt(attempt_id: &str, epoch: u64) -> WorkAttemptV1 {
    attempt_with_identity(identity(attempt_id), epoch)
}

fn attempt_at(task: &str, run: &str, attempt_id: &str) -> WorkAttemptV1 {
    attempt_with_identity(
        WorkAttemptIdentityV1::new(
            id::<TaskId>(task),
            id::<RunId>(run),
            id::<AttemptId>(attempt_id),
        )
        .unwrap(),
        1,
    )
}

fn attempt_with_identity(identity: WorkAttemptIdentityV1, epoch: u64) -> WorkAttemptV1 {
    attempt_with_effect(identity, epoch, WorkEffectStateV1::Observational)
}

fn attempt_with_effect(
    identity: WorkAttemptIdentityV1,
    epoch: u64,
    effect_state: WorkEffectStateV1,
) -> WorkAttemptV1 {
    let binding = WorkAttemptProjectionBindingV1::new(
        WorkGraphVersionV1::new(3).unwrap(),
        WorkProductEventSequenceV1::new(7).unwrap(),
        WorkProductSourceWatermarkV1::new(Default::default()).unwrap(),
        digest('f'),
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
        fixture_abs_root("/tmp/attempt-storage"),
        Some(id::<RefId>("refs/heads/attempt-storage")),
        id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        "Execute the admitted provider step.".to_owned(),
        1,
        effect_state,
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
        provider_session: None,
        provider_fallback: None,
        observed_at: UtcMicros(500),
    }
}

fn evidence_with_session(attempt: &WorkAttemptV1, session_id: &str) -> WorkAttemptEvidenceRecordV1 {
    WorkAttemptEvidenceRecordV1 {
        provider_session: Some(
            ObservationSourceIdentityV1::for_provider(
                id::<ProviderId>("provider.work.claude-code-cli"),
                id::<SessionId>(session_id),
            )
            .unwrap(),
        ),
        ..evidence(attempt)
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

fn failed(attempt: &WorkAttemptV1) -> WorkAttemptV1 {
    let terminal = WorkTerminalEvidenceV1::failed(digest('8'), UtcMicros(500)).unwrap();
    running(attempt)
        .transition(
            WorkAttemptStateV1::Failed,
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

fn retry_write(original: &WorkAttemptV1) -> WorkRetryWriteV1 {
    let new_identity = WorkAttemptIdentityV1::new(
        original.identity().task_id().clone(),
        original.identity().run_id().clone(),
        id::<AttemptId>("attempt.retry.global.new"),
    )
    .unwrap();
    let binding = original.projection_binding().clone();
    let execution = original.execution();
    let envelope = WorkExecutionEnvelopeV1::new(
        new_identity.clone(),
        binding.clone(),
        execution.operation().clone(),
        execution.execution_snapshot().clone(),
        execution.project_id().clone(),
        execution.repository_id().clone(),
        execution.worktree_id().clone(),
        execution.worktree_root().to_owned(),
        execution.reference().cloned(),
        execution.commit().clone(),
        execution.instructions().to_owned(),
        execution.cancellation_generation() + 1,
        execution.effect_state(),
    )
    .unwrap();
    let retry_attempt = WorkAttemptV1::new(
        new_identity.clone(),
        binding,
        envelope,
        WorkLeaseFenceV1::new(
            id::<WorkLeaseId>("lease.retry.global.new"),
            WorkFenceEpochV1::new(2).unwrap(),
        )
        .unwrap(),
        WorkAttemptStateV1::RecoveryRequired,
        None,
        Vec::new(),
        WorkCancellationStateV1::None,
        WorkRecoveryStateV1::RecoveryRequired {
            source_attempt_id: Some(original.identity().attempt_id().clone()),
            reason: WorkRestartReasonV1::FailureObserved,
        },
        original.requested_route().clone(),
        None,
        None,
    )
    .unwrap();
    let terminal = original.terminal().expect("failed attempt terminal");
    let (evidence_digest, observed_at) = match terminal {
        WorkTerminalEvidenceV1::Failed {
            evidence_digest,
            observed_at,
        } => (evidence_digest.clone(), *observed_at),
        _ => panic!("fixture is failed"),
    };
    let failure = WorkRetryFailureSelectorV1 {
        source: WorkRetrySourceV1::Runtime,
        cause: WorkRetryCauseV1::RuntimeFailure,
        evidence_ref: format!("runtime-terminal:{}", evidence_digest.as_str()),
    };
    let command = tracedecay_application::RetryWorkAttemptCommandV1 {
        original_attempt: original.identity().clone(),
        new_attempt_id: new_identity.attempt_id().clone(),
        failure: failure.clone(),
        command_id: id("command.retry.global"),
    };
    let receipt = WorkRetryReceiptV1::new(
        command,
        VerifiedWorkRetryFailureV1 {
            selector: failure,
            evidence_digest,
            observed_at,
        },
        new_identity,
        observed_at,
        UtcMicros(700),
    )
    .unwrap();
    WorkRetryWriteV1 {
        receipt,
        attempt: retry_attempt,
    }
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
        WorkAttemptInsertOutcome::Replayed(Box::new(first.clone()))
    );
    assert_eq!(store.count("work_attempts_v1"), 1);
    assert_eq!(
        store
            .storage()
            .load_admission_kind(&authority, first.identity())
            .unwrap(),
        WorkAttemptAdmissionKind::Ordinary
    );
    assert_eq!(
        store
            .storage()
            .load_synthesis(&authority, first.identity())
            .unwrap_err(),
        WorkAttemptStorageError::AttemptConflict
    );
    // The same identity with different content is a conflict, not a refresh.
    let divergent = attempt("attempt.storage.1", 2);
    assert_eq!(
        store.storage().insert(&authority, &divergent).unwrap_err(),
        WorkAttemptStorageError::AttemptConflict
    );
    assert_eq!(store.count("work_attempts_v1"), 1);
}

#[test]
fn synthesis_admission_replays_one_durable_result_and_refuses_changed_requests() {
    let store = RegisteredWorkStore::start("attempt-synthesis-insert");
    let authority = authority("actor.attempt.synthesis");
    let admitted_attempt = attempt("attempt.storage.synthesis", 1);
    let source = identity("attempt.storage.source");
    let source_digest = digest('7');
    let source_set = WorkSynthesisSourceSetV1::seal(vec![WorkSynthesisSourceEnvelopeV1 {
        source: source.clone(),
        outcome: WorkSynthesisSourceOutcomeV1::Succeeded {
            artifacts: vec![source_digest.clone()],
        },
    }])
    .unwrap();
    let admission = WorkSynthesisAdmissionV1 {
        attempt: admitted_attempt.clone(),
        source_set,
        groups: vec![WorkSynthesisEvidenceGroupV1 {
            artifacts: vec![source_digest.clone()],
            sources: vec![source],
        }],
        draft: WorkflowSynthesisDraft {
            output_name: id::<WorkflowOutputName>("output.storage.synthesis"),
            synthesis_attempt: admitted_attempt.identity().clone(),
            cited_source_digests: BTreeSet::from([source_digest]),
        },
        uncited: Vec::new(),
    };
    let record = WorkSynthesisAdmissionRecordV1 {
        request_digest: digest('8'),
        result: admission.clone(),
    };
    let policy = concurrency(1, 1, 1);

    assert_eq!(
        store
            .storage()
            .insert_synthesis_bounded(&authority, &record, &policy)
            .unwrap(),
        WorkSynthesisInsertOutcome::Inserted
    );
    assert_eq!(
        store
            .storage()
            .insert_synthesis_bounded(&authority, &record, &policy)
            .unwrap(),
        WorkSynthesisInsertOutcome::Replayed(Box::new(admission.clone()))
    );
    assert_eq!(store.count("work_attempts_v1"), 1);

    let changed = WorkSynthesisAdmissionRecordV1 {
        request_digest: digest('9'),
        result: admission,
    };
    assert_eq!(
        store
            .storage()
            .insert_synthesis_bounded(&authority, &changed, &policy)
            .unwrap_err(),
        WorkAttemptStorageError::AttemptConflict
    );
    assert_eq!(store.count("work_attempts_v1"), 1);
    assert_eq!(
        store
            .storage()
            .load_admission_kind(&authority, admitted_attempt.identity())
            .unwrap(),
        WorkAttemptAdmissionKind::Synthesis
    );

    let running_attempt = running(&admitted_attempt);
    store
        .storage()
        .update(
            &authority,
            admitted_attempt.lease(),
            WorkAttemptStateV1::Leased,
            &running_attempt,
            None,
        )
        .unwrap();
    assert_eq!(
        store
            .storage()
            .load_synthesis(&authority, admitted_attempt.identity())
            .unwrap(),
        record
    );

    let store = store.restart("attempt-synthesis-insert");
    assert_eq!(
        store
            .storage()
            .load_synthesis(&authority, admitted_attempt.identity())
            .unwrap(),
        record
    );
    assert_eq!(
        store
            .storage()
            .load(&authority, admitted_attempt.identity())
            .unwrap(),
        running_attempt
    );
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
            Some(&evidence_with_session(
                &closing_running,
                "session.provider.reported",
            )),
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

#[test]
fn list_pages_rows_in_identity_order_with_exact_remaining_counts() {
    let store = RegisteredWorkStore::start("attempt-list");
    let authority = authority("actor.attempt.list");
    // Inserted deliberately out of identity order; "attempt.10" sorts before
    // "attempt.9" under the byte order both SQLite BINARY collation and the
    // domain identity Ord use.
    let rows = [
        attempt_at("task.b", "run.1", "attempt.2"),
        attempt_at("task.a", "run.2", "attempt.1"),
        attempt_at("task.a", "run.1", "attempt.9"),
        attempt_at("task.a", "run.1", "attempt.10"),
    ];
    for row in &rows {
        store.storage().insert(&authority, row).unwrap();
    }
    // A terminal attempt stays listed: the list is the durable evidence
    // surface, not the open set.
    let closing = &rows[2];
    let closing_running = running(closing);
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

    let expected_order = [
        "task.a/run.1/attempt.10",
        "task.a/run.1/attempt.9",
        "task.a/run.2/attempt.1",
        "task.b/run.1/attempt.2",
    ];
    let first = store.storage().list(&authority, None, 3).unwrap();
    assert_eq!(first.remaining, 4);
    assert_eq!(first.attempts.len(), 3);
    let listed = first
        .attempts
        .iter()
        .map(|attempt| {
            format!(
                "{}/{}/{}",
                attempt.identity().task_id().as_str(),
                attempt.identity().run_id().as_str(),
                attempt.identity().attempt_id().as_str()
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(listed, &expected_order[..3]);
    assert_eq!(first.attempts[1].state(), WorkAttemptStateV1::Succeeded);

    // The next page starts strictly after the cursor identity.
    let cursor = first.attempts.last().unwrap().identity().clone();
    let second = store.storage().list(&authority, Some(&cursor), 3).unwrap();
    assert_eq!(second.remaining, 1);
    assert_eq!(second.attempts.len(), 1);
    assert_eq!(second.attempts[0].identity().task_id().as_str(), "task.b");

    // A limit past the end returns the complete remainder, nothing invented.
    let all = store.storage().list(&authority, None, 100).unwrap();
    assert_eq!(all.remaining, 4);
    assert_eq!(all.attempts.len(), 4);
}

#[test]
fn evidence_pages_carry_artifacts_and_typed_evidence_in_identity_order() {
    let store = RegisteredWorkStore::start("attempt-evidence-page");
    let mine = authority("actor.attempt.evidence");
    let rows = [
        attempt_at("task.a", "run.1", "attempt.1"),
        attempt_at("task.b", "run.1", "attempt.1"),
        attempt_at("task.c", "run.1", "attempt.1"),
    ];
    for row in &rows {
        store.storage().insert(&mine, row).unwrap();
    }
    // Settle the first attempt with artifacts and sealed evidence; the other
    // two stay leased with neither.
    let closing = &rows[0];
    let closing_running = running(closing);
    store
        .storage()
        .update(
            &mine,
            closing.lease(),
            WorkAttemptStateV1::Leased,
            &closing_running,
            None,
        )
        .unwrap();
    let artifacts = vec![
        WorkArtifactRefV1::new(id("artifact.storage.log"), digest('7'), 128).unwrap(),
        WorkArtifactRefV1::new(id("artifact.storage.patch"), digest('8'), 4_096).unwrap(),
    ];
    let sealed_evidence = evidence_with_session(&closing_running, "session.provider.reported");
    let terminal =
        WorkTerminalEvidenceV1::succeeded(sealed_evidence.digest().unwrap(), UtcMicros(500))
            .unwrap();
    let closed = closing_running
        .transition(
            WorkAttemptStateV1::Succeeded,
            None,
            artifacts.clone(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(requested_route()),
            Some(terminal),
            closing_running.lease().clone(),
        )
        .unwrap();
    store
        .storage()
        .update(
            &mine,
            closing_running.lease(),
            WorkAttemptStateV1::Running,
            &closed,
            Some(&sealed_evidence),
        )
        .unwrap();

    let first = store.storage().evidence_page(&mine, None, 2).unwrap();
    assert_eq!(first.remaining, 3);
    assert_eq!(first.rows.len(), 2);
    assert_eq!(first.rows[0].identity, *closed.identity());
    assert_eq!(first.rows[0].artifacts, artifacts);
    let sealed = first.rows[0]
        .evidence
        .as_ref()
        .expect("the settled attempt must carry its sealed evidence record");
    assert_eq!(sealed.identity, *closed.identity());
    assert_eq!(
        sealed
            .provider_session
            .as_ref()
            .map(ObservationSourceIdentityV1::session_id)
            .map(SessionId::as_str),
        Some("session.provider.reported")
    );
    assert_eq!(
        sealed.outcome,
        WorkAttemptProviderOutcomeV1::Exited { code: 0 }
    );
    assert!(first.rows[1].artifacts.is_empty());
    assert!(
        first.rows[1].evidence.is_none(),
        "an unsettled attempt has no evidence record, not a fabricated one"
    );

    let exact = store
        .storage()
        .attempt_receipt(&mine, closed.identity())
        .expect("exact rooted receipt lookup");
    assert_eq!(exact.identity, *closed.identity());
    assert_eq!(exact.artifacts, artifacts);
    assert_eq!(
        exact
            .evidence
            .as_ref()
            .and_then(|record| record.provider_session.as_ref())
            .map(ObservationSourceIdentityV1::session_id)
            .map(SessionId::as_str),
        Some("session.provider.reported"),
        "provider session identity must commit atomically with terminal evidence"
    );

    // The next page starts strictly after the cursor identity and stays
    // consistent with the remaining count.
    let cursor = first.rows.last().unwrap().identity.clone();
    let second = store
        .storage()
        .evidence_page(&mine, Some(&cursor), 2)
        .unwrap();
    assert_eq!(second.remaining, 1);
    assert_eq!(second.rows.len(), 1);
    assert_eq!(
        second.rows[0].identity.task_id().as_str(),
        "task.c",
        "the evidence page order is the attempt list order"
    );

    // Foreign authorities observe nothing, not an empty-but-real page.
    let stranger = authority("actor.attempt.evidence.stranger");
    let foreign = store.storage().evidence_page(&stranger, None, 10).unwrap();
    assert_eq!(foreign.remaining, 0);
    assert!(foreign.rows.is_empty());
}

#[test]
fn list_is_scoped_to_the_exact_authority() {
    let store = RegisteredWorkStore::start("attempt-list-isolation");
    let owner = authority("actor.attempt.list.owner");
    let stranger = authority("actor.attempt.list.stranger");
    store
        .storage()
        .insert(&owner, &attempt("attempt.storage.1", 1))
        .unwrap();
    let foreign = store.storage().list(&stranger, None, 10).unwrap();
    assert_eq!(foreign.remaining, 0);
    assert!(foreign.attempts.is_empty());
    let owned = store.storage().list(&owner, None, 10).unwrap();
    assert_eq!(owned.remaining, 1);
    assert_eq!(owned.attempts.len(), 1);
}
