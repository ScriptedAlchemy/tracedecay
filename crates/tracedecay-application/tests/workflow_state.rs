use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    WorkflowAggregateAppendRequest, WorkflowAggregateSnapshot, WorkflowAppendOutcome,
    WorkflowDefinitionAggregateService, WorkflowDefinitionEventStorePort,
    WorkflowRunAggregateService, WorkflowRunEventStorePort, WorkflowStateApplicationError,
    WorkflowStateStoreError,
};
use tracedecay_domain::workflow_state::{
    WorkflowAggregateVersionV1, WorkflowCommandContextV1, WorkflowDefinitionEventV1,
    WorkflowDefinitionProjectionV1, WorkflowDefinitionStatusV1, WorkflowRunEventV1,
    WorkflowRunProjectionV1, WorkflowRunStatusV1, WorkflowStepStatusV1,
};
use tracedecay_domain::{
    ManifestDigest, ProjectId, RunId, UtcMicros, WorkCommandId, WorkflowDefinitionId,
    WorkflowDefinitionV1, WorkflowOperationRef, WorkflowOutputName, WorkflowStepId, WorkflowStepV1,
};

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

fn context(command: &str, digest_byte: char, time: i64) -> WorkflowCommandContextV1 {
    WorkflowCommandContextV1::new(
        id::<WorkCommandId>(command),
        digest(digest_byte),
        UtcMicros(time),
    )
}

