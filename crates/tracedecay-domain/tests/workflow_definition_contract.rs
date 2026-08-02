use std::collections::BTreeSet;

use serde_json::json;
use tracedecay_domain::configuration::safe_work_topology_policy_v1;
use tracedecay_domain::{
    MAX_WORKFLOW_FAN_OUT, MAX_WORKFLOW_INPUTS, MAX_WORKFLOW_OUTPUTS, MAX_WORKFLOW_PREDECESSORS,
    MAX_WORKFLOW_STEPS, ManifestDigest, ProjectId, ProviderId, RunId, UtcMicros, WorkArtifactId,
    WorkArtifactRefV1, WorkCommandId, WorkProviderBackendV1, WorkProviderRouteId,
    WorkProviderRouteV1, WorkflowDefinitionError, WorkflowDefinitionId, WorkflowDefinitionV1,
    WorkflowFanOutV1, WorkflowOperationRef, WorkflowOutputName, WorkflowOutputReferenceV1,
    WorkflowPlacementReceiptV1, WorkflowRunCommandV1, WorkflowRunEventContextV1,
    WorkflowRunEventV1, WorkflowRunProjectionV1, WorkflowRunStateError, WorkflowRunStatusV1,
    WorkflowStepEffectOutcomeV1, WorkflowStepEffectReceiptV1, WorkflowStepId, WorkflowStepOutputV1,
    WorkflowStepV1,
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
    inputs: Vec<WorkflowOutputReferenceV1>,
    outputs: &[&str],
) -> WorkflowStepV1 {
    WorkflowStepV1 {
        step_id: id(step_id),
        operation: id(&format!("operation.{step_id}.v1")),
        predecessors: predecessors.iter().map(|value| id(value)).collect(),
        inputs,
        outputs: outputs.iter().map(|value| id(value)).collect(),
        fan_out: None,
    }
}

