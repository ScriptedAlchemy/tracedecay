//! Durable workflow authority over the registered Work writer.

use std::sync::{Arc, Barrier};

use tracedecay_application::{
    AuthorityReceipt, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, EffectId, IdempotencyKey, PolicyDecisionRef, RequestContext, RequestId,
    ResolvedScope, TaskHandoffAuthorityError, TaskHandoffAuthorityPort, TaskHandoffConsumeOutcome,
    TaskHandoffGrant, TaskHandoffRedeemed, TaskHandoffScope, WorkflowDefinitionAuthorityPort,
    WorkflowDefinitionLifecycleCommand, WorkflowDefinitionLifecycleState,
    WorkflowEffectAuthorityPortV1, WorkflowEffectIdentityV1, WorkflowEffectJournalStateV1,
    WorkflowEffectOperationV1, WorkflowEffectOutcomeV1, WorkflowEffectPreparedV1,
    WorkflowEffectProblemV1, WorkflowEffectReceiptContextV1, WorkflowEffectSuccessV1,
    WorkflowLifecycleOperation,
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

#[derive(Clone, Copy)]
struct EffectAuthorityBinding {
    grant_revision: u64,
    grant_digest: char,
    policy_revision: u64,
    policy_digest: char,
    configuration_digest: char,
    catalog_digest: char,
    privacy_digest: char,
}

impl EffectAuthorityBinding {
    const BASE: Self = Self {
        grant_revision: 1,
        grant_digest: '1',
        policy_revision: 1,
        policy_digest: '6',
        configuration_digest: '8',
        catalog_digest: '9',
        privacy_digest: 'a',
    };
}

fn effect_context(actor: &str, grant_revision: u64, grant_digest: char) -> RequestContext {
    let scope = ResolvedScope::new(
        id("project.workflow.runtime-store"),
        id("repository.workflow.runtime-store"),
        id("worktree.workflow.runtime-store"),
        None,
    )
    .unwrap();
    let actor: ActorId = id(actor);
    let grant = CapabilityGrantSnapshot::new(
        id::<CapabilityGrantId>("grant.workflow.runtime-store"),
        grant_revision,
        digest(grant_digest),
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
    effect_identity_at(
        operation,
        actor,
        input,
        EffectAuthorityBinding::BASE,
        UtcMicros(10),
    )
}

fn effect_identity_at(
    operation: WorkflowEffectOperationV1,
    actor: &str,
    input: char,
    binding: EffectAuthorityBinding,
    started_at: UtcMicros,
) -> WorkflowEffectIdentityV1 {
    let context = effect_context(actor, binding.grant_revision, binding.grant_digest);
    let policy = PolicyDecisionRef::new(
        "policy.workflow.runtime-store.v1",
        binding.policy_revision,
        digest(binding.policy_digest),
        ComponentVersion::new("workflow-runtime-store.v1").unwrap(),
    )
    .unwrap();
    let authority = AuthorityReceipt::from_context(&context, policy, started_at).unwrap();
    let receipt_context = WorkflowEffectReceiptContextV1::new(
        id(&format!("use-case.workflow.{}", operation.as_str())),
        id::<EffectId>(&format!("effect.workflow.runtime-store.{input}")),
        authority,
        digest('7'),
        digest(binding.configuration_digest),
        digest(binding.catalog_digest),
        digest(binding.privacy_digest),
    );
    WorkflowEffectIdentityV1::new(
        operation,
        id::<IdempotencyKey>(&format!("workflow.effect.{input}")),
        context.request_id().clone(),
        context.actor().clone(),
        context.scope().clone(),
        digest(input),
        started_at,
        context.deadline().clone(),
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
                 DROP TABLE workflow_artifact_payloads;
                 DROP TABLE workflow_definition_disposition;
                 DROP TABLE workflow_definition_source_journal;
                 DROP TABLE workflow_definition_transition_journal;
                 DROP TABLE workflow_effect_journal;
                 DROP TABLE workflow_run_journal;
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
            "workflow-extra-schema-identity",
            "PRAGMA ignore_check_constraints = ON;
             INSERT INTO workflow_schema (
                 singleton,
                 schema_version,
                 definition_digest
             ) VALUES (
                 2,
                 1,
                 'sha256:5bb8241c0964fa921f40ed8c4cc44887572bc3e2295fdee93622e1039e9e3bcd'
             );",
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
fn definition_effects_retain_sources_without_sql_topology_authority() {
    let store = RegisteredWorkflowStore::start("workflow-definition-sources");
    let authority = authority(&store);
    let first = definition(1, "operation.prepare.v1");
    let second = definition(2, "operation.prepare.v1");
    let conflicting = definition(1, "operation.prepare.v2");

    let first_identity = effect_identity(
        WorkflowEffectOperationV1::RegisterDefinition,
        "actor.workflow.source",
        'f',
    );
    let first_prepared = WorkflowEffectPreparedV1::register_definition(
        first_identity.input_digest().clone(),
        first.clone(),
    );
    let first_record = WorkflowEffectAuthorityPortV1::execute_effect(
        &authority,
        &first_identity,
        &first_prepared,
        UtcMicros(20),
    )
    .unwrap();
    assert_eq!(
        WorkflowEffectAuthorityPortV1::execute_effect(
            &authority,
            &first_identity,
            &first_prepared,
            UtcMicros(30),
        )
        .unwrap(),
        first_record
    );

    let repeated_identity = effect_identity(
        WorkflowEffectOperationV1::RegisterDefinition,
        "actor.workflow.source",
        'g',
    );
    let repeated_prepared = WorkflowEffectPreparedV1::register_definition(
        repeated_identity.input_digest().clone(),
        first.clone(),
    );
    assert_eq!(
        WorkflowEffectAuthorityPortV1::execute_effect(
            &authority,
            &repeated_identity,
            &repeated_prepared,
            UtcMicros(20),
        )
        .unwrap()
        .terminal()
        .unwrap()
        .outcome(),
        &WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionRegistered(Box::new(
            first.clone()
        )))
    );

    let conflicting_identity = effect_identity(
        WorkflowEffectOperationV1::RegisterDefinition,
        "actor.workflow.source",
        'h',
    );
    let conflicting_prepared = WorkflowEffectPreparedV1::register_definition(
        conflicting_identity.input_digest().clone(),
        conflicting,
    );
    assert_eq!(
        WorkflowEffectAuthorityPortV1::execute_effect(
            &authority,
            &conflicting_identity,
            &conflicting_prepared,
            UtcMicros(20),
        )
        .unwrap()
        .terminal()
        .unwrap()
        .outcome(),
        &WorkflowEffectOutcomeV1::Problem(
            tracedecay_application::WorkflowEffectProblemV1::InvalidRequest
        )
    );

    let second_identity = effect_identity(
        WorkflowEffectOperationV1::RegisterDefinition,
        "actor.workflow.source",
        'i',
    );
    let second_prepared = WorkflowEffectPreparedV1::register_definition(
        second_identity.input_digest().clone(),
        second.clone(),
    );
    WorkflowEffectAuthorityPortV1::execute_effect(
        &authority,
        &second_identity,
        &second_prepared,
        UtcMicros(20),
    )
    .unwrap();

    store.inspect(|connection| {
        let mut statement = connection
            .prepare(
                "SELECT payload FROM workflow_definition_source_journal
                 ORDER BY definition_version",
            )
            .unwrap();
        let retained = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|payload| serde_json::from_str::<WorkflowDefinition>(&payload.unwrap()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(retained, vec![first, second]);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema
                     WHERE type = 'table' AND name = 'workflow_definitions'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    });
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
fn definition_source_journal_and_handoff_survive_registered_store_restart() {
    let store = RegisteredWorkflowStore::start("workflow-restart");
    let authority = authority(&store);
    let first = definition(1, "operation.prepare.v1");
    let identity = effect_identity(
        WorkflowEffectOperationV1::RegisterDefinition,
        "actor.workflow.source",
        'j',
    );
    let prepared =
        WorkflowEffectPreparedV1::register_definition(identity.input_digest().clone(), first);
    let definition_record = WorkflowEffectAuthorityPortV1::execute_effect(
        &authority,
        &identity,
        &prepared,
        UtcMicros(20),
    )
    .unwrap();

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
        WorkflowEffectAuthorityPortV1::execute_effect(
            &authority,
            &identity,
            &prepared,
            UtcMicros(30),
        )
        .unwrap(),
        definition_record
    );
    assert_eq!(store.count("workflow_definition_source_journal"), 1);
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
    let prepared =
        WorkflowEffectPreparedV1::handoff_issue(identity.input_digest().clone(), grant.clone());

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
        &WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::HandoffIssued(Box::new(grant)))
    );
    assert_eq!(store.count("workflow_handoffs"), 1);
}

#[test]
fn lost_redeem_response_replays_success_instead_of_token_replay() {
    let store = RegisteredWorkflowStore::start("workflow-effect-redeem-replay");
    let workflow_authority = authority(&store);
    let scope = handoff_scope();
    let secret = "r".repeat(48);
    let grant = TaskHandoffGrant::new(
        scope.clone(),
        token_digest(&secret),
        UtcMicros(10),
        UtcMicros(60_000_010),
    )
    .unwrap();
    TaskHandoffAuthorityPort::issue(&workflow_authority, &grant).unwrap();
    let identity = effect_identity(
        WorkflowEffectOperationV1::HandoffRedeem,
        "actor.workflow.target",
        '3',
    );
    let prepared = WorkflowEffectPreparedV1::handoff_redeem(
        identity.input_digest().clone(),
        token_digest(&secret),
        scope.clone(),
        UtcMicros(20),
    );

    let first = WorkflowEffectAuthorityPortV1::execute_effect(
        &workflow_authority,
        &identity,
        &prepared,
        UtcMicros(21),
    )
    .unwrap();
    let restarted = store.restart("workflow-effect-redeem-replay");
    let restarted_authority = authority(&restarted);
    let retry = WorkflowEffectAuthorityPortV1::execute_effect(
        &restarted_authority,
        &identity,
        &prepared,
        UtcMicros(40),
    )
    .unwrap();

    assert_eq!(retry, first);
    assert_eq!(
        retry.terminal().unwrap().outcome(),
        &WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::HandoffRedeemed(Box::new(
            TaskHandoffRedeemed { scope }
        )))
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
    let prepared = WorkflowEffectPreparedV1::handoff_issue(identity.input_digest().clone(), grant);

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
    let workflow_authority = authority(&store);
    let identity = effect_identity(
        WorkflowEffectOperationV1::RegisterDefinition,
        "actor.workflow.source",
        '4',
    );
    let prepared = WorkflowEffectPreparedV1::register_definition(
        identity.input_digest().clone(),
        definition(1, "operation.prepare.v1"),
    );
    let reserved =
        WorkflowEffectAuthorityPortV1::reserve_effect(&workflow_authority, &identity, &prepared)
            .unwrap();
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
    let restarted_authority = authority(&restarted);
    let reconciled = WorkflowEffectAuthorityPortV1::execute_effect(
        &restarted_authority,
        &identity,
        &prepared,
        UtcMicros(20),
    )
    .unwrap();

    assert_eq!(reconciled.state(), WorkflowEffectJournalStateV1::Reconciled);
    assert_eq!(restarted.count("workflow_definition_source_journal"), 1);
}

#[test]
fn authority_drift_cannot_alias_an_existing_effect_reservation() {
    let store = RegisteredWorkflowStore::start("workflow-effect-authority-drift");
    let authority = authority(&store);
    let original = effect_identity_at(
        WorkflowEffectOperationV1::RegisterDefinition,
        "actor.workflow.source",
        'b',
        EffectAuthorityBinding::BASE,
        UtcMicros(10),
    );
    let prepared = WorkflowEffectPreparedV1::register_definition(
        original.input_digest().clone(),
        definition(1, "operation.prepare.v1"),
    );
    WorkflowEffectAuthorityPortV1::reserve_effect(&authority, &original, &prepared).unwrap();

    let base = EffectAuthorityBinding::BASE;
    for drifted_binding in [
        EffectAuthorityBinding {
            grant_revision: 2,
            ..base
        },
        EffectAuthorityBinding {
            grant_digest: '2',
            ..base
        },
        EffectAuthorityBinding {
            policy_revision: 2,
            ..base
        },
        EffectAuthorityBinding {
            policy_digest: '3',
            ..base
        },
        EffectAuthorityBinding {
            configuration_digest: '4',
            ..base
        },
        EffectAuthorityBinding {
            catalog_digest: '5',
            ..base
        },
        EffectAuthorityBinding {
            privacy_digest: 'b',
            ..base
        },
    ] {
        let drifted = effect_identity_at(
            WorkflowEffectOperationV1::RegisterDefinition,
            "actor.workflow.source",
            'b',
            drifted_binding,
            UtcMicros(20),
        );
        assert_eq!(
            WorkflowEffectAuthorityPortV1::execute_effect(
                &authority,
                &drifted,
                &prepared,
                UtcMicros(30),
            )
            .unwrap_err(),
            tracedecay_application::WorkflowEffectAuthorityErrorV1::IdentityConflict
        );
    }
    assert_eq!(store.count("workflow_definition_source_journal"), 0);
}

#[test]
fn prepared_input_cannot_mutate_under_another_inputs_receipt() {
    let store = RegisteredWorkflowStore::start("workflow-effect-input-swap");
    let authority = authority(&store);
    let identity = effect_identity(
        WorkflowEffectOperationV1::RegisterDefinition,
        "actor.workflow.source",
        'c',
    );
    let prepared = WorkflowEffectPreparedV1::register_definition(
        digest('d'),
        definition(1, "operation.wrong-input.v1"),
    );

    assert_eq!(
        WorkflowEffectAuthorityPortV1::execute_effect(
            &authority,
            &identity,
            &prepared,
            UtcMicros(20),
        )
        .unwrap_err(),
        tracedecay_application::WorkflowEffectAuthorityErrorV1::IdentityConflict
    );
    assert_eq!(store.count("workflow_definition_source_journal"), 0);
}

#[test]
fn restart_uses_the_reserved_preparation_and_timestamps() {
    let store = RegisteredWorkflowStore::start("workflow-effect-reserved-preparation");
    let workflow_authority = authority(&store);
    let identity = effect_identity_at(
        WorkflowEffectOperationV1::HandoffIssue,
        "actor.workflow.source",
        'd',
        EffectAuthorityBinding::BASE,
        UtcMicros(10),
    );
    let original_grant = TaskHandoffGrant::new(
        handoff_scope(),
        token_digest(&"t".repeat(48)),
        UtcMicros(10),
        UtcMicros(60_000_010),
    )
    .unwrap();
    let original = WorkflowEffectPreparedV1::handoff_issue(
        identity.input_digest().clone(),
        original_grant.clone(),
    );
    WorkflowEffectAuthorityPortV1::reserve_effect(&workflow_authority, &identity, &original)
        .unwrap();
    store.inspect(|connection| {
        connection
            .execute(
                "UPDATE workflow_effect_journal SET state = 'in_flight'
                 WHERE idempotency_key = ?1",
                [identity.idempotency_key().as_str()],
            )
            .unwrap();
    });
    let store = store.restart("workflow-effect-reserved-preparation");
    let restarted_authority = authority(&store);
    let retry_identity = effect_identity_at(
        WorkflowEffectOperationV1::HandoffIssue,
        "actor.workflow.source",
        'd',
        EffectAuthorityBinding::BASE,
        UtcMicros(40),
    );
    let recomputed = WorkflowEffectPreparedV1::handoff_issue(
        retry_identity.input_digest().clone(),
        TaskHandoffGrant::new(
            handoff_scope(),
            token_digest(&"t".repeat(48)),
            UtcMicros(40),
            UtcMicros(60_000_040),
        )
        .unwrap(),
    );

    let record = WorkflowEffectAuthorityPortV1::execute_effect(
        &restarted_authority,
        &retry_identity,
        &recomputed,
        UtcMicros(50),
    )
    .unwrap();

    assert_eq!(
        record.terminal().unwrap().identity().started_at(),
        UtcMicros(10)
    );
    assert_eq!(
        record.terminal().unwrap().outcome(),
        &WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::HandoffIssued(Box::new(
            original_grant
        )))
    );
}

#[test]
fn concurrent_exact_replays_all_return_the_committed_terminal() {
    let store = RegisteredWorkflowStore::start("workflow-effect-concurrent-replay");
    let authority = authority(&store);
    let identity = effect_identity(
        WorkflowEffectOperationV1::RegisterDefinition,
        "actor.workflow.source",
        'e',
    );
    let prepared = WorkflowEffectPreparedV1::register_definition(
        identity.input_digest().clone(),
        definition(1, "operation.prepare.v1"),
    );
    let barrier = Arc::new(Barrier::new(8));
    let handles = (0..8)
        .map(|_| {
            let authority = authority.clone();
            let identity = identity.clone();
            let prepared = prepared.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                WorkflowEffectAuthorityPortV1::execute_effect(
                    &authority,
                    &identity,
                    &prepared,
                    UtcMicros(20),
                )
            })
        })
        .collect::<Vec<_>>();
    let records = handles
        .into_iter()
        .map(|handle| handle.join().unwrap().unwrap())
        .collect::<Vec<_>>();

    assert!(records.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(store.count("workflow_definition_source_journal"), 1);
}

