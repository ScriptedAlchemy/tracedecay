//! Durable workflow authority over the registered Work exact-SQL channel.

use tracedecay_application::{
    TaskHandoffAuthorityError, TaskHandoffAuthorityPort, TaskHandoffConsumeOutcome,
    TaskHandoffGrantV1, TaskHandoffScopeV1, WorkflowChildRecordV1,
    WorkflowDefinitionAuthorityError, WorkflowDefinitionAuthorityPort,
    WorkflowExecutionAdmissionV1, WorkflowExecutionAuthorityError, WorkflowExecutionAuthorityPort,
    WorkflowExecutionFenceV1, WorkflowExecutionIdentityV1, WorkflowExecutionTruthV1,
    WorkflowFanOutCheckpointV1,
};
use tracedecay_domain::{
    ActorId, AttemptId, ManifestDigest, ProjectId, RepositoryId, RunId, TaskId, ThreadId,
    UtcMicros, WorkAttemptIdentityV1, WorkFenceEpochV1, WorkLeaseFenceV1, WorkLeaseId,
    WorkflowDefinitionId, WorkflowDefinitionV1, WorkflowOperationRef, WorkflowOutputName,
    WorkflowStepId, WorkflowStepV1, WorktreeId, canonical_sha256,
};
use tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority;

mod work_registered_store;

use work_registered_store::RegisteredWorkStore;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

/// A distinct, valid `sha256:`-tagged digest per input byte.
///
/// Callers pick arbitrary ASCII letters as mnemonics, but a `ManifestDigest`
/// only accepts lowercase hex (`0-9a-f`); encoding the byte's own value as
/// two hex digits keeps every mnemonic both valid and mutually distinct.
fn digest(byte: char) -> ManifestDigest {
    let hex_byte = format!("{:02x}", u32::from(byte) & 0xff);
    ManifestDigest::new(format!("sha256:{}", hex_byte.repeat(32))).unwrap()
}

fn definition(version: u64, operation: &str) -> WorkflowDefinitionV1 {
    WorkflowDefinitionV1::new(
        id("workflow.definition.runtime-store"),
        version,
        id::<ProjectId>("project.workflow.runtime-store"),
        vec![WorkflowStepV1 {
            step_id: id::<WorkflowStepId>("prepare"),
            operation: id::<WorkflowOperationRef>(operation),
            predecessors: Default::default(),
            inputs: Vec::new(),
            outputs: vec![id::<WorkflowOutputName>("context")],
            fan_out: None,
        }],
        digest('a'),
        digest('b'),
        digest('c'),
    )
    .unwrap()
}

fn handoff_scope() -> TaskHandoffScopeV1 {
    TaskHandoffScopeV1::new(
        id::<ProjectId>("project.workflow.runtime-store"),
        id::<RepositoryId>("repository.workflow.runtime-store"),
        id::<WorktreeId>("worktree.workflow.runtime-store"),
        id::<WorkflowDefinitionId>("workflow.definition.runtime-store"),
        1,
        id::<WorkflowStepId>("prepare"),
        id::<TaskId>("task.workflow.runtime-store.prepare"),
        id::<ThreadId>("thread.workflow.runtime-store"),
        id::<RunId>("run.workflow.runtime-store"),
        id::<ActorId>("actor.workflow.source"),
        id::<ActorId>("actor.workflow.target"),
    )
    .unwrap()
}

fn token_digest(secret: &str) -> ManifestDigest {
    canonical_sha256(&("tracedecay.application.task-handoff.v1", secret)).unwrap()
}

fn execution_identity() -> WorkflowExecutionIdentityV1 {
    WorkflowExecutionIdentityV1 {
        definition_id: id("workflow.definition.runtime-store"),
        definition_version: 1,
        run_id: id::<RunId>("run.workflow.runtime-store"),
        step_id: id::<WorkflowStepId>("prepare"),
    }
}

