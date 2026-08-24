//! Driver-neutral, payload-free evidence-assembly persistence contracts.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::canonical_text::{CANONICAL_TEXT_MAX_BYTES, is_canonical_text_within};
use tracedecay_domain::{
    AnchorOwnerBindingV1, BlobId, CanonicalObservationIdV1, CanonicalSourceOccurrenceSetIdV1,
    CapabilityId, ComponentVersion, CoverageReportV1, EvidenceAssemblyPublicationReceiptIdV1,
    EvidenceSpanIdV1, EvidenceSpanProjectionReceiptIdV1, ManifestDigest,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceGenerationV1,
    ObservationSourceIdentityV1, ObservationSourceRangeV1, PrivacyDomainBoundLocatorDigest,
    PrivacyDomainId, ProjectionGenerationId, RepositoryCaptureId, RepositoryId, RetrievalAnchorId,
    RetrievalAnchorRecordV3, RetrievalAnchorTargetV3, RetrieverContributionIdV1,
    SanitizationReceiptRefV1, ScopeResolutionId, SourceOccurrenceId, TemporalModeV1, UseCaseId,
    UtcMicros, VectorWatermark, canonical_sha256,
};

pub const MAX_EVIDENCE_ASSEMBLY_MEMBERS_V1: usize = 4_096;
const SOURCE_OCCURRENCE_ID_DOMAIN_V1: &str = "tracedecay.source-occurrence.identity.v1";
const OCCURRENCE_SET_ID_DOMAIN_V1: &str = "tracedecay.source-occurrence-set.identity.v1";
const EVIDENCE_SPAN_ID_DOMAIN_V1: &str = "tracedecay.evidence-span.identity.v1";
const PROJECTION_RECEIPT_ID_DOMAIN_V1: &str =
    "tracedecay.evidence-span-projection-receipt.identity.v1";
const RETRIEVER_CONTRIBUTION_ID_DOMAIN_V1: &str = "tracedecay.retriever-contribution.identity.v1";
const PUBLICATION_RECEIPT_ID_DOMAIN_V1: &str =
    "tracedecay.evidence-assembly-publication.identity.v1";

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum EvidenceAssemblyStoreError {
    #[error("evidence assembly store data is invalid: {0}")]
    InvalidData(String),
    #[error("evidence assembly replay conflicts with existing material")]
    ReplayConflict,
    #[error("evidence assembly target is unavailable")]
    Unavailable,
    #[error("evidence catalog binding does not match ordering proof")]
    CatalogMismatch,
    #[error("evidence integration manifest does not match ordering proof")]
    IntegrationManifestMismatch,
    #[error("evidence ordering proof is stale")]
    StaleOrderingProof,
    #[error("evidence occurrences do not share a comparable source order")]
    IncomparableSourceOrder,
    #[error("evidence consecutiveness was not verified")]
    UnverifiedConsecutiveness,
    #[error("evidence request digest does not match the owner privacy binding")]
    RequestPrivacyBindingMismatch,
    #[error("evidence sanitization receipt roles are incomplete or reused")]
    ReceiptRoleMismatch,
    #[error("evidence temporal horizon does not cover every member")]
    HorizonMismatch,
}

pub type EvidenceAssemblyStoreResult<T> = Result<T, EvidenceAssemblyStoreError>;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct SanitizedObservationByteRangeV1 {
    pub start: u64,
    pub end: u64,
}

