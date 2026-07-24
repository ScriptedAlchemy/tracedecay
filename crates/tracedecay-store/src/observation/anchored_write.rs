use tracedecay_domain::{
    AnchorSourceGenerationV2, DurableObservationV1, EvidenceAvailabilityV1,
    GenerationBoundRepositoryProvenanceV1, ObservationScopeV1, ObservationSourceCursorV1,
    ProjectionGenerationId, RetrievalAnchorId, RetrievalAnchorRecordV2, RetrievalAnchorTargetV2,
};

use super::{ObservationStoreError, ObservationStoreResult, ObservationWrite};

pub(super) fn validate_retrieval_anchor_binding(
    observation: &DurableObservationV1,
    retrieval_anchor: &RetrievalAnchorRecordV2,
    projection_generation: &ProjectionGenerationId,
) -> ObservationStoreResult<()> {
    if !matches!(
        retrieval_anchor.target(),
        RetrievalAnchorTargetV2::ExactObservation(observation_id)
            if observation_id == observation.observation_id()
    ) {
        return Err(ObservationStoreError::RetrievalAnchorObservationMismatch);
    }
    if retrieval_anchor.owner() != observation.scope() {
        return Err(ObservationStoreError::RetrievalAnchorOwnerMismatch);
    }
    if retrieval_anchor.source_generation()
        != &AnchorSourceGenerationV2::Observation(observation.identity().generation())
    {
        return Err(ObservationStoreError::RetrievalAnchorSourceGenerationMismatch);
    }
    if retrieval_anchor.source_observations() != std::slice::from_ref(observation.observation_id())
    {
        return Err(ObservationStoreError::RetrievalAnchorSourceLineageMismatch);
    }
    if retrieval_anchor.projection_generation() != projection_generation {
        return Err(ObservationStoreError::RetrievalAnchorProjectionGenerationMismatch);
    }
    Ok(())
}

/// Optional repository evidence captured after observation sanitization.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepositoryProvenanceAttachmentV1 {
    availability: EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    anchor: Option<RetrievalAnchorRecordV2>,
}

impl RepositoryProvenanceAttachmentV1 {
    pub fn new(
        availability: EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1>,
        anchor: Option<RetrievalAnchorRecordV2>,
    ) -> ObservationStoreResult<Self> {
        if availability.value().is_some() != anchor.is_some() {
            return Err(ObservationStoreError::RepositoryProvenanceAvailabilityMismatch);
        }
        if let Some(provenance) = availability.value() {
            provenance
                .validate()
                .map_err(ObservationStoreError::RepositoryProvenanceContract)?;
        }
        if let Some(anchor) = &anchor {
            anchor
                .validate()
                .map_err(ObservationStoreError::RepositoryProvenanceContract)?;
        }
        Ok(Self {
            availability,
            anchor,
        })
    }

    pub fn unavailable() -> Self {
        Self {
            availability: EvidenceAvailabilityV1::Unavailable,
            anchor: None,
        }
    }

    pub fn availability(&self) -> &EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1> {
        &self.availability
    }

    pub fn provenance(&self) -> Option<&GenerationBoundRepositoryProvenanceV1> {
        self.availability.value()
    }

    pub fn anchor(&self) -> Option<&RetrievalAnchorRecordV2> {
        self.anchor.as_ref()
    }

