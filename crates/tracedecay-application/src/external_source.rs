//! Application admission for one sanitized external-source page.
//!
//! This layer owns no connector, scheduler, or store implementation. It turns
//! pinned source authority and an admitted page into a bounded commit-ready
//! value for the authoritative store adapter.

use std::collections::BTreeSet;

use thiserror::Error;
use tracedecay_domain::{
    DomainError, ManifestDigest, SourceAggregateFrontierV1, SourceBindingV1, SourceCaptureModeV1,
    SourceContentStateV1, SourceDefinitionV1, SourceObjectObservationV1, SourcePartitionFrontierV1,
    SourceSnapshotCompletionV1,
};

pub const MAX_SOURCE_OBSERVATIONS_PER_ADMISSION_V1: usize = 10_000;

#[derive(Debug, Error)]
pub enum SourceCaptureAdmissionErrorV1 {
    #[error("external source domain contract is invalid")]
    Domain(#[from] DomainError),
    #[error("event source admissions cannot carry canonical source content")]
    EventContent,
    #[error("source admission contains duplicate native objects")]
    DuplicateNativeObject,
    #[error("source snapshot completion does not match its complete partition frontier")]
    SnapshotCompletionMismatch,
    #[error("source admission exceeds the bounded object limit")]
    TooManyObjects,
}

/// One already-sanitized provider page, pinned to one definition/binding and
/// ready for an atomic source commit. Capture does not persist it itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceCaptureAdmissionV1 {
    definition: SourceDefinitionV1,
    binding: SourceBindingV1,
    expected_frontier: Option<SourceAggregateFrontierV1>,
    next_frontier: SourceAggregateFrontierV1,
    observations: Vec<SourceObjectObservationV1>,
    snapshot_completion: Option<SourceSnapshotCompletionV1>,
    idempotency_key: ManifestDigest,
    request_digest: ManifestDigest,
}

impl SourceCaptureAdmissionV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        definition: SourceDefinitionV1,
        binding: SourceBindingV1,
        expected_frontier: Option<SourceAggregateFrontierV1>,
        next_partition: SourcePartitionFrontierV1,
        observations: Vec<SourceObjectObservationV1>,
        snapshot_completion: Option<SourceSnapshotCompletionV1>,
        idempotency_key: ManifestDigest,
        request_digest: ManifestDigest,
    ) -> Result<Self, SourceCaptureAdmissionErrorV1> {
        definition.validate()?;
        binding.validate_against(&definition)?;
        if definition.capture_mode == SourceCaptureModeV1::Event {
            return Err(SourceCaptureAdmissionErrorV1::EventContent);
        }
        if observations.len() > MAX_SOURCE_OBSERVATIONS_PER_ADMISSION_V1 {
            return Err(SourceCaptureAdmissionErrorV1::TooManyObjects);
        }
        let binding_identity = binding.immutable_identity()?;
        if next_partition.binding() != &binding_identity {
            return Err(SourceCaptureAdmissionErrorV1::SnapshotCompletionMismatch);
        }
        if let Some(expected) = &expected_frontier
            && expected.binding() != &binding_identity
        {
            return Err(SourceCaptureAdmissionErrorV1::SnapshotCompletionMismatch);
        }
        let mut native_objects = BTreeSet::new();
        for observation in &observations {
            observation.validate()?;
            if !native_objects.insert(observation.native_object().clone()) {
                return Err(SourceCaptureAdmissionErrorV1::DuplicateNativeObject);
            }
        }
        if let Some(completion) = &snapshot_completion {
            completion.validate()?;
            if next_partition.coverage() != tracedecay_domain::SourceCoverageV1::Complete
                || next_partition.snapshot() != Some(completion.snapshot())
                || next_partition.partition() != completion.partition()
            {
                return Err(SourceCaptureAdmissionErrorV1::SnapshotCompletionMismatch);
            }
            let staged_live = observations
                .iter()
                .filter(|observation| {
                    observation.content_state() != SourceContentStateV1::AuthoritativeDeleted
                })
                .map(|observation| observation.native_object().clone())
                .collect::<BTreeSet<_>>();
            if staged_live != *completion.present_objects() {
                return Err(SourceCaptureAdmissionErrorV1::SnapshotCompletionMismatch);
            }
        } else if next_partition.coverage() == tracedecay_domain::SourceCoverageV1::Complete {
            return Err(SourceCaptureAdmissionErrorV1::SnapshotCompletionMismatch);
        }
        idempotency_key.validate()?;
        request_digest.validate()?;
        let next_frontier = SourceAggregateFrontierV1::with_updated_partition(
            binding_identity,
            expected_frontier.as_ref(),
            next_partition,
        )?;
        Ok(Self {
            definition,
            binding,
            expected_frontier,
            next_frontier,
            observations,
            snapshot_completion,
            idempotency_key,
            request_digest,
        })
    }

    #[allow(clippy::type_complexity)]
    pub fn into_parts(
        self,
    ) -> (
        SourceDefinitionV1,
        SourceBindingV1,
        Option<SourceAggregateFrontierV1>,
        SourceAggregateFrontierV1,
        Vec<SourceObjectObservationV1>,
        Option<SourceSnapshotCompletionV1>,
        ManifestDigest,
        ManifestDigest,
    ) {
        (
            self.definition,
            self.binding,
            self.expected_frontier,
            self.next_frontier,
            self.observations,
            self.snapshot_completion,
            self.idempotency_key,
            self.request_digest,
        )
    }

    pub fn next_frontier(&self) -> &SourceAggregateFrontierV1 {
        &self.next_frontier
    }
}