impl SanitizedObservationByteRangeV1 {
    pub fn new(start: u64, end: u64) -> EvidenceAssemblyStoreResult<Self> {
        if start >= end {
            return Err(invalid("sanitized observation byte range"));
        }
        Ok(Self { start, end })
    }

    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        Self::new(self.start, self.end).map(|_| ())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceOccurrenceCoordinateV1 {
    ObservationProjection {
        canonical_observation_id: CanonicalObservationIdV1,
        source_range: ObservationSourceRangeV1,
        projection_output_ordinal: u64,
        sanitized_byte_range: SanitizedObservationByteRangeV1,
    },
    ImmutableBlobSlice {
        repository_id: RepositoryId,
        blob_id: BlobId,
        byte_start: u64,
        byte_end: u64,
    },
    CapturedWorktreeSlice {
        repository_id: RepositoryId,
        repository_capture_id: RepositoryCaptureId,
        path_locator_digest: PrivacyDomainBoundLocatorDigest,
        byte_start: u64,
        byte_end: u64,
    },
}

impl SourceOccurrenceCoordinateV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        match self {
            Self::ObservationProjection {
                canonical_observation_id,
                source_range,
                sanitized_byte_range,
                ..
            } => {
                CanonicalObservationIdV1::new(canonical_observation_id.as_str())
                    .map_err(invalid)?;
                ObservationSourceRangeV1::new(source_range.start(), source_range.end())
                    .map_err(invalid)?;
                sanitized_byte_range.validate()
            }
            Self::ImmutableBlobSlice {
                repository_id,
                blob_id,
                byte_start,
                byte_end,
            } => {
                repository_id.validate().map_err(invalid)?;
                blob_id.validate().map_err(invalid)?;
                validate_half_open(*byte_start, *byte_end, "immutable blob byte range")
            }
            Self::CapturedWorktreeSlice {
                repository_id,
                repository_capture_id,
                path_locator_digest,
                byte_start,
                byte_end,
            } => {
                repository_id.validate().map_err(invalid)?;
                repository_capture_id.validate().map_err(invalid)?;
                path_locator_digest.validate().map_err(invalid)?;
                validate_half_open(*byte_start, *byte_end, "captured worktree byte range")
            }
        }
    }

    pub const fn is_code(&self) -> bool {
        matches!(
            self,
            Self::ImmutableBlobSlice { .. } | Self::CapturedWorktreeSlice { .. }
        )
    }

    pub fn source_order(&self) -> u64 {
        match self {
            Self::ObservationProjection { source_range, .. } => source_range.start(),
            Self::ImmutableBlobSlice { byte_start, .. }
            | Self::CapturedWorktreeSlice { byte_start, .. } => *byte_start,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceOccurrenceKindV1 {
    Message,
    ToolInvocation,
    ToolResult,
    CodeChunk,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceOccurrenceRelationV1 {
    ToolResultFor {
        invocation_occurrence_id: SourceOccurrenceId,
    },
    DerivedFromOccurrence {
        source_occurrence_id: SourceOccurrenceId,
    },
}

impl SourceOccurrenceRelationV1 {
    fn source_id(&self) -> &SourceOccurrenceId {
        match self {
            Self::ToolResultFor {
                invocation_occurrence_id,
            } => invocation_occurrence_id,
            Self::DerivedFromOccurrence {
                source_occurrence_id,
            } => source_occurrence_id,
        }
    }

    fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.source_id().validate().map_err(invalid)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceOccurrenceSanitizationV1 {
    pub capture: SanitizationReceiptRefV1,
    pub projection: SanitizationReceiptRefV1,
}

impl SourceOccurrenceSanitizationV1 {
    pub fn new(
        capture: SanitizationReceiptRefV1,
        projection: SanitizationReceiptRefV1,
    ) -> EvidenceAssemblyStoreResult<Self> {
        let value = Self {
            capture,
            projection,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.capture.validate().map_err(invalid)?;
        self.projection.validate().map_err(invalid)?;
        if self.capture == self.projection {
            return Err(EvidenceAssemblyStoreError::ReceiptRoleMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceTimelineKeyV1 {
    pub source: ObservationSourceIdentityV1,
    pub scope: ObservationScopeV1,
    pub source_generation: ObservationSourceGenerationV1,
    pub ordering_domain: ObservationOrderingDomainV1,
}

impl SourceTimelineKeyV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.source.validate().map_err(invalid)?;
        self.scope.validate().map_err(invalid)?;
        ObservationSourceGenerationV1::new(self.source_generation.generation_id())
            .map_err(invalid)?;
        Ok(())
    }

    pub fn digest(&self) -> EvidenceAssemblyStoreResult<ManifestDigest> {
        self.validate()?;
        canonical_sha256(self).map_err(invalid)
    }
}

pub type EvidenceSourceTimelineV1 = SourceTimelineKeyV1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct EvidenceAssemblyIdempotencyKeyV1(ManifestDigest);

impl EvidenceAssemblyIdempotencyKeyV1 {
    pub fn new(value: ManifestDigest) -> EvidenceAssemblyStoreResult<Self> {
        value.validate().map_err(invalid)?;
        Ok(Self(value))
    }

    pub fn as_digest(&self) -> &ManifestDigest {
        &self.0
    }

    pub fn derive(
        owner: &AnchorOwnerBindingV1,
        key_epoch: u64,
        privacy_key: &[u8],
        raw_request_key: &[u8],
    ) -> EvidenceAssemblyStoreResult<Self> {
        owner.validate().map_err(invalid)?;
        if key_epoch == 0
            || privacy_key.len() < 16
            || raw_request_key.is_empty()
            || raw_request_key.len() > 4_096
        {
            return Err(invalid("evidence assembly idempotency key material"));
        }
        Self::new(keyed_canonical_digest(
            privacy_key,
            &(
                "tracedecay.evidence-assembly-idempotency.v1",
                owner,
                owner.privacy_domain_id(),
                key_epoch,
                raw_request_key,
            ),
        )?)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceAssemblyOwnerV1 {
    pub owner: AnchorOwnerBindingV1,
    pub scope_digest: ManifestDigest,
    pub key_epoch: u64,
}

impl EvidenceAssemblyOwnerV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.owner.validate().map_err(invalid)?;
        self.scope_digest.validate().map_err(invalid)?;
        if self.key_epoch == 0 {
            return Err(invalid("evidence assembly privacy key epoch"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSourceOccurrenceRecordV1 {
    pub occurrence_id: SourceOccurrenceId,
    pub owner: AnchorOwnerBindingV1,
    pub timeline: EvidenceSourceTimelineV1,
    pub exact_source_anchor: RetrievalAnchorId,
    pub occurrence_anchor: RetrievalAnchorRecordV3,
    pub source_order: u64,
    pub coordinate: SourceOccurrenceCoordinateV1,
    pub occurrence_kind: SourceOccurrenceKindV1,
    pub relations: Vec<SourceOccurrenceRelationV1>,
    pub projector_version: ComponentVersion,
    pub sanitization: SourceOccurrenceSanitizationV1,
    pub knowledge_time: UtcMicros,
    pub valid_time: Option<UtcMicros>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceOccurrenceIdentityProjectionV1 {
    pub owner: AnchorOwnerBindingV1,
    pub timeline: EvidenceSourceTimelineV1,
    pub exact_source_anchor: RetrievalAnchorId,
    pub source_order: u64,
    pub coordinate: SourceOccurrenceCoordinateV1,
    pub occurrence_kind: SourceOccurrenceKindV1,
    pub relations: Vec<SourceOccurrenceRelationV1>,
    pub projector_version: ComponentVersion,
}

impl SourceOccurrenceIdentityProjectionV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.owner.validate().map_err(invalid)?;
        self.timeline.validate()?;
        self.exact_source_anchor.validate().map_err(invalid)?;
        self.coordinate.validate()?;
        self.projector_version.validate().map_err(invalid)?;
        let owner_scope_matches = match (self.owner.project_id(), &self.timeline.scope) {
            (None, ObservationScopeV1::Profile) => true,
            (
                Some(owner_project),
                ObservationScopeV1::Project {
                    project_id: source_project,
                },
            ) => owner_project == source_project,
            _ => false,
        };
        if !owner_scope_matches {
            return Err(invalid("source occurrence timeline owner scope"));
        }
        if self.source_order != self.coordinate.source_order()
            || (self.coordinate.is_code()
                && self.timeline.ordering_domain != ObservationOrderingDomainV1::FileBytes)
        {
            return Err(EvidenceAssemblyStoreError::IncomparableSourceOrder);
        }
        if self.relations.len() > MAX_EVIDENCE_ASSEMBLY_MEMBERS_V1 {
            return Err(invalid("source occurrence relation count"));
        }
        for relation in &self.relations {
            relation.validate()?;
        }
        ensure_unique(
            &self
                .relations
                .iter()
                .map(|relation| relation.source_id().clone())
                .collect::<Vec<_>>(),
            "source occurrence relations",
        )?;
        let tool_result_relations = self
            .relations
            .iter()
            .filter(|relation| matches!(relation, SourceOccurrenceRelationV1::ToolResultFor { .. }))
            .count();
        if (self.occurrence_kind == SourceOccurrenceKindV1::ToolResult
            && tool_result_relations != 1)
            || (self.occurrence_kind != SourceOccurrenceKindV1::ToolResult
                && tool_result_relations != 0)
            || (self.occurrence_kind == SourceOccurrenceKindV1::CodeChunk)
                != self.coordinate.is_code()
        {
            return Err(invalid(
                "source occurrence kind/coordinate/relation binding",
            ));
        }
        Ok(())
    }
}

pub fn derive_source_occurrence_id_v1(
    projection: &SourceOccurrenceIdentityProjectionV1,
) -> EvidenceAssemblyStoreResult<SourceOccurrenceId> {
    projection.validate()?;
    let digest = canonical_identity_digest(SOURCE_OCCURRENCE_ID_DOMAIN_V1, projection)?;
    SourceOccurrenceId::new(digest.as_str()).map_err(invalid)
}

impl EvidenceSourceOccurrenceRecordV1 {
    pub fn identity_projection(&self) -> SourceOccurrenceIdentityProjectionV1 {
        SourceOccurrenceIdentityProjectionV1 {
            owner: self.owner.clone(),
            timeline: self.timeline.clone(),
            exact_source_anchor: self.exact_source_anchor.clone(),
            source_order: self.source_order,
            coordinate: self.coordinate.clone(),
            occurrence_kind: self.occurrence_kind,
            relations: self.relations.clone(),
            projector_version: self.projector_version.clone(),
        }
    }

    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.occurrence_id.validate().map_err(invalid)?;
        self.owner.validate().map_err(invalid)?;
        self.timeline.validate()?;
        self.exact_source_anchor.validate().map_err(invalid)?;
        self.coordinate.validate()?;
        self.sanitization.validate()?;
        self.occurrence_anchor.validate().map_err(invalid)?;
        match self.occurrence_anchor.target() {
            RetrievalAnchorTargetV3::ExactSourceOccurrence(target)
                if target == &self.occurrence_id => {}
            _ => return Err(invalid("source occurrence anchor target")),
        }
        if self.occurrence_anchor.owner() != &self.owner {
            return Err(invalid("source occurrence anchor owner"));
        }
        validate_derived_anchor_lineage(
            &self.occurrence_anchor,
            &self.owner,
            std::slice::from_ref(&self.exact_source_anchor),
            "source occurrence anchor lineage",
        )?;
        if self
            .relations
            .iter()
            .any(|relation| relation.source_id() == &self.occurrence_id)
        {
            return Err(invalid("source occurrence self relation"));
        }
        self.identity_projection().validate()?;
        if self.occurrence_id != derive_source_occurrence_id_v1(&self.identity_projection())? {
            return Err(invalid("source occurrence identity"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalSourceOccurrenceSetRecordV1 {
    pub occurrence_set_id: CanonicalSourceOccurrenceSetIdV1,
    pub owner: AnchorOwnerBindingV1,
    /// Canonical set order, sorted by immutable occurrence identity.
    pub members: Vec<SourceOccurrenceId>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalSourceOccurrenceSetIdentityProjectionV1 {
    pub owner: AnchorOwnerBindingV1,
    pub canonical_members: Vec<SourceOccurrenceId>,
}

impl CanonicalSourceOccurrenceSetIdentityProjectionV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.owner.validate().map_err(invalid)?;
        validate_member_count(self.canonical_members.len())?;
        if self
            .canonical_members
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            return Err(invalid("canonical occurrence set member order"));
        }
        Ok(())
    }
}

pub fn derive_canonical_source_occurrence_set_id_v1(
    projection: &CanonicalSourceOccurrenceSetIdentityProjectionV1,
) -> EvidenceAssemblyStoreResult<CanonicalSourceOccurrenceSetIdV1> {
    projection.validate()?;
    let digest = canonical_identity_digest(OCCURRENCE_SET_ID_DOMAIN_V1, projection)?;
    CanonicalSourceOccurrenceSetIdV1::new(digest.as_str()).map_err(invalid)
}

impl CanonicalSourceOccurrenceSetRecordV1 {
    pub fn identity_projection(&self) -> CanonicalSourceOccurrenceSetIdentityProjectionV1 {
        CanonicalSourceOccurrenceSetIdentityProjectionV1 {
            owner: self.owner.clone(),
            canonical_members: self.members.clone(),
        }
    }

    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.occurrence_set_id.validate().map_err(invalid)?;
        self.owner.validate().map_err(invalid)?;
        validate_member_count(self.members.len())?;
        if self.members.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(invalid("canonical occurrence set member order"));
        }
        if self.occurrence_set_id
            != derive_canonical_source_occurrence_set_id_v1(&self.identity_projection())?
        {
            return Err(invalid("canonical occurrence set identity"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceCapabilityCatalogBindingV1 {
    pub connector_id: String,
    pub root_id: String,
    pub capability_id: CapabilityId,
    pub catalog_digest: ManifestDigest,
    pub integration_manifest_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub authorization_scope_digest: ManifestDigest,
    pub projector_revision: ComponentVersion,
    pub source_watermark: ManifestDigest,
}

impl SourceCapabilityCatalogBindingV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        validate_label(&self.connector_id, "evidence source connector id")?;
        validate_label(&self.root_id, "evidence source root id")?;
        self.capability_id.validate().map_err(invalid)?;
        self.catalog_digest.validate().map_err(invalid)?;
        self.integration_manifest_digest
            .validate()
            .map_err(invalid)?;
        self.configuration_digest.validate().map_err(invalid)?;
        self.authorization_scope_digest
            .validate()
            .map_err(invalid)?;
        self.projector_revision.validate().map_err(invalid)?;
        self.source_watermark.validate().map_err(invalid)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
// Boxing the large variant is wire-transparent but would change this public
// store-protocol API and ripple through construction/match sites.
#[allow(clippy::large_enum_variant)]
pub enum EvidenceSpanCatalogBindingV1 {
    IntrinsicCanonicalOrdering,
    SourceCapability {
        binding: SourceCapabilityCatalogBindingV1,
    },
}

impl EvidenceSpanCatalogBindingV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        match self {
            Self::IntrinsicCanonicalOrdering => Ok(()),
            Self::SourceCapability { binding } => binding.validate(),
        }
    }

    fn source_capability(&self) -> Option<&SourceCapabilityCatalogBindingV1> {
        match self {
            Self::IntrinsicCanonicalOrdering => None,
            Self::SourceCapability { binding } => Some(binding),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedSourceOrderingProofV1 {
    pub timeline: SourceTimelineKeyV1,
    pub catalog_binding: SourceCapabilityCatalogBindingV1,
    pub ordered_occurrence_ids: Vec<SourceOccurrenceId>,
    pub source_orders: Vec<u64>,
}

impl VerifiedSourceOrderingProofV1 {
    pub fn verify(
        expected_timeline: SourceTimelineKeyV1,
        expected_binding: SourceCapabilityCatalogBindingV1,
        observed_binding: SourceCapabilityCatalogBindingV1,
        ordered_occurrence_ids: Vec<SourceOccurrenceId>,
        source_orders: Vec<u64>,
    ) -> EvidenceAssemblyStoreResult<Self> {
        expected_timeline.validate()?;
        expected_binding.validate()?;
        observed_binding.validate()?;
        if expected_binding.catalog_digest != observed_binding.catalog_digest {
            return Err(EvidenceAssemblyStoreError::CatalogMismatch);
        }
        if expected_binding.integration_manifest_digest
            != observed_binding.integration_manifest_digest
        {
            return Err(EvidenceAssemblyStoreError::IntegrationManifestMismatch);
        }
        if expected_binding != observed_binding {
            return Err(EvidenceAssemblyStoreError::StaleOrderingProof);
        }
        let proof = Self {
            timeline: expected_timeline,
            catalog_binding: expected_binding,
            ordered_occurrence_ids,
            source_orders,
        };
        proof.validate()?;
        Ok(proof)
    }

    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.timeline.validate()?;
        self.catalog_binding.validate()?;
        validate_member_count(self.ordered_occurrence_ids.len())?;
        if self.ordered_occurrence_ids.len() != self.source_orders.len() {
            return Err(EvidenceAssemblyStoreError::IncomparableSourceOrder);
        }
        ensure_unique(
            &self.ordered_occurrence_ids,
            "verified ordering occurrence ids",
        )?;
        if self
            .source_orders
            .windows(2)
            .any(|pair| pair[0].checked_add(1) != Some(pair[1]))
        {
            return Err(EvidenceAssemblyStoreError::UnverifiedConsecutiveness);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSpanRunV1 {
    pub assembly_ordinal: u64,
    pub timeline: SourceTimelineKeyV1,
    pub ordering_proof: VerifiedSourceOrderingProofV1,
    pub timeline_digest: ManifestDigest,
    pub first_source_order: u64,
    pub last_source_order: u64,
    pub occurrence_ids: Vec<SourceOccurrenceId>,
}

impl EvidenceSpanRunV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.timeline.validate()?;
        self.ordering_proof.validate()?;
        self.timeline_digest.validate().map_err(invalid)?;
        validate_member_count(self.occurrence_ids.len())?;
        let expected_last = self
            .first_source_order
            .checked_add(u64::try_from(self.occurrence_ids.len() - 1).map_err(invalid)?)
            .ok_or_else(|| invalid("evidence span source order overflow"))?;
        if self.last_source_order != expected_last {
            return Err(invalid("evidence span run adjacency"));
        }
        if self.timeline_digest != self.timeline.digest()?
            || self.ordering_proof.timeline != self.timeline
            || self.ordering_proof.ordered_occurrence_ids != self.occurrence_ids
            || self.ordering_proof.source_orders.first().copied() != Some(self.first_source_order)
            || self.ordering_proof.source_orders.last().copied() != Some(self.last_source_order)
        {
            return Err(EvidenceAssemblyStoreError::UnverifiedConsecutiveness);
        }
        ensure_unique(&self.occurrence_ids, "evidence span run occurrences")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSpanHorizonV1 {
    pub knowledge_through: UtcMicros,
    pub valid_through: Option<UtcMicros>,
    pub contains_unknown_valid_time: bool,
}

impl EvidenceSpanHorizonV1 {
    pub fn validate_members(
        &self,
        members: &[EvidenceSourceOccurrenceRecordV1],
    ) -> EvidenceAssemblyStoreResult<()> {
        validate_member_count(members.len())?;
        let max_knowledge = members
            .iter()
            .map(|member| member.knowledge_time)
            .max_by_key(|time| time.0)
            .ok_or(EvidenceAssemblyStoreError::HorizonMismatch)?;
        let known_valid = members
            .iter()
            .filter_map(|member| member.valid_time)
            .max_by_key(|time| time.0);
        let has_unknown = members.iter().any(|member| member.valid_time.is_none());
        if self.knowledge_through.0 < max_knowledge.0
            || self.contains_unknown_valid_time != has_unknown
            || match (self.valid_through, known_valid) {
                (Some(bound), Some(maximum)) => bound.0 < maximum.0,
                (None, Some(_)) => true,
                _ => false,
            }
        {
            return Err(EvidenceAssemblyStoreError::HorizonMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSpanRecordV1 {
    pub span_id: EvidenceSpanIdV1,
    pub anchor: RetrievalAnchorRecordV3,
    pub owner: AnchorOwnerBindingV1,
    pub occurrence_set_id: CanonicalSourceOccurrenceSetIdV1,
    pub runs: Vec<EvidenceSpanRunV1>,
    pub exact_source_anchors: Vec<RetrievalAnchorId>,
    pub projector_version: ComponentVersion,
    pub horizon: EvidenceSpanHorizonV1,
    pub catalog_binding: EvidenceSpanCatalogBindingV1,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSpanIdentityProjectionV1 {
    pub owner: AnchorOwnerBindingV1,
    pub occurrence_set_id: CanonicalSourceOccurrenceSetIdV1,
    pub ordered_runs: Vec<EvidenceSpanRunV1>,
    pub exact_source_anchors: Vec<RetrievalAnchorId>,
    pub projector_version: ComponentVersion,
    pub horizon: EvidenceSpanHorizonV1,
    pub catalog_binding: EvidenceSpanCatalogBindingV1,
}

impl EvidenceSpanIdentityProjectionV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.owner.validate().map_err(invalid)?;
        self.occurrence_set_id.validate().map_err(invalid)?;
        self.projector_version.validate().map_err(invalid)?;
        self.catalog_binding.validate()?;
        validate_member_count(self.ordered_runs.len())?;
        for (ordinal, run) in self.ordered_runs.iter().enumerate() {
            run.validate()?;
            if run.assembly_ordinal != u64::try_from(ordinal).map_err(invalid)? {
                return Err(invalid("evidence span run order"));
            }
        }
        let occurrence_count = self
            .ordered_runs
            .iter()
            .map(|run| run.occurrence_ids.len())
            .sum::<usize>();
        validate_member_count(occurrence_count)?;
        if self.exact_source_anchors.len() != occurrence_count {
            return Err(invalid("evidence span exact source cardinality"));
        }
        for run in &self.ordered_runs {
            match (
                self.catalog_binding.source_capability(),
                Some(&run.ordering_proof.catalog_binding),
            ) {
                (Some(expected), Some(observed)) if expected == observed => {}
                (None, _) if run.occurrence_ids.len() == 1 => {}
                (Some(_), _) => return Err(EvidenceAssemblyStoreError::CatalogMismatch),
                (None, _) => return Err(EvidenceAssemblyStoreError::UnverifiedConsecutiveness),
            }
        }
        Ok(())
    }
}

pub fn derive_evidence_span_id_v1(
    projection: &EvidenceSpanIdentityProjectionV1,
) -> EvidenceAssemblyStoreResult<EvidenceSpanIdV1> {
    projection.validate()?;
    let digest = canonical_identity_digest(EVIDENCE_SPAN_ID_DOMAIN_V1, projection)?;
    EvidenceSpanIdV1::new(digest.as_str()).map_err(invalid)
}

impl EvidenceSpanRecordV1 {
    pub fn identity_projection(&self) -> EvidenceSpanIdentityProjectionV1 {
        EvidenceSpanIdentityProjectionV1 {
            owner: self.owner.clone(),
            occurrence_set_id: self.occurrence_set_id.clone(),
            ordered_runs: self.runs.clone(),
            exact_source_anchors: self.exact_source_anchors.clone(),
            projector_version: self.projector_version.clone(),
            horizon: self.horizon.clone(),
            catalog_binding: self.catalog_binding.clone(),
        }
    }

    pub fn ordered_occurrence_ids(&self) -> Vec<SourceOccurrenceId> {
        self.runs
            .iter()
            .flat_map(|run| run.occurrence_ids.iter().cloned())
            .collect()
    }

    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.owner.validate().map_err(invalid)?;
        self.projector_version.validate().map_err(invalid)?;
        self.catalog_binding.validate()?;
        self.anchor.validate().map_err(invalid)?;
        match self.anchor.target() {
            RetrievalAnchorTargetV3::ExactEvidenceSpan(target) if target == &self.span_id => {}
            _ => return Err(invalid("evidence span anchor target")),
        }
        if self.anchor.owner() != &self.owner {
            return Err(invalid("evidence span anchor owner"));
        }
        validate_member_count(self.runs.len())?;
        for (ordinal, run) in self.runs.iter().enumerate() {
            run.validate()?;
            if run.assembly_ordinal != u64::try_from(ordinal).map_err(invalid)? {
                return Err(invalid("evidence span run order"));
            }
        }
        let occurrences = self.ordered_occurrence_ids();
        validate_member_count(occurrences.len())?;
        ensure_unique(&occurrences, "evidence span occurrences")?;
        if self.exact_source_anchors.len() != occurrences.len() {
            return Err(invalid("evidence span exact source cardinality"));
        }
        if self.span_id != derive_evidence_span_id_v1(&self.identity_projection())? {
            return Err(invalid("evidence span identity"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSpanMemberReceiptBindingV1 {
    pub occurrence_id: SourceOccurrenceId,
    pub sanitization: SourceOccurrenceSanitizationV1,
}

impl EvidenceSpanMemberReceiptBindingV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.occurrence_id.validate().map_err(invalid)?;
        self.sanitization.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSpanProjectionReceiptV1 {
    pub projection_receipt_id: EvidenceSpanProjectionReceiptIdV1,
    pub span_id: EvidenceSpanIdV1,
    pub projector_snapshot: String,
    pub projection_generation: ProjectionGenerationId,
    pub projection_watermark: VectorWatermark,
    pub source_watermark: ManifestDigest,
    pub member_receipts: Vec<EvidenceSpanMemberReceiptBindingV1>,
    pub ordered_occurrence_ids: Vec<SourceOccurrenceId>,
    pub exact_source_anchors: Vec<RetrievalAnchorId>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSpanProjectionReceiptIdentityProjectionV1 {
    pub span_id: EvidenceSpanIdV1,
    pub projector_snapshot: String,
    pub projection_generation: ProjectionGenerationId,
    pub projection_watermark: VectorWatermark,
    pub source_watermark: ManifestDigest,
    pub member_receipts: Vec<EvidenceSpanMemberReceiptBindingV1>,
    pub ordered_occurrence_ids: Vec<SourceOccurrenceId>,
    pub exact_source_anchors: Vec<RetrievalAnchorId>,
}

impl EvidenceSpanProjectionReceiptIdentityProjectionV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        EvidenceSpanIdV1::new(self.span_id.as_str()).map_err(invalid)?;
        validate_label(&self.projector_snapshot, "evidence projector snapshot")?;
        self.projection_generation.validate().map_err(invalid)?;
        self.source_watermark.validate().map_err(invalid)?;
        validate_member_count(self.ordered_occurrence_ids.len())?;
        if self.ordered_occurrence_ids.len() != self.exact_source_anchors.len()
            || self.member_receipts.len() != self.ordered_occurrence_ids.len()
        {
            return Err(invalid("evidence projection receipt cardinality"));
        }
        for binding in &self.member_receipts {
            binding.validate()?;
        }
        if self
            .member_receipts
            .iter()
            .map(|binding| &binding.occurrence_id)
            .ne(self.ordered_occurrence_ids.iter())
        {
            return Err(EvidenceAssemblyStoreError::ReceiptRoleMismatch);
        }
        ensure_unique(
            &self.ordered_occurrence_ids,
            "evidence projection receipt occurrences",
        )?;
        Ok(())
    }
}

pub fn derive_evidence_span_projection_receipt_id_v1(
    projection: &EvidenceSpanProjectionReceiptIdentityProjectionV1,
) -> EvidenceAssemblyStoreResult<EvidenceSpanProjectionReceiptIdV1> {
    projection.validate()?;
    let digest = canonical_identity_digest(PROJECTION_RECEIPT_ID_DOMAIN_V1, projection)?;
    EvidenceSpanProjectionReceiptIdV1::new(digest.as_str()).map_err(invalid)
}

impl EvidenceSpanProjectionReceiptV1 {
    pub fn identity_projection(&self) -> EvidenceSpanProjectionReceiptIdentityProjectionV1 {
        EvidenceSpanProjectionReceiptIdentityProjectionV1 {
            span_id: self.span_id.clone(),
            projector_snapshot: self.projector_snapshot.clone(),
            projection_generation: self.projection_generation.clone(),
            projection_watermark: self.projection_watermark.clone(),
            source_watermark: self.source_watermark.clone(),
            member_receipts: self.member_receipts.clone(),
            ordered_occurrence_ids: self.ordered_occurrence_ids.clone(),
            exact_source_anchors: self.exact_source_anchors.clone(),
        }
    }

    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.projection_receipt_id.validate().map_err(invalid)?;
        self.identity_projection().validate()?;
        if self.projection_receipt_id
            != derive_evidence_span_projection_receipt_id_v1(&self.identity_projection())?
        {
            return Err(invalid("evidence projection receipt identity"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrieverIdentityV1 {
    pub capability_id: CapabilityId,
    pub component_version: ComponentVersion,
}

impl RetrieverIdentityV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.capability_id.validate().map_err(invalid)?;
        self.component_version.validate().map_err(invalid)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrivacyBoundRequestEnvelopeV1 {
    pub use_case_id: UseCaseId,
    pub scope_resolution_id: ScopeResolutionId,
    pub temporal_mode: TemporalModeV1,
    pub horizon: EvidenceSpanHorizonV1,
    pub requested_capabilities: Vec<CapabilityId>,
}

impl PrivacyBoundRequestEnvelopeV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.use_case_id.validate().map_err(invalid)?;
        self.scope_resolution_id.validate().map_err(invalid)?;
        if self.requested_capabilities.is_empty()
            || self
                .requested_capabilities
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            return Err(invalid("privacy-bound request capabilities"));
        }
        for capability in &self.requested_capabilities {
            capability.validate().map_err(invalid)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrivacyBoundRequestDigestV1 {
    pub privacy_domain_id: PrivacyDomainId,
    pub key_epoch: u64,
    pub digest: ManifestDigest,
}

impl PrivacyBoundRequestDigestV1 {
    pub fn derive(
        privacy_domain_id: PrivacyDomainId,
        key_epoch: u64,
        privacy_key: &[u8],
        envelope: &PrivacyBoundRequestEnvelopeV1,
    ) -> EvidenceAssemblyStoreResult<Self> {
        privacy_domain_id.validate().map_err(invalid)?;
        envelope.validate()?;
        if key_epoch == 0 || privacy_key.len() < 16 {
            return Err(EvidenceAssemblyStoreError::RequestPrivacyBindingMismatch);
        }
        let digest = keyed_canonical_digest(
            privacy_key,
            &(
                "tracedecay.privacy-bound-request.v1",
                privacy_domain_id.as_str(),
                key_epoch,
                envelope,
            ),
        )?;
        Ok(Self {
            privacy_domain_id,
            key_epoch,
            digest,
        })
    }

    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.privacy_domain_id.validate().map_err(invalid)?;
        self.digest.validate().map_err(invalid)?;
        if self.key_epoch == 0 {
            return Err(EvidenceAssemblyStoreError::RequestPrivacyBindingMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrieverWatermarkBindingV1 {
    pub source_watermark: ManifestDigest,
    pub projection_watermark: VectorWatermark,
    pub index_watermark: Option<ManifestDigest>,
    pub summary_watermark: Option<ManifestDigest>,
}

impl RetrieverWatermarkBindingV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.source_watermark.validate().map_err(invalid)?;
        if let Some(index) = &self.index_watermark {
            index.validate().map_err(invalid)?;
        }
        if let Some(summary) = &self.summary_watermark {
            summary.validate().map_err(invalid)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrieverContributionRecordV1 {
    pub contribution_id: RetrieverContributionIdV1,
    pub anchor: RetrievalAnchorRecordV3,
    pub owner: EvidenceAssemblyOwnerV1,
    pub retriever: RetrieverIdentityV1,
    pub catalog_binding: SourceCapabilityCatalogBindingV1,
    pub request_digest: PrivacyBoundRequestDigestV1,
    pub scope_resolution_id: ScopeResolutionId,
    pub temporal_mode: TemporalModeV1,
    pub watermarks: RetrieverWatermarkBindingV1,
    pub horizon: EvidenceSpanHorizonV1,
    pub occurrence_set_id: CanonicalSourceOccurrenceSetIdV1,
    pub span_id: EvidenceSpanIdV1,
    pub span_anchor_id: RetrievalAnchorId,
    pub exact_source_anchors: Vec<RetrievalAnchorId>,
    pub coverage: CoverageReportV1,
    pub created_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrieverContributionIdentityProjectionV1 {
    pub owner: EvidenceAssemblyOwnerV1,
    pub retriever: RetrieverIdentityV1,
    pub catalog_binding: SourceCapabilityCatalogBindingV1,
    pub request_digest: PrivacyBoundRequestDigestV1,
    pub scope_resolution_id: ScopeResolutionId,
    pub temporal_mode: TemporalModeV1,
    pub watermarks: RetrieverWatermarkBindingV1,
    pub horizon: EvidenceSpanHorizonV1,
    pub occurrence_set_id: CanonicalSourceOccurrenceSetIdV1,
    pub span_id: EvidenceSpanIdV1,
    pub span_anchor_id: RetrievalAnchorId,
    pub exact_source_anchors: Vec<RetrievalAnchorId>,
    pub coverage: CoverageReportV1,
}

impl RetrieverContributionIdentityProjectionV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.owner.validate()?;
        self.retriever.validate()?;
        self.catalog_binding.validate()?;
        self.request_digest.validate()?;
        self.scope_resolution_id.validate().map_err(invalid)?;
        self.watermarks.validate()?;
        self.coverage.validate().map_err(invalid)?;
        if &self.request_digest.privacy_domain_id != self.owner.owner.privacy_domain_id()
            || self.request_digest.key_epoch != self.owner.key_epoch
        {
            return Err(EvidenceAssemblyStoreError::RequestPrivacyBindingMismatch);
        }
        self.occurrence_set_id.validate().map_err(invalid)?;
        EvidenceSpanIdV1::new(self.span_id.as_str()).map_err(invalid)?;
        self.span_anchor_id.validate().map_err(invalid)?;
        validate_member_count(self.exact_source_anchors.len())?;
        Ok(())
    }
}

pub fn derive_retriever_contribution_id_v1(
    projection: &RetrieverContributionIdentityProjectionV1,
) -> EvidenceAssemblyStoreResult<RetrieverContributionIdV1> {
    projection.validate()?;
    let digest = canonical_identity_digest(RETRIEVER_CONTRIBUTION_ID_DOMAIN_V1, projection)?;
    RetrieverContributionIdV1::new(digest.as_str()).map_err(invalid)
}

impl RetrieverContributionRecordV1 {
    pub fn identity_projection(&self) -> RetrieverContributionIdentityProjectionV1 {
        RetrieverContributionIdentityProjectionV1 {
            owner: self.owner.clone(),
            retriever: self.retriever.clone(),
            catalog_binding: self.catalog_binding.clone(),
            request_digest: self.request_digest.clone(),
            scope_resolution_id: self.scope_resolution_id.clone(),
            temporal_mode: self.temporal_mode,
            watermarks: self.watermarks.clone(),
            horizon: self.horizon.clone(),
            occurrence_set_id: self.occurrence_set_id.clone(),
            span_id: self.span_id.clone(),
            span_anchor_id: self.span_anchor_id.clone(),
            exact_source_anchors: self.exact_source_anchors.clone(),
            coverage: self.coverage.clone(),
        }
    }

    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.contribution_id.validate().map_err(invalid)?;
        self.owner.validate()?;
        self.anchor.validate().map_err(invalid)?;
        match self.anchor.target() {
            RetrievalAnchorTargetV3::RetrieverContribution(target)
                if target == &self.contribution_id => {}
            _ => return Err(invalid("retriever contribution anchor target")),
        }
        if self.anchor.owner() != &self.owner.owner {
            return Err(invalid("retriever contribution anchor owner"));
        }
        validate_derived_anchor_lineage(
            &self.anchor,
            &self.owner.owner,
            std::slice::from_ref(&self.span_anchor_id),
            "retriever contribution anchor lineage",
        )?;
        self.identity_projection().validate()?;
        validate_member_count(self.exact_source_anchors.len())?;
        if self.contribution_id != derive_retriever_contribution_id_v1(&self.identity_projection())?
        {
            return Err(invalid("retriever contribution identity"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceAssemblyPublicationReceiptV1 {
    pub publication_receipt_id: EvidenceAssemblyPublicationReceiptIdV1,
    pub owner: EvidenceAssemblyOwnerV1,
    pub assembly_digest: ManifestDigest,
    pub occurrence_set_id: CanonicalSourceOccurrenceSetIdV1,
    pub span_id: EvidenceSpanIdV1,
    pub span_anchor_id: RetrievalAnchorId,
    pub contribution_id: RetrieverContributionIdV1,
    pub contribution_anchor_id: RetrievalAnchorId,
    pub projection_receipt_id: EvidenceSpanProjectionReceiptIdV1,
    pub ordered_occurrence_ids: Vec<SourceOccurrenceId>,
    pub exact_source_anchors: Vec<RetrievalAnchorId>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceAssemblyPublicationIdentityProjectionV1 {
    pub owner: EvidenceAssemblyOwnerV1,
    pub idempotency_key: EvidenceAssemblyIdempotencyKeyV1,
    pub assembly_digest: ManifestDigest,
    pub occurrence_set_id: CanonicalSourceOccurrenceSetIdV1,
    pub span_id: EvidenceSpanIdV1,
    pub span_anchor_id: RetrievalAnchorId,
    pub contribution_id: RetrieverContributionIdV1,
    pub contribution_anchor_id: RetrievalAnchorId,
    pub projection_receipt_id: EvidenceSpanProjectionReceiptIdV1,
    pub ordered_occurrence_ids: Vec<SourceOccurrenceId>,
    pub exact_source_anchors: Vec<RetrievalAnchorId>,
}

impl EvidenceAssemblyPublicationIdentityProjectionV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.owner.validate()?;
        self.idempotency_key
            .as_digest()
            .validate()
            .map_err(invalid)?;
        self.assembly_digest.validate().map_err(invalid)?;
        self.occurrence_set_id.validate().map_err(invalid)?;
        EvidenceSpanIdV1::new(self.span_id.as_str()).map_err(invalid)?;
        self.span_anchor_id.validate().map_err(invalid)?;
        self.contribution_id.validate().map_err(invalid)?;
        self.contribution_anchor_id.validate().map_err(invalid)?;
        self.projection_receipt_id.validate().map_err(invalid)?;
        if self.ordered_occurrence_ids.is_empty()
            || self.ordered_occurrence_ids.len() != self.exact_source_anchors.len()
        {
            return Err(invalid("evidence publication receipt cardinality"));
        }
        ensure_unique(
            &self.ordered_occurrence_ids,
            "evidence publication receipt occurrences",
        )?;
        Ok(())
    }
}

pub fn derive_evidence_assembly_publication_receipt_id_v1(
    projection: &EvidenceAssemblyPublicationIdentityProjectionV1,
) -> EvidenceAssemblyStoreResult<EvidenceAssemblyPublicationReceiptIdV1> {
    projection.validate()?;
    let digest = canonical_identity_digest(PUBLICATION_RECEIPT_ID_DOMAIN_V1, projection)?;
    EvidenceAssemblyPublicationReceiptIdV1::new(digest.as_str()).map_err(invalid)
}

impl EvidenceAssemblyPublicationReceiptV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.publication_receipt_id.validate().map_err(invalid)?;
        self.owner.validate()?;
        self.assembly_digest.validate().map_err(invalid)?;
        if self.ordered_occurrence_ids.is_empty()
            || self.ordered_occurrence_ids.len() != self.exact_source_anchors.len()
        {
            return Err(invalid("evidence publication receipt cardinality"));
        }
        Ok(())
    }

    pub fn identity_projection(
        &self,
        idempotency_key: EvidenceAssemblyIdempotencyKeyV1,
    ) -> EvidenceAssemblyPublicationIdentityProjectionV1 {
        EvidenceAssemblyPublicationIdentityProjectionV1 {
            owner: self.owner.clone(),
            idempotency_key,
            assembly_digest: self.assembly_digest.clone(),
            occurrence_set_id: self.occurrence_set_id.clone(),
            span_id: self.span_id.clone(),
            span_anchor_id: self.span_anchor_id.clone(),
            contribution_id: self.contribution_id.clone(),
            contribution_anchor_id: self.contribution_anchor_id.clone(),
            projection_receipt_id: self.projection_receipt_id.clone(),
            ordered_occurrence_ids: self.ordered_occurrence_ids.clone(),
            exact_source_anchors: self.exact_source_anchors.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceAssemblyWriteV1 {
    pub owner: EvidenceAssemblyOwnerV1,
    pub idempotency_key: EvidenceAssemblyIdempotencyKeyV1,
    pub occurrences: Vec<EvidenceSourceOccurrenceRecordV1>,
    pub occurrence_set: CanonicalSourceOccurrenceSetRecordV1,
    pub span: EvidenceSpanRecordV1,
    pub projection_receipt: EvidenceSpanProjectionReceiptV1,
    pub contribution: RetrieverContributionRecordV1,
    pub receipt: EvidenceAssemblyPublicationReceiptV1,
}

impl EvidenceAssemblyWriteV1 {
    pub fn validate(&self) -> EvidenceAssemblyStoreResult<()> {
        self.owner.validate()?;
        validate_member_count(self.occurrences.len())?;
        let mut occurrence_ids = Vec::with_capacity(self.occurrences.len());
        let mut source_anchors = Vec::with_capacity(self.occurrences.len());
        let mut occurrence_anchor_ids = Vec::with_capacity(self.occurrences.len());
        for occurrence in &self.occurrences {
            occurrence.validate()?;
            if occurrence.owner != self.owner.owner {
                return Err(invalid("evidence occurrence owner"));
            }
            occurrence_ids.push(occurrence.occurrence_id.clone());
            source_anchors.push(occurrence.exact_source_anchor.clone());
            occurrence_anchor_ids.push(occurrence.occurrence_anchor.anchor_id().clone());
        }
        ensure_unique(&occurrence_ids, "evidence assembly occurrences")?;
        let by_id = self
            .occurrences
            .iter()
            .map(|occurrence| (&occurrence.occurrence_id, occurrence))
            .collect::<BTreeMap<_, _>>();
        for occurrence in &self.occurrences {
            let owner_scope_matches =
                match (occurrence.owner.project_id(), &occurrence.timeline.scope) {
                    (None, ObservationScopeV1::Profile) => true,
                    (
                        Some(owner_project),
                        ObservationScopeV1::Project {
                            project_id: source_project,
                        },
                    ) => owner_project == source_project,
                    _ => false,
                };
            if !owner_scope_matches {
                return Err(invalid("source occurrence timeline owner scope"));
            }
            if let SourceOccurrenceCoordinateV1::ObservationProjection { source_range, .. } =
                &occurrence.coordinate
                && occurrence.source_order != source_range.start()
            {
                return Err(EvidenceAssemblyStoreError::IncomparableSourceOrder);
            }
            for relation in &occurrence.relations {
                if let SourceOccurrenceRelationV1::ToolResultFor {
                    invocation_occurrence_id,
                } = relation
                {
                    let Some(invocation) = by_id.get(invocation_occurrence_id) else {
                        return Err(invalid("tool result invocation occurrence"));
                    };
                    if invocation.occurrence_kind != SourceOccurrenceKindV1::ToolInvocation
                        || invocation.owner != occurrence.owner
                        || invocation.timeline != occurrence.timeline
                    {
                        return Err(invalid("tool result invocation binding"));
                    }
                }
            }
        }
        self.occurrence_set.validate()?;
        self.span.validate()?;
        self.span.horizon.validate_members(&self.occurrences)?;
        for run in &self.span.runs {
            for (ordinal, occurrence_id) in run.occurrence_ids.iter().enumerate() {
                let Some(occurrence) = by_id.get(occurrence_id) else {
                    return Err(invalid("evidence run occurrence"));
                };
                if occurrence.timeline != run.timeline
                    || run.ordering_proof.source_orders.get(ordinal).copied()
                        != Some(occurrence.source_order)
                {
                    return Err(EvidenceAssemblyStoreError::IncomparableSourceOrder);
                }
            }
        }
        self.projection_receipt.validate()?;
        self.contribution.validate()?;
        self.receipt.validate()?;
        validate_derived_anchor_lineage(
            &self.span.anchor,
            &self.owner.owner,
            &occurrence_anchor_ids,
            "evidence span anchor lineage",
        )?;
        let catalog_mismatch = self
            .span
            .catalog_binding
            .source_capability()
            .is_some_and(|binding| binding != &self.contribution.catalog_binding);
        let mut canonical_occurrences = occurrence_ids.clone();
        canonical_occurrences.sort();
        let ordered_span_occurrences = self.span.ordered_occurrence_ids();
        if self.occurrence_set.owner != self.owner.owner
            || self.span.owner != self.owner.owner
            || self.contribution.owner != self.owner
            || self.receipt.owner != self.owner
            || self.occurrence_set.members != canonical_occurrences
            || ordered_span_occurrences != occurrence_ids
            || self.span.exact_source_anchors != source_anchors
            || self.projection_receipt.span_id != self.span.span_id
            || self.projection_receipt.ordered_occurrence_ids != occurrence_ids
            || self.projection_receipt.exact_source_anchors != source_anchors
            || self
                .projection_receipt
                .member_receipts
                .iter()
                .map(|binding| &binding.sanitization)
                .ne(self
                    .occurrences
                    .iter()
                    .map(|occurrence| &occurrence.sanitization))
            || self.contribution.occurrence_set_id != self.occurrence_set.occurrence_set_id
            || self.contribution.span_id != self.span.span_id
            || self.contribution.span_anchor_id != *self.span.anchor.anchor_id()
            || self.contribution.exact_source_anchors != source_anchors
            || self.contribution.horizon != self.span.horizon
            || catalog_mismatch
            || self.receipt.occurrence_set_id != self.occurrence_set.occurrence_set_id
            || self.receipt.span_id != self.span.span_id
            || self.receipt.span_anchor_id != *self.span.anchor.anchor_id()
            || self.receipt.contribution_id != self.contribution.contribution_id
            || self.receipt.contribution_anchor_id != *self.contribution.anchor.anchor_id()
            || self.receipt.projection_receipt_id != self.projection_receipt.projection_receipt_id
            || self.receipt.ordered_occurrence_ids != occurrence_ids
            || self.receipt.exact_source_anchors != source_anchors
        {
            return Err(invalid("evidence assembly cross-record binding"));
        }
        let expected_digest = self.compute_assembly_digest()?;
        if self.receipt.assembly_digest != expected_digest {
            return Err(invalid("evidence assembly digest"));
        }
        let expected_receipt_id = derive_evidence_assembly_publication_receipt_id_v1(
            &self
                .receipt
                .identity_projection(self.idempotency_key.clone()),
        )?;
        if self.receipt.publication_receipt_id != expected_receipt_id {
            return Err(invalid("evidence assembly publication identity"));
        }
        Ok(())
    }

    pub fn compute_assembly_digest(&self) -> EvidenceAssemblyStoreResult<ManifestDigest> {
        canonical_sha256(&(
            "tracedecay.evidence-assembly.write.v1",
            &self.owner,
            &self.idempotency_key,
            &self.occurrences,
            &self.occurrence_set,
            &self.span,
            &self.projection_receipt,
            &self.contribution,
        ))
        .map_err(invalid)
    }
}

fn validate_derived_anchor_lineage(
    anchor: &RetrievalAnchorRecordV3,
    owner: &AnchorOwnerBindingV1,
    expected_sources: &[RetrievalAnchorId],
    field: &'static str,
) -> EvidenceAssemblyStoreResult<()> {
    if anchor.source_anchors().len() != expected_sources.len() {
        return Err(invalid(field));
    }
    for (ordinal, (source, expected_id)) in anchor
        .source_anchors()
        .iter()
        .zip(expected_sources)
        .enumerate()
    {
        if source.source_ordinal() != u64::try_from(ordinal).map_err(invalid)?
            || source.anchor_id() != expected_id
            || source.owner() != owner
        {
            return Err(invalid(field));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceAssemblyDrilldownPageV1 {
    pub contribution: RetrieverContributionRecordV1,
    pub span: EvidenceSpanRecordV1,
    pub occurrence_set_id: CanonicalSourceOccurrenceSetIdV1,
    pub occurrences: Vec<EvidenceSourceOccurrenceRecordV1>,
    pub next_ordinal: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceAssemblyReadOperationV1 {
    PublicationByIdempotency {
        owner: EvidenceAssemblyOwnerV1,
        idempotency_key: EvidenceAssemblyIdempotencyKeyV1,
    },
    ContributionPage {
        owner: EvidenceAssemblyOwnerV1,
        contribution_id: RetrieverContributionIdV1,
        start_ordinal: u64,
        page_size: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
// Boxing the large variant is wire-transparent but would change this public
// store-protocol API and ripple through construction/match sites.
#[allow(clippy::large_enum_variant)]
pub enum EvidenceAssemblyReadResultV1 {
    Publication(Option<EvidenceAssemblyPublicationReceiptV1>),
    ContributionPage(Option<EvidenceAssemblyDrilldownPageV1>),
}

fn validate_member_count(count: usize) -> EvidenceAssemblyStoreResult<()> {
    if count == 0 || count > MAX_EVIDENCE_ASSEMBLY_MEMBERS_V1 {
        return Err(invalid("evidence assembly member count"));
    }
    Ok(())
}

fn validate_half_open(
    start: u64,
    end: u64,
    field: &'static str,
) -> EvidenceAssemblyStoreResult<()> {
    if start >= end {
        return Err(invalid(field));
    }
    Ok(())
}

fn ensure_unique<T: Ord>(values: &[T], field: &'static str) -> EvidenceAssemblyStoreResult<()> {
    let mut seen = BTreeSet::new();
    if values.iter().any(|value| !seen.insert(value)) {
        return Err(invalid(field));
    }
    Ok(())
}

fn validate_label(value: &str, field: &'static str) -> EvidenceAssemblyStoreResult<()> {
    if !is_canonical_text_within(value, CANONICAL_TEXT_MAX_BYTES) {
        return Err(invalid(field));
    }
    Ok(())
}

fn invalid(error: impl std::fmt::Display) -> EvidenceAssemblyStoreError {
    EvidenceAssemblyStoreError::InvalidData(error.to_string())
}

fn canonical_identity_digest<T: Serialize>(
    domain: &'static str,
    projection: &T,
) -> EvidenceAssemblyStoreResult<ManifestDigest> {
    canonical_sha256(&(domain, projection)).map_err(invalid)
}

fn keyed_canonical_digest<T: Serialize>(
    key: &[u8],
    material: &T,
) -> EvidenceAssemblyStoreResult<ManifestDigest> {
    canonical_sha256(&("tracedecay.privacy-keyed-digest.v1", key, material)).map_err(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        AccessPolicyDigest, AnchorDurabilityClass, AnchorLineageRefV3, AnchorProvenanceRelationV2,
        AnchorSourceGenerationV3, BlobId, EvidenceClass, PayloadAccessState,
        PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectId, ProviderId, RepositoryId,
        ResolutionAuthorizationV1, RetentionClass, SessionId, UserProfileId,
    };

    fn owner() -> EvidenceAssemblyOwnerV1 {
        EvidenceAssemblyOwnerV1 {
            owner: AnchorOwnerBindingV1::for_project(
                UserProfileId::new("profile.fixture").unwrap(),
                ProjectId::new("project.fixture").unwrap(),
                PrivacyDomainId::new("privacy.fixture").unwrap(),
            )
            .unwrap(),
            scope_digest: ManifestDigest::new(format!("sha256:{}", "aa".repeat(32))).unwrap(),
            key_epoch: 1,
        }
    }

    fn occurrence_projection() -> SourceOccurrenceIdentityProjectionV1 {
        SourceOccurrenceIdentityProjectionV1 {
            owner: owner().owner,
            timeline: EvidenceSourceTimelineV1 {
                source: ObservationSourceIdentityV1::for_provider(
                    ProviderId::new("provider.fixture").unwrap(),
                    SessionId::new("session.fixture").unwrap(),
                )
                .unwrap(),
                scope: ObservationScopeV1::Project {
                    project_id: ProjectId::new("project.fixture").unwrap(),
                },
                source_generation: ObservationSourceGenerationV1::new(1).unwrap(),
                ordering_domain: ObservationOrderingDomainV1::DaemonSequence,
            },
            exact_source_anchor: RetrievalAnchorId::new("retrieval.source.fixture").unwrap(),
            source_order: 4,
            coordinate: SourceOccurrenceCoordinateV1::ObservationProjection {
                canonical_observation_id: CanonicalObservationIdV1::new(format!(
                    "sha256:{}",
                    "44".repeat(32)
                ))
                .unwrap(),
                source_range: ObservationSourceRangeV1::new(4, 5).unwrap(),
                projection_output_ordinal: 0,
                sanitized_byte_range: SanitizedObservationByteRangeV1::new(0, 1).unwrap(),
            },
            occurrence_kind: SourceOccurrenceKindV1::Message,
            relations: Vec::new(),
            projector_version: ComponentVersion::new("projector.v1").unwrap(),
        }
    }

    fn catalog_binding() -> SourceCapabilityCatalogBindingV1 {
        let digest = ManifestDigest::new(format!("sha256:{}", "aa".repeat(32))).unwrap();
        SourceCapabilityCatalogBindingV1 {
            connector_id: "connector.fixture".to_owned(),
            root_id: "root.fixture".to_owned(),
            capability_id: CapabilityId::new("capability.fixture").unwrap(),
            catalog_digest: digest.clone(),
            integration_manifest_digest: digest.clone(),
            configuration_digest: digest.clone(),
            authorization_scope_digest: digest.clone(),
            projector_revision: ComponentVersion::new("projector.v1").unwrap(),
            source_watermark: digest,
        }
    }

    fn retrieval_anchor(
        target: RetrievalAnchorTargetV3,
        sources: Vec<RetrievalAnchorId>,
    ) -> RetrievalAnchorRecordV3 {
        let owner = owner().owner;
        RetrievalAnchorRecordV3::new(tracedecay_domain::RetrievalAnchorRecordV3Parts {
            target,
            owner: owner.clone(),
            aliases: vec![],
            occurred_at: None,
            ingested_at: UtcMicros(1),
            evidence_class: EvidenceClass::Observed,
            source_generation: AnchorSourceGenerationV3::Unknown,
            projection_generation: ProjectionGenerationId::new("projection.fixture").unwrap(),
            projection_watermark: VectorWatermark::default(),
            coverage: CoverageReportV1::default(),
            source_observations: vec![],
            source_anchors: sources
                .into_iter()
                .enumerate()
                .map(|(ordinal, source)| {
                    AnchorLineageRefV3::new(
                        u64::try_from(ordinal).unwrap(),
                        AnchorProvenanceRelationV2::DerivedFrom,
                        source,
                        owner.clone(),
                    )
                    .unwrap()
                })
                .collect(),
            authorization: ResolutionAuthorizationV1 {
                resolved_scope_id: ScopeResolutionId::new("scope.fixture").unwrap(),
                privacy_domain_id: PrivacyDomainId::new("privacy.fixture").unwrap(),
                access_policy_digest: AccessPolicyDigest::new(format!(
                    "sha256:{}",
                    "aa".repeat(32)
                ))
                .unwrap(),
                capability_id: CapabilityId::new("capability.fixture").unwrap(),
                canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(format!(
                    "sha256:{}",
                    "bb".repeat(32)
                ))
                .unwrap(),
            },
            payload_access: PayloadAccessState::Eligible,
            retention_class: RetentionClass::new("retention.fixture").unwrap(),
            durability: AnchorDurabilityClass::DurableEvidence,
        })
        .unwrap()
    }

    #[test]
    fn privacy_bound_digests_separate_domains_epochs_and_keys() {
        let envelope = PrivacyBoundRequestEnvelopeV1 {
            use_case_id: UseCaseId::new("use-case.fixture").unwrap(),
            scope_resolution_id: ScopeResolutionId::new("scope.fixture").unwrap(),
            temporal_mode: TemporalModeV1::Current,
            horizon: EvidenceSpanHorizonV1 {
                knowledge_through: UtcMicros(1),
                valid_through: Some(UtcMicros(1)),
                contains_unknown_valid_time: false,
            },
            requested_capabilities: vec![CapabilityId::new("capability.fixture").unwrap()],
        };
        let privacy_one = PrivacyDomainId::new("privacy.fixture").unwrap();
        let privacy_two = PrivacyDomainId::new("privacy.other").unwrap();
        let key_one = b"fixture-privacy-key-one";
        let key_two = b"fixture-privacy-key-two";
        let first = PrivacyBoundRequestDigestV1::derive(privacy_one.clone(), 1, key_one, &envelope)
            .unwrap();
        assert_ne!(
            first,
            PrivacyBoundRequestDigestV1::derive(privacy_two.clone(), 1, key_one, &envelope)
                .unwrap()
        );
        assert_ne!(
            first,
            PrivacyBoundRequestDigestV1::derive(privacy_one.clone(), 2, key_one, &envelope)
                .unwrap()
        );
        assert_ne!(
            first,
            PrivacyBoundRequestDigestV1::derive(privacy_one, 1, key_two, &envelope).unwrap()
        );

        let owner_one = owner().owner;
        let owner_two = AnchorOwnerBindingV1::for_project(
            UserProfileId::new("profile.fixture").unwrap(),
            ProjectId::new("project.fixture").unwrap(),
            privacy_two,
        )
        .unwrap();
        assert_ne!(
            EvidenceAssemblyIdempotencyKeyV1::derive(&owner_one, 1, key_one, b"caller-key")
                .unwrap(),
            EvidenceAssemblyIdempotencyKeyV1::derive(&owner_two, 1, key_one, b"caller-key")
                .unwrap()
        );
    }

    #[test]
    fn occurrence_identity_is_deterministic_and_rekeys_immutable_material() {
        let projection = occurrence_projection();
        let replay = derive_source_occurrence_id_v1(&projection).unwrap();
        assert_eq!(replay, derive_source_occurrence_id_v1(&projection).unwrap());

        let mut changed = projection;
        changed.projector_version = ComponentVersion::new("projector.v2").unwrap();
        assert_ne!(replay, derive_source_occurrence_id_v1(&changed).unwrap());
    }

    #[test]
    fn occurrence_anchor_binds_exact_lineage() {
        let projection = occurrence_projection();
        let occurrence_id = derive_source_occurrence_id_v1(&projection).unwrap();
        let anchor = retrieval_anchor(
            RetrievalAnchorTargetV3::ExactSourceOccurrence(occurrence_id),
            vec![projection.exact_source_anchor.clone()],
        );
        validate_derived_anchor_lineage(
            &anchor,
            &projection.owner,
            std::slice::from_ref(&projection.exact_source_anchor),
            "test occurrence lineage",
        )
        .unwrap();
        assert!(
            validate_derived_anchor_lineage(
                &anchor,
                &projection.owner,
                &[RetrievalAnchorId::new("retrieval.other.fixture").unwrap()],
                "test occurrence lineage",
            )
            .is_err()
        );
    }

    #[test]
    fn occurrence_set_identity_requires_canonical_membership_order() {
        let first = SourceOccurrenceId::new(format!("sha256:{}", "11".repeat(32))).unwrap();
        let second = SourceOccurrenceId::new(format!("sha256:{}", "22".repeat(32))).unwrap();
        let canonical = CanonicalSourceOccurrenceSetIdentityProjectionV1 {
            owner: owner().owner,
            canonical_members: vec![first.clone(), second.clone()],
        };
        assert!(
            derive_canonical_source_occurrence_set_id_v1(&canonical)
                .unwrap()
                .as_str()
                .starts_with("sha256:")
        );
        assert!(matches!(
            derive_canonical_source_occurrence_set_id_v1(
                &CanonicalSourceOccurrenceSetIdentityProjectionV1 {
                    owner: owner().owner,
                    canonical_members: vec![second, first],
                }
            ),
            Err(EvidenceAssemblyStoreError::InvalidData(_))
        ));
    }

    #[test]
    fn mixed_message_tool_and_code_runs_reject_order_and_kind_lookalikes() {
        let message_projection = occurrence_projection();
        let message_id = derive_source_occurrence_id_v1(&message_projection).unwrap();

        let mut invocation_projection = occurrence_projection();
        invocation_projection.source_order = 5;
        invocation_projection.coordinate = observation_coordinate(5, "55");
        invocation_projection.occurrence_kind = SourceOccurrenceKindV1::ToolInvocation;
        let invocation_id = derive_source_occurrence_id_v1(&invocation_projection).unwrap();

        let mut result_projection = occurrence_projection();
        result_projection.source_order = 6;
        result_projection.coordinate = observation_coordinate(6, "66");
        result_projection.occurrence_kind = SourceOccurrenceKindV1::ToolResult;
        result_projection.relations = vec![SourceOccurrenceRelationV1::ToolResultFor {
            invocation_occurrence_id: invocation_id.clone(),
        }];
        let result_id = derive_source_occurrence_id_v1(&result_projection).unwrap();

        let code_timeline = SourceTimelineKeyV1 {
            source: ObservationSourceIdentityV1::for_provider(
                ProviderId::new("git.fixture").unwrap(),
                SessionId::new("capture.fixture").unwrap(),
            )
            .unwrap(),
            scope: ObservationScopeV1::Project {
                project_id: ProjectId::new("project.fixture").unwrap(),
            },
            source_generation: ObservationSourceGenerationV1::new(2).unwrap(),
            ordering_domain: ObservationOrderingDomainV1::FileBytes,
        };
        let code_projection = SourceOccurrenceIdentityProjectionV1 {
            owner: owner().owner,
            timeline: code_timeline.clone(),
            exact_source_anchor: RetrievalAnchorId::new("retrieval.code.fixture").unwrap(),
            source_order: 0,
            coordinate: SourceOccurrenceCoordinateV1::ImmutableBlobSlice {
                repository_id: RepositoryId::new("repository.fixture").unwrap(),
                blob_id: BlobId::new("blob.fixture").unwrap(),
                byte_start: 0,
                byte_end: 8,
            },
            occurrence_kind: SourceOccurrenceKindV1::CodeChunk,
            relations: Vec::new(),
            projector_version: ComponentVersion::new("projector.v1").unwrap(),
        };
        let code_id = derive_source_occurrence_id_v1(&code_projection).unwrap();

        let observation_ids = vec![message_id.clone(), invocation_id.clone(), result_id.clone()];
        let observation_run = EvidenceSpanRunV1 {
            assembly_ordinal: 0,
            timeline: message_projection.timeline.clone(),
            ordering_proof: VerifiedSourceOrderingProofV1::verify(
                message_projection.timeline.clone(),
                catalog_binding(),
                catalog_binding(),
                observation_ids.clone(),
                vec![4, 5, 6],
            )
            .unwrap(),
            timeline_digest: message_projection.timeline.digest().unwrap(),
            first_source_order: 4,
            last_source_order: 6,
            occurrence_ids: observation_ids,
        };
        let code_run = EvidenceSpanRunV1 {
            assembly_ordinal: 1,
            timeline: code_timeline.clone(),
            ordering_proof: VerifiedSourceOrderingProofV1::verify(
                code_timeline.clone(),
                catalog_binding(),
                catalog_binding(),
                vec![code_id.clone()],
                vec![0],
            )
            .unwrap(),
            timeline_digest: code_timeline.digest().unwrap(),
            first_source_order: 0,
            last_source_order: 0,
            occurrence_ids: vec![code_id.clone()],
        };
        let occurrence_set_id = derive_canonical_source_occurrence_set_id_v1(
            &CanonicalSourceOccurrenceSetIdentityProjectionV1 {
                owner: owner().owner,
                canonical_members: {
                    let mut ids = vec![
                        message_id.clone(),
                        invocation_id.clone(),
                        result_id,
                        code_id,
                    ];
                    ids.sort();
                    ids
                },
            },
        )
        .unwrap();
        let observation_anchor = RetrievalAnchorId::new("retrieval.source.fixture").unwrap();
        let projection = EvidenceSpanIdentityProjectionV1 {
            owner: owner().owner,
            occurrence_set_id,
            ordered_runs: vec![observation_run, code_run],
            exact_source_anchors: vec![
                observation_anchor.clone(),
                observation_anchor.clone(),
                observation_anchor,
                RetrievalAnchorId::new("retrieval.code.fixture").unwrap(),
            ],
            projector_version: ComponentVersion::new("projector.v1").unwrap(),
            horizon: EvidenceSpanHorizonV1 {
                knowledge_through: UtcMicros(7),
                valid_through: Some(UtcMicros(7)),
                contains_unknown_valid_time: false,
            },
            catalog_binding: EvidenceSpanCatalogBindingV1::SourceCapability {
                binding: catalog_binding(),
            },
        };
        let forward = derive_evidence_span_id_v1(&projection).unwrap();
        let mut reversed = projection;
        reversed.ordered_runs.reverse();
        for (ordinal, run) in reversed.ordered_runs.iter_mut().enumerate() {
            run.assembly_ordinal = u64::try_from(ordinal).unwrap();
        }
        assert_ne!(forward, derive_evidence_span_id_v1(&reversed).unwrap());

        let mut missing_pair = result_projection;
        missing_pair.relations.clear();
        assert!(derive_source_occurrence_id_v1(&missing_pair).is_err());
        assert!(matches!(
            VerifiedSourceOrderingProofV1::verify(
                message_projection.timeline,
                catalog_binding(),
                catalog_binding(),
                vec![message_id, invocation_id],
                vec![4, 6],
            ),
            Err(EvidenceAssemblyStoreError::UnverifiedConsecutiveness)
        ));
        let mut code_lookalike = invocation_projection;
        code_lookalike.occurrence_kind = SourceOccurrenceKindV1::CodeChunk;
        assert!(derive_source_occurrence_id_v1(&code_lookalike).is_err());
    }

    fn observation_coordinate(
        source_order: u64,
        digest_byte: &str,
    ) -> SourceOccurrenceCoordinateV1 {
        SourceOccurrenceCoordinateV1::ObservationProjection {
            canonical_observation_id: CanonicalObservationIdV1::new(format!(
                "sha256:{}",
                digest_byte.repeat(32)
            ))
            .unwrap(),
            source_range: ObservationSourceRangeV1::new(source_order, source_order + 1).unwrap(),
            projection_output_ordinal: 0,
            sanitized_byte_range: SanitizedObservationByteRangeV1::new(0, 1).unwrap(),
        }
    }
}