    pub(super) fn validate_for_observation(
        &self,
        observation: &DurableObservationV1,
        projection_generation: &ProjectionGenerationId,
    ) -> ObservationStoreResult<()> {
        let (Some(provenance), Some(anchor)) = (self.availability.value(), self.anchor.as_ref())
        else {
            return if self.availability.value().is_none() && self.anchor.is_none() {
                Ok(())
            } else {
                Err(ObservationStoreError::RepositoryProvenanceAvailabilityMismatch)
            };
        };
        provenance
            .validate()
            .map_err(ObservationStoreError::RepositoryProvenanceContract)?;
        anchor
            .validate()
            .map_err(ObservationStoreError::RepositoryProvenanceContract)?;
        let project_id = match observation.scope() {
            ObservationScopeV1::Project { project_id } => project_id,
            ObservationScopeV1::Profile => {
                return Err(ObservationStoreError::RepositoryProvenanceBindingMismatch);
            }
        };
        if provenance.generation_id() != projection_generation
            || provenance.source_observation() != Some(observation.observation_id())
            || provenance.capture().project_id() != Some(project_id)
            || anchor.owner() != observation.scope()
            || anchor.projection_generation() != projection_generation
            || anchor.source_observations() != [observation.observation_id().clone()]
            || !matches!(
                anchor.source_generation(),
                AnchorSourceGenerationV2::RepositoryCapture(capture_id)
                    if capture_id == provenance.capture_id()
            )
            || !matches!(
                anchor.target(),
                RetrievalAnchorTargetV2::RepositoryCapture {
                    repository_id,
                    capture_id,
                    receipt,
                } if repository_id == provenance.capture().repository_id()
                    && capture_id == provenance.capture_id()
                    && receipt == observation.receipt().receipt()
            )
        {
            return Err(ObservationStoreError::RepositoryProvenanceBindingMismatch);
        }
        Ok(())
    }
}

impl Default for RepositoryProvenanceAttachmentV1 {
    fn default() -> Self {
        Self::unavailable()
    }
}

/// One observation write and its stable V2 retrieval anchor.
///
/// Stores commit every part of this value in one authoritative transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchoredObservationWrite {
    write: ObservationWrite,
    retrieval_anchor: RetrievalAnchorRecordV2,
    projection_generation: ProjectionGenerationId,
    repository_provenance: RepositoryProvenanceAttachmentV1,
}

impl AnchoredObservationWrite {
    pub fn new(
        write: ObservationWrite,
        retrieval_anchor: RetrievalAnchorRecordV2,
        projection_generation: ProjectionGenerationId,
    ) -> ObservationStoreResult<Self> {
        validate_retrieval_anchor_binding(
            write.observation(),
            &retrieval_anchor,
            &projection_generation,
        )?;
        Ok(Self {
            write,
            retrieval_anchor,
            projection_generation,
            repository_provenance: RepositoryProvenanceAttachmentV1::unavailable(),
        })
    }

    pub fn with_repository_provenance_attachment(
        mut self,
        availability: EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1>,
        anchor: Option<RetrievalAnchorRecordV2>,
    ) -> ObservationStoreResult<Self> {
        let repository_provenance = RepositoryProvenanceAttachmentV1::new(availability, anchor)?;
        repository_provenance
            .validate_for_observation(self.write.observation(), &self.projection_generation)?;
        self.repository_provenance = repository_provenance;
        Ok(self)
    }

    pub fn write(&self) -> &ObservationWrite {
        &self.write
    }

    pub fn observation(&self) -> &DurableObservationV1 {
        self.write.observation()
    }

    pub fn expected_cursor(&self) -> Option<&ObservationSourceCursorV1> {
        self.write.expected_cursor()
    }

    pub fn next_cursor(&self) -> &ObservationSourceCursorV1 {
        self.write.next_cursor()
    }

    pub fn retrieval_anchor(&self) -> &RetrievalAnchorRecordV2 {
        &self.retrieval_anchor
    }

    pub fn retrieval_anchor_id(&self) -> &RetrievalAnchorId {
        self.retrieval_anchor.anchor_id()
    }

    pub fn projection_generation(&self) -> &ProjectionGenerationId {
        &self.projection_generation
    }

    pub fn repository_provenance_attachment(&self) -> &RepositoryProvenanceAttachmentV1 {
        &self.repository_provenance
    }

    pub fn into_parts(
        self,
    ) -> (
        ObservationWrite,
        RetrievalAnchorRecordV2,
        ProjectionGenerationId,
        RepositoryProvenanceAttachmentV1,
    ) {
        (
            self.write,
            self.retrieval_anchor,
            self.projection_generation,
            self.repository_provenance,
        )
    }
}
