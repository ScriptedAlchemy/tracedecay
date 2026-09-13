use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{WorkArtifactRefV1, WorkAttemptIdentityV1, WorkflowOutputName};

use super::WorkflowRunStateError;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowOutputArtifact {
    attempt_identity: WorkAttemptIdentityV1,
    artifact: WorkArtifactRefV1,
}

impl WorkflowOutputArtifact {
    pub const fn new(attempt_identity: WorkAttemptIdentityV1, artifact: WorkArtifactRefV1) -> Self {
        Self {
            attempt_identity,
            artifact,
        }
    }

    pub fn attempt_identity(&self) -> &WorkAttemptIdentityV1 {
        &self.attempt_identity
    }

    pub fn artifact(&self) -> &WorkArtifactRefV1 {
        &self.artifact
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStepOutput {
    output_name: WorkflowOutputName,
    artifacts: Vec<WorkflowOutputArtifact>,
}

impl WorkflowStepOutput {
    pub fn new(
        output_name: WorkflowOutputName,
        mut artifacts: Vec<WorkflowOutputArtifact>,
    ) -> Result<Self, WorkflowRunStateError> {
        artifacts.sort_by(|left, right| left.attempt_identity.cmp(&right.attempt_identity));
        let attempt_count = artifacts
            .iter()
            .map(|artifact| artifact.attempt_identity())
            .collect::<BTreeSet<_>>()
            .len();
        let artifact_count = artifacts
            .iter()
            .map(|artifact| artifact.artifact().artifact_id())
            .collect::<BTreeSet<_>>()
            .len();
        if artifacts.is_empty()
            || attempt_count != artifacts.len()
            || artifact_count != artifacts.len()
        {
            return Err(WorkflowRunStateError::InvalidStepOutputs);
        }
        Ok(Self {
            output_name,
            artifacts,
        })
    }

    pub fn output_name(&self) -> &WorkflowOutputName {
        &self.output_name
    }

    pub fn artifacts(&self) -> &[WorkflowOutputArtifact] {
        &self.artifacts
    }

    pub(super) fn validate(&self) -> Result<(), WorkflowRunStateError> {
        if Self::new(self.output_name.clone(), self.artifacts.clone())? != *self {
            return Err(WorkflowRunStateError::InvalidStepOutputs);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for WorkflowStepOutput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            output_name: WorkflowOutputName,
            artifacts: Vec<WorkflowOutputArtifact>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.output_name, wire.artifacts).map_err(serde::de::Error::custom)
    }
}
