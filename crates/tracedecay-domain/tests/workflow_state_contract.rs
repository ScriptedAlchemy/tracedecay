use std::collections::BTreeSet;

use tracedecay_domain::workflow_state::{
    WorkflowAggregateVersionV1, WorkflowCommandContextV1, WorkflowDefinitionCommandV1,
    WorkflowDefinitionProjectionV1, WorkflowDefinitionStatusV1, WorkflowRunCommandV1,
    WorkflowRunEventKindV1, WorkflowRunProjectionV1, WorkflowRunStatusV1, WorkflowStateError,
    WorkflowStepStatusV1,
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

fn context(command: &str, time: i64) -> WorkflowCommandContextV1 {
    WorkflowCommandContextV1::new(id::<WorkCommandId>(command), digest('a'), UtcMicros(time))
}

fn definition() -> WorkflowDefinitionV1 {
    WorkflowDefinitionV1::new(
        id::<WorkflowDefinitionId>("workflow.release"),
        7,
        id::<ProjectId>("project.example"),
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

fn active_definition() -> WorkflowDefinitionProjectionV1 {
    let gated = BTreeSet::from([id::<WorkflowStepId>("step.deploy")]);
    let registered = WorkflowDefinitionProjectionV1::register(
        definition(),
        gated,
        context("command.register", 1),
    )
    .unwrap();
    let validated = registered
        .handle(
            WorkflowDefinitionCommandV1::Validate,
            context("command.validate", 2),
        )
        .unwrap();
    let active = validated
        .handle(
            WorkflowDefinitionCommandV1::Activate,
            context("command.activate", 3),
        )
        .unwrap();
    assert_eq!(active.status(), WorkflowDefinitionStatusV1::Active);
    active
}

#[test]
fn definition_versions_follow_the_immutable_lifecycle() {
    let active = active_definition();
    assert_eq!(active.aggregate_version().get(), 3);
    assert_eq!(active.definition().definition_version(), 7);
    assert_eq!(
        active
            .handle(
                WorkflowDefinitionCommandV1::Validate,
                context("command.revalidate", 4),
            )
            .unwrap_err(),
        WorkflowStateError::InvalidTransition
    );

    let retired = active
        .handle(
            WorkflowDefinitionCommandV1::Retire,
            context("command.retire", 5),
        )
        .unwrap();
    assert_eq!(retired.status(), WorkflowDefinitionStatusV1::Retired);
    assert_eq!(retired.aggregate_version().get(), 4);
    assert_eq!(retired.history().len(), 4);
    assert!(retired.history().iter().all(|event| {
        event.command_id().as_str().starts_with("command.") && event.input_digest() == &digest('a')
    }));
}

#[test]
fn aggregate_versions_reject_zero_on_every_input_path() {
    assert_eq!(
        WorkflowAggregateVersionV1::new(0).unwrap_err(),
        WorkflowStateError::InvalidAggregateVersion
    );
    assert!(serde_json::from_str::<WorkflowAggregateVersionV1>("0").is_err());
}

#[test]
fn dag_and_approval_gate_release_steps_in_order() {
    let run = WorkflowRunProjectionV1::start(
        id::<RunId>("run.release"),
        &active_definition(),
        context("command.start", 4),
    )
    .unwrap();
    assert_eq!(
        run.step_status(&id::<WorkflowStepId>("step.build")),
        Some(WorkflowStepStatusV1::Ready)
    );
    assert_eq!(
        run.step_status(&id::<WorkflowStepId>("step.deploy")),
        Some(WorkflowStepStatusV1::Blocked)
    );

    let run = run
        .handle(
            WorkflowRunCommandV1::StartStep {
                step_id: id("step.build"),
            },
            context("command.start-build", 5),
        )
        .unwrap()
        .handle(
            WorkflowRunCommandV1::CompleteStep {
                step_id: id("step.build"),
            },
            context("command.complete-build", 6),
        )
        .unwrap();
    assert_eq!(
        run.step_status(&id::<WorkflowStepId>("step.deploy")),
        Some(WorkflowStepStatusV1::AwaitingApproval)
    );
    assert_eq!(
        run.handle(
            WorkflowRunCommandV1::RecordApproval {
                step_id: id("step.deploy"),
            },
            context("command.approve-too-soon", 7),
        )
        .unwrap_err(),
        WorkflowStateError::ApprovalNotRequested
    );

    let run = run
        .handle(
            WorkflowRunCommandV1::RequestApproval {
                step_id: id("step.deploy"),
            },
            context("command.request-approval", 8),
        )
        .unwrap()
        .handle(
            WorkflowRunCommandV1::RecordApproval {
                step_id: id("step.deploy"),
            },
            context("command.record-approval", 9),
        )
        .unwrap();
    assert_eq!(
        run.step_status(&id::<WorkflowStepId>("step.deploy")),
        Some(WorkflowStepStatusV1::Ready)
    );
}

#[test]
fn approval_denial_requires_a_request_and_fails_the_run_with_distinct_evidence() {
    let run = WorkflowRunProjectionV1::start(
        id::<RunId>("run.denied"),
        &active_definition(),
        context("command.start-denied", 4),
    )
    .unwrap()
    .handle(
        WorkflowRunCommandV1::StartStep {
            step_id: id("step.build"),
        },
        context("command.start-build-denied", 5),
    )
    .unwrap()
    .handle(
        WorkflowRunCommandV1::CompleteStep {
            step_id: id("step.build"),
        },
        context("command.complete-build-denied", 6),
    )
    .unwrap();
    assert_eq!(
        run.handle(
            WorkflowRunCommandV1::RecordDenial {
                step_id: id("step.deploy"),
            },
            context("command.deny-too-soon", 7),
        )
        .unwrap_err(),
        WorkflowStateError::ApprovalNotRequested
    );

    let denied = run
        .handle(
            WorkflowRunCommandV1::RequestApproval {
                step_id: id("step.deploy"),
            },
            context("command.request-before-denial", 8),
        )
        .unwrap()
        .handle(
            WorkflowRunCommandV1::RecordDenial {
                step_id: id("step.deploy"),
            },
            context("command.record-denial", 9),
        )
        .unwrap();
    assert_eq!(denied.status(), WorkflowRunStatusV1::Failed);
    assert_eq!(
        denied.step_status(&id::<WorkflowStepId>("step.deploy")),
        Some(WorkflowStepStatusV1::Failed)
    );
    assert!(matches!(
        denied.history().last().map(|event| event.event()),
        Some(WorkflowRunEventKindV1::DenialRecorded { .. })
    ));
}

#[test]
fn pause_cancel_restart_and_terminal_states_are_fenced() {
    let run = WorkflowRunProjectionV1::start(
        id::<RunId>("run.release"),
        &active_definition(),
        context("command.start", 4),
    )
    .unwrap()
    .handle(
        WorkflowRunCommandV1::StartStep {
            step_id: id("step.build"),
        },
        context("command.start-build", 5),
    )
    .unwrap()
    .handle(WorkflowRunCommandV1::Pause, context("command.pause", 6))
    .unwrap();
    assert_eq!(run.status(), WorkflowRunStatusV1::Paused);

    let resumed = run
        .handle(WorkflowRunCommandV1::Resume, context("command.resume", 7))
        .unwrap();
    let restarted = resumed
        .handle(WorkflowRunCommandV1::Restart, context("command.restart", 8))
        .unwrap();
    assert_eq!(restarted.authority_epoch(), 2);
    assert_eq!(
        restarted.step_status(&id::<WorkflowStepId>("step.build")),
        Some(WorkflowStepStatusV1::ReconcileRequired)
    );

    let cancelling = restarted
        .handle(
            WorkflowRunCommandV1::RequestCancellation,
            context("command.cancel", 9),
        )
        .unwrap();
    assert_eq!(cancelling.status(), WorkflowRunStatusV1::Cancelling);
    assert_eq!(
        cancelling
            .handle(
                WorkflowRunCommandV1::StartStep {
                    step_id: id("step.deploy"),
                },
                context("command.fenced", 10),
            )
            .unwrap_err(),
        WorkflowStateError::InvalidTransition
    );
    let cancelled = cancelling
        .handle(
            WorkflowRunCommandV1::ReconcileStepCancelled {
                step_id: id("step.build"),
            },
            context("command.reconcile", 11),
        )
        .unwrap();
    assert_eq!(cancelled.status(), WorkflowRunStatusV1::Cancelled);
    assert!(cancelled.is_terminal());
    assert!(cancelled.history().iter().all(|event| {
        !event.command_id().as_str().is_empty() && event.input_digest() == &digest('a')
    }));
}

#[test]
fn successful_last_step_makes_the_run_terminal() {
    let run = WorkflowRunProjectionV1::start(
        id::<RunId>("run.success"),
        &WorkflowDefinitionProjectionV1::register(
            WorkflowDefinitionV1::new(
                id("workflow.single"),
                1,
                id("project.example"),
                vec![WorkflowStepV1 {
                    step_id: id("step.only"),
                    operation: id("operation.only"),
                    predecessors: BTreeSet::new(),
                    inputs: Vec::new(),
                    outputs: Vec::new(),
                    fan_out: None,
                }],
                digest('b'),
                digest('c'),
                digest('d'),
            )
            .unwrap(),
            BTreeSet::new(),
            context("command.register-single", 1),
        )
        .unwrap()
        .handle(
            WorkflowDefinitionCommandV1::Validate,
            context("command.validate-single", 2),
        )
        .unwrap()
        .handle(
            WorkflowDefinitionCommandV1::Activate,
            context("command.activate-single", 3),
        )
        .unwrap(),
        context("command.start-single", 4),
    )
    .unwrap()
    .handle(
        WorkflowRunCommandV1::StartStep {
            step_id: id("step.only"),
        },
        context("command.start-only", 5),
    )
    .unwrap()
    .handle(
        WorkflowRunCommandV1::CompleteStep {
            step_id: id("step.only"),
        },
        context("command.complete-only", 6),
    )
    .unwrap();
    assert_eq!(run.status(), WorkflowRunStatusV1::Succeeded);
    assert!(run.is_terminal());
}