fn fence(epoch: u64, attempt: &str) -> WorkflowExecutionFenceV1 {
    WorkflowExecutionFenceV1 {
        attempt_id: id::<AttemptId>(attempt),
        lease: WorkLeaseFenceV1::new(
            id::<WorkLeaseId>("lease.workflow.runtime-store"),
            WorkFenceEpochV1::new(epoch).unwrap(),
        )
        .unwrap(),
    }
}

fn plan_digest(byte: char) -> ManifestDigest {
    digest(byte)
}

fn checkpoint(plan: ManifestDigest) -> WorkflowFanOutCheckpointV1 {
    WorkflowFanOutCheckpointV1 {
        plan_digest: plan,
        children: vec![WorkflowChildRecordV1 {
            task_id: id::<TaskId>("task.workflow.runtime-store.child"),
            attempt_identity: WorkAttemptIdentityV1::new(
                id::<TaskId>("task.workflow.runtime-store.child"),
                id::<RunId>("run.workflow.runtime-store"),
                id::<AttemptId>("attempt.workflow.runtime-store.child"),
            )
            .unwrap(),
        }],
    }
}

fn terminal_truth() -> WorkflowExecutionTruthV1 {
    WorkflowExecutionTruthV1::Completed {
        checkpoint: checkpoint(plan_digest('5')),
    }
}

#[test]
fn definitions_activate_and_reject_conflicting_payloads() {
    let store = RegisteredWorkStore::start("workflow-definitions");
    let authority = WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap();
    let first = definition(1, "operation.prepare.v1");
    let second = definition(2, "operation.prepare.v1");
    let conflicting = definition(1, "operation.prepare.v2");

    WorkflowDefinitionAuthorityPort::insert(&authority, &first).unwrap();
    assert_eq!(
        WorkflowDefinitionAuthorityPort::insert(&authority, &first).unwrap_err(),
        WorkflowDefinitionAuthorityError::AlreadyExists
    );
    assert_eq!(
        WorkflowDefinitionAuthorityPort::insert(&authority, &conflicting).unwrap_err(),
        WorkflowDefinitionAuthorityError::Conflict
    );
    WorkflowDefinitionAuthorityPort::insert(&authority, &second).unwrap();

    assert_eq!(
        WorkflowDefinitionAuthorityPort::load(&authority, first.definition_id(), 1)
            .unwrap()
            .as_ref(),
        Some(&first)
    );
    assert_eq!(
        WorkflowDefinitionAuthorityPort::active_version(&authority, first.definition_id()).unwrap(),
        None
    );

    WorkflowDefinitionAuthorityPort::compare_and_swap_activation(
        &authority,
        first.definition_id(),
        None,
        1,
    )
    .unwrap();
    assert_eq!(
        WorkflowDefinitionAuthorityPort::active_version(&authority, first.definition_id()).unwrap(),
        Some(1)
    );
    assert_eq!(
        WorkflowDefinitionAuthorityPort::compare_and_swap_activation(
            &authority,
            first.definition_id(),
            None,
            2,
        )
        .unwrap_err(),
        WorkflowDefinitionAuthorityError::Conflict
    );
    WorkflowDefinitionAuthorityPort::compare_and_swap_activation(
        &authority,
        first.definition_id(),
        Some(1),
        2,
    )
    .unwrap();
    assert_eq!(
        WorkflowDefinitionAuthorityPort::active_version(&authority, first.definition_id()).unwrap(),
        Some(2)
    );

    assert_eq!(store.count("workflow_definitions_v1"), 2);
    assert_eq!(store.count("workflow_activations_v1"), 1);
}

