//! Durable provider placement and step effect receipts for Workflow runs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::configuration::WorktreePlacementModeV1;
use crate::{
    ManifestDigest, RunId, WorkProviderBackendV1, WorkProviderRouteV1, WorkflowStepId,
    WorkflowStepOutputV1, canonical_sha256,
};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkflowReceiptError {
    #[error("workflow placement receipt is invalid")]
    InvalidPlacement,
    #[error("workflow step effect receipt is invalid")]
    InvalidEffect,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPlacementReceiptV1 {
    run_id: RunId,
    step_id: WorkflowStepId,
    route: WorkProviderRouteV1,
    backend: WorkProviderBackendV1,
    model: String,
    configuration_digest: ManifestDigest,
    topology_digest: ManifestDigest,
    provider_registry_digest: ManifestDigest,
    worktree_placement: WorktreePlacementModeV1,
    placement_digest: ManifestDigest,
}

impl WorkflowPlacementReceiptV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        run_id: RunId,
        step_id: WorkflowStepId,
        route: WorkProviderRouteV1,
        backend: WorkProviderBackendV1,
        model: String,
        configuration_digest: ManifestDigest,
        topology_digest: ManifestDigest,
        provider_registry_digest: ManifestDigest,
        worktree_placement: WorktreePlacementModeV1,
    ) -> Result<Self, WorkflowReceiptError> {
        if !valid_model(&model) {
            return Err(WorkflowReceiptError::InvalidPlacement);
        }
        let placement_digest = placement_digest(
            &run_id,
            &step_id,
            &route,
            backend,
            &model,
            &configuration_digest,
            &topology_digest,
            &provider_registry_digest,
            &worktree_placement,
        )?;
        Ok(Self {
            run_id,
            step_id,
            route,
            backend,
            model,
            configuration_digest,
            topology_digest,
            provider_registry_digest,
            worktree_placement,
            placement_digest,
        })
    }

    pub fn validate(&self) -> Result<(), WorkflowReceiptError> {
        if !valid_model(&self.model)
            || self.placement_digest
                != placement_digest(
                    &self.run_id,
                    &self.step_id,
                    &self.route,
                    self.backend,
                    &self.model,
                    &self.configuration_digest,
                    &self.topology_digest,
                    &self.provider_registry_digest,
                    &self.worktree_placement,
                )?
        {
            return Err(WorkflowReceiptError::InvalidPlacement);
        }
        Ok(())
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn step_id(&self) -> &WorkflowStepId {
        &self.step_id
    }

    pub fn route(&self) -> &WorkProviderRouteV1 {
        &self.route
    }

    pub const fn backend(&self) -> WorkProviderBackendV1 {
        self.backend
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn configuration_digest(&self) -> &ManifestDigest {
        &self.configuration_digest
    }

    pub fn topology_digest(&self) -> &ManifestDigest {
        &self.topology_digest
    }

    pub fn provider_registry_digest(&self) -> &ManifestDigest {
        &self.provider_registry_digest
    }

    pub fn worktree_placement(&self) -> &WorktreePlacementModeV1 {
        &self.worktree_placement
    }

    pub fn placement_digest(&self) -> &ManifestDigest {
        &self.placement_digest
    }
}

fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 256
        && model.trim() == model
        && !model.chars().any(char::is_control)
}