fn register(authority: &WorkflowSqliteAuthority, definition: &WorkflowDefinition, input: char) {
    let identity = effect_identity(
        WorkflowEffectOperationV1::RegisterDefinition,
        "actor.workflow.source",
        input,
    );
    let prepared = WorkflowEffectPreparedV1::register_definition(
        identity.input_digest().clone(),
        definition.clone(),
    );
    WorkflowEffectAuthorityPortV1::execute_effect(authority, &identity, &prepared, UtcMicros(20))
        .unwrap();
}

fn lifecycle_command(
    definition: &WorkflowDefinition,
    operation: WorkflowLifecycleOperation,
    expected_revision: u64,
    transitioned_at: i64,
) -> WorkflowDefinitionLifecycleCommand {
    WorkflowDefinitionLifecycleCommand {
        definition_id: definition.definition_id().clone(),
        definition_version: definition.definition_version(),
        operation,
        expected_revision,
        transitioned_at: UtcMicros(transitioned_at),
    }
}

fn lifecycle_effect(
    authority: &WorkflowSqliteAuthority,
    operation: WorkflowEffectOperationV1,
    input: char,
    command: WorkflowDefinitionLifecycleCommand,
) -> WorkflowEffectOutcomeV1 {
    let identity = effect_identity(operation, "actor.workflow.source", input);
    let prepared = match operation {
        WorkflowEffectOperationV1::ActivateDefinition => {
            WorkflowEffectPreparedV1::activate_definition(identity.input_digest().clone(), command)
        }
        WorkflowEffectOperationV1::RetireDefinition => {
            WorkflowEffectPreparedV1::retire_definition(identity.input_digest().clone(), command)
        }
        WorkflowEffectOperationV1::RejectDefinition => {
            WorkflowEffectPreparedV1::reject_definition(identity.input_digest().clone(), command)
        }
        _ => panic!("not a lifecycle operation"),
    };
    WorkflowEffectAuthorityPortV1::execute_effect(authority, &identity, &prepared, UtcMicros(40))
        .unwrap()
        .terminal()
        .unwrap()
        .outcome()
        .clone()
}

