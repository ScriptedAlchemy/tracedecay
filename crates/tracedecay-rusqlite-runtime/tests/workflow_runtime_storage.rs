//! Durable workflow authority over the registered Work exact-SQL channel.

use std::sync::Arc;

use tracedecay_application::{
    TaskHandoffAuthorityError, TaskHandoffAuthorityPort, TaskHandoffConsumeOutcome,
    TaskHandoffGrantV1, TaskHandoffScopeV1, WorkflowDefinitionAuthorityError,
    WorkflowDefinitionAuthorityPort, WorkflowRunAppendOutcomeV1, WorkflowRunAppendRequestV1,
    WorkflowRunStoragePort,
};
use tracedecay_domain::configuration::safe_work_topology_policy_v1;
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, ProviderId, RepositoryId, RunId, TaskId, ThreadId,
    UtcMicros, WorkCommandId, WorkProviderBackendV1, WorkProviderRouteId, WorkProviderRouteV1,
    WorkflowDefinitionId, WorkflowDefinitionV1, WorkflowOperationRef, WorkflowOutputName,
    WorkflowPlacementReceiptV1, WorkflowRunCommandV1, WorkflowRunEventContextV1,
    WorkflowRunEventV1, WorkflowStepId, WorkflowStepV1, WorktreeId, canonical_sha256,
};
use tracedecay_rusqlite_runtime::migration_sql::{
    MigrationSqlError, MigrationSqlHandle, MigrationSqlWriteAuthority, MigrationSqlWriteIntent,
};
use tracedecay_rusqlite_runtime::workflow::{
    WorkflowSqliteAuthority, WorkflowSqliteAuthorityBuildError, migrate_workflow_schema_v2,
};

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

fn placement(run_id: RunId) -> WorkflowPlacementReceiptV1 {
    WorkflowPlacementReceiptV1::new(
        run_id,
        id::<WorkflowStepId>("prepare"),
        WorkProviderRouteV1::new(
            id::<ProviderId>("provider.workflow.runtime-store"),
            id::<WorkProviderRouteId>("route.workflow.runtime-store.v1"),
        )
        .unwrap(),
        WorkProviderBackendV1::CodexAppServer,
        "model.workflow.runtime-store".to_owned(),
        digest('b'),
        digest('d'),
        digest('8'),
        safe_work_topology_policy_v1().placement,
    )
    .unwrap()
}

struct AllowWorkflowSchemaMigration;

impl MigrationSqlWriteAuthority for AllowWorkflowSchemaMigration {
    fn verify(&self, _: MigrationSqlWriteIntent) -> Result<(), MigrationSqlError> {
        Ok(())
    }
}

fn schema_migration_handle(store: &RegisteredWorkStore) -> MigrationSqlHandle {
    store
        .migration_handle()
        .clone()
        .with_write_authority(Arc::new(AllowWorkflowSchemaMigration))
        .unwrap()
}

fn migrated_authority(store: &RegisteredWorkStore) -> WorkflowSqliteAuthority {
    migrate_workflow_schema_v2(&schema_migration_handle(store)).unwrap();
    WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap()
}

#[test]
fn runtime_requires_explicit_workflow_migration() {
    let store = RegisteredWorkStore::start("workflow-explicit-migration");

    assert!(matches!(
        WorkflowSqliteAuthority::from_work_storage(store.storage()),
        Err(WorkflowSqliteAuthorityBuildError::MigrationRequired)
    ));
    assert_eq!(
        store.inspect(|connection| {
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema
                     WHERE type = 'table' AND name LIKE 'workflow_%'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap()
        }),
        0,
        "runtime attachment must not create workflow schema"
    );

    assert!(migrate_workflow_schema_v2(store.migration_handle()).is_err());
    migrate_workflow_schema_v2(&schema_migration_handle(&store)).unwrap();
    assert!(WorkflowSqliteAuthority::from_work_storage(store.storage()).is_ok());
    assert_eq!(store.count("workflow_schema_v2"), 1);
    assert_eq!(store.count("workflow_executions_v1"), 0);
}

