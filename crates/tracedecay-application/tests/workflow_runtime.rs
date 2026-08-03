use tracedecay_application::{
    CancellationContext, WorkflowFailurePolicy, WorkflowFanOutInput, WorkflowFanOutRequest,
    WorkflowFanOutRuntimeError, WorkflowProviderAdmission, prepare_workflow_fan_out,
};
use tracedecay_domain::configuration::safe_work_topology_policy_v1;
use tracedecay_domain::{
    AttemptId, CommitId, ManifestDigest, ProjectId, ProviderId, RunId, UtcMicros,
    WorkEffectStateV1, WorkExecutionBudgetV1, WorkFenceEpochV1, WorkLeaseFenceV1, WorkLeaseId,
    WorkProviderBackendV1, WorkProviderRouteId, WorkProviderRouteV1, WorkflowDefinition,
    WorkflowFanOut, WorkflowOperationRef, WorkflowOutputName, WorkflowStepId, WorkflowStep,
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

fn request(inputs: &[&str], max_width: u32, max_parallel: u32) -> WorkflowFanOutRequest {
    let definition = WorkflowDefinition::new(
        id("workflow.definition.runtime"),
        1,
        id::<ProjectId>("project.workflow.runtime"),
        vec![WorkflowStep {
            step_id: id::<WorkflowStepId>("fan-out"),
            operation: id::<WorkflowOperationRef>("operation.work.attempt_start"),
            predecessors: Default::default(),
            inputs: Vec::new(),
            outputs: vec![id::<WorkflowOutputName>("finding")],
            fan_out: Some(WorkflowFanOut { max_width }),
        }],
        digest('a'),
        digest('b'),
        digest('c'),
    )
    .unwrap();
    WorkflowFanOutRequest {
        definition,
        run_id: id::<RunId>("run.workflow.runtime"),
        step_id: id::<WorkflowStepId>("fan-out"),
        fence: tracedecay_application::WorkflowExecutionFence {
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
        failure_policy: WorkflowFailurePolicy::Collect,
        provider: WorkflowProviderAdmission {
            route: WorkProviderRouteV1::new(
                id::<ProviderId>("provider.work.codex-app-server"),
                id::<WorkProviderRouteId>("route.work.codex-app-server.v1"),
            )
            .unwrap(),
            backend: WorkProviderBackendV1::CodexAppServer,
            model: "gpt-test".to_owned(),
            configuration_digest: digest('b'),
            topology_digest: digest('d'),
            provider_registry_digest: digest('e'),
            worktree_placement: safe_work_topology_policy_v1().placement,
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
            .map(|(index, identity)| WorkflowFanOutInput {
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