#[test]
fn registration_seeds_a_candidate_disposition_that_activation_advances() {
    let store = RegisteredWorkflowStore::start("workflow-lifecycle-activate");
    let authority = authority(&store);
    let definition = definition(1, "operation.prepare.v1");
    register(&authority, &definition, 'p');

    let candidate = WorkflowDefinitionAuthorityPort::load_disposition(
        &authority,
        definition.definition_id(),
        1,
    )
    .unwrap()
    .unwrap();
    assert_eq!(candidate.state, WorkflowDefinitionLifecycleState::Candidate);
    assert_eq!(candidate.revision, 1);

    let activated = lifecycle_effect(
        &authority,
        WorkflowEffectOperationV1::ActivateDefinition,
        'q',
        lifecycle_command(&definition, WorkflowLifecycleOperation::Activate, 1, 50),
    );
    let WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionActivated(disposition)) =
        activated
    else {
        panic!("activation must succeed from the candidate disposition");
    };
    assert_eq!(disposition.state, WorkflowDefinitionLifecycleState::Active);
    assert_eq!(disposition.revision, 3);

    // Plan 32 keeps `candidate -> validated -> active`, so the intermediate
    // state is an immutable history entry of its own.
    let history = WorkflowDefinitionAuthorityPort::transition_history(
        &authority,
        definition.definition_id(),
        1,
    )
    .unwrap();
    assert_eq!(
        history
            .iter()
            .map(|entry| (entry.from_state, entry.to_state, entry.to_revision))
            .collect::<Vec<_>>(),
        vec![
            (
                WorkflowDefinitionLifecycleState::Candidate,
                WorkflowDefinitionLifecycleState::Validated,
                2
            ),
            (
                WorkflowDefinitionLifecycleState::Validated,
                WorkflowDefinitionLifecycleState::Active,
                3
            ),
        ]
    );

    let retired = lifecycle_effect(
        &authority,
        WorkflowEffectOperationV1::RetireDefinition,
        'r',
        lifecycle_command(&definition, WorkflowLifecycleOperation::Retire, 3, 60),
    );
    let WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionRetired(retired)) =
        retired
    else {
        panic!("retirement must succeed from the active disposition");
    };
    assert_eq!(retired.state, WorkflowDefinitionLifecycleState::Retired);
    assert_eq!(retired.revision, 4);
}