#[test]
fn run_journal_appends_replays_and_rebuilds_after_restart() {
    let store = RegisteredWorkStore::start("workflow-run-journal");
    let authority = migrated_authority(&store);
    let run_id = id::<RunId>("run.workflow.runtime-store.journal");
    let admitted = WorkflowRunEventV1::admitted(
        run_id.clone(),
        definition(1, "operation.prepare.v1"),
        digest('d'),
        digest('8'),
        WorkflowRunEventContextV1 {
            command_id: id::<WorkCommandId>("command.workflow.runtime-store.admit"),
            input_digest: digest('e'),
            occurred_at: UtcMicros(10),
        },
    )
    .unwrap();
    let request = WorkflowRunAppendRequestV1 {
        expected_sequence: None,
        event: admitted,
    };
    let projection = match WorkflowRunStoragePort::append(&authority, &request).unwrap() {
        WorkflowRunAppendOutcomeV1::Appended(projection) => projection,
        WorkflowRunAppendOutcomeV1::Replayed(_) => panic!("first append replayed"),
    };
    assert!(matches!(
        WorkflowRunStoragePort::append(&authority, &request).unwrap(),
        WorkflowRunAppendOutcomeV1::Replayed(_)
    ));

    let started = projection
        .next_event(
            WorkflowRunCommandV1::StartStep {
                step_id: id::<WorkflowStepId>("prepare"),
                placement: placement(run_id.clone()),
            },
            WorkflowRunEventContextV1 {
                command_id: id::<WorkCommandId>("command.workflow.runtime-store.start"),
                input_digest: digest('f'),
                occurred_at: UtcMicros(11),
            },
        )
        .unwrap();
    WorkflowRunStoragePort::append(
        &authority,
        &WorkflowRunAppendRequestV1 {
            expected_sequence: Some(1),
            event: started,
        },
    )
    .unwrap();

    let store = store.restart("workflow-run-journal");
    let authority = WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap();
    let history = WorkflowRunStoragePort::load(&authority, &run_id).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(
        WorkflowRunStoragePort::projection(&authority, &run_id)
            .unwrap()
            .sequence(),
        2
    );
    assert_eq!(store.count("workflow_run_events_v2"), 2);
    assert_eq!(store.count("workflow_run_heads_v2"), 1);
}

#[test]
fn execution_lease_renewal_rejects_replacement_outer_attempt() {
    let store = RegisteredWorkStore::start("workflow-outer-attempt-cas");
    let authority = WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap();
    let identity = execution_identity();
    let plan = plan_digest('5');
    let first_fence = fence(1, "attempt.workflow.runtime-store.1");
    let replacement_attempt = fence(2, "attempt.workflow.runtime-store.replacement");

    assert_eq!(
        WorkflowExecutionAuthorityPort::begin(&authority, &identity, &first_fence, &plan).unwrap(),
        WorkflowExecutionAdmissionV1::Execute
    );
    assert_eq!(
        WorkflowExecutionAuthorityPort::begin(&authority, &identity, &replacement_attempt, &plan,)
            .unwrap(),
        WorkflowExecutionAdmissionV1::StaleLease
    );
}

#[test]
fn execution_terminal_replay_rejects_replacement_outer_attempt() {
    let store = RegisteredWorkStore::start("workflow-terminal-replay-fence");
    let authority = WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap();
    let identity = execution_identity();
    let plan = plan_digest('5');
    let first_fence = fence(1, "attempt.workflow.runtime-store.1");
    let replacement_attempt = fence(2, "attempt.workflow.runtime-store.replacement");
    let checkpoint = checkpoint(plan.clone());
    let truth = WorkflowExecutionTruthV1::Completed {
        checkpoint: checkpoint.clone(),
    };

    assert_eq!(
        WorkflowExecutionAuthorityPort::begin(&authority, &identity, &first_fence, &plan).unwrap(),
        WorkflowExecutionAdmissionV1::Execute
    );
    WorkflowExecutionAuthorityPort::checkpoint(&authority, &identity, &first_fence, &checkpoint)
        .unwrap();
    WorkflowExecutionAuthorityPort::complete(&authority, &identity, &first_fence, &truth).unwrap();
    assert_eq!(
        WorkflowExecutionAuthorityPort::begin(&authority, &identity, &replacement_attempt, &plan,)
            .unwrap(),
        WorkflowExecutionAdmissionV1::StaleLease
    );
}

#[test]
fn execution_checkpoint_cas_rejects_child_regression() {
    let store = RegisteredWorkStore::start("workflow-checkpoint-cas");
    let authority = WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap();
    let identity = execution_identity();
    let plan = plan_digest('5');
    let first_fence = fence(1, "attempt.workflow.runtime-store.1");

    assert_eq!(
        WorkflowExecutionAuthorityPort::begin(&authority, &identity, &first_fence, &plan).unwrap(),
        WorkflowExecutionAdmissionV1::Execute
    );
    let mut checkpoint = checkpoint(plan.clone());
    checkpoint.children[0].receipt = None;
    WorkflowExecutionAuthorityPort::checkpoint(&authority, &identity, &first_fence, &checkpoint)
        .unwrap();
    let regressed = WorkflowFanOutCheckpointV1 {
        plan_digest: plan,
        children: Vec::new(),
    };
    assert_eq!(
        WorkflowExecutionAuthorityPort::checkpoint(
            &authority,
            &identity,
            &first_fence,
            &regressed,
        )
        .unwrap_err(),
        WorkflowExecutionAuthorityError::Conflict
    );
}

