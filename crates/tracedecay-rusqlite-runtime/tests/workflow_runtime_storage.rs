//! Durable workflow authority over the registered Work writer.

use tracedecay_application::{
    TaskHandoffAuthorityError, TaskHandoffAuthorityPort, TaskHandoffConsumeOutcome,
    TaskHandoffGrant, TaskHandoffScope, WorkflowDefinitionAuthorityError,
    WorkflowDefinitionAuthorityPort,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RepositoryId, RunId, TaskId, ThreadId, UtcMicros,
    WorkflowDefinition, WorkflowDefinitionId, WorkflowOperationRef, WorkflowOutputName,
    WorkflowStep, WorkflowStepId, WorktreeId, canonical_sha256,
};
use tracedecay_rusqlite_runtime::workflow::{
    WorkflowSqliteAuthority, WorkflowSqliteAuthorityBuildError,
};

mod registered_workflow_store;

use registered_workflow_store::RegisteredWorkflowStore;

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

fn definition(version: u64, operation: &str) -> WorkflowDefinition {
    WorkflowDefinition::new(
        id("workflow.definition.runtime-store"),
        version,
        id::<ProjectId>("project.workflow.runtime-store"),
        vec![WorkflowStep {
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

fn handoff_scope() -> TaskHandoffScope {
    TaskHandoffScope::new(
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

fn authority(store: &RegisteredWorkflowStore) -> WorkflowSqliteAuthority {
    WorkflowSqliteAuthority::from_registered(store.storage().clone()).unwrap()
}

#[test]
fn non_final_store_requires_reset_without_runtime_schema_mutation() {
    let store =
        RegisteredWorkflowStore::start_with_setup("workflow-reset-required", |connection| {
            connection
                .execute_batch(
                    "DROP TABLE workflow_handoffs;
                 DROP TABLE workflow_activations;
                 DROP TABLE workflow_definitions;
                 DROP TABLE workflow_schema;",
                )
                .unwrap();
        });

    assert!(matches!(
        WorkflowSqliteAuthority::from_registered(store.storage().clone()),
        Err(WorkflowSqliteAuthorityBuildError::ResetRequired)
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
        "runtime attachment must not mutate a non-final store"
    );
}

#[test]
fn attachment_rejects_wrong_schema_version_digest_and_definition() {
    for (name, mutation) in [
        (
            "workflow-wrong-version",
            "PRAGMA ignore_check_constraints = ON;
             UPDATE workflow_schema SET schema_version = 2;",
        ),
        (
            "workflow-wrong-digest",
            "UPDATE workflow_schema SET definition_digest = 'sha256:wrong';",
        ),
        (
            "workflow-wrong-definition",
            "DROP TABLE workflow_handoffs;
             CREATE TABLE workflow_handoffs (
                 token_digest TEXT NOT NULL PRIMARY KEY,
                 scope_payload TEXT NOT NULL
             ) STRICT;",
        ),
    ] {
        let store = RegisteredWorkflowStore::start_with_setup(name, |connection| {
            connection.execute_batch(mutation).unwrap();
        });
        assert!(matches!(
            WorkflowSqliteAuthority::from_registered(store.storage().clone()),
            Err(WorkflowSqliteAuthorityBuildError::ResetRequired)
        ));
    }
}

#[test]
fn definitions_activate_and_reject_conflicting_payloads() {
    let store = RegisteredWorkflowStore::start("workflow-definitions");
    let authority = authority(&store);
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
        Some(1),
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
            Some(2),
        )
        .unwrap_err(),
        WorkflowDefinitionAuthorityError::Conflict
    );
    WorkflowDefinitionAuthorityPort::compare_and_swap_activation(
        &authority,
        first.definition_id(),
        Some(1),
        Some(2),
    )
    .unwrap();
    assert_eq!(
        WorkflowDefinitionAuthorityPort::active_version(&authority, first.definition_id()).unwrap(),
        Some(2)
    );
    assert_eq!(
        WorkflowDefinitionAuthorityPort::list(&authority, Some(first.definition_id()))
            .unwrap()
            .iter()
            .map(WorkflowDefinition::definition_version)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    WorkflowDefinitionAuthorityPort::compare_and_swap_activation(
        &authority,
        first.definition_id(),
        Some(2),
        None,
    )
    .unwrap();
    assert_eq!(
        WorkflowDefinitionAuthorityPort::active_version(&authority, first.definition_id()).unwrap(),
        None
    );
    assert_eq!(
        WorkflowDefinitionAuthorityPort::list(&authority, None).unwrap(),
        vec![first, second],
        "retirement must preserve immutable definition history"
    );

    assert_eq!(store.count("workflow_definitions"), 2);
    assert_eq!(store.count("workflow_activations"), 0);
}

#[test]
fn handoff_persists_digest_only_and_classifies_consume_outcomes() {
    let store = RegisteredWorkflowStore::start("workflow-handoff");
    let authority = authority(&store);
    let scope = handoff_scope();
    let secret = "s".repeat(48);
    let grant = TaskHandoffGrant::new(
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
                "SELECT scope_payload FROM workflow_handoffs WHERE token_digest = ?1",
                [grant.token_digest().as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!payload.contains(&secret));
        let persisted: TaskHandoffScope = serde_json::from_str(&payload).unwrap();
        assert_eq!(persisted, scope);
        assert_eq!(
            persisted.thread_id().as_str(),
            "thread.workflow.runtime-store"
        );
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM workflow_handoffs WHERE scope_payload LIKE ?1",
                [format!("%{secret}%")],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    });

    let wrong_scope = TaskHandoffScope::new(
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

    let expired = TaskHandoffGrant::new(
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
    let store = RegisteredWorkflowStore::start("workflow-restart");
    let authority = authority(&store);
    let first = definition(1, "operation.prepare.v1");
    WorkflowDefinitionAuthorityPort::insert(&authority, &first).unwrap();
    WorkflowDefinitionAuthorityPort::compare_and_swap_activation(
        &authority,
        first.definition_id(),
        None,
        Some(1),
    )
    .unwrap();

    let scope = handoff_scope();
    let grant = TaskHandoffGrant::new(
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
    let authority = WorkflowSqliteAuthority::from_registered(store.storage().clone()).unwrap();
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