#[test]
fn rejection_is_terminal_and_illegal_transitions_are_conflicts() {
    let store = RegisteredWorkflowStore::start("workflow-lifecycle-reject");
    let authority = authority(&store);
    let definition = definition(1, "operation.prepare.v1");
    register(&authority, &definition, 's');

    let rejected = lifecycle_effect(
        &authority,
        WorkflowEffectOperationV1::RejectDefinition,
        't',
        lifecycle_command(&definition, WorkflowLifecycleOperation::Reject, 1, 70),
    );
    let WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionRejected(rejected)) =
        rejected
    else {
        panic!("rejection must succeed from the candidate disposition");
    };
    assert_eq!(rejected.state, WorkflowDefinitionLifecycleState::Rejected);

    assert_eq!(
        lifecycle_effect(
            &authority,
            WorkflowEffectOperationV1::ActivateDefinition,
            'u',
            lifecycle_command(&definition, WorkflowLifecycleOperation::Activate, 2, 71),
        ),
        WorkflowEffectOutcomeV1::Problem(WorkflowEffectProblemV1::Conflict),
        "a rejected disposition is terminal"
    );
    assert_eq!(
        lifecycle_effect(
            &authority,
            WorkflowEffectOperationV1::RetireDefinition,
            'v',
            lifecycle_command(&definition, WorkflowLifecycleOperation::Retire, 2, 72),
        ),
        WorkflowEffectOutcomeV1::Problem(WorkflowEffectProblemV1::Conflict),
        "retirement has no edge out of a rejected disposition"
    );

    let unregistered = WorkflowDefinitionLifecycleCommand {
        definition_id: definition.definition_id().clone(),
        definition_version: 9,
        operation: WorkflowLifecycleOperation::Activate,
        expected_revision: 1,
        transitioned_at: UtcMicros(73),
    };
    assert_eq!(
        lifecycle_effect(
            &authority,
            WorkflowEffectOperationV1::ActivateDefinition,
            'w',
            unregistered,
        ),
        WorkflowEffectOutcomeV1::Problem(WorkflowEffectProblemV1::NotFoundOrNotAuthorized)
    );
}