#[test]
fn execution_checkpoint_rejects_unjoined_child_identity_and_lease() {
    let store = RegisteredWorkStore::start("workflow-checkpoint-child-fence");
    let authority = WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap();
    let identity = execution_identity();
    let plan = plan_digest('5');
    let first_fence = fence(1, "attempt.workflow.runtime-store.1");

    assert_eq!(
        WorkflowExecutionAuthorityPort::begin(&authority, &identity, &first_fence, &plan).unwrap(),
        WorkflowExecutionAdmissionV1::Execute
    );

    let mut mismatched = checkpoint(plan.clone());
    mismatched.children[0].task_id = id::<TaskId>("task.workflow.runtime-store.child-mismatch");
    assert_eq!(
        WorkflowExecutionAuthorityPort::checkpoint(
            &authority,
            &identity,
            &first_fence,
            &mismatched,
        )
        .unwrap_err(),
        WorkflowExecutionAuthorityError::Conflict
    );

    let mut colliding = checkpoint(plan);
    let mut second = colliding.children[0].clone();
    second.task_id = id::<TaskId>("task.workflow.runtime-store.child-second");
    second.attempt_identity = WorkAttemptIdentityV1::new(
        second.task_id.clone(),
        id::<RunId>("run.workflow.runtime-store"),
        id::<AttemptId>("attempt.workflow.runtime-store.child-second"),
    )
    .unwrap();
    colliding.children.push(second);
    assert_eq!(
        WorkflowExecutionAuthorityPort::checkpoint(
            &authority,
            &identity,
            &first_fence,
            &colliding,
        )
        .unwrap_err(),
        WorkflowExecutionAuthorityError::Conflict
    );
}

#[test]
fn execution_completion_rejects_truth_for_another_plan() {
    let store = RegisteredWorkStore::start("workflow-completion-cas");
    let authority = WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap();
    let identity = execution_identity();
    let plan = plan_digest('5');
    let first_fence = fence(1, "attempt.workflow.runtime-store.1");

    assert_eq!(
        WorkflowExecutionAuthorityPort::begin(&authority, &identity, &first_fence, &plan).unwrap(),
        WorkflowExecutionAdmissionV1::Execute
    );
    let wrong_truth = WorkflowExecutionTruthV1::Completed {
        checkpoint: checkpoint(plan_digest('6')),
    };
    assert_eq!(
        WorkflowExecutionAuthorityPort::complete(
            &authority,
            &identity,
            &first_fence,
            &wrong_truth,
        )
        .unwrap_err(),
        WorkflowExecutionAuthorityError::Conflict
    );
}

#[test]
fn execution_completion_rejects_child_without_terminal_receipt() {
    let store = RegisteredWorkStore::start("workflow-completion-receipt");
    let authority = WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap();
    let identity = execution_identity();
    let plan = plan_digest('5');
    let first_fence = fence(1, "attempt.workflow.runtime-store.1");
    let mut checkpoint = checkpoint(plan.clone());
    checkpoint.children[0].receipt = None;

    assert_eq!(
        WorkflowExecutionAuthorityPort::begin(&authority, &identity, &first_fence, &plan).unwrap(),
        WorkflowExecutionAdmissionV1::Execute
    );
    WorkflowExecutionAuthorityPort::checkpoint(&authority, &identity, &first_fence, &checkpoint)
        .unwrap();
    let truth = WorkflowExecutionTruthV1::Completed { checkpoint };
    assert_eq!(
        WorkflowExecutionAuthorityPort::complete(&authority, &identity, &first_fence, &truth)
            .unwrap_err(),
        WorkflowExecutionAuthorityError::Conflict
    );
}

#[test]
fn definitions_activate_and_reject_conflicting_payloads() {
    let store = RegisteredWorkStore::start("workflow-definitions");
    let authority = migrated_authority(&store);
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
    let authority = migrated_authority(&store);
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
fn definition_and_handoff_survive_registered_store_restart() {
    let store = RegisteredWorkStore::start("workflow-restart");
    let authority = migrated_authority(&store);
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