#[test]
fn handoff_persists_digest_only_and_classifies_consume_outcomes() {
    let store = RegisteredWorkStore::start("workflow-handoff");
    let authority = WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap();
    let scope = handoff_scope();
    let secret = "s".repeat(48);
    let grant = TaskHandoffGrantV1::new(
        scope.clone(),
        token_digest(&secret),
        UtcMicros(10),
        UtcMicros(20),
    )
    .unwrap();

    TaskHandoffAuthorityPort::issue(&authority, &grant).unwrap();
    assert_eq!(
        TaskHandoffAuthorityPort::issue(&authority, &grant).unwrap_err(),
        TaskHandoffAuthorityError::Conflict
    );

    store.inspect(|connection| {
        let payload: String = connection
            .query_row(
                "SELECT scope_payload FROM workflow_handoffs_v1 WHERE token_digest = ?1",
                [grant.token_digest().as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!payload.contains(&secret));
        let persisted: TaskHandoffScopeV1 = serde_json::from_str(&payload).unwrap();
        assert_eq!(persisted, scope);
        assert_eq!(
            persisted.thread_id().as_str(),
            "thread.workflow.runtime-store"
        );
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM workflow_handoffs_v1 WHERE scope_payload LIKE ?1",
                [format!("%{secret}%")],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    });

    let wrong_scope = TaskHandoffScopeV1::new(
        scope.project_id().clone(),
        scope.repository_id().clone(),
        scope.worktree_id().clone(),
        scope.definition_id().clone(),
        scope.definition_version(),
        scope.step_id().clone(),
        id::<TaskId>("task.workflow.runtime-store.other"),
        scope.thread_id().clone(),
        scope.run_id().clone(),
        scope.from_actor_id().clone(),
        scope.to_actor_id().clone(),
    )
    .unwrap();
    assert_eq!(
        TaskHandoffAuthorityPort::consume(
            &authority,
            grant.token_digest(),
            &wrong_scope,
            UtcMicros(15),
        )
        .unwrap(),
        TaskHandoffConsumeOutcome::ScopeMismatch
    );
    assert_eq!(
        TaskHandoffAuthorityPort::consume(&authority, &digest('4'), &scope, UtcMicros(15),)
            .unwrap(),
        TaskHandoffConsumeOutcome::Missing
    );

    let expired = TaskHandoffGrantV1::new(
        scope.clone(),
        token_digest(&"e".repeat(48)),
        UtcMicros(10),
        UtcMicros(20),
    )
    .unwrap();
    TaskHandoffAuthorityPort::issue(&authority, &expired).unwrap();
    assert_eq!(
        TaskHandoffAuthorityPort::consume(
            &authority,
            expired.token_digest(),
            &scope,
            UtcMicros(20),
        )
        .unwrap(),
        TaskHandoffConsumeOutcome::Expired
    );

    assert_eq!(
        TaskHandoffAuthorityPort::consume(&authority, grant.token_digest(), &scope, UtcMicros(19),)
            .unwrap(),
        TaskHandoffConsumeOutcome::Consumed
    );
    assert_eq!(
        TaskHandoffAuthorityPort::consume(&authority, grant.token_digest(), &scope, UtcMicros(19),)
            .unwrap(),
        TaskHandoffConsumeOutcome::Replay
    );
}

#[test]
fn execution_checkpoints_recover_replay_and_survive_restart() {
    let store = RegisteredWorkStore::start("workflow-execution");
    let authority = WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap();
    let identity = execution_identity();
    let plan = plan_digest('5');
    let first_fence = fence(1, "attempt.workflow.runtime-store.1");
    let newer_fence = fence(2, "attempt.workflow.runtime-store.2");
    let stale_fence = fence(1, "attempt.workflow.runtime-store.stale");
    let other_lease = WorkflowExecutionFenceV1 {
        attempt_id: id::<AttemptId>("attempt.workflow.runtime-store.other-lease"),
        lease: WorkLeaseFenceV1::new(
            id::<WorkLeaseId>("lease.workflow.runtime-store.other"),
            WorkFenceEpochV1::new(3).unwrap(),
        )
        .unwrap(),
    };

    assert_eq!(
        WorkflowExecutionAuthorityPort::begin(&authority, &identity, &first_fence, &plan).unwrap(),
        WorkflowExecutionAdmissionV1::Execute
    );
    assert_eq!(
        WorkflowExecutionAuthorityPort::begin(&authority, &identity, &stale_fence, &plan).unwrap(),
        WorkflowExecutionAdmissionV1::StaleLease
    );
    assert_eq!(
        WorkflowExecutionAuthorityPort::begin(&authority, &identity, &other_lease, &plan).unwrap(),
        WorkflowExecutionAdmissionV1::StaleLease
    );
    assert_eq!(
        WorkflowExecutionAuthorityPort::begin(&authority, &identity, &newer_fence, &digest('6'))
            .unwrap(),
        WorkflowExecutionAdmissionV1::PlanConflict
    );

    let checkpoint = checkpoint(plan.clone());
    WorkflowExecutionAuthorityPort::checkpoint(&authority, &identity, &first_fence, &checkpoint)
        .unwrap();
    assert_eq!(
        WorkflowExecutionAuthorityPort::checkpoint(
            &authority,
            &identity,
            &newer_fence,
            &checkpoint,
        )
        .unwrap_err(),
        WorkflowExecutionAuthorityError::Conflict
    );

    assert_eq!(
        WorkflowExecutionAuthorityPort::begin(&authority, &identity, &newer_fence, &plan).unwrap(),
        WorkflowExecutionAdmissionV1::Recover {
            checkpoint: checkpoint.clone(),
        }
    );

    let truth = terminal_truth();
    WorkflowExecutionAuthorityPort::complete(&authority, &identity, &newer_fence, &truth).unwrap();
    assert_eq!(
        WorkflowExecutionAuthorityPort::begin(
            &authority,
            &identity,
            &fence(3, "attempt.workflow.runtime-store.3"),
            &plan,
        )
        .unwrap(),
        WorkflowExecutionAdmissionV1::Replay(truth.clone())
    );
    assert_eq!(
        WorkflowExecutionAuthorityPort::begin(
            &authority,
            &identity,
            &fence(3, "attempt.workflow.runtime-store.3"),
            &digest('7'),
        )
        .unwrap(),
        WorkflowExecutionAdmissionV1::PlanConflict
    );

    let store = store.restart("workflow-execution");
    let authority = WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap();
    assert_eq!(
        WorkflowDefinitionAuthorityPort::active_version(
            &authority,
            &id::<WorkflowDefinitionId>("workflow.definition.runtime-store"),
        )
        .unwrap(),
        None
    );
    assert_eq!(
        WorkflowExecutionAuthorityPort::begin(
            &authority,
            &identity,
            &fence(4, "attempt.workflow.runtime-store.4"),
            &plan,
        )
        .unwrap(),
        WorkflowExecutionAdmissionV1::Replay(truth)
    );
    assert_eq!(store.count("workflow_executions_v1"), 1);
}

#[test]
fn definition_and_handoff_survive_registered_store_restart() {
    let store = RegisteredWorkStore::start("workflow-restart");
    let authority = WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap();
    let first = definition(1, "operation.prepare.v1");
    WorkflowDefinitionAuthorityPort::insert(&authority, &first).unwrap();
    WorkflowDefinitionAuthorityPort::compare_and_swap_activation(
        &authority,
        first.definition_id(),
        None,
        1,
    )
    .unwrap();

    let scope = handoff_scope();
    let grant = TaskHandoffGrantV1::new(
        scope.clone(),
        token_digest(&"r".repeat(48)),
        UtcMicros(10),
        UtcMicros(50),
    )
    .unwrap();
    TaskHandoffAuthorityPort::issue(&authority, &grant).unwrap();
    assert_eq!(
        TaskHandoffAuthorityPort::consume(&authority, grant.token_digest(), &scope, UtcMicros(11),)
            .unwrap(),
        TaskHandoffConsumeOutcome::Consumed
    );

    let store = store.restart("workflow-restart");
    let authority = WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap();
    assert_eq!(
        WorkflowDefinitionAuthorityPort::load(&authority, first.definition_id(), 1)
            .unwrap()
            .as_ref(),
        Some(&first)
    );
    assert_eq!(
        WorkflowDefinitionAuthorityPort::active_version(&authority, first.definition_id()).unwrap(),
        Some(1)
    );
    assert_eq!(
        TaskHandoffAuthorityPort::consume(&authority, grant.token_digest(), &scope, UtcMicros(12),)
            .unwrap(),
        TaskHandoffConsumeOutcome::Replay
    );
}
