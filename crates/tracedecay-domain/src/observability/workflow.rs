//! Canonical Plan 32 Workflow settlement observations for Plan 26.
//!
//! These payloads are projections of durable Workflow journal and fan-out
//! census facts. They never treat a provider terminal as an independently
//! reviewed task outcome or manufacture unavailable resource counters.

use serde::{Deserialize, Serialize};

use crate::{CoverageStateV1, ManifestDigest, RunId, UtcMicros, WorkflowRunStatus};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkflowLifecycleObservedV1 {
    pub run_id: RunId,
    pub workflow_sequence: u64,
    pub definition_ref: String,
    pub definition_version: u64,
    pub topology_digest: ManifestDigest,
    pub provider_registry_digest: ManifestDigest,
    pub status: WorkflowRunStatus,
    pub started_at: UtcMicros,
    pub observed_at: UtcMicros,
    pub total_steps: u32,
    pub coverage: CoverageStateV1,
}

impl WorkflowLifecycleObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.run_id.validate().map_err(|_| "workflow_run_id")?;
        self.topology_digest
            .validate()
            .map_err(|_| "workflow_topology_digest")?;
        self.provider_registry_digest
            .validate()
            .map_err(|_| "workflow_provider_registry_digest")?;
        if self.workflow_sequence == 0
            || self.definition_version == 0
            || self.total_steps == 0
            || self.observed_at < self.started_at
            || !crate::canonical_text::is_canonical_text_within(&self.definition_ref, 256)
            || self.coverage != CoverageStateV1::Known
        {
            return Err("workflow_lifecycle");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkflowOutcomeObservedV1 {
    pub run_id: RunId,
    pub workflow_sequence: u64,
    pub status: WorkflowRunStatus,
    pub total_steps: u32,
    pub succeeded_steps: u32,
    pub failed_steps: u32,
    pub cancelled_steps: u32,
    pub unknown_steps: u32,
    pub eligible_attempts: u32,
    pub observed_attempts: u32,
    pub succeeded_attempts: u32,
    pub failed_attempts: u32,
    pub timed_out_attempts: u32,
    pub cancelled_attempts: u32,
    pub unknown_attempts: u32,
    pub coverage: CoverageStateV1,
}

impl WorkflowOutcomeObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.run_id.validate().map_err(|_| "workflow_run_id")?;
        let Some(classified_steps) = self
            .succeeded_steps
            .checked_add(self.failed_steps)
            .and_then(|value| value.checked_add(self.cancelled_steps))
        else {
            return Err("workflow_outcome");
        };
        let Some(classified_attempts) = self
            .succeeded_attempts
            .checked_add(self.failed_attempts)
            .and_then(|value| value.checked_add(self.timed_out_attempts))
            .and_then(|value| value.checked_add(self.cancelled_attempts))
        else {
            return Err("workflow_outcome");
        };
        if self.workflow_sequence == 0
            || !self.status.is_terminal()
            || self.total_steps == 0
            || classified_steps.checked_add(self.unknown_steps) != Some(self.total_steps)
            || self.observed_attempts > self.eligible_attempts
            || classified_attempts != self.observed_attempts
            || self.observed_attempts.checked_add(self.unknown_attempts)
                != Some(self.eligible_attempts)
            || ((self.unknown_steps == 0 && self.unknown_attempts == 0)
                != (self.coverage == CoverageStateV1::Known))
            || !matches!(
                self.coverage,
                CoverageStateV1::Known | CoverageStateV1::Partial
            )
        {
            return Err("workflow_outcome");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkflowResourceObservedV1 {
    pub run_id: RunId,
    pub workflow_sequence: u64,
    pub eligible_attempts: u32,
    pub observed_attempts: u32,
    pub artifact_count: u64,
    pub observed_duration_micros: Option<u64>,
    pub critical_path_duration_micros: Option<u64>,
    pub coverage: CoverageStateV1,
}

impl WorkflowResourceObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.run_id.validate().map_err(|_| "workflow_run_id")?;
        let durations_complete =
            self.observed_duration_micros.is_some() && self.critical_path_duration_micros.is_some();
        if self.workflow_sequence == 0
            || self.observed_attempts > self.eligible_attempts
            || (self.artifact_count > 0 && self.observed_attempts == 0)
            || self
                .observed_duration_micros
                .zip(self.critical_path_duration_micros)
                .is_some_and(|(observed, critical)| critical > observed)
            || (self.coverage == CoverageStateV1::Known
                && (!durations_complete || self.observed_attempts != self.eligible_attempts))
            || (self.coverage == CoverageStateV1::Unknown
                && (self.observed_attempts > 0
                    || self.artifact_count > 0
                    || self.observed_duration_micros.is_some()
                    || self.critical_path_duration_micros.is_some()))
            || !matches!(
                self.coverage,
                CoverageStateV1::Known | CoverageStateV1::Partial | CoverageStateV1::Unknown
            )
        {
            return Err("workflow_resource");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    #[test]
    fn lifecycle_requires_exact_journal_coverage() {
        let mut observation = WorkflowLifecycleObservedV1 {
            run_id: RunId::new("run.workflow.observed").unwrap(),
            workflow_sequence: 2,
            definition_ref: "workflow.definition.observed".to_owned(),
            definition_version: 1,
            topology_digest: digest('a'),
            provider_registry_digest: digest('b'),
            status: WorkflowRunStatus::Running,
            started_at: UtcMicros(10),
            observed_at: UtcMicros(20),
            total_steps: 2,
            coverage: CoverageStateV1::Known,
        };
        assert_eq!(observation.validate(), Ok(()));
        observation.coverage = CoverageStateV1::Partial;
        assert_eq!(observation.validate(), Err("workflow_lifecycle"));
    }

    #[test]
    fn outcome_preserves_unknown_attempt_denominator() {
        let observation = WorkflowOutcomeObservedV1 {
            run_id: RunId::new("run.workflow.partial").unwrap(),
            workflow_sequence: 5,
            status: WorkflowRunStatus::Failed,
            total_steps: 2,
            succeeded_steps: 1,
            failed_steps: 1,
            cancelled_steps: 0,
            unknown_steps: 0,
            eligible_attempts: 3,
            observed_attempts: 2,
            succeeded_attempts: 1,
            failed_attempts: 1,
            timed_out_attempts: 0,
            cancelled_attempts: 0,
            unknown_attempts: 1,
            coverage: CoverageStateV1::Partial,
        };
        assert_eq!(observation.validate(), Ok(()));
    }

    #[test]
    fn known_outcome_requires_every_step_to_be_classified() {
        let mut observation = WorkflowOutcomeObservedV1 {
            run_id: RunId::new("run.workflow.unclassified-step").unwrap(),
            workflow_sequence: 6,
            status: WorkflowRunStatus::Failed,
            total_steps: 3,
            succeeded_steps: 0,
            failed_steps: 1,
            cancelled_steps: 0,
            unknown_steps: 2,
            eligible_attempts: 1,
            observed_attempts: 1,
            succeeded_attempts: 0,
            failed_attempts: 1,
            timed_out_attempts: 0,
            cancelled_attempts: 0,
            unknown_attempts: 0,
            coverage: CoverageStateV1::Known,
        };
        assert_eq!(observation.validate(), Err("workflow_outcome"));
        observation.coverage = CoverageStateV1::Partial;
        assert_eq!(observation.validate(), Ok(()));
    }

    #[test]
    fn known_resource_requires_every_eligible_attempt_and_duration() {
        let mut observation = WorkflowResourceObservedV1 {
            run_id: RunId::new("run.workflow.resources").unwrap(),
            workflow_sequence: 3,
            eligible_attempts: 2,
            observed_attempts: 1,
            artifact_count: 1,
            observed_duration_micros: Some(100),
            critical_path_duration_micros: Some(80),
            coverage: CoverageStateV1::Partial,
        };
        assert_eq!(observation.validate(), Ok(()));
        observation.coverage = CoverageStateV1::Known;
        assert_eq!(observation.validate(), Err("workflow_resource"));
    }
}
