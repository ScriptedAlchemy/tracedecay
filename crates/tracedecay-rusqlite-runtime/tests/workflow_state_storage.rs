use std::collections::BTreeSet;

use tracedecay_application::{
    WorkflowDefinitionAggregateService, WorkflowRunAggregateService, WorkflowStateApplicationError,
};
use tracedecay_domain::{
    ManifestDigest, ProjectId, RunId, UtcMicros, WorkCommandId, WorkflowCommandContextV1,
    WorkflowDefinitionId, WorkflowDefinitionStatusV1, WorkflowDefinitionV1, WorkflowOperationRef,
    WorkflowOutputName, WorkflowRunStatusV1, WorkflowStepId, WorkflowStepStatusV1, WorkflowStepV1,
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

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn context(command: &str, byte: char, time: i64) -> WorkflowCommandContextV1 {
    WorkflowCommandContextV1::new(id::<WorkCommandId>(command), digest(byte), UtcMicros(time))
}

fn definition() -> WorkflowDefinitionV1 {
    WorkflowDefinitionV1::new(
        id::<WorkflowDefinitionId>("workflow.state-storage"),
        1,
        id::<ProjectId>("project.state-storage"),
        vec![
            WorkflowStepV1 {
                step_id: id::<WorkflowStepId>("step.build"),
                operation: id::<WorkflowOperationRef>("operation.build"),
                predecessors: BTreeSet::new(),
                inputs: Vec::new(),
                outputs: vec![id::<WorkflowOutputName>("artifact")],
                fan_out: None,
            },
            WorkflowStepV1 {
                step_id: id::<WorkflowStepId>("step.release"),
                operation: id::<WorkflowOperationRef>("operation.release"),
                predecessors: BTreeSet::from([id::<WorkflowStepId>("step.build")]),
                inputs: Vec::new(),
                outputs: Vec::new(),
                fan_out: None,
            },
        ],
        digest('b'),
        digest('c'),
        digest('d'),
    )
    .unwrap()
}

#[test]
fn definition_events_heads_cas_and_idempotency_survive_restart() {
    let store = RegisteredWorkStore::start("workflow-state-definition");
    let authority = WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap();
    let service = WorkflowDefinitionAggregateService::new(authority);
    let registered = service
        .register(
            definition(),
            BTreeSet::from([id("step.release")]),
            context("command.register", 'e', 1),
        )
        .unwrap();
    let replay = service
        .register(
            definition(),
            BTreeSet::from([id("step.release")]),
            context("command.register", 'e', 1),
        )
        .unwrap();
    assert_eq!(replay, registered);
    assert_eq!(
        service
            .register(
                definition(),
                BTreeSet::from([id("step.release")]),
                context("command.register", 'f', 1),
            )
            .unwrap_err(),
        WorkflowStateApplicationError::IdempotencyConflict
    );
    let validated = service
        .validate(
            definition().definition_id(),
            1,
            registered.aggregate_version(),
            context("command.validate", '1', 2),
        )
        .unwrap();
    assert_eq!(
        service
            .activate(
                definition().definition_id(),
                1,
                registered.aggregate_version(),
                context("command.activate.stale", '2', 3),
            )
            .unwrap_err(),
        WorkflowStateApplicationError::VersionConflict
    );
    let active = service
        .activate(
            definition().definition_id(),
            1,
            validated.aggregate_version(),
            context("command.activate", '3', 3),
        )
        .unwrap();
    assert_eq!(active.status(), WorkflowDefinitionStatusV1::Active);
    assert_eq!(store.count("workflow_definition_events_v1"), 3);
    assert_eq!(store.count("workflow_definition_heads_v1"), 1);

    let store = store.restart("workflow-state-definition-restart");
    let authority = WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap();
    let loaded = WorkflowDefinitionAggregateService::new(authority)
        .load(definition().definition_id(), 1)
        .unwrap();
    assert_eq!(loaded.projection, active);
    assert_eq!(loaded.events.len(), 3);
}

#[test]
fn run_restart_cancellation_reconciliation_and_history_survive_restart() {
    let store = RegisteredWorkStore::start("workflow-state-run");
    let authority = WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap();
    let definitions = WorkflowDefinitionAggregateService::new(authority.clone());
    let registered = definitions
        .register(
            definition(),
            BTreeSet::from([id("step.release")]),
            context("command.register.run", '3', 1),
        )
        .unwrap();
    let validated = definitions
        .validate(
            definition().definition_id(),
            1,
            registered.aggregate_version(),
            context("command.validate.run", '4', 2),
        )
        .unwrap();
    let active = definitions
        .activate(
            definition().definition_id(),
            1,
            validated.aggregate_version(),
            context("command.activate.run", '5', 3),
        )
        .unwrap();

    let runs = WorkflowRunAggregateService::new(authority);
    let run_id = id::<RunId>("run.state-storage");
    let admitted = runs
        .admit(run_id.clone(), &active, context("command.admit", '6', 4))
        .unwrap();
    assert_eq!(
        runs.admit(run_id.clone(), &active, context("command.admit", '6', 4))
            .unwrap(),
        admitted
    );
    assert_eq!(
        runs.admit(run_id.clone(), &active, context("command.admit", '7', 4))
            .unwrap_err(),
        WorkflowStateApplicationError::IdempotencyConflict
    );
    let running = runs
        .start(
            &run_id,
            admitted.aggregate_version(),
            id("step.build"),
            context("command.start", '8', 5),
        )
        .unwrap();
    assert_eq!(
        runs.pause(
            &run_id,
            admitted.aggregate_version(),
            context("command.pause.stale", '9', 6),
        )
        .unwrap_err(),
        WorkflowStateApplicationError::VersionConflict
    );
    let restarted = runs
        .restart(
            &run_id,
            running.aggregate_version(),
            context("command.restart", 'a', 7),
        )
        .unwrap();
    assert_eq!(restarted.authority_epoch(), 2);
    assert_eq!(
        restarted.step_status(&id("step.build")),
        Some(WorkflowStepStatusV1::ReconcileRequired)
    );
    let cancelling = runs
        .cancel(
            &run_id,
            restarted.aggregate_version(),
            context("command.cancel", 'b', 8),
        )
        .unwrap();
    assert_eq!(cancelling.status(), WorkflowRunStatusV1::Cancelling);
    let cancelled = runs
        .reconcile(
            &run_id,
            cancelling.aggregate_version(),
            id("step.build"),
            context("command.reconcile", 'c', 9),
        )
        .unwrap();
    assert_eq!(cancelled.status(), WorkflowRunStatusV1::Cancelled);
    assert_eq!(store.count("workflow_run_events_v1"), 5);
    assert_eq!(store.count("workflow_run_heads_v1"), 1);

    let store = store.restart("workflow-state-run-restart");
    let authority = WorkflowSqliteAuthority::from_work_storage(store.storage()).unwrap();
    let loaded = WorkflowRunAggregateService::new(authority)
        .load(&run_id)
        .unwrap();
    assert_eq!(loaded.projection, cancelled);
    assert_eq!(loaded.events.len(), 5);
}