fn definition() -> WorkflowDefinitionV1 {
    WorkflowDefinitionV1::new(
        id::<WorkflowDefinitionId>("workflow.application"),
        3,
        id::<ProjectId>("project.application"),
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
                step_id: id::<WorkflowStepId>("step.deploy"),
                operation: id::<WorkflowOperationRef>("operation.deploy"),
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

type DefinitionKey = (WorkflowDefinitionId, u64);
type DefinitionHistories = Arc<Mutex<BTreeMap<DefinitionKey, Vec<WorkflowDefinitionEventV1>>>>;

#[derive(Clone, Default)]
struct DefinitionStore {
    histories: DefinitionHistories,
}

impl WorkflowDefinitionEventStorePort for DefinitionStore {
    fn load(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<
        WorkflowAggregateSnapshot<WorkflowDefinitionProjectionV1, WorkflowDefinitionEventV1>,
        WorkflowStateStoreError,
    > {
        let events = self.history(definition_id, definition_version)?;
        let projection = WorkflowDefinitionProjectionV1::rebuild(&events)
            .map_err(WorkflowStateStoreError::InvalidHistory)?;
        Ok(WorkflowAggregateSnapshot { projection, events })
    }

    fn history(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Vec<WorkflowDefinitionEventV1>, WorkflowStateStoreError> {
        self.histories
            .lock()
            .unwrap()
            .get(&(definition_id.clone(), definition_version))
            .cloned()
            .ok_or(WorkflowStateStoreError::NotFoundOrNotAuthorized)
    }

    fn append(
        &self,
        request: &WorkflowAggregateAppendRequest<WorkflowDefinitionEventV1>,
    ) -> Result<WorkflowAppendOutcome<WorkflowDefinitionProjectionV1>, WorkflowStateStoreError>
    {
        let event = &request.event;
        let key = (event.definition_id().clone(), event.definition_version());
        let mut histories = self.histories.lock().unwrap();
        let existing = histories.get(&key).cloned().unwrap_or_default();
        if let Some(prior) = existing
            .iter()
            .find(|prior| prior.command_id() == event.command_id())
        {
            if prior.input_digest() != event.input_digest() {
                return Err(WorkflowStateStoreError::IdempotencyConflict);
            }
            return WorkflowDefinitionProjectionV1::rebuild(&existing)
                .map(WorkflowAppendOutcome::Replayed)
                .map_err(WorkflowStateStoreError::InvalidHistory);
        }
        let current = existing
            .last()
            .map(WorkflowDefinitionEventV1::aggregate_version);
        if current != request.expected_version {
            return Err(WorkflowStateStoreError::VersionConflict);
        }
        let history = histories.entry(key).or_default();
        history.push(event.clone());
        WorkflowDefinitionProjectionV1::rebuild(history)
            .map(WorkflowAppendOutcome::Appended)
            .map_err(WorkflowStateStoreError::InvalidHistory)
    }
}

#[test]
fn definition_commands_are_cas_checked_and_idempotent() {
    let service = WorkflowDefinitionAggregateService::new(DefinitionStore::default());
    let registered = service
        .register(
            definition(),
            BTreeSet::from([id("step.deploy")]),
            context("command.register", 'a', 1),
        )
        .unwrap();
    let replayed = service
        .register(
            definition(),
            BTreeSet::from([id("step.deploy")]),
            context("command.register", 'a', 1),
        )
        .unwrap();
    assert_eq!(registered, replayed);

    let conflict = service
        .register(
            definition(),
            BTreeSet::from([id("step.deploy")]),
            context("command.register", 'e', 1),
        )
        .unwrap_err();
    assert_eq!(conflict, WorkflowStateApplicationError::IdempotencyConflict);

    let validated = service
        .validate(
            definition().definition_id(),
            3,
            WorkflowAggregateVersionV1::initial(),
            context("command.validate", 'f', 2),
        )
        .unwrap();
    assert_eq!(validated.status(), WorkflowDefinitionStatusV1::Validated);
    assert_eq!(
        service
            .validate(
                definition().definition_id(),
                3,
                WorkflowAggregateVersionV1::initial(),
                context("command.validate", 'f', 2),
            )
            .unwrap(),
        validated
    );
    assert_eq!(
        service
            .validate(
                definition().definition_id(),
                3,
                WorkflowAggregateVersionV1::initial(),
                context("command.validate", '1', 2),
            )
            .unwrap_err(),
        WorkflowStateApplicationError::IdempotencyConflict
    );

    let stale = service
        .activate(
            definition().definition_id(),
            3,
            WorkflowAggregateVersionV1::initial(),
            context("command.activate-stale", '1', 3),
        )
        .unwrap_err();
    assert_eq!(stale, WorkflowStateApplicationError::VersionConflict);

    let active = service
        .activate(
            definition().definition_id(),
            3,
            validated.aggregate_version(),
            context("command.activate", '2', 3),
        )
        .unwrap();
    let retired = service
        .retire(
            definition().definition_id(),
            3,
            active.aggregate_version(),
            context("command.retire", '3', 4),
        )
        .unwrap();
    assert_eq!(retired.status(), WorkflowDefinitionStatusV1::Retired);
}

fn active_definition() -> WorkflowDefinitionProjectionV1 {
    let service = WorkflowDefinitionAggregateService::new(DefinitionStore::default());
    let registered = service
        .register(
            definition(),
            BTreeSet::from([id("step.deploy")]),
            context("command.definition-register", '4', 1),
        )
        .unwrap();
    let validated = service
        .validate(
            definition().definition_id(),
            3,
            registered.aggregate_version(),
            context("command.definition-validate", '5', 2),
        )
        .unwrap();
    service
        .activate(
            definition().definition_id(),
            3,
            validated.aggregate_version(),
            context("command.definition-activate", '6', 3),
        )
        .unwrap()
}

type RunHistories = Arc<Mutex<BTreeMap<RunId, Vec<WorkflowRunEventV1>>>>;

#[derive(Clone, Default)]
struct RunStore {
    histories: RunHistories,
}

impl WorkflowRunEventStorePort for RunStore {
    fn load(
        &self,
        run_id: &RunId,
    ) -> Result<
        WorkflowAggregateSnapshot<WorkflowRunProjectionV1, WorkflowRunEventV1>,
        WorkflowStateStoreError,
    > {
        let events = self.history(run_id)?;
        let projection = WorkflowRunProjectionV1::rebuild(&events)
            .map_err(WorkflowStateStoreError::InvalidHistory)?;
        Ok(WorkflowAggregateSnapshot { projection, events })
    }

    fn history(&self, run_id: &RunId) -> Result<Vec<WorkflowRunEventV1>, WorkflowStateStoreError> {
        self.histories
            .lock()
            .unwrap()
            .get(run_id)
            .cloned()
            .ok_or(WorkflowStateStoreError::NotFoundOrNotAuthorized)
    }

    fn append(
        &self,
        request: &WorkflowAggregateAppendRequest<WorkflowRunEventV1>,
    ) -> Result<WorkflowAppendOutcome<WorkflowRunProjectionV1>, WorkflowStateStoreError> {
        let event = &request.event;
        let mut histories = self.histories.lock().unwrap();
        let existing = histories.get(event.run_id()).cloned().unwrap_or_default();
        if let Some(prior) = existing
            .iter()
            .find(|prior| prior.command_id() == event.command_id())
        {
            if prior.input_digest() != event.input_digest() {
                return Err(WorkflowStateStoreError::IdempotencyConflict);
            }
            return WorkflowRunProjectionV1::rebuild(&existing)
                .map(WorkflowAppendOutcome::Replayed)
                .map_err(WorkflowStateStoreError::InvalidHistory);
        }
        let current = existing.last().map(WorkflowRunEventV1::aggregate_version);
        if current != request.expected_version {
            return Err(WorkflowStateStoreError::VersionConflict);
        }
        let history = histories.entry(event.run_id().clone()).or_default();
        history.push(event.clone());
        WorkflowRunProjectionV1::rebuild(history)
            .map(WorkflowAppendOutcome::Appended)
            .map_err(WorkflowStateStoreError::InvalidHistory)
    }
}

#[test]
fn run_admission_and_approval_commands_reach_success() {
    let service = WorkflowRunAggregateService::new(RunStore::default());
    let run_id = id::<RunId>("run.application.success");
    let admitted = service
        .admit(
            run_id.clone(),
            &active_definition(),
            context("command.run-admit", '7', 4),
        )
        .unwrap();
    let replayed = service
        .admit(
            run_id.clone(),
            &active_definition(),
            context("command.run-admit", '7', 4),
        )
        .unwrap();
    assert_eq!(admitted, replayed);
    assert_eq!(
        service
            .admit(
                run_id.clone(),
                &active_definition(),
                context("command.run-admit", '8', 4),
            )
            .unwrap_err(),
        WorkflowStateApplicationError::IdempotencyConflict
    );

    let build = id::<WorkflowStepId>("step.build");
    let deploy = id::<WorkflowStepId>("step.deploy");
    let running = service
        .start(
            &run_id,
            admitted.aggregate_version(),
            build.clone(),
            context("command.build-start", '9', 5),
        )
        .unwrap();
    assert_eq!(
        service
            .start(
                &run_id,
                admitted.aggregate_version(),
                build.clone(),
                context("command.build-start", '9', 5),
            )
            .unwrap(),
        running
    );
    assert_eq!(
        service
            .start(
                &run_id,
                admitted.aggregate_version(),
                build.clone(),
                context("command.build-start", '1', 5),
            )
            .unwrap_err(),
        WorkflowStateApplicationError::IdempotencyConflict
    );
    let built = service
        .succeed(
            &run_id,
            running.aggregate_version(),
            build,
            context("command.build-succeed", 'a', 6),
        )
        .unwrap();
    assert_eq!(
        built.step_status(&deploy),
        Some(WorkflowStepStatusV1::AwaitingApproval)
    );
    let requested = service
        .request_approval(
            &run_id,
            built.aggregate_version(),
            deploy.clone(),
            context("command.deploy-request", 'b', 7),
        )
        .unwrap();
    let approved = service
        .approve(
            &run_id,
            requested.aggregate_version(),
            deploy.clone(),
            context("command.deploy-approve", 'c', 8),
        )
        .unwrap();
    let deploying = service
        .start(
            &run_id,
            approved.aggregate_version(),
            deploy.clone(),
            context("command.deploy-start", 'd', 9),
        )
        .unwrap();
    let succeeded = service
        .succeed(
            &run_id,
            deploying.aggregate_version(),
            deploy,
            context("command.deploy-succeed", 'e', 10),
        )
        .unwrap();
    assert_eq!(succeeded.status(), WorkflowRunStatusV1::Succeeded);
}

#[test]
fn run_control_denial_and_failure_commands_preserve_distinct_states() {
    let service = WorkflowRunAggregateService::new(RunStore::default());
    let run_id = id::<RunId>("run.application.control");
    let admitted = service
        .admit(
            run_id.clone(),
            &active_definition(),
            context("command.control-admit", '7', 4),
        )
        .unwrap();
    let running = service
        .start(
            &run_id,
            admitted.aggregate_version(),
            id("step.build"),
            context("command.control-start", '8', 5),
        )
        .unwrap();
    let paused = service
        .pause(
            &run_id,
            running.aggregate_version(),
            context("command.control-pause", '9', 6),
        )
        .unwrap();
    let resumed = service
        .resume(
            &run_id,
            paused.aggregate_version(),
            context("command.control-resume", 'a', 7),
        )
        .unwrap();
    let restarted = service
        .restart(
            &run_id,
            resumed.aggregate_version(),
            context("command.control-restart", 'b', 8),
        )
        .unwrap();
    assert_eq!(restarted.authority_epoch(), 2);
    let cancelling = service
        .cancel(
            &run_id,
            restarted.aggregate_version(),
            context("command.control-cancel", 'c', 9),
        )
        .unwrap();
    let cancelled = service
        .reconcile(
            &run_id,
            cancelling.aggregate_version(),
            id("step.build"),
            context("command.control-reconcile", 'd', 10),
        )
        .unwrap();
    assert_eq!(cancelled.status(), WorkflowRunStatusV1::Cancelled);

    let denied_run = id::<RunId>("run.application.denied");
    let admitted = service
        .admit(
            denied_run.clone(),
            &active_definition(),
            context("command.denied-admit", '1', 4),
        )
        .unwrap();
    let running = service
        .start(
            &denied_run,
            admitted.aggregate_version(),
            id("step.build"),
            context("command.denied-start", '2', 5),
        )
        .unwrap();
    let built = service
        .succeed(
            &denied_run,
            running.aggregate_version(),
            id("step.build"),
            context("command.denied-build", '3', 6),
        )
        .unwrap();
    let requested = service
        .request_approval(
            &denied_run,
            built.aggregate_version(),
            id("step.deploy"),
            context("command.denied-request", '4', 7),
        )
        .unwrap();
    let denied = service
        .deny(
            &denied_run,
            requested.aggregate_version(),
            id("step.deploy"),
            context("command.denied-record", '5', 8),
        )
        .unwrap();
    assert_eq!(denied.status(), WorkflowRunStatusV1::Failed);

    let failed_run = id::<RunId>("run.application.failed");
    let admitted = service
        .admit(
            failed_run.clone(),
            &active_definition(),
            context("command.failed-admit", '6', 4),
        )
        .unwrap();
    let running = service
        .start(
            &failed_run,
            admitted.aggregate_version(),
            id("step.build"),
            context("command.failed-start", '7', 5),
        )
        .unwrap();
    let failed = service
        .fail(
            &failed_run,
            running.aggregate_version(),
            id("step.build"),
            context("command.failed-record", '8', 6),
        )
        .unwrap();
    assert_eq!(failed.status(), WorkflowRunStatusV1::Failed);
}
