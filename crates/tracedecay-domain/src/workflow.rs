//! Declarative, dependency-neutral workflow definition contracts.

use std::collections::{BTreeMap, BTreeSet};

use petgraph::algo::is_cyclic_directed;
use petgraph::graph::DiGraph;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{
    ManifestDigest, ProjectId, WorkflowDefinitionId, WorkflowOperationRef, WorkflowOutputName,
    WorkflowStepId,
};

pub const MAX_WORKFLOW_STEPS: usize = 1_024;
pub const MAX_WORKFLOW_PREDECESSORS: usize = 256;
pub const MAX_WORKFLOW_INPUTS: usize = 256;
pub const MAX_WORKFLOW_OUTPUTS: usize = 256;
pub const MAX_WORKFLOW_FAN_OUT: u32 = 256;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkflowDefinitionError {
    #[error("workflow definition version must be non-zero")]
    InvalidDefinitionVersion,
    #[error("workflow step count {count} is outside 1..={max}")]
    InvalidStepCount { count: usize, max: usize },
    #[error("workflow step identity is duplicated: {step_id}")]
    DuplicateStepId { step_id: WorkflowStepId },
    #[error("workflow step {step_id} has too many predecessors")]
    TooManyPredecessors { step_id: WorkflowStepId },
    #[error("workflow step {step_id} references missing predecessor {predecessor}")]
    DanglingPredecessor {
        step_id: WorkflowStepId,
        predecessor: WorkflowStepId,
    },
    #[error("workflow predecessor graph contains a cycle")]
    PredecessorCycle,
    #[error("workflow step {step_id} has too many inputs")]
    TooManyInputs { step_id: WorkflowStepId },
    #[error("workflow step {step_id} repeats an input reference")]
    DuplicateInput { step_id: WorkflowStepId },
    #[error(
        "workflow step {step_id} references unknown output {output_name} from {producer_step_id}"
    )]
    UnknownProducerOutput {
        step_id: WorkflowStepId,
        producer_step_id: WorkflowStepId,
        output_name: WorkflowOutputName,
    },
    #[error("workflow step {step_id} consumes output from non-predecessor {producer_step_id}")]
    OutputProducerNotPredecessor {
        step_id: WorkflowStepId,
        producer_step_id: WorkflowStepId,
    },
    #[error("workflow step {step_id} has too many outputs")]
    TooManyOutputs { step_id: WorkflowStepId },
    #[error("workflow step {step_id} repeats output {output_name}")]
    DuplicateOutputName {
        step_id: WorkflowStepId,
        output_name: WorkflowOutputName,
    },
    #[error("workflow step {step_id} has invalid fan-out width {max_width}")]
    InvalidFanOut {
        step_id: WorkflowStepId,
        max_width: u32,
    },
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFanOutV1 {
    pub max_width: u32,
}

#[derive(
    Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(deny_unknown_fields)]
