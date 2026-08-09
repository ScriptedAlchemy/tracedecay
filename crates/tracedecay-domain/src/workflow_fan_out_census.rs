//! Persisted, generation-bound measurements of one Workflow fan-out run.
//!
//! A census is deliberately richer than the flattened observability sample.
//! Missing classification evidence stays typed here; only a census whose
//! execution dimensions are all exact may be projected to Plan 26 telemetry.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    ExecutionPlacementV1, ExecutionTopologyKindV1, IntegrationStrategyV1, ManifestDigest,
    ProjectionGenerationId, ProviderId, ReviewTopologyV1, RunId, UtcMicros, WorkAttemptIdentityV1,
    WorkTopologyBranchV1,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowCensusEvidenceReasonV1 {
    FirstObservation,
    WorkProjectionUnavailable,
    WorkGenerationMismatch,
    AttemptUnavailable,
    ProgressFrontierUnavailable,
    SharedAuthorityEvidenceUnavailable,
    IncompleteWorkflow,
    DuplicateAdjudicationUnavailable,
    ReadinessEvidenceUnavailable,
    InconsistentPinnedTopology,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkflowCensusCountV1 {
    Known {
        value: u16,
    },
    Partial {
        observed: u16,
        reason: WorkflowCensusEvidenceReasonV1,
    },
    Unavailable {
        reason: WorkflowCensusEvidenceReasonV1,
    },
}

impl WorkflowCensusCountV1 {
    pub const fn known(&self) -> Option<u16> {
        match self {
            Self::Known { value } => Some(*value),
            Self::Partial { .. } | Self::Unavailable { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkflowCensusDurationV1 {
    Known {
        micros: u64,
    },
    Partial {
        observed_micros: u64,
        reason: WorkflowCensusEvidenceReasonV1,
    },
    Unavailable {
        reason: WorkflowCensusEvidenceReasonV1,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowProviderCapacityV1 {
    pub provider_id: ProviderId,
    pub maximum_global_active: u16,
    pub maximum_active_per_repository: u16,
    pub maximum_parallel_per_task: u16,
    pub admitted: WorkflowCensusCountV1,
    pub active: WorkflowCensusCountV1,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkflowProviderCapacityEvidenceV1 {
    Known {
        providers: Vec<WorkflowProviderCapacityV1>,
    },
    Unavailable {
        reason: WorkflowCensusEvidenceReasonV1,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkflowCensusGenerationV1 {
    Exact {
        generation_id: ProjectionGenerationId,
    },
    Unavailable {
        reason: WorkflowCensusEvidenceReasonV1,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowExecutionTopologyClassificationV1 {
    pub topology: ExecutionTopologyKindV1,
    pub placement: ExecutionPlacementV1,
    pub branch_topology: WorkTopologyBranchV1,
    pub review_topology: ReviewTopologyV1,
    pub integration_strategy: IntegrationStrategyV1,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkflowExecutionTopologyEvidenceV1 {
    Known {
        value: WorkflowExecutionTopologyClassificationV1,
    },
    Unavailable {
        reason: WorkflowCensusEvidenceReasonV1,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowAttemptFrontierV1 {
    pub attempt: WorkAttemptIdentityV1,
    pub completed: Option<u64>,
}

/// One exact Workflow journal transition joined to the Work generation that
/// supplied its accepted-proposal and attempt classifications.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFanOutCensusV1 {
    pub run_id: RunId,
    pub workflow_sequence: u64,
    pub topology_digest: ManifestDigest,
    pub provider_registry_digest: ManifestDigest,
    pub work_generation: WorkflowCensusGenerationV1,
    pub execution_topology: WorkflowExecutionTopologyEvidenceV1,
    pub interval_started_at: UtcMicros,
    pub observed_at: UtcMicros,
    pub requested_width: WorkflowCensusCountV1,
    pub accepted_width: WorkflowCensusCountV1,
    pub admitted_width: WorkflowCensusCountV1,
    pub active_width: WorkflowCensusCountV1,
    pub useful_width: WorkflowCensusCountV1,
    pub runnable_count: WorkflowCensusCountV1,
    pub blocked_count: WorkflowCensusCountV1,
    pub shared_authority_serialized_count: WorkflowCensusCountV1,
    pub provider_capacities: WorkflowProviderCapacityEvidenceV1,
    pub observed_duration: WorkflowCensusDurationV1,
    pub critical_path_duration: WorkflowCensusDurationV1,
    pub attempt_frontiers: Vec<WorkflowAttemptFrontierV1>,
}

impl WorkflowFanOutCensusV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.workflow_sequence == 0 || self.observed_at < self.interval_started_at {
            return Err("workflow_fan_out_census_interval");
        }
        let requested = self.requested_width.known();
        let accepted = self.accepted_width.known();
        let admitted = self.admitted_width.known();
        let active = self.active_width.known();
        let useful = self.useful_width.known();
        if accepted.zip(requested).is_some_and(|(a, r)| a > r)
            || admitted.zip(accepted).is_some_and(|(a, r)| a > r)
            || active.zip(admitted).is_some_and(|(a, r)| a > r)
            || useful.zip(active).is_some_and(|(a, r)| a > r)
            || self
                .shared_authority_serialized_count
                .known()
                .zip(admitted)
                .is_some_and(|(serialized, admitted)| serialized > admitted)
        {
            return Err("workflow_fan_out_census_widths");
        }
        if self
            .attempt_frontiers
            .windows(2)
            .any(|pair| pair[0].attempt >= pair[1].attempt)
            || self
                .provider_capacities
                .providers()
                .is_some_and(|providers| {
                    providers
                        .windows(2)
                        .any(|pair| pair[0].provider_id >= pair[1].provider_id)
                })
        {
            return Err("workflow_fan_out_census_order");
        }
        Ok(())
    }

    /// Flattens only complete evidence. A typed partial census remains durable
    /// but cannot silently become a zero-filled observability sample.
    pub fn execution_topology_sample(&self) -> Option<crate::ExecutionTopologySampledV1> {
        let WorkflowCensusGenerationV1::Exact { .. } = &self.work_generation else {
            return None;
        };
        let WorkflowExecutionTopologyEvidenceV1::Known { value } = &self.execution_topology else {
            return None;
        };
        let sample = crate::ExecutionTopologySampledV1 {
            topology: value.topology,
            placement: value.placement,
            branch_topology: value.branch_topology,
            review_topology: value.review_topology,
            integration_strategy: value.integration_strategy,
            requested_width: self.requested_width.known()?,
            accepted_width: self.accepted_width.known()?,
            admitted_width: self.admitted_width.known()?,
            active_width: self.active_width.known()?,
            useful_width: self.useful_width.known()?,
            runnable_count: self.runnable_count.known()?,
            blocked_count: self.blocked_count.known()?,
            shared_authority_serialized_count: self.shared_authority_serialized_count.known()?,
            local_anchor_refs: Vec::new(),
        };
        sample.validate().ok()?;
        Some(sample)
    }
}

impl WorkflowProviderCapacityEvidenceV1 {
    fn providers(&self) -> Option<&[WorkflowProviderCapacityV1]> {
        match self {
            Self::Known { providers } => Some(providers),
            Self::Unavailable { .. } => None,
        }
    }
}
