use tracedecay_application::{
    CancellationContext, WorkflowFailurePolicyV1, WorkflowFanOutCheckpointV1,
    WorkflowFanOutInputV1, WorkflowFanOutRequestV1, WorkflowFanOutRuntimeError,
    WorkflowProviderAdmissionV1, prepare_workflow_fan_out, validate_workflow_checkpoint,
};
use tracedecay_domain::{
    AttemptId, CommitId, ManifestDigest, ProjectId, ProviderId, RunId, UtcMicros,
    WorkEffectStateV1, WorkExecutionBudgetV1, WorkFenceEpochV1, WorkLeaseFenceV1, WorkLeaseId,
    WorkProviderBackendV1, WorkProviderRouteId, WorkProviderRouteV1, WorkflowDefinitionV1,
    WorkflowFanOutV1, WorkflowOperationRef, WorkflowOutputName, WorkflowStepId, WorkflowStepV1,
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

fn request(inputs: &[&str], max_width: u32, max_parallel: u32) -> WorkflowFanOutRequestV1 {
    let definition = WorkflowDefinitionV1::new(
        id("workflow.definition.runtime"),
        1,
        id::<ProjectId>("project.workflow.runtime"),
        vec![WorkflowStepV1 {
            step_id: id::<WorkflowStepId>("fan-out"),
            operation: id::<WorkflowOperationRef>("operation.work.attempt_start"),
            predecessors: Default::default(),
            inputs: Vec::new(),
            outputs: vec![id::<WorkflowOutputName>("finding")],
            fan_out: Some(WorkflowFanOutV1 { max_width }),
        }],
        digest('a'),
        digest('b'),
        digest('c'),
    )
    .unwrap();
    WorkflowFanOutRequestV1 {
        definition,
        run_id: id::<RunId>("run.workflow.runtime"),
        step_id: id::<WorkflowStepId>("fan-out"),
        fence: tracedecay_application::WorkflowExecutionFenceV1 {
            attempt_id: id::<AttemptId>("attempt.workflow.runtime"),
            lease: WorkLeaseFenceV1::new(
                id::<WorkLeaseId>("lease.workflow.runtime"),
                WorkFenceEpochV1::new(1).unwrap(),
            )
            .unwrap(),
        },
        admitted_at: UtcMicros(100),
        cancellation: CancellationContext::active("cancel.workflow.runtime").unwrap(),
        max_parallel,
        failure_policy: WorkflowFailurePolicyV1::Collect,
        provider: WorkflowProviderAdmissionV1 {
            route: WorkProviderRouteV1::new(
                id::<ProviderId>("provider.work.codex-app-server"),
                id::<WorkProviderRouteId>("route.work.codex-app-server.v1"),
            )
            .unwrap(),
            backend: WorkProviderBackendV1::CodexAppServer,
            model: "gpt-test".to_owned(),
            configuration_digest: digest('b'),
            reference: None,
            commit: id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
            deadline: UtcMicros(1_000),
            cancellation_generation: 1,
            budget: WorkExecutionBudgetV1::new(16_384, 16_384, 65_536).unwrap(),
            effect_state: WorkEffectStateV1::Observational,
        },
        inputs: inputs
            .iter()
            .enumerate()
            .map(|(index, identity)| WorkflowFanOutInputV1 {
                identity: (*identity).to_owned(),
                input_digest: digest(char::from(b'1' + u8::try_from(index).unwrap())),
            })
            .collect(),
    }
}

#[test]
fn planner_separates_fan_out_width_from_parallelism() {
    let plan = prepare_workflow_fan_out(&request(&["c", "a", "b"], 4, 2)).unwrap();

    assert_eq!(plan.max_parallel, 2);
    assert_eq!(plan.children.len(), 3);
    assert_eq!(
        plan.children
            .iter()
            .map(|child| child.input.identity.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "c"]
    );
    assert!(
        plan.children
            .iter()
            .all(|child| child.task_id.as_str().starts_with("workflow-child:"))
    );
}

#[test]
fn planner_rejects_width_parallelism_and_duplicate_violations() {
    assert_eq!(
        prepare_workflow_fan_out(&request(&["a", "b"], 1, 1)).unwrap_err(),
        WorkflowFanOutRuntimeError::FanOutLimitExceeded {
            limit: 1,
            actual: 2,
        }
    );
    assert_eq!(
        prepare_workflow_fan_out(&request(&["a", "b"], 2, 3)).unwrap_err(),
        WorkflowFanOutRuntimeError::InvalidParallelism
    );
    assert_eq!(
        prepare_workflow_fan_out(&request(&["same", "same"], 2, 1)).unwrap_err(),
        WorkflowFanOutRuntimeError::DuplicateChildIdentity("same".to_owned())
    );
}

#[test]
fn provider_admission_is_part_of_the_immutable_plan() {
    let first = prepare_workflow_fan_out(&request(&["a"], 1, 1)).unwrap();
    let mut changed = request(&["a"], 1, 1);
    changed.provider.model = "different-model".to_owned();
    let changed = prepare_workflow_fan_out(&changed).unwrap();

    assert_ne!(first.plan_digest, changed.plan_digest);
    assert_ne!(
        first.children[0].proposal_digest,
        changed.children[0].proposal_digest
    );
}

#[test]
fn child_attempt_identity_survives_workflow_fence_renewal() {
    let first = prepare_workflow_fan_out(&request(&["a", "b"], 2, 1)).unwrap();
    let mut retried = request(&["a", "b"], 2, 1);
    retried.fence.attempt_id = id::<AttemptId>("attempt.workflow.runtime.retry");
    retried.fence.lease = WorkLeaseFenceV1::new(
        id::<WorkLeaseId>("lease.workflow.runtime.retry"),
        WorkFenceEpochV1::new(2).unwrap(),
    )
    .unwrap();
    let retried = prepare_workflow_fan_out(&retried).unwrap();

    assert_eq!(first.plan_digest, retried.plan_digest);
    assert_eq!(
        first
            .children
            .iter()
            .map(|child| (&child.task_id, &child.attempt_identity))
            .collect::<Vec<_>>(),
        retried
            .children
            .iter()
            .map(|child| (&child.task_id, &child.attempt_identity))
            .collect::<Vec<_>>()
    );
}

#[test]
fn checkpoint_round_trip_preserves_child_fence_and_terminal_receipt() {
    let plan = prepare_workflow_fan_out(&request(&["a"], 1, 1)).unwrap();
    let child = &plan.children[0];
    let checkpoint_value = serde_json::json!({
        "plan_digest": plan.plan_digest,
        "children": [{
            "task_id": child.task_id,
            "attempt_identity": child.attempt_identity,
            "lease": {
                "lease_id": "lease.workflow.runtime.child",
                "epoch": 1
            },
            "receipt": {
                "observation_digest": digest('d'),
                "terminal_receipt_digest": digest('e')
            }
        }]
    });
    let checkpoint: WorkflowFanOutCheckpointV1 =
        serde_json::from_value(checkpoint_value.clone()).expect("durable checkpoint");

    validate_workflow_checkpoint(&plan, &checkpoint).unwrap();
    assert_eq!(serde_json::to_value(checkpoint).unwrap(), checkpoint_value);
}