#[test]
fn a_replayed_lifecycle_command_appends_no_second_history_entry() {
    let store = RegisteredWorkflowStore::start("workflow-lifecycle-replay");
    let authority = authority(&store);
    let definition = definition(1, "operation.prepare.v1");
    register(&authority, &definition, 'x');
    // Registration is replay-safe over the already seeded disposition.
    register(&authority, &definition, 'x');

    let command = lifecycle_command(&definition, WorkflowLifecycleOperation::Activate, 1, 80);
    let first = lifecycle_effect(
        &authority,
        WorkflowEffectOperationV1::ActivateDefinition,
        'y',
        command.clone(),
    );
    // A fresh idempotency key replays the same compare-and-swap against an
    // already advanced disposition; the journal, not the effect key, is what
    // makes it observably identical.
    let replayed = lifecycle_effect(
        &authority,
        WorkflowEffectOperationV1::ActivateDefinition,
        'z',
        command,
    );
    assert_eq!(first, replayed);

    assert_eq!(
        WorkflowDefinitionAuthorityPort::transition_history(
            &authority,
            definition.definition_id(),
            1
        )
        .unwrap()
        .len(),
        2,
        "replay must not append a second immutable history entry"
    );
    assert_eq!(
        store.inspect(|connection| {
            connection
                .query_row(
                    "SELECT revision FROM workflow_definition_disposition
                     WHERE definition_id = ?1 AND definition_version = 1",
                    [definition.definition_id().as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap()
        }),
        3
    );
}

#[test]
fn a_stale_expected_revision_is_a_compare_and_swap_conflict() {
    let store = RegisteredWorkflowStore::start("workflow-lifecycle-cas");
    let authority = authority(&store);
    let definition = definition(1, "operation.prepare.v1");
    register(&authority, &definition, 'k');

    lifecycle_effect(
        &authority,
        WorkflowEffectOperationV1::ActivateDefinition,
        'l',
        lifecycle_command(&definition, WorkflowLifecycleOperation::Activate, 1, 90),
    );
    assert_eq!(
        lifecycle_effect(
            &authority,
            WorkflowEffectOperationV1::RetireDefinition,
            'm',
            lifecycle_command(&definition, WorkflowLifecycleOperation::Retire, 1, 91),
        ),
        WorkflowEffectOutcomeV1::Problem(WorkflowEffectProblemV1::Conflict),
        "retiring against a superseded revision must not silently overwrite"
    );
    assert_eq!(
        WorkflowDefinitionAuthorityPort::load_disposition(
            &authority,
            definition.definition_id(),
            1
        )
        .unwrap()
        .unwrap()
        .state,
        WorkflowDefinitionLifecycleState::Active
    );
}
