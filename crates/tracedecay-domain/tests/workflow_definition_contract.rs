use std::collections::BTreeSet;

use serde_json::json;
use tracedecay_domain::configuration::safe_work_topology_policy_v1;
use tracedecay_domain::{
    AttemptId, MAX_WORKFLOW_FAN_OUT, MAX_WORKFLOW_INPUTS, MAX_WORKFLOW_OUTPUTS,
    MAX_WORKFLOW_PREDECESSORS, MAX_WORKFLOW_STEPS, ManifestDigest, ProjectId, ProviderId, RunId,
    TaskId, UtcMicros, WorkArtifactId, WorkArtifactRefV1, WorkAttemptIdentityV1, WorkCommandId,
    WorkProviderBackendV1, WorkProviderRouteId, WorkProviderRouteV1, WorkflowDefinition,
    WorkflowDefinitionError, WorkflowDefinitionId, WorkflowFanOut, WorkflowOperationRef,
    WorkflowOutputArtifact, WorkflowOutputName, WorkflowOutputReference, WorkflowPlacementReceipt,
    WorkflowRunCommand, WorkflowRunEvent, WorkflowRunEventContext, WorkflowRunProjection,
    WorkflowRunStateError, WorkflowRunStatus, WorkflowStep, WorkflowStepEffectOutcome,
    WorkflowStepEffectReceipt, WorkflowStepId, WorkflowStepOutput, WorkflowStepStatus,
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

fn step(
    step_id: &str,
    predecessors: &[&str],
    inputs: Vec<WorkflowOutputReference>,
    outputs: &[&str],
) -> WorkflowStep {
    WorkflowStep {
        step_id: id(step_id),
        operation: id(&format!("operation.{step_id}.v1")),
        predecessors: predecessors.iter().map(|value| id(value)).collect(),
        inputs,
        outputs: outputs.iter().map(|value| id(value)).collect(),
        fan_out: None,
    }
}

fn output(producer_step_id: &str, output_name: &str) -> WorkflowOutputReference {
    WorkflowOutputReference {
        producer_step_id: id(producer_step_id),
        output_name: id(output_name),
    }
}

fn names(prefix: &str, count: usize) -> Vec<String> {
    (0..count)
        .map(|ordinal| format!("{prefix}-{ordinal}"))
        .collect()
}

fn borrowed(values: &[String]) -> Vec<&str> {
    values.iter().map(String::as_str).collect()
}

fn definition(steps: Vec<WorkflowStep>) -> Result<WorkflowDefinition, WorkflowDefinitionError> {
    WorkflowDefinition::new(
        id("workflow.definition.fixture"),
        1,
        id::<ProjectId>("project.workflow.fixture"),
        steps,
        digest('a'),
        digest('b'),
        digest('c'),
    )
}

#[test]
fn valid_two_step_definition_accepts_declared_predecessor_output() {
    let prepare = step("prepare", &[], vec![], &["context"]);
    let review = step(
        "review",
        &["prepare"],
        vec![output("prepare", "context")],
        &["finding"],
    );

    definition(vec![prepare, review]).unwrap();
}

#[test]
fn duplicate_step_ids_are_rejected() {
    let error = definition(vec![
        step("prepare", &[], vec![], &["first"]),
        step("prepare", &[], vec![], &["second"]),
    ])
    .unwrap_err();

    assert!(matches!(
        error,
        WorkflowDefinitionError::DuplicateStepId { .. }
    ));
}

#[test]
fn workflow_step_count_is_bounded() {
    assert!(matches!(
        definition(Vec::new()),
        Err(WorkflowDefinitionError::InvalidStepCount { .. })
    ));

    let at_maximum = names("step", MAX_WORKFLOW_STEPS)
        .iter()
        .map(|step_id| step(step_id, &[], vec![], &[]))
        .collect();
    definition(at_maximum).unwrap();

    let steps = (0..=MAX_WORKFLOW_STEPS)
        .map(|ordinal| step(&format!("step-{ordinal}"), &[], vec![], &[]))
        .collect();
    assert!(matches!(
        definition(steps),
        Err(WorkflowDefinitionError::InvalidStepCount { .. })
    ));
}

/// Builds `count` zero-predecessor producer steps plus one consumer that names
/// each of them as a predecessor, so only the fan-in count can be at fault.
fn fan_in(count: usize) -> Vec<WorkflowStep> {
    let producers = names("producer", count);
    let mut steps = producers
        .iter()
        .map(|producer| step(producer, &[], vec![], &[]))
        .collect::<Vec<_>>();
    steps.push(step("consumer", &borrowed(&producers), vec![], &[]));
    steps
}

#[test]
fn fan_in_is_accepted_at_the_declared_maximum_and_rejected_beyond_it() {
    definition(fan_in(MAX_WORKFLOW_PREDECESSORS)).unwrap();

    let error = definition(fan_in(MAX_WORKFLOW_PREDECESSORS + 1)).unwrap_err();
    assert!(matches!(
        error,
        WorkflowDefinitionError::TooManyPredecessors { .. }
    ));
}

#[test]
fn declared_outputs_are_accepted_at_the_maximum_and_rejected_beyond_it() {
    let at_maximum = names("out", MAX_WORKFLOW_OUTPUTS);
    definition(vec![step("prepare", &[], vec![], &borrowed(&at_maximum))]).unwrap();

    let beyond = names("out", MAX_WORKFLOW_OUTPUTS + 1);
    let error = definition(vec![step("prepare", &[], vec![], &borrowed(&beyond))]).unwrap_err();
    assert!(matches!(
        error,
        WorkflowDefinitionError::TooManyOutputs { .. }
    ));
}

#[test]
fn consumed_inputs_are_rejected_beyond_the_maximum_even_when_every_reference_resolves() {
    let bulk = names("out", MAX_WORKFLOW_OUTPUTS);
    let mut inputs = bulk
        .iter()
        .map(|output_name| output("bulk", output_name))
        .collect::<Vec<_>>();
    inputs.push(output("extra", "tail"));
    assert_eq!(inputs.len(), MAX_WORKFLOW_INPUTS + 1);

    let error = definition(vec![
        step("bulk", &[], vec![], &borrowed(&bulk)),
        step("extra", &[], vec![], &["tail"]),
        step("consumer", &["bulk", "extra"], inputs, &[]),
    ])
    .unwrap_err();

    assert!(matches!(
        error,
        WorkflowDefinitionError::TooManyInputs { .. }
    ));
}

#[test]
fn a_repeated_resolvable_input_reference_is_rejected() {
    let error = definition(vec![
        step("prepare", &[], vec![], &["context"]),
        step(
            "review",
            &["prepare"],
            vec![output("prepare", "context"), output("prepare", "context")],
            &["finding"],
        ),
    ])
    .unwrap_err();

    assert!(matches!(
        error,
        WorkflowDefinitionError::DuplicateInput { .. }
    ));
}

#[test]
fn dangling_predecessor_is_rejected() {
    let error = definition(vec![step("review", &["missing"], vec![], &["finding"])]).unwrap_err();

    assert!(matches!(
        error,
        WorkflowDefinitionError::DanglingPredecessor { .. }
    ));
}

/// Recursive dispatch must be rejected rather than diverging, whether the step
/// names itself or reaches itself through other steps.
#[test]
fn predecessor_cycle_is_rejected() {
    let self_dispatch = definition(vec![step("loop", &["loop"], vec![], &["result"])]).unwrap_err();
    assert!(matches!(
        self_dispatch,
        WorkflowDefinitionError::PredecessorCycle
    ));

    let two_step = definition(vec![
        step("first", &["second"], vec![], &["first_output"]),
        step("second", &["first"], vec![], &["second_output"]),
    ])
    .unwrap_err();
    assert!(matches!(
        two_step,
        WorkflowDefinitionError::PredecessorCycle
    ));

    let indirect = definition(vec![
        step("first", &["third"], vec![], &[]),
        step("second", &["first"], vec![], &[]),
        step("third", &["second"], vec![], &[]),
    ])
    .unwrap_err();
    assert!(matches!(
        indirect,
        WorkflowDefinitionError::PredecessorCycle
    ));
}

#[test]
fn invalid_output_reference_is_rejected() {
    let error = definition(vec![
        step("prepare", &[], vec![], &["context"]),
        step(
            "review",
            &["prepare"],
            vec![output("prepare", "missing_output")],
            &["finding"],
        ),
    ])
    .unwrap_err();

    assert!(matches!(
        error,
        WorkflowDefinitionError::UnknownProducerOutput { .. }
    ));
}

#[test]
fn output_reference_from_non_predecessor_is_rejected() {
    let error = definition(vec![
        step("unrelated", &[], vec![], &["context"]),
        step(
            "review",
            &[],
            vec![output("unrelated", "context")],
            &["finding"],
        ),
    ])
    .unwrap_err();

    assert!(matches!(
        error,
        WorkflowDefinitionError::OutputProducerNotPredecessor { .. }
    ));
}

#[test]
fn fan_out_is_accepted_across_the_declared_range_and_rejected_outside_it() {
    let with_fan_out = |max_width| {
        let mut fan_out = step("review", &[], vec![], &["finding"]);
        fan_out.fan_out = Some(WorkflowFanOut { max_width });
        definition(vec![fan_out])
    };

    for max_width in [1, MAX_WORKFLOW_FAN_OUT] {
        with_fan_out(max_width).unwrap();
    }
    for max_width in [0, MAX_WORKFLOW_FAN_OUT + 1] {
        assert!(matches!(
            with_fan_out(max_width).unwrap_err(),
            WorkflowDefinitionError::InvalidFanOut { .. }
        ));
    }
}

#[test]
fn duplicate_output_names_are_rejected() {
    let error =
        definition(vec![step("prepare", &[], vec![], &["context", "context"])]).unwrap_err();

    assert!(matches!(
        error,
        WorkflowDefinitionError::DuplicateOutputName { .. }
    ));
}

/// Deserialization is a second construction path; it must reapply every
/// invariant the constructor enforces rather than trusting the wire.
#[test]
fn wire_definitions_are_revalidated_during_deserialization() {
    let valid =
        serde_json::to_value(definition(vec![step("prepare", &[], vec![], &["context"])]).unwrap())
            .unwrap();

    let mut unknown_field = valid.clone();
    unknown_field
        .as_object_mut()
        .unwrap()
        .insert("scheduler".to_owned(), json!("must not exist"));
    assert!(serde_json::from_value::<WorkflowDefinition>(unknown_field).is_err());

    let mut zero_version = valid.clone();
    zero_version["definition_version"] = json!(0);
    assert!(serde_json::from_value::<WorkflowDefinition>(zero_version).is_err());

    let mut unbounded_fan_out = valid.clone();
    unbounded_fan_out["steps"][0]["fan_out"] = json!({ "max_width": MAX_WORKFLOW_FAN_OUT + 1 });
    assert!(serde_json::from_value::<WorkflowDefinition>(unbounded_fan_out).is_err());

    let mut unbounded_fan_in = valid;
    unbounded_fan_in["steps"][0]["predecessors"] =
        json!(names("producer", MAX_WORKFLOW_PREDECESSORS + 1));
    assert!(serde_json::from_value::<WorkflowDefinition>(unbounded_fan_in).is_err());
}

#[test]
fn identities_are_canonical_product_data_strings() {
    for invalid in ["", " leading", "trailing ", "line\nbreak"] {
        assert!(WorkflowDefinitionId::new(invalid).is_err());
        assert!(WorkflowStepId::new(invalid).is_err());
        assert!(WorkflowOutputName::new(invalid).is_err());
        assert!(WorkflowOperationRef::new(invalid).is_err());
    }

    let unique = BTreeSet::from([id::<WorkflowStepId>("prepare"), id("review")]);
    assert_eq!(unique.len(), 2);
}

fn run_context(command: &str, byte: char, occurred_at: i64) -> WorkflowRunEventContext {
    WorkflowRunEventContext {
        command_id: id::<WorkCommandId>(command),
        input_digest: digest(byte),
        occurred_at: UtcMicros(occurred_at),
    }
}

fn placement(
    run_id: &str,
    step_id: &str,
    configuration: char,
    topology: char,
    registry: char,
) -> WorkflowPlacementReceipt {
    WorkflowPlacementReceipt::new(
        id::<RunId>(run_id),
        id::<WorkflowStepId>(step_id),
        WorkProviderRouteV1::new(
            id::<ProviderId>("provider.workflow.test"),
            id::<WorkProviderRouteId>("route.workflow.test.v1"),
        )
        .unwrap(),
        WorkProviderBackendV1::CodexAppServer,
        "model.workflow.test".to_owned(),
        digest(configuration),
        digest(topology),
        digest(registry),
        safe_work_topology_policy_v1().placement,
    )
    .unwrap()
}

fn attempt(child: &str) -> WorkAttemptIdentityV1 {
    WorkAttemptIdentityV1::new(
        id::<TaskId>(&format!("task.workflow.{child}")),
        id::<RunId>(&format!("run.work.{child}")),
        id::<AttemptId>(&format!("attempt.workflow.{child}")),
    )
    .unwrap()
}

fn step_output(output_name: &str, child: &str, artifact: WorkArtifactRefV1) -> WorkflowStepOutput {
    WorkflowStepOutput::new(
        id(output_name),
        vec![WorkflowOutputArtifact::new(attempt(child), artifact)],
    )
    .unwrap()
}

#[test]
fn named_output_artifact_sets_are_non_empty_unique_and_canonical() {
    let artifact = |child: &str, byte: char| {
        WorkflowOutputArtifact::new(
            attempt(child),
            WorkArtifactRefV1::new(
                id::<WorkArtifactId>(&format!("artifact.workflow.{child}")),
                digest(byte),
                1,
            )
            .unwrap(),
        )
    };
    assert_eq!(
        WorkflowStepOutput::new(id("finding"), Vec::new()).unwrap_err(),
        WorkflowRunStateError::InvalidStepOutputs
    );
    let duplicate = artifact("duplicate", '1');
    assert_eq!(
        WorkflowStepOutput::new(id("finding"), vec![duplicate.clone(), duplicate]).unwrap_err(),
        WorkflowRunStateError::InvalidStepOutputs
    );
    let output = WorkflowStepOutput::new(
        id("finding"),
        vec![artifact("zeta", '2'), artifact("alpha", '3')],
    )
    .unwrap();

    assert_eq!(
        output
            .artifacts()
            .iter()
            .map(|artifact| artifact.attempt_identity().task_id().as_str())
            .collect::<Vec<_>>(),
        vec!["task.workflow.alpha", "task.workflow.zeta"]
    );
}

#[test]
fn run_projection_releases_a_dependent_with_the_exact_predecessor_artifact() {
    let mut prepare = step("prepare", &[], vec![], &["context"]);
    prepare.fan_out = Some(WorkflowFanOut { max_width: 2 });
    let definition = definition(vec![
        prepare,
        step(
            "review",
            &["prepare"],
            vec![output("prepare", "context")],
            &["finding"],
        ),
    ])
    .unwrap();
    let admitted = WorkflowRunEvent::admitted(
        id::<RunId>("run.workflow.dataflow"),
        definition,
        digest('d'),
        digest('8'),
        run_context("workflow.admit", 'e', 1),
    )
    .unwrap();
    let mut run = WorkflowRunProjection::rebuild(&[admitted]).unwrap();
    assert_eq!(
        run.step(&id("prepare")).unwrap().status(),
        WorkflowStepStatus::Ready
    );
    assert_eq!(
        run.step(&id("review")).unwrap().status(),
        WorkflowStepStatus::Blocked
    );

    let started = run
        .next_event(
            WorkflowRunCommand::StartStep {
                step_id: id("prepare"),
                placement: placement("run.workflow.dataflow", "prepare", 'b', 'd', '8'),
            },
            run_context("workflow.prepare.start", 'f', 2),
        )
        .unwrap();
    run = run.apply(&started).unwrap();

    let artifact =
        WorkArtifactRefV1::new(id::<WorkArtifactId>("artifact.context"), digest('1'), 42).unwrap();
    let second_artifact = WorkArtifactRefV1::new(
        id::<WorkArtifactId>("artifact.context.second"),
        digest('2'),
        7,
    )
    .unwrap();
    let completed_output = WorkflowStepOutput::new(
        id("context"),
        vec![
            WorkflowOutputArtifact::new(attempt("prepare-b"), second_artifact.clone()),
            WorkflowOutputArtifact::new(attempt("prepare-a"), artifact.clone()),
        ],
    )
    .unwrap();
    let completed = run
        .next_event(
            WorkflowRunCommand::CompleteStep {
                step_id: id("prepare"),
                outputs: vec![completed_output.clone()],
                effect_receipt: WorkflowStepEffectReceipt::new(
                    id::<RunId>("run.workflow.dataflow"),
                    id::<WorkflowStepId>("prepare"),
                    placement("run.workflow.dataflow", "prepare", 'b', 'd', '8')
                        .placement_digest()
                        .clone(),
                    WorkflowStepEffectOutcome::Completed,
                    digest('9'),
                    &[completed_output],
                )
                .unwrap(),
            },
            run_context("workflow.prepare.complete", '2', 3),
        )
        .unwrap();
    run = run.apply(&completed).unwrap();

    assert_eq!(
        run.step(&id("review")).unwrap().status(),
        WorkflowStepStatus::Ready
    );
    let recorded = run
        .step(&id("prepare"))
        .unwrap()
        .outputs()
        .get(&id::<WorkflowOutputName>("context"))
        .unwrap();
    assert_eq!(recorded.artifacts()[0].artifact(), &artifact);
    assert_eq!(recorded.artifacts()[1].artifact(), &second_artifact);
    assert_eq!(run.status(), WorkflowRunStatus::Running);
}

#[test]
fn run_projection_rejects_a_digest_only_or_misnamed_output() {
    let definition = definition(vec![
        step("prepare", &[], vec![], &["context"]),
        step(
            "review",
            &["prepare"],
            vec![output("prepare", "context")],
            &[],
        ),
    ])
    .unwrap();
    let admitted = WorkflowRunEvent::admitted(
        id::<RunId>("run.workflow.invalid-output"),
        definition,
        digest('d'),
        digest('8'),
        run_context("workflow.invalid.admit", '3', 1),
    )
    .unwrap();
    let mut run = WorkflowRunProjection::rebuild(&[admitted]).unwrap();
    run = run
        .apply(
            &run.next_event(
                WorkflowRunCommand::StartStep {
                    step_id: id("prepare"),
                    placement: placement("run.workflow.invalid-output", "prepare", 'b', 'd', '8'),
                },
                run_context("workflow.invalid.start", '4', 2),
            )
            .unwrap(),
        )
        .unwrap();
    let wrong =
        WorkArtifactRefV1::new(id::<WorkArtifactId>("artifact.wrong"), digest('5'), 1).unwrap();
    let wrong_output = step_output("undeclared", "prepare", wrong);

    let error = run
        .next_event(
            WorkflowRunCommand::CompleteStep {
                step_id: id("prepare"),
                outputs: vec![wrong_output.clone()],
                effect_receipt: WorkflowStepEffectReceipt::new(
                    id::<RunId>("run.workflow.invalid-output"),
                    id::<WorkflowStepId>("prepare"),
                    placement("run.workflow.invalid-output", "prepare", 'b', 'd', '8')
                        .placement_digest()
                        .clone(),
                    WorkflowStepEffectOutcome::Completed,
                    digest('9'),
                    &[wrong_output],
                )
                .unwrap(),
            },
            run_context("workflow.invalid.complete", '6', 3),
        )
        .unwrap_err();

    assert_eq!(
        error,
        tracedecay_domain::WorkflowRunStateError::InvalidStepOutputs
    );
}

#[test]
fn run_projection_journals_bound_placement_and_effect_receipts() {
    let run_id = id::<RunId>("run.workflow.receipts");
    let step_id = id::<WorkflowStepId>("prepare");
    let configuration_digest = digest('b');
    let topology_digest = digest('d');
    let registry_digest = digest('8');
    let admitted = WorkflowRunEvent::admitted(
        run_id.clone(),
        definition(vec![step("prepare", &[], vec![], &["context"])]).unwrap(),
        topology_digest.clone(),
        registry_digest.clone(),
        run_context("workflow.receipts.admit", 'e', 1),
    )
    .unwrap();
    let run = WorkflowRunProjection::rebuild(&[admitted]).unwrap();
    let placement = WorkflowPlacementReceipt::new(
        run_id.clone(),
        step_id.clone(),
        WorkProviderRouteV1::new(
            id::<ProviderId>("provider.workflow.test"),
            id::<WorkProviderRouteId>("route.workflow.test.v1"),
        )
        .unwrap(),
        WorkProviderBackendV1::CodexAppServer,
        "model.workflow.test".to_owned(),
        configuration_digest,
        topology_digest,
        registry_digest,
        safe_work_topology_policy_v1().placement,
    )
    .unwrap();
    let started = run
        .next_event(
            WorkflowRunCommand::StartStep {
                step_id: step_id.clone(),
                placement: placement.clone(),
            },
            run_context("workflow.receipts.start", 'f', 2),
        )
        .unwrap();
    let run = run.apply(&started).unwrap();
    let outputs = vec![step_output(
        "context",
        "receipt",
        WorkArtifactRefV1::new(
            id::<WorkArtifactId>("artifact.workflow.receipts"),
            digest('1'),
            42,
        )
        .unwrap(),
    )];
    let effect = WorkflowStepEffectReceipt::new(
        run_id,
        step_id.clone(),
        placement.placement_digest().clone(),
        WorkflowStepEffectOutcome::Completed,
        digest('2'),
        &outputs,
    )
    .unwrap();
    let completed = run
        .next_event(
            WorkflowRunCommand::CompleteStep {
                step_id: step_id.clone(),
                outputs,
                effect_receipt: effect.clone(),
            },
            run_context("workflow.receipts.complete", '3', 3),
        )
        .unwrap();
    let rebuilt =
        WorkflowRunProjection::rebuild(&[run.history()[0].clone(), started, completed]).unwrap();

    let step = rebuilt.step(&step_id).unwrap();
    assert_eq!(step.placement_receipt(), Some(&placement));
    assert_eq!(step.effect_receipt(), Some(&effect));
}

#[test]
fn run_projection_rejects_receipts_bound_to_other_runtime_state() {
    let run_id = id::<RunId>("run.workflow.receipt-binding");
    let step_id = id::<WorkflowStepId>("prepare");
    let admitted = WorkflowRunEvent::admitted(
        run_id.clone(),
        definition(vec![step("prepare", &[], vec![], &[])]).unwrap(),
        digest('d'),
        digest('8'),
        run_context("workflow.receipt-binding.admit", 'e', 1),
    )
    .unwrap();
    let run = WorkflowRunProjection::rebuild(&[admitted]).unwrap();
    let stale_placement = WorkflowPlacementReceipt::new(
        run_id,
        step_id.clone(),
        WorkProviderRouteV1::new(
            id::<ProviderId>("provider.workflow.test"),
            id::<WorkProviderRouteId>("route.workflow.test.v1"),
        )
        .unwrap(),
        WorkProviderBackendV1::CodexAppServer,
        "model.workflow.test".to_owned(),
        digest('9'),
        digest('d'),
        digest('8'),
        safe_work_topology_policy_v1().placement,
    )
    .unwrap();

    assert_eq!(
        run.next_event(
            WorkflowRunCommand::StartStep {
                step_id,
                placement: stale_placement,
            },
            run_context("workflow.receipt-binding.start", 'f', 2),
        )
        .unwrap_err(),
        WorkflowRunStateError::InvalidPlacementReceipt
    );
}

#[test]
fn fan_out_rejects_a_declared_output_missing_one_child_artifact() {
    let run_id = id::<RunId>("run.workflow.missing-child-output");
    let step_id = id::<WorkflowStepId>("prepare");
    let mut fan_out_step = step("prepare", &[], vec![], &["analysis", "evidence"]);
    fan_out_step.fan_out = Some(WorkflowFanOut { max_width: 2 });
    let admitted = WorkflowRunEvent::admitted(
        run_id.clone(),
        definition(vec![fan_out_step]).unwrap(),
        digest('d'),
        digest('8'),
        run_context("workflow.missing-child.admit", 'e', 1),
    )
    .unwrap();
    let run = WorkflowRunProjection::rebuild(&[admitted]).unwrap();
    let placement = placement(
        "run.workflow.missing-child-output",
        "prepare",
        'b',
        'd',
        '8',
    );
    let started = run
        .next_event(
            WorkflowRunCommand::StartStep {
                step_id: step_id.clone(),
                placement: placement.clone(),
            },
            run_context("workflow.missing-child.start", 'f', 2),
        )
        .unwrap();
    let run = run.apply(&started).unwrap();
    let child_artifact = |child: &str, artifact_name: &str, byte: char| {
        WorkflowOutputArtifact::new(
            attempt(child),
            WorkArtifactRefV1::new(id::<WorkArtifactId>(artifact_name), digest(byte), 1).unwrap(),
        )
    };
    let outputs = vec![
        WorkflowStepOutput::new(
            id("analysis"),
            vec![
                child_artifact("alpha", "artifact.analysis.alpha", '1'),
                child_artifact("beta", "artifact.analysis.beta", '2'),
            ],
        )
        .unwrap(),
        WorkflowStepOutput::new(
            id("evidence"),
            vec![child_artifact("alpha", "artifact.evidence.alpha", '3')],
        )
        .unwrap(),
    ];
    let effect = WorkflowStepEffectReceipt::new(
        run_id,
        step_id.clone(),
        placement.placement_digest().clone(),
        WorkflowStepEffectOutcome::Completed,
        digest('4'),
        &outputs,
    )
    .unwrap();

    assert_eq!(
        run.next_event(
            WorkflowRunCommand::CompleteStep {
                step_id,
                outputs,
                effect_receipt: effect,
            },
            run_context("workflow.missing-child.complete", '5', 3),
        )
        .unwrap_err(),
        WorkflowRunStateError::InvalidStepOutputs
    );
}
