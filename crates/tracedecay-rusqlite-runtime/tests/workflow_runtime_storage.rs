//! Durable workflow authority over the registered Work writer.

use tracedecay_application::{
    AuthorityReceipt, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, EffectId, IdempotencyKey, PolicyDecisionRef, RequestContext, RequestId,
    ResolvedScope, TaskHandoffAuthorityError, TaskHandoffAuthorityPort, TaskHandoffConsumeOutcome,
    TaskHandoffGrant, TaskHandoffRedeemed, TaskHandoffScope, WorkflowDefinitionAuthorityError,
    WorkflowDefinitionAuthorityPort, WorkflowEffectAuthorityPortV1, WorkflowEffectIdentityV1,
    WorkflowEffectJournalStateV1, WorkflowEffectOperationV1, WorkflowEffectOutcomeV1,
    WorkflowEffectPreparedV1, WorkflowEffectReceiptContextV1, WorkflowEffectSuccessV1,
};
use tracedecay_domain::{
    ActorId, ComponentVersion, ManifestDigest, ProjectId, RepositoryId, RunId, TaskId, ThreadId,
    UtcMicros, WorkflowDefinition, WorkflowDefinitionId, WorkflowOperationRef, WorkflowOutputName,
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

fn effect_context(actor: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        id("project.workflow.runtime-store"),
        id("repository.workflow.runtime-store"),
        id("worktree.workflow.runtime-store"),
        None,
    )
    .unwrap();
    let actor = id(actor);
    let grant = CapabilityGrantSnapshot::new(
        id::<CapabilityGrantId>("grant.workflow.runtime-store"),
        1,
        digest('1'),
        actor.clone(),
        UtcMicros(1),
        UtcMicros(90_000_000),
        scope.clone(),
        [id("capability.workflow.handoff_issue")]
            .into_iter()
            .collect(),
        [id("use-case.workflow.handoff_issue")]
            .into_iter()
            .collect(),
        DisclosureClass::Metadata,
    )
    .unwrap();
    RequestContext::new(
        actor,
        scope,
        grant,
        id::<RequestId>("request.workflow.runtime-store"),
        Deadline::new(UtcMicros(80_000_000)).unwrap(),
        CancellationContext::active("cancel.workflow.runtime-store").unwrap(),
    )
    .unwrap()
}

fn effect_identity(
    operation: WorkflowEffectOperationV1,
    actor: &str,
    input: char,
) -> WorkflowEffectIdentityV1 {
    let context = effect_context(actor);
    let policy = PolicyDecisionRef::new(
        "policy.workflow.runtime-store.v1",
        1,
        digest('6'),
        ComponentVersion::new("workflow-runtime-store.v1").unwrap(),
    )
    .unwrap();
    let authority = AuthorityReceipt::from_context(&context, policy, UtcMicros(10)).unwrap();
    let receipt_context = WorkflowEffectReceiptContextV1::new(
        id(&format!("use-case.workflow.{}", operation.as_str())),
        id::<EffectId>(&format!("effect.workflow.runtime-store.{input}")),
        authority,
        digest('7'),
        digest('8'),
        digest('9'),
        digest('a'),
    );
    WorkflowEffectIdentityV1::new(
        operation,
        id::<IdempotencyKey>(&format!("workflow.effect.{input}")),
        context.request_id().clone(),
        context.actor().clone(),
        context.scope().clone(),
        digest(input),
        UtcMicros(10),
        context.deadline().clone(),
        context.cancellation().clone(),
        receipt_context,
    )
    .unwrap()
}

