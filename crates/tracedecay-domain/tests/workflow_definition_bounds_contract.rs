use serde_json::json;
use tracedecay_domain::{
    MAX_WORKFLOW_FAN_OUT, MAX_WORKFLOW_INPUTS, MAX_WORKFLOW_OUTPUTS, MAX_WORKFLOW_PREDECESSORS,
    MAX_WORKFLOW_STEPS, ManifestDigest, ProjectId, WorkflowDefinitionError, WorkflowDefinitionV1,
    WorkflowFanOutV1, WorkflowOutputReferenceV1, WorkflowStepV1,
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
    predecessors: &[String],
    inputs: Vec<WorkflowOutputReferenceV1>,
    outputs: &[String],
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

fn definition(steps: Vec<WorkflowStepV1>) -> Result<WorkflowDefinitionV1, WorkflowDefinitionError> {
    WorkflowDefinitionV1::new(
        id("workflow.definition.bounds"),
        1,
        id::<ProjectId>("project.workflow.bounds"),
        steps,
        digest('a'),
        digest('b'),
        digest('c'),
    )
}

/// Builds `count` zero-predecessor producer steps plus one consumer that names
/// each of them as a predecessor, so only the fan-in count can be at fault.
fn fan_in(count: usize) -> Vec<WorkflowStepV1> {
    let producers = names("producer", count);
    let mut steps = producers
        .iter()
        .map(|producer| step(producer, &[], vec![], &[]))
        .collect::<Vec<_>>();
    steps.push(step("consumer", &producers, vec![], &[]));
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
    definition(vec![step("prepare", &[], vec![], &at_maximum)]).unwrap();

    let beyond = names("out", MAX_WORKFLOW_OUTPUTS + 1);
    let error = definition(vec![step("prepare", &[], vec![], &beyond)]).unwrap_err();
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
        step("bulk", &[], vec![], &bulk),
        step("extra", &[], vec![], &["tail".to_owned()]),
        step(
            "consumer",
            &["bulk".to_owned(), "extra".to_owned()],
            inputs,
            &[],
        ),
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
        step("prepare", &[], vec![], &["context".to_owned()]),
        step(
            "review",
            &["prepare".to_owned()],
            vec![output("prepare", "context"), output("prepare", "context")],
            &["finding".to_owned()],
        ),
    ])
    .unwrap_err();

    assert!(matches!(
        error,
        WorkflowDefinitionError::DuplicateInput { .. }
    ));
}

#[test]
fn recursive_step_dispatch_is_rejected_rather_than_diverging() {
    let self_dispatch = definition(vec![step(
        "loop",
        &["loop".to_owned()],
        vec![],
        &["result".to_owned()],
    )])
    .unwrap_err();
    assert!(matches!(
        self_dispatch,
        WorkflowDefinitionError::PredecessorCycle
    ));

    let indirect = definition(vec![
        step("first", &["third".to_owned()], vec![], &[]),
        step("second", &["first".to_owned()], vec![], &[]),
        step("third", &["second".to_owned()], vec![], &[]),
    ])
    .unwrap_err();
    assert!(matches!(
        indirect,
        WorkflowDefinitionError::PredecessorCycle
    ));
}

#[test]
fn fan_out_is_accepted_across_the_whole_declared_range() {
    for max_parallel in [1, MAX_WORKFLOW_FAN_OUT] {
        let mut fan_out = step("review", &[], vec![], &["finding".to_owned()]);
        fan_out.fan_out = Some(WorkflowFanOutV1 { max_parallel });
        definition(vec![fan_out]).unwrap();
    }
}

#[test]
fn step_count_is_accepted_at_the_declared_maximum() {
    let steps = names("step", MAX_WORKFLOW_STEPS)
        .iter()
        .map(|step_id| step(step_id, &[], vec![], &[]))
        .collect::<Vec<_>>();

    definition(steps).unwrap();
}

#[test]
fn wire_definitions_reject_bound_violations_after_deserialization() {
    let valid = definition(vec![step("prepare", &[], vec![], &["context".to_owned()])]).unwrap();

    let mut unbounded_fan_out = serde_json::to_value(&valid).unwrap();
    unbounded_fan_out["steps"][0]["fan_out"] = json!({ "max_parallel": MAX_WORKFLOW_FAN_OUT + 1 });
    assert!(serde_json::from_value::<WorkflowDefinitionV1>(unbounded_fan_out).is_err());

    let mut unbounded_fan_in = serde_json::to_value(&valid).unwrap();
    unbounded_fan_in["steps"][0]["predecessors"] =
        json!(names("producer", MAX_WORKFLOW_PREDECESSORS + 1));
    assert!(serde_json::from_value::<WorkflowDefinitionV1>(unbounded_fan_in).is_err());
}
