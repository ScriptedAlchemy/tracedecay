use std::collections::BTreeSet;

use serde_json::json;
use tracedecay_domain::{
    MAX_WORKFLOW_FAN_OUT, MAX_WORKFLOW_STEPS, ManifestDigest, ProjectId, WorkflowDefinitionError,
    WorkflowDefinitionId, WorkflowDefinitionV1, WorkflowFanOutV1, WorkflowOperationRef,
    WorkflowOutputName, WorkflowOutputReferenceV1, WorkflowStepId, WorkflowStepV1,
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

    let steps = (0..=MAX_WORKFLOW_STEPS)
        .map(|ordinal| step(&format!("step-{ordinal}"), &[], vec![], &[]))
        .collect();
    assert!(matches!(
        definition(steps),
        Err(WorkflowDefinitionError::InvalidStepCount { .. })
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

#[test]
fn predecessor_cycle_is_rejected() {
    let error = definition(vec![
        step("first", &["second"], vec![], &["first_output"]),
        step("second", &["first"], vec![], &["second_output"]),
    ])
    .unwrap_err();

    assert!(matches!(error, WorkflowDefinitionError::PredecessorCycle));
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
fn zero_and_unbounded_fan_out_are_rejected() {
    for max_width in [0, MAX_WORKFLOW_FAN_OUT + 1] {
        let mut fan_out = step("review", &[], vec![], &["finding"]);
        fan_out.fan_out = Some(WorkflowFanOutV1 { max_width });
        let error = definition(vec![fan_out]).unwrap_err();
        assert!(matches!(
            error,
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

#[test]
fn unknown_json_fields_are_rejected() {
    let mut value =
        serde_json::to_value(definition(vec![step("prepare", &[], vec![], &["context"])]).unwrap())
            .unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("scheduler".to_owned(), json!("must not exist"));

    assert!(serde_json::from_value::<WorkflowDefinitionV1>(value).is_err());
}

#[test]
fn invalid_wire_definitions_are_rejected_during_deserialization() {
    let mut value =
        serde_json::to_value(definition(vec![step("prepare", &[], vec![], &["context"])]).unwrap())
            .unwrap();
    value["definition_version"] = json!(0);

    assert!(serde_json::from_value::<WorkflowDefinitionV1>(value).is_err());
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