fn output(producer_step_id: &str, output_name: &str) -> WorkflowOutputReferenceV1 {
    WorkflowOutputReferenceV1 {
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

fn definition(steps: Vec<WorkflowStepV1>) -> Result<WorkflowDefinitionV1, WorkflowDefinitionError> {
    WorkflowDefinitionV1::new(
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
fn fan_in(count: usize) -> Vec<WorkflowStepV1> {
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
        fan_out.fan_out = Some(WorkflowFanOutV1 { max_width });
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
    assert!(serde_json::from_value::<WorkflowDefinitionV1>(unknown_field).is_err());

    let mut zero_version = valid.clone();
    zero_version["definition_version"] = json!(0);
    assert!(serde_json::from_value::<WorkflowDefinitionV1>(zero_version).is_err());

    let mut unbounded_fan_out = valid.clone();
    unbounded_fan_out["steps"][0]["fan_out"] = json!({ "max_width": MAX_WORKFLOW_FAN_OUT + 1 });
    assert!(serde_json::from_value::<WorkflowDefinitionV1>(unbounded_fan_out).is_err());

    let mut unbounded_fan_in = valid;
    unbounded_fan_in["steps"][0]["predecessors"] =
        json!(names("producer", MAX_WORKFLOW_PREDECESSORS + 1));
    assert!(serde_json::from_value::<WorkflowDefinitionV1>(unbounded_fan_in).is_err());
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

fn run_context(command: &str, byte: char, occurred_at: i64) -> WorkflowRunEventContextV1 {
    WorkflowRunEventContextV1 {
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
) -> WorkflowPlacementReceiptV1 {
    WorkflowPlacementReceiptV1::new(
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

#[test]
fn run_projection_releases_a_dependent_with_the_exact_predecessor_artifact() {
    let definition = definition(vec![
        step("prepare", &[], vec![], &["context"]),
        step(
            "review",
            &["prepare"],
            vec![output("prepare", "context")],
            &["finding"],
        ),
    ])
    .unwrap();
    let admitted = WorkflowRunEventV1::admitted(
        id::<RunId>("run.workflow.dataflow"),
        definition,
        digest('d'),
        digest('8'),
        run_context("workflow.admit", 'e', 1),
    )
    .unwrap();
    let mut run = WorkflowRunProjectionV1::rebuild(&[admitted]).unwrap();
    assert_eq!(run.ready_steps(), vec![id::<WorkflowStepId>("prepare")]);

    let started = run
        .next_event(
            WorkflowRunCommandV1::StartStep {
                step_id: id("prepare"),
                placement: placement("run.workflow.dataflow", "prepare", 'b', 'd', '8'),
            },
            run_context("workflow.prepare.start", 'f', 2),
        )
        .unwrap();
    run = run.apply(&started).unwrap();

    let artifact =
        WorkArtifactRefV1::new(id::<WorkArtifactId>("artifact.context"), digest('1'), 42).unwrap();
    let completed = run
        .next_event(
            WorkflowRunCommandV1::CompleteStep {
                step_id: id("prepare"),
                outputs: vec![WorkflowStepOutputV1 {
                    output_name: id("context"),
                    artifact: artifact.clone(),
                }],
                effect_receipt: WorkflowStepEffectReceiptV1::new(
                    id::<RunId>("run.workflow.dataflow"),
                    id::<WorkflowStepId>("prepare"),
                    placement("run.workflow.dataflow", "prepare", 'b', 'd', '8')
                        .placement_digest()
                        .clone(),
                    WorkflowStepEffectOutcomeV1::Completed,
                    digest('9'),
                    &[WorkflowStepOutputV1 {
                        output_name: id("context"),
                        artifact: artifact.clone(),
                    }],
                )
                .unwrap(),
            },
            run_context("workflow.prepare.complete", '2', 3),
        )
        .unwrap();
    run = run.apply(&completed).unwrap();

    assert_eq!(run.ready_steps(), vec![id::<WorkflowStepId>("review")]);
    assert_eq!(run.resolved_inputs(&id("review")).unwrap(), vec![artifact]);
    assert_eq!(run.status(), WorkflowRunStatusV1::Running);
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
    let admitted = WorkflowRunEventV1::admitted(
        id::<RunId>("run.workflow.invalid-output"),
        definition,
        digest('d'),
        digest('8'),
        run_context("workflow.invalid.admit", '3', 1),
    )
    .unwrap();
    let mut run = WorkflowRunProjectionV1::rebuild(&[admitted]).unwrap();
    run = run
        .apply(
            &run.next_event(
                WorkflowRunCommandV1::StartStep {
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

    let error = run
        .next_event(
            WorkflowRunCommandV1::CompleteStep {
                step_id: id("prepare"),
                outputs: vec![WorkflowStepOutputV1 {
                    output_name: id("undeclared"),
                    artifact: wrong.clone(),
                }],
                effect_receipt: WorkflowStepEffectReceiptV1::new(
                    id::<RunId>("run.workflow.invalid-output"),
                    id::<WorkflowStepId>("prepare"),
                    placement("run.workflow.invalid-output", "prepare", 'b', 'd', '8')
                        .placement_digest()
                        .clone(),
                    WorkflowStepEffectOutcomeV1::Completed,
                    digest('9'),
                    &[WorkflowStepOutputV1 {
                        output_name: id("undeclared"),
                        artifact: wrong,
                    }],
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
    let admitted = WorkflowRunEventV1::admitted(
        run_id.clone(),
        definition(vec![step("prepare", &[], vec![], &["context"])]).unwrap(),
        topology_digest.clone(),
        registry_digest.clone(),
        run_context("workflow.receipts.admit", 'e', 1),
    )
    .unwrap();
    let run = WorkflowRunProjectionV1::rebuild(&[admitted]).unwrap();
    let placement = WorkflowPlacementReceiptV1::new(
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
            WorkflowRunCommandV1::StartStep {
                step_id: step_id.clone(),
                placement: placement.clone(),
            },
            run_context("workflow.receipts.start", 'f', 2),
        )
        .unwrap();
    let run = run.apply(&started).unwrap();
    let outputs = vec![WorkflowStepOutputV1 {
        output_name: id("context"),
        artifact: WorkArtifactRefV1::new(
            id::<WorkArtifactId>("artifact.workflow.receipts"),
            digest('1'),
            42,
        )
        .unwrap(),
    }];
    let effect = WorkflowStepEffectReceiptV1::new(
        run_id,
        step_id.clone(),
        placement.placement_digest().clone(),
        WorkflowStepEffectOutcomeV1::Completed,
        digest('2'),
        &outputs,
    )
    .unwrap();
    let completed = run
        .next_event(
            WorkflowRunCommandV1::CompleteStep {
                step_id: step_id.clone(),
                outputs,
                effect_receipt: effect.clone(),
            },
            run_context("workflow.receipts.complete", '3', 3),
        )
        .unwrap();
    let rebuilt =
        WorkflowRunProjectionV1::rebuild(&[run.history()[0].clone(), started, completed]).unwrap();

    let step = rebuilt.step(&step_id).unwrap();
    assert_eq!(step.placement_receipt(), Some(&placement));
    assert_eq!(step.effect_receipt(), Some(&effect));
}

#[test]
fn run_projection_rejects_receipts_bound_to_other_runtime_state() {
    let run_id = id::<RunId>("run.workflow.receipt-binding");
    let step_id = id::<WorkflowStepId>("prepare");
    let admitted = WorkflowRunEventV1::admitted(
        run_id.clone(),
        definition(vec![step("prepare", &[], vec![], &[])]).unwrap(),
        digest('d'),
        digest('8'),
        run_context("workflow.receipt-binding.admit", 'e', 1),
    )
    .unwrap();
    let run = WorkflowRunProjectionV1::rebuild(&[admitted]).unwrap();
    let stale_placement = WorkflowPlacementReceiptV1::new(
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
            WorkflowRunCommandV1::StartStep {
                step_id,
                placement: stale_placement,
            },
            run_context("workflow.receipt-binding.start", 'f', 2),
        )
        .unwrap_err(),
        WorkflowRunStateError::InvalidPlacementReceipt
    );
}