#[test]
fn non_final_store_requires_reset_without_runtime_schema_mutation() {
    let store =
        RegisteredWorkflowStore::start_with_setup("workflow-reset-required", |connection| {
            connection
                .execute_batch(
                    "DROP TABLE workflow_handoffs;
                 DROP TABLE workflow_definitions;
                 DROP TABLE workflow_effect_journal;
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
fn definitions_preserve_history_and_reject_conflicting_payloads() {
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
        WorkflowDefinitionAuthorityPort::list(&authority, Some(first.definition_id()))
            .unwrap()
            .iter()
            .map(WorkflowDefinition::definition_version)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(
        WorkflowDefinitionAuthorityPort::list(&authority, None).unwrap(),
        vec![first, second],
        "definition reads must preserve immutable history"
    );

    assert_eq!(store.count("workflow_definitions"), 2);
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
        UtcMicros(60_000_010),
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
        UtcMicros(60_000_010),
    )
    .unwrap();
    TaskHandoffAuthorityPort::issue(&authority, &expired).unwrap();
    assert_eq!(
        TaskHandoffAuthorityPort::consume(
            &authority,
            expired.token_digest(),
            &scope,
            UtcMicros(60_000_010),
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

    let scope = handoff_scope();
    let grant = TaskHandoffGrant::new(
        scope.clone(),
        token_digest(&"r".repeat(48)),
        UtcMicros(10),
        UtcMicros(60_000_010),
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
        TaskHandoffAuthorityPort::consume(&authority, grant.token_digest(), &scope, UtcMicros(12),)
            .unwrap(),
        TaskHandoffConsumeOutcome::Replay
    );
}

#[test]
fn lost_issue_response_replays_the_exact_committed_terminal() {
    let store = RegisteredWorkflowStore::start("workflow-effect-issue-replay");
    let authority = authority(&store);
    let scope = handoff_scope();
    let grant = TaskHandoffGrant::new(
        scope,
        token_digest(&"i".repeat(48)),
        UtcMicros(10),
        UtcMicros(60_000_010),
    )
    .unwrap();
    let identity = effect_identity(
        WorkflowEffectOperationV1::HandoffIssue,
        "actor.workflow.source",
        '2',
    );
    let prepared = WorkflowEffectPreparedV1::HandoffIssue(grant.clone());

    let first = WorkflowEffectAuthorityPortV1::execute_effect(
        &authority,
        &identity,
        &prepared,
        UtcMicros(20),
    )
    .unwrap();
    let retry = WorkflowEffectAuthorityPortV1::execute_effect(
        &authority,
        &identity,
        &prepared,
        UtcMicros(30),
    )
    .unwrap();

    assert_eq!(retry, first);
    assert_eq!(first.state(), WorkflowEffectJournalStateV1::Reconciled);
    assert_eq!(
        first.terminal().unwrap().outcome(),
        &WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::HandoffIssued(grant))
    );
    assert_eq!(store.count("workflow_handoffs"), 1);
}

#[test]
fn lost_redeem_response_replays_success_instead_of_token_replay() {
    let store = RegisteredWorkflowStore::start("workflow-effect-redeem-replay");
    let authority = authority(&store);
    let scope = handoff_scope();
    let secret = "r".repeat(48);
    let grant = TaskHandoffGrant::new(
        scope.clone(),
        token_digest(&secret),
        UtcMicros(10),
        UtcMicros(60_000_010),
    )
    .unwrap();
    TaskHandoffAuthorityPort::issue(&authority, &grant).unwrap();
    let identity = effect_identity(
        WorkflowEffectOperationV1::HandoffRedeem,
        "actor.workflow.target",
        '3',
    );
    let prepared = WorkflowEffectPreparedV1::HandoffRedeem {
        token_digest: token_digest(&secret),
        expected_scope: scope.clone(),
        consumed_at: UtcMicros(20),
    };

    let first = WorkflowEffectAuthorityPortV1::execute_effect(
        &authority,
        &identity,
        &prepared,
        UtcMicros(21),
    )
    .unwrap();
    let restarted = store.restart("workflow-effect-redeem-replay");
    let authority = authority(&restarted);
    let retry = WorkflowEffectAuthorityPortV1::execute_effect(
        &authority,
        &identity,
        &prepared,
        UtcMicros(40),
    )
    .unwrap();

    assert_eq!(retry, first);
    assert_eq!(
        retry.terminal().unwrap().outcome(),
        &WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::HandoffRedeemed(
            TaskHandoffRedeemed { scope }
        ))
    );
}

#[test]
fn rejected_effect_replays_the_exact_problem_without_reapplying() {
    let store = RegisteredWorkflowStore::start("workflow-effect-problem-replay");
    let authority = authority(&store);
    let scope = handoff_scope();
    let grant = TaskHandoffGrant::new(
        scope,
        token_digest(&"p".repeat(48)),
        UtcMicros(10),
        UtcMicros(60_000_010),
    )
    .unwrap();
    TaskHandoffAuthorityPort::issue(&authority, &grant).unwrap();
    let identity = effect_identity(
        WorkflowEffectOperationV1::HandoffIssue,
        "actor.workflow.source",
        '5',
    );
    let prepared = WorkflowEffectPreparedV1::HandoffIssue(grant);

    let first = WorkflowEffectAuthorityPortV1::execute_effect(
        &authority,
        &identity,
        &prepared,
        UtcMicros(20),
    )
    .unwrap();
    let retry = WorkflowEffectAuthorityPortV1::execute_effect(
        &authority,
        &identity,
        &prepared,
        UtcMicros(30),
    )
    .unwrap();

    assert_eq!(retry, first);
    assert_eq!(
        retry.terminal().unwrap().outcome(),
        &WorkflowEffectOutcomeV1::Problem(
            tracedecay_application::WorkflowEffectProblemV1::InvalidRequest
        )
    );
    assert_eq!(store.count("workflow_handoffs"), 1);
}

#[test]
fn restart_reconciles_a_reserved_in_flight_effect_before_mutation() {
    let store = RegisteredWorkflowStore::start("workflow-effect-in-flight");
    let authority = authority(&store);
    let identity = effect_identity(
        WorkflowEffectOperationV1::RegisterDefinition,
        "actor.workflow.source",
        '4',
    );
    let reserved = WorkflowEffectAuthorityPortV1::reserve_effect(&authority, &identity).unwrap();
    assert_eq!(reserved.state(), WorkflowEffectJournalStateV1::BeforeEffect);
    store.inspect(|connection| {
        connection
            .execute(
                "UPDATE workflow_effect_journal
                 SET state = 'in_flight'
                 WHERE idempotency_key = ?1",
                [identity.idempotency_key().as_str()],
            )
            .unwrap();
    });
    let restarted = store.restart("workflow-effect-in-flight");
    let authority = authority(&restarted);
    let prepared =
        WorkflowEffectPreparedV1::RegisterDefinition(definition(1, "operation.prepare.v1"));

    let reconciled = WorkflowEffectAuthorityPortV1::execute_effect(
        &authority,
        &identity,
        &prepared,
        UtcMicros(20),
    )
    .unwrap();

    assert_eq!(reconciled.state(), WorkflowEffectJournalStateV1::Reconciled);
    assert_eq!(restarted.count("workflow_definitions"), 1);
}