pub struct WorkflowOutputReferenceV1 {
    pub producer_step_id: WorkflowStepId,
    pub output_name: WorkflowOutputName,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStepV1 {
    pub step_id: WorkflowStepId,
    pub operation: WorkflowOperationRef,
    pub predecessors: BTreeSet<WorkflowStepId>,
    pub inputs: Vec<WorkflowOutputReferenceV1>,
    pub outputs: Vec<WorkflowOutputName>,
    pub fan_out: Option<WorkflowFanOutV1>,
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionV1 {
    definition_id: WorkflowDefinitionId,
    definition_version: u64,
    project_id: ProjectId,
    steps: Vec<WorkflowStepV1>,
    pinned_policy_digest: ManifestDigest,
    pinned_configuration_digest: ManifestDigest,
    pinned_catalog_digest: ManifestDigest,
}

impl WorkflowDefinitionV1 {
    pub fn new(
        definition_id: WorkflowDefinitionId,
        definition_version: u64,
        project_id: ProjectId,
        steps: Vec<WorkflowStepV1>,
        pinned_policy_digest: ManifestDigest,
        pinned_configuration_digest: ManifestDigest,
        pinned_catalog_digest: ManifestDigest,
    ) -> Result<Self, WorkflowDefinitionError> {
        let definition = Self {
            definition_id,
            definition_version,
            project_id,
            steps,
            pinned_policy_digest,
            pinned_configuration_digest,
            pinned_catalog_digest,
        };
        definition.validate()?;
        Ok(definition)
    }

    pub fn definition_id(&self) -> &WorkflowDefinitionId {
        &self.definition_id
    }

    pub const fn definition_version(&self) -> u64 {
        self.definition_version
    }

    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub fn steps(&self) -> &[WorkflowStepV1] {
        &self.steps
    }

    pub fn pinned_policy_digest(&self) -> &ManifestDigest {
        &self.pinned_policy_digest
    }

    pub fn pinned_configuration_digest(&self) -> &ManifestDigest {
        &self.pinned_configuration_digest
    }

    pub fn pinned_catalog_digest(&self) -> &ManifestDigest {
        &self.pinned_catalog_digest
    }

    pub fn validate(&self) -> Result<(), WorkflowDefinitionError> {
        if self.definition_version == 0 {
            return Err(WorkflowDefinitionError::InvalidDefinitionVersion);
        }
        if self.steps.is_empty() || self.steps.len() > MAX_WORKFLOW_STEPS {
            return Err(WorkflowDefinitionError::InvalidStepCount {
                count: self.steps.len(),
                max: MAX_WORKFLOW_STEPS,
            });
        }

        let mut steps = BTreeMap::new();
        for step in &self.steps {
            if steps.insert(step.step_id.clone(), step).is_some() {
                return Err(WorkflowDefinitionError::DuplicateStepId {
                    step_id: step.step_id.clone(),
                });
            }
        }

        for step in &self.steps {
            self.validate_step(step, &steps)?;
        }
        self.validate_acyclic(&steps)
    }

    fn validate_step(
        &self,
        step: &WorkflowStepV1,
        steps: &BTreeMap<WorkflowStepId, &WorkflowStepV1>,
    ) -> Result<(), WorkflowDefinitionError> {
        if step.predecessors.len() > MAX_WORKFLOW_PREDECESSORS {
            return Err(WorkflowDefinitionError::TooManyPredecessors {
                step_id: step.step_id.clone(),
            });
        }
        for predecessor in &step.predecessors {
            if !steps.contains_key(predecessor) {
                return Err(WorkflowDefinitionError::DanglingPredecessor {
                    step_id: step.step_id.clone(),
                    predecessor: predecessor.clone(),
                });
            }
        }

        if step.inputs.len() > MAX_WORKFLOW_INPUTS {
            return Err(WorkflowDefinitionError::TooManyInputs {
                step_id: step.step_id.clone(),
            });
        }
        let mut inputs = BTreeSet::new();
        for input in &step.inputs {
            if !inputs.insert(input.clone()) {
                return Err(WorkflowDefinitionError::DuplicateInput {
                    step_id: step.step_id.clone(),
                });
            }
            let producer = steps.get(&input.producer_step_id).ok_or_else(|| {
                WorkflowDefinitionError::UnknownProducerOutput {
                    step_id: step.step_id.clone(),
                    producer_step_id: input.producer_step_id.clone(),
                    output_name: input.output_name.clone(),
                }
            })?;
            if !producer.outputs.contains(&input.output_name) {
                return Err(WorkflowDefinitionError::UnknownProducerOutput {
                    step_id: step.step_id.clone(),
                    producer_step_id: input.producer_step_id.clone(),
                    output_name: input.output_name.clone(),
                });
            }
            if !step.predecessors.contains(&input.producer_step_id) {
                return Err(WorkflowDefinitionError::OutputProducerNotPredecessor {
                    step_id: step.step_id.clone(),
                    producer_step_id: input.producer_step_id.clone(),
                });
            }
        }

        if step.outputs.len() > MAX_WORKFLOW_OUTPUTS {
            return Err(WorkflowDefinitionError::TooManyOutputs {
                step_id: step.step_id.clone(),
            });
        }
        let mut outputs = BTreeSet::new();
        for output_name in &step.outputs {
            if !outputs.insert(output_name) {
                return Err(WorkflowDefinitionError::DuplicateOutputName {
                    step_id: step.step_id.clone(),
                    output_name: output_name.clone(),
                });
            }
        }

        if let Some(fan_out) = step.fan_out
            && !(1..=MAX_WORKFLOW_FAN_OUT).contains(&fan_out.max_width)
        {
            return Err(WorkflowDefinitionError::InvalidFanOut {
                step_id: step.step_id.clone(),
                max_width: fan_out.max_width,
            });
        }
        Ok(())
    }

    fn validate_acyclic(
        &self,
        steps: &BTreeMap<WorkflowStepId, &WorkflowStepV1>,
    ) -> Result<(), WorkflowDefinitionError> {
        let mut graph = DiGraph::<&WorkflowStepId, ()>::new();
        let nodes = steps
            .keys()
            .map(|step_id| (step_id, graph.add_node(step_id)))
            .collect::<BTreeMap<_, _>>();
        for (step_id, step) in steps {
            let Some(&target) = nodes.get(step_id) else {
                return Err(WorkflowDefinitionError::PredecessorCycle);
            };
            for predecessor in &step.predecessors {
                let Some(&source) = nodes.get(predecessor) else {
                    return Err(WorkflowDefinitionError::DanglingPredecessor {
                        step_id: step_id.clone(),
                        predecessor: predecessor.clone(),
                    });
                };
                graph.add_edge(source, target, ());
            }
        }
        if is_cyclic_directed(&graph) {
            return Err(WorkflowDefinitionError::PredecessorCycle);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for WorkflowDefinitionV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            definition_id: WorkflowDefinitionId,
            definition_version: u64,
            project_id: ProjectId,
            steps: Vec<WorkflowStepV1>,
            pinned_policy_digest: ManifestDigest,
            pinned_configuration_digest: ManifestDigest,
            pinned_catalog_digest: ManifestDigest,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.definition_id,
            wire.definition_version,
            wire.project_id,
            wire.steps,
            wire.pinned_policy_digest,
            wire.pinned_configuration_digest,
            wire.pinned_catalog_digest,
        )
        .map_err(serde::de::Error::custom)
    }
}