#[allow(clippy::too_many_arguments)]
fn placement_digest(
    run_id: &RunId,
    step_id: &WorkflowStepId,
    route: &WorkProviderRouteV1,
    backend: WorkProviderBackendV1,
    model: &str,
    configuration_digest: &ManifestDigest,
    topology_digest: &ManifestDigest,
    provider_registry_digest: &ManifestDigest,
    worktree_placement: &WorktreePlacementModeV1,
) -> Result<ManifestDigest, WorkflowReceiptError> {
    canonical_sha256(&(
        "tracedecay.domain.workflow-placement.v1",
        run_id,
        step_id,
        route,
        backend,
        model,
        configuration_digest,
        topology_digest,
        provider_registry_digest,
        worktree_placement,
    ))
    .map_err(|_| WorkflowReceiptError::InvalidPlacement)
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStepEffectOutcomeV1 {
    Completed,
    Failed,
    Cancelled,
    TimedOut,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStepEffectReceiptV1 {
    run_id: RunId,
    step_id: WorkflowStepId,
    placement_digest: ManifestDigest,
    outcome: WorkflowStepEffectOutcomeV1,
    effect_digest: ManifestDigest,
    output_set_digest: ManifestDigest,
    receipt_digest: ManifestDigest,
}

impl WorkflowStepEffectReceiptV1 {
    pub fn new(
        run_id: RunId,
        step_id: WorkflowStepId,
        placement_digest: ManifestDigest,
        outcome: WorkflowStepEffectOutcomeV1,
        effect_digest: ManifestDigest,
        outputs: &[WorkflowStepOutputV1],
    ) -> Result<Self, WorkflowReceiptError> {
        let output_set_digest = output_set_digest(outputs)?;
        let receipt_digest = effect_receipt_digest(
            &run_id,
            &step_id,
            &placement_digest,
            outcome,
            &effect_digest,
            &output_set_digest,
        )?;
        Ok(Self {
            run_id,
            step_id,
            placement_digest,
            outcome,
            effect_digest,
            output_set_digest,
            receipt_digest,
        })
    }

    pub fn validate(&self) -> Result<(), WorkflowReceiptError> {
        if self.receipt_digest
            != effect_receipt_digest(
                &self.run_id,
                &self.step_id,
                &self.placement_digest,
                self.outcome,
                &self.effect_digest,
                &self.output_set_digest,
            )?
        {
            return Err(WorkflowReceiptError::InvalidEffect);
        }
        Ok(())
    }

    pub fn validate_outputs(
        &self,
        outputs: &[WorkflowStepOutputV1],
    ) -> Result<(), WorkflowReceiptError> {
        self.validate()?;
        if self.output_set_digest != output_set_digest(outputs)? {
            return Err(WorkflowReceiptError::InvalidEffect);
        }
        Ok(())
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn step_id(&self) -> &WorkflowStepId {
        &self.step_id
    }

    pub fn placement_digest(&self) -> &ManifestDigest {
        &self.placement_digest
    }

    pub const fn outcome(&self) -> WorkflowStepEffectOutcomeV1 {
        self.outcome
    }

    pub fn effect_digest(&self) -> &ManifestDigest {
        &self.effect_digest
    }

    pub fn output_set_digest(&self) -> &ManifestDigest {
        &self.output_set_digest
    }

    pub fn receipt_digest(&self) -> &ManifestDigest {
        &self.receipt_digest
    }
}

fn output_set_digest(
    outputs: &[WorkflowStepOutputV1],
) -> Result<ManifestDigest, WorkflowReceiptError> {
    let mut ordered = outputs.to_vec();
    ordered.sort_by(|left, right| left.output_name().cmp(right.output_name()));
    canonical_sha256(&("tracedecay.domain.workflow-output-set.v1", ordered))
        .map_err(|_| WorkflowReceiptError::InvalidEffect)
}

fn effect_receipt_digest(
    run_id: &RunId,
    step_id: &WorkflowStepId,
    placement_digest: &ManifestDigest,
    outcome: WorkflowStepEffectOutcomeV1,
    effect_digest: &ManifestDigest,
    output_set_digest: &ManifestDigest,
) -> Result<ManifestDigest, WorkflowReceiptError> {
    canonical_sha256(&(
        "tracedecay.domain.workflow-step-effect.v1",
        run_id,
        step_id,
        placement_digest,
        outcome,
        effect_digest,
        output_set_digest,
    ))
    .map_err(|_| WorkflowReceiptError::InvalidEffect)
}
