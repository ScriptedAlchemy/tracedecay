#![allow(dead_code)] // production evidence-assembly authority; mounted via RegisteredGlobalDb

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_application::{
    DisclosureClass, RequestAdmission, RequestContext as ProductRequestContext,
};
use tracedecay_domain::{
    ManifestDigest, RetrievalAnchorId, RetrievalAnchorRecordV2, TemporalModeV1, UtcMicros,
    canonical_sha256,
};

use crate::daemon::store_runtime::registry::StoreRuntimeHandle;
use crate::db::engine::{Executor, QueryExecutor, Rows, Value, params};
use crate::db::{Database, DatabaseAccessMode};
use crate::global_db::RegisteredGlobalDb;

const MAX_ASSEMBLY_MEMBERS: usize = 4_096;
const OCCURRENCE_ID_DOMAIN: &str = "tracedecay.evidence.source-occurrence.v1";
const OCCURRENCE_SET_ID_DOMAIN: &str = "tracedecay.evidence.occurrence-set.v1";
const SPAN_ID_DOMAIN: &str = "tracedecay.evidence.span.v1";
const PROJECTION_RECEIPT_ID_DOMAIN: &str = "tracedecay.evidence.projection-receipt.v1";
const CONTRIBUTION_ID_DOMAIN: &str = "tracedecay.evidence.retriever-contribution.v1";
const PUBLICATION_RECEIPT_ID_DOMAIN: &str = "tracedecay.evidence.publication-receipt.v1";
const DERIVED_ANCHOR_ID_DOMAIN: &str = "tracedecay.evidence.derived-anchor.v1";

macro_rules! evidence_id {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            fn from_material<T: Serialize>(
                domain: &'static str,
                material: &T,
            ) -> Result<Self, EvidenceAssemblyError> {
                Ok(Self(derived_id(domain, material)?))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    )+};
}

evidence_id!(
    SourceOccurrenceId,
    CanonicalSourceOccurrenceSetId,
    EvidenceSpanId,
    EvidenceSpanProjectionReceiptId,
    RetrieverContributionId,
    EvidenceAssemblyPublicationReceiptId,
);

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum EvidenceAssemblyError {
    #[error("invalid evidence assembly contract: {0}")]
    Invalid(&'static str),
    #[error("evidence assembly exceeds its bounded member limit")]
    MemberLimit,
    #[error("no source evidence was selected")]
    NoEvidence,
    #[error("source evidence is not consecutive in its frozen generation")]
    NonConsecutive,
    #[error("source evidence crosses an owner, timeline, or generation boundary")]
    BoundaryMismatch,
    #[error("source anchor is unavailable: {0}")]
    SourceAnchorUnavailable(String),
    #[error("evidence publication replay conflicts with existing material")]
    ReplayConflict,
    #[error("evidence target is unavailable")]
    UnauthorizedOrUnknown,
    #[error("session retrieval did not return usable evidence")]
    SessionRetrievalUnavailable,
    #[error("evidence serialization failed: {0}")]
    Serialization(String),
    #[error("evidence storage failed: {0}")]
    Storage(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceOwnerBinding {
    project_id: String,
    scope_digest: String,
    privacy_domain_id: String,
    key_epoch: u64,
}

impl EvidenceOwnerBinding {
    pub fn for_feedback_context(
        context: &ProductRequestContext,
        privacy_domain_id: impl Into<String>,
        key_epoch: u64,
    ) -> Result<Self, EvidenceAssemblyError> {
        let privacy_domain_id = privacy_domain_id.into();
        validate_label(&privacy_domain_id, "privacy domain")?;
        if key_epoch == 0 {
            return Err(EvidenceAssemblyError::Invalid("privacy key epoch"));
        }
        Ok(Self {
            project_id: context.scope().project_id.as_str().to_owned(),
            scope_digest: context.scope().scope_digest.as_str().to_owned(),
            privacy_domain_id,
            key_epoch,
        })
    }

    fn validate_feedback_context(
        &self,
        context: &ProductRequestContext,
    ) -> Result<(), EvidenceAssemblyError> {
        if self.project_id != context.scope().project_id.as_str()
            || self.scope_digest != context.scope().scope_digest.as_str()
            || context.grant().disclosure < DisclosureClass::Evidence
        {
            return Err(EvidenceAssemblyError::UnauthorizedOrUnknown);
        }
        Ok(())
    }

    fn digest(&self) -> Result<ManifestDigest, EvidenceAssemblyError> {
        canonical_sha256(self).map_err(contract_error)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceTimelineKey {
    provider_id: String,
    source_id: String,
    source_generation: String,
    ordering_domain: String,
}

impl SourceTimelineKey {
    pub fn new(
        provider_id: impl Into<String>,
        source_id: impl Into<String>,
        source_generation: impl Into<String>,
        ordering_domain: impl Into<String>,
    ) -> Result<Self, EvidenceAssemblyError> {
        let timeline = Self {
            provider_id: provider_id.into(),
            source_id: source_id.into(),
            source_generation: source_generation.into(),
            ordering_domain: ordering_domain.into(),
        };
        for (value, field) in [
            (&timeline.provider_id, "provider id"),
            (&timeline.source_id, "source id"),
            (&timeline.source_generation, "source generation"),
            (&timeline.ordering_domain, "ordering domain"),
        ] {
            validate_label(value, field)?;
        }
        Ok(timeline)
    }

    fn digest(&self) -> Result<ManifestDigest, EvidenceAssemblyError> {
        canonical_sha256(self).map_err(contract_error)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceOccurrenceKind {
    SessionOccurrence,
    FeedbackFinding,
    FeedbackImpact,
    CodeChunk,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceOccurrenceCoordinate {
    observation_sequence: u64,
    projection_output_ordinal: u64,
    source_order: u64,
}

impl SourceOccurrenceCoordinate {
    pub const fn new(
        observation_sequence: u64,
        projection_output_ordinal: u64,
        source_order: u64,
    ) -> Self {
        Self {
            observation_sequence,
            projection_output_ordinal,
            source_order,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceOccurrenceRecord {
    occurrence_id: SourceOccurrenceId,
    owner: EvidenceOwnerBinding,
    timeline: SourceTimelineKey,
    exact_source_anchor: RetrievalAnchorId,
    canonical_observation_id: Option<String>,
    coordinate: SourceOccurrenceCoordinate,
    kind: SourceOccurrenceKind,
    projector_revision: String,
}

impl SourceOccurrenceRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: EvidenceOwnerBinding,
        timeline: SourceTimelineKey,
        exact_source_anchor: RetrievalAnchorId,
        canonical_observation_id: Option<String>,
        coordinate: SourceOccurrenceCoordinate,
        kind: SourceOccurrenceKind,
        projector_revision: impl Into<String>,
    ) -> Result<Self, EvidenceAssemblyError> {
        exact_source_anchor
            .validate()
            .map_err(|_| EvidenceAssemblyError::Invalid("exact source anchor"))?;
        let projector_revision = projector_revision.into();
        validate_label(&projector_revision, "projector revision")?;
        if let Some(observation_id) = &canonical_observation_id {
            validate_label(observation_id, "canonical observation id")?;
        }
        let identity = (
            OCCURRENCE_ID_DOMAIN,
            &owner,
            &timeline,
            &exact_source_anchor,
            &canonical_observation_id,
            &coordinate,
            kind,
            &projector_revision,
        );
        Ok(Self {
            occurrence_id: SourceOccurrenceId::from_material(OCCURRENCE_ID_DOMAIN, &identity)?,
            owner,
            timeline,
            exact_source_anchor,
            canonical_observation_id,
            coordinate,
            kind,
            projector_revision,
        })
    }

    pub fn occurrence_id(&self) -> &SourceOccurrenceId {
        &self.occurrence_id
    }

    pub fn exact_source_anchor(&self) -> &RetrievalAnchorId {
        &self.exact_source_anchor
    }

    pub const fn source_order(&self) -> u64 {
        self.coordinate.source_order
    }

    fn record_digest(&self) -> Result<ManifestDigest, EvidenceAssemblyError> {
        canonical_sha256(self).map_err(contract_error)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalSourceOccurrenceSet {
    occurrence_set_id: CanonicalSourceOccurrenceSetId,
    owner: EvidenceOwnerBinding,
    members: Vec<SourceOccurrenceRecord>,
}

impl CanonicalSourceOccurrenceSet {
    pub fn new(
        owner: EvidenceOwnerBinding,
        mut members: Vec<SourceOccurrenceRecord>,
    ) -> Result<Self, EvidenceAssemblyError> {
        if members.is_empty() {
            return Err(EvidenceAssemblyError::NoEvidence);
        }
        if members.len() > MAX_ASSEMBLY_MEMBERS {
            return Err(EvidenceAssemblyError::MemberLimit);
        }
        if members.iter().any(|member| member.owner != owner) {
            return Err(EvidenceAssemblyError::BoundaryMismatch);
        }
        members.sort_by(|left, right| left.occurrence_id.cmp(&right.occurrence_id));
        if members
            .windows(2)
            .any(|pair| pair[0].occurrence_id == pair[1].occurrence_id)
        {
            return Err(EvidenceAssemblyError::Invalid(
                "duplicate source occurrence",
            ));
        }
        let member_ids = members
            .iter()
            .map(|member| &member.occurrence_id)
            .collect::<Vec<_>>();
        let occurrence_set_id = CanonicalSourceOccurrenceSetId::from_material(
            OCCURRENCE_SET_ID_DOMAIN,
            &(OCCURRENCE_SET_ID_DOMAIN, &owner, member_ids),
        )?;
        Ok(Self {
            occurrence_set_id,
            owner,
            members,
        })
    }

    pub fn occurrence_set_id(&self) -> &CanonicalSourceOccurrenceSetId {
        &self.occurrence_set_id
    }

    pub fn members(&self) -> &[SourceOccurrenceRecord] {
        &self.members
    }

    fn record_digest(&self) -> Result<ManifestDigest, EvidenceAssemblyError> {
        canonical_sha256(self).map_err(contract_error)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSpanProducerKind {
    SessionSpan,
    SessionBurst,
    FeedbackCycle,
    Generic,
}

impl EvidenceSpanProducerKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::SessionSpan => "session_span",
            Self::SessionBurst => "session_burst",
            Self::FeedbackCycle => "feedback_cycle",
            Self::Generic => "generic",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceOrderingProof {
    timeline_digest: ManifestDigest,
    source_generation: String,
    first_source_order: u64,
    last_source_order: u64,
    ordered_member_digest: ManifestDigest,
    adjacency_revision: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSpanRun {
    assembly_ordinal: u64,
    timeline: SourceTimelineKey,
    proof: EvidenceOrderingProof,
    occurrence_ids: Vec<SourceOccurrenceId>,
}

impl EvidenceSpanRun {
    fn verify(
        assembly_ordinal: u64,
        members: &[SourceOccurrenceRecord],
        adjacency_revision: &str,
    ) -> Result<Self, EvidenceAssemblyError> {
        if members.is_empty() {
            return Err(EvidenceAssemblyError::NoEvidence);
        }
        validate_label(adjacency_revision, "adjacency revision")?;
        let timeline = members[0].timeline.clone();
        if members.iter().any(|member| member.timeline != timeline) {
            return Err(EvidenceAssemblyError::BoundaryMismatch);
        }
        if members.windows(2).any(|pair| {
            pair[0]
                .source_order()
                .checked_add(1)
                .is_none_or(|next| next != pair[1].source_order())
        }) {
            return Err(EvidenceAssemblyError::NonConsecutive);
        }
        let occurrence_ids = members
            .iter()
            .map(|member| member.occurrence_id.clone())
            .collect::<Vec<_>>();
        let ordered_member_digest = canonical_sha256(&occurrence_ids).map_err(contract_error)?;
        Ok(Self {
            assembly_ordinal,
            timeline: timeline.clone(),
            proof: EvidenceOrderingProof {
                timeline_digest: timeline.digest()?,
                source_generation: timeline.source_generation.clone(),
                first_source_order: members[0].source_order(),
                last_source_order: members[members.len() - 1].source_order(),
                ordered_member_digest,
                adjacency_revision: adjacency_revision.to_owned(),
            },
            occurrence_ids,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceAssemblyCoverage {
    eligible: u64,
    selected: u64,
    omitted: u64,
    hidden: u64,
    unknown: u64,
    redacted: u64,
    complete: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSpanRecord {
    span_id: EvidenceSpanId,
    anchor_id: RetrievalAnchorId,
    owner: EvidenceOwnerBinding,
    occurrence_set_id: CanonicalSourceOccurrenceSetId,
    runs: Vec<EvidenceSpanRun>,
    producer_kind: EvidenceSpanProducerKind,
    producer_revision: String,
    coverage: EvidenceAssemblyCoverage,
    exact_source_anchors: Vec<RetrievalAnchorId>,
}

impl EvidenceSpanRecord {
    #[allow(clippy::too_many_arguments)]
    fn new(
        owner: EvidenceOwnerBinding,
        occurrence_set: &CanonicalSourceOccurrenceSet,
        runs: Vec<EvidenceSpanRun>,
        producer_kind: EvidenceSpanProducerKind,
        producer_revision: impl Into<String>,
        coverage: EvidenceAssemblyCoverage,
        ordered_occurrences: &[SourceOccurrenceRecord],
    ) -> Result<Self, EvidenceAssemblyError> {
        if runs.is_empty() {
            return Err(EvidenceAssemblyError::NoEvidence);
        }
        let producer_revision = producer_revision.into();
        validate_label(&producer_revision, "span producer revision")?;
        let flattened = runs
            .iter()
            .flat_map(|run| run.occurrence_ids.iter().cloned())
            .collect::<Vec<_>>();
        let mut flattened_canonical = flattened.clone();
        flattened_canonical.sort();
        let set_ids = occurrence_set
            .members
            .iter()
            .map(|member| member.occurrence_id.clone())
            .collect::<Vec<_>>();
        if flattened.len() != set_ids.len() || flattened_canonical != set_ids {
            return Err(EvidenceAssemblyError::Invalid(
                "span and occurrence set membership",
            ));
        }
        let exact_source_anchors = ordered_occurrences
            .iter()
            .map(|member| member.exact_source_anchor.clone())
            .collect::<Vec<_>>();
        let identity = (
            SPAN_ID_DOMAIN,
            &owner,
            &occurrence_set.occurrence_set_id,
            &runs,
            producer_kind,
            &producer_revision,
            &coverage,
            &exact_source_anchors,
        );
        let span_id = EvidenceSpanId::from_material(SPAN_ID_DOMAIN, &identity)?;
        let anchor_id = derived_anchor_id("evidence_span", span_id.as_str(), &owner)?;
        Ok(Self {
            span_id,
            anchor_id,
            owner,
            occurrence_set_id: occurrence_set.occurrence_set_id.clone(),
            runs,
            producer_kind,
            producer_revision,
            coverage,
            exact_source_anchors,
        })
    }

    pub fn span_id(&self) -> &EvidenceSpanId {
        &self.span_id
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        &self.anchor_id
    }

    fn record_digest(&self) -> Result<ManifestDigest, EvidenceAssemblyError> {
        canonical_sha256(self).map_err(contract_error)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSpanProjectionReceipt {
    projection_receipt_id: EvidenceSpanProjectionReceiptId,
    span_id: EvidenceSpanId,
    projector_snapshot: String,
    member_occurrence_ids: Vec<SourceOccurrenceId>,
    member_source_anchors: Vec<RetrievalAnchorId>,
}

impl EvidenceSpanProjectionReceipt {
    fn new(
        span: &EvidenceSpanRecord,
        projector_snapshot: impl Into<String>,
        ordered_occurrences: &[SourceOccurrenceRecord],
    ) -> Result<Self, EvidenceAssemblyError> {
        let projector_snapshot = projector_snapshot.into();
        validate_label(&projector_snapshot, "projector snapshot")?;
        let member_occurrence_ids = ordered_occurrences
            .iter()
            .map(|member| member.occurrence_id.clone())
            .collect::<Vec<_>>();
        let member_source_anchors = ordered_occurrences
            .iter()
            .map(|member| member.exact_source_anchor.clone())
            .collect::<Vec<_>>();
        let identity = (
            PROJECTION_RECEIPT_ID_DOMAIN,
            &span.span_id,
            &projector_snapshot,
            &member_occurrence_ids,
            &member_source_anchors,
        );
        Ok(Self {
            projection_receipt_id: EvidenceSpanProjectionReceiptId::from_material(
                PROJECTION_RECEIPT_ID_DOMAIN,
                &identity,
            )?,
            span_id: span.span_id.clone(),
            projector_snapshot,
            member_occurrence_ids,
            member_source_anchors,
        })
    }

    fn record_digest(&self) -> Result<ManifestDigest, EvidenceAssemblyError> {
        canonical_sha256(self).map_err(contract_error)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrieverWatermarkBinding {
    source: String,
    projection: String,
    index: String,
    summary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrieverContributionRecord {
    contribution_id: RetrieverContributionId,
    anchor_id: RetrievalAnchorId,
    owner: EvidenceOwnerBinding,
    retriever_id: String,
    component_version: String,
    request_digest: String,
    temporal_mode: TemporalModeV1,
    watermarks: RetrieverWatermarkBinding,
    occurrence_set_id: CanonicalSourceOccurrenceSetId,
    span_id: EvidenceSpanId,
    span_anchor_id: RetrievalAnchorId,
    exact_source_anchors: Vec<RetrievalAnchorId>,
    coverage: EvidenceAssemblyCoverage,
    created_at: UtcMicros,
}

impl RetrieverContributionRecord {
    #[allow(clippy::too_many_arguments)]
    fn new(
        owner: EvidenceOwnerBinding,
        retriever_id: impl Into<String>,
        component_version: impl Into<String>,
        request_digest: String,
        temporal_mode: TemporalModeV1,
        watermarks: RetrieverWatermarkBinding,
        occurrence_set: &CanonicalSourceOccurrenceSet,
        span: &EvidenceSpanRecord,
        created_at: UtcMicros,
    ) -> Result<Self, EvidenceAssemblyError> {
        let retriever_id = retriever_id.into();
        let component_version = component_version.into();
        validate_label(&retriever_id, "retriever id")?;
        validate_label(&component_version, "retriever component version")?;
        validate_label(&request_digest, "privacy-bound request digest")?;
        let identity = (
            CONTRIBUTION_ID_DOMAIN,
            &owner,
            &retriever_id,
            &component_version,
            &request_digest,
            temporal_mode,
            &watermarks,
            &occurrence_set.occurrence_set_id,
            &span.span_id,
            &span.exact_source_anchors,
            &span.coverage,
        );
        let contribution_id =
            RetrieverContributionId::from_material(CONTRIBUTION_ID_DOMAIN, &identity)?;
        let anchor_id =
            derived_anchor_id("retriever_contribution", contribution_id.as_str(), &owner)?;
        Ok(Self {
            contribution_id,
            anchor_id,
            owner,
            retriever_id,
            component_version,
            request_digest,
            temporal_mode,
            watermarks,
            occurrence_set_id: occurrence_set.occurrence_set_id.clone(),
            span_id: span.span_id.clone(),
            span_anchor_id: span.anchor_id.clone(),
            exact_source_anchors: span.exact_source_anchors.clone(),
            coverage: span.coverage.clone(),
            created_at,
        })
    }

    pub fn contribution_id(&self) -> &RetrieverContributionId {
        &self.contribution_id
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        &self.anchor_id
    }

    fn record_digest(&self) -> Result<ManifestDigest, EvidenceAssemblyError> {
        canonical_sha256(self).map_err(contract_error)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DerivedEvidenceAnchor {
    anchor_id: RetrievalAnchorId,
    owner: EvidenceOwnerBinding,
    target_kind: String,
    target_id: String,
}

#[derive(Clone, Debug)]
struct EvidenceAssemblyWrite {
    owner: EvidenceOwnerBinding,
    idempotency_key: String,
    occurrence_set: CanonicalSourceOccurrenceSet,
    ordered_occurrences: Vec<SourceOccurrenceRecord>,
    span: EvidenceSpanRecord,
    projection_receipt: EvidenceSpanProjectionReceipt,
    contribution: RetrieverContributionRecord,
    anchors: Vec<DerivedEvidenceAnchor>,
    assembly_digest: ManifestDigest,
}

impl EvidenceAssemblyWrite {
    fn validate(&self) -> Result<(), EvidenceAssemblyError> {
        if self.ordered_occurrences.is_empty()
            || self.ordered_occurrences.len() > MAX_ASSEMBLY_MEMBERS
            || self
                .ordered_occurrences
                .iter()
                .any(|occurrence| occurrence.owner != self.owner)
        {
            return Err(EvidenceAssemblyError::Invalid(
                "evidence assembly occurrence membership",
            ));
        }
        let ordered_ids = self
            .ordered_occurrences
            .iter()
            .map(|occurrence| occurrence.occurrence_id.clone())
            .collect::<Vec<_>>();
        let ordered_anchors = self
            .ordered_occurrences
            .iter()
            .map(|occurrence| occurrence.exact_source_anchor.clone())
            .collect::<Vec<_>>();
        let span_ids = self
            .span
            .runs
            .iter()
            .flat_map(|run| run.occurrence_ids.iter().cloned())
            .collect::<Vec<_>>();
        if self.span.owner != self.owner
            || self.contribution.owner != self.owner
            || self.span.occurrence_set_id != self.occurrence_set.occurrence_set_id
            || self.contribution.occurrence_set_id != self.occurrence_set.occurrence_set_id
            || self.contribution.span_id != self.span.span_id
            || self.contribution.span_anchor_id != self.span.anchor_id
            || self.projection_receipt.span_id != self.span.span_id
            || self.projection_receipt.member_occurrence_ids != ordered_ids
            || self.projection_receipt.member_source_anchors != ordered_anchors
            || self.span.exact_source_anchors != ordered_anchors
            || self.contribution.exact_source_anchors != ordered_anchors
            || span_ids != ordered_ids
        {
            return Err(EvidenceAssemblyError::Invalid(
                "evidence assembly receipt membership",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceAssemblyPublicationReceipt {
    publication_receipt_id: EvidenceAssemblyPublicationReceiptId,
    owner: EvidenceOwnerBinding,
    assembly_digest: ManifestDigest,
    occurrence_set_id: CanonicalSourceOccurrenceSetId,
    span_id: EvidenceSpanId,
    span_anchor_id: RetrievalAnchorId,
    contribution_id: RetrieverContributionId,
    contribution_anchor_id: RetrievalAnchorId,
    projection_receipt_id: EvidenceSpanProjectionReceiptId,
    ordered_occurrence_ids: Vec<SourceOccurrenceId>,
    exact_source_anchors: Vec<RetrievalAnchorId>,
}

impl EvidenceAssemblyPublicationReceipt {
    fn from_write(write: &EvidenceAssemblyWrite) -> Result<Self, EvidenceAssemblyError> {
        let identity = (
            PUBLICATION_RECEIPT_ID_DOMAIN,
            &write.owner,
            &write.assembly_digest,
            write.occurrence_set.occurrence_set_id(),
            write.span.span_id(),
            write.contribution.contribution_id(),
            &write.projection_receipt.projection_receipt_id,
        );
        Ok(Self {
            publication_receipt_id: EvidenceAssemblyPublicationReceiptId::from_material(
                PUBLICATION_RECEIPT_ID_DOMAIN,
                &identity,
            )?,
            owner: write.owner.clone(),
            assembly_digest: write.assembly_digest.clone(),
            occurrence_set_id: write.occurrence_set.occurrence_set_id.clone(),
            span_id: write.span.span_id.clone(),
            span_anchor_id: write.span.anchor_id.clone(),
            contribution_id: write.contribution.contribution_id.clone(),
            contribution_anchor_id: write.contribution.anchor_id.clone(),
            projection_receipt_id: write.projection_receipt.projection_receipt_id.clone(),
            ordered_occurrence_ids: write
                .ordered_occurrences
                .iter()
                .map(|occurrence| occurrence.occurrence_id.clone())
                .collect(),
            exact_source_anchors: write
                .ordered_occurrences
                .iter()
                .map(|occurrence| occurrence.exact_source_anchor.clone())
                .collect(),
        })
    }

    pub fn contribution_id(&self) -> &RetrieverContributionId {
        &self.contribution_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvidenceAssemblyPublicationOutcome {
    Published(EvidenceAssemblyPublicationReceipt),
    Replayed(EvidenceAssemblyPublicationReceipt),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionDerivedEvidenceKind {
    Span,
    Burst,
}

#[derive(Clone, Debug)]
pub struct SessionEvidencePolicy {
    pub kind: SessionDerivedEvidenceKind,
    pub max_members: usize,
    pub algorithm_revision: String,
    pub adjacency_revision: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExactSourceDisposition {
    Available,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedExactSource {
    pub occurrence: SourceOccurrenceRecord,
    pub disposition: ExactSourceDisposition,
    pub anchor: Option<RetrievalAnchorRecordV2>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceDrilldownPage {
    pub contribution: RetrieverContributionRecord,
    pub span: EvidenceSpanRecord,
    pub occurrence_set_id: CanonicalSourceOccurrenceSetId,
    pub exact_sources: Vec<AuthorizedExactSource>,
    pub next_ordinal: Option<u64>,
    pub coverage: EvidenceAssemblyCoverage,
}

/// Typed Stage-C adapter for canonical V3 evidence assemblies.
///
/// This is intentionally an alternate path until callers can construct the V3
/// records directly. It accepts only a daemon-verified runtime handle and the
/// authoritative profile identity carried by the daemon; it never infers
/// either identity from a path, label, database, or request payload.
#[derive(Clone)]
pub(crate) struct RuntimeEvidenceAssemblyStore {
    profile_id: tracedecay_domain::UserProfileId,
    runtime: StoreRuntimeHandle,
    authority: crate::db::DatabaseAuthority,
}

#[derive(Clone, Debug, PartialEq, Eq)]
// Resolved carries the full anchor record; boxing would ripple through store match sites.
#[allow(clippy::large_enum_variant)]
pub(crate) enum EvidenceAssemblyAnchorResolutionV1 {
    Resolved {
        record: tracedecay_domain::RetrievalAnchorRecordV3,
        derivatives: Vec<tracedecay_store::RetrievalAnchorDerivativeV1>,
    },
    Tombstone(tracedecay_store::RetrievalAnchorTombstoneV1),
    Unavailable,
}

impl RuntimeEvidenceAssemblyStore {
    pub(crate) fn new(
        profile_id: tracedecay_domain::UserProfileId,
        runtime: StoreRuntimeHandle,
        authority: crate::db::DatabaseAuthority,
    ) -> tracedecay_store::EvidenceAssemblyStoreResult<Self> {
        let binding = runtime.binding();
        if binding.shard_id.profile_id != profile_id
            || authority.canonical_database_path() != runtime.locator().path()
            || !matches!(
                binding.shard_id.scope,
                tracedecay_store::StoreShardScopeV1::Project { .. }
                    | tracedecay_store::StoreShardScopeV1::ProjectSessions { .. }
                    | tracedecay_store::StoreShardScopeV1::ProfileSessions
            )
        {
            return Err(evidence_runtime_invalid(
                "evidence runtime identity does not match the injected profile scope",
            ));
        }
        Ok(Self {
            profile_id,
            runtime,
            authority,
        })
    }

    pub(crate) fn profile_id(&self) -> &tracedecay_domain::UserProfileId {
        &self.profile_id
    }

    fn validate_owner(
        &self,
        owner: &tracedecay_store::EvidenceAssemblyOwnerV1,
    ) -> tracedecay_store::EvidenceAssemblyStoreResult<()> {
        owner.validate()?;
        let project_matches = match &self.runtime.binding().shard_id.scope {
            tracedecay_store::StoreShardScopeV1::Project { project_id }
            | tracedecay_store::StoreShardScopeV1::ProjectSessions { project_id } => {
                owner.owner.project_id() == Some(project_id)
            }
            tracedecay_store::StoreShardScopeV1::ProfileSessions => {
                owner.owner.project_id().is_none()
            }
            _ => false,
        };
        if owner.owner.profile_id() != &self.profile_id || !project_matches {
            return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable);
        }
        Ok(())
    }

    pub(crate) async fn resolve_anchor(
        &self,
        context: &ProductRequestContext,
        owner: &tracedecay_store::EvidenceAssemblyOwnerV1,
        anchor_id: &RetrievalAnchorId,
    ) -> tracedecay_store::EvidenceAssemblyStoreResult<EvidenceAssemblyAnchorResolutionV1> {
        authorize_runtime_anchor_resolution_at(context, owner, evidence_runtime_now())?;
        self.validate_owner(owner)?;
        anchor_id.validate().map_err(evidence_runtime_invalid)?;

        let anchor_owner = tracedecay_store::RetrievalAnchorOwnerV1::V3(owner.owner.clone());
        let database =
            Database::publish_runtime(self.runtime.clone(), DatabaseAccessMode::ReadOnly)
                .await
                .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
        if database.canonical_database_path() != self.authority.canonical_database_path() {
            return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable);
        }
        let snapshot = database
            .begin_engine_read_snapshot("resolve evidence assembly anchor")
            .await
            .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
        resolve_anchor_snapshot(&snapshot, anchor_id, &anchor_owner).await
    }

    fn read(
        &self,
        operation: tracedecay_store::EvidenceAssemblyReadOperationV1,
    ) -> tracedecay_store::EvidenceAssemblyStoreResult<tracedecay_store::EvidenceAssemblyReadResultV1>
    {
        match &operation {
            tracedecay_store::EvidenceAssemblyReadOperationV1::PublicationByIdempotency {
                owner,
                ..
            }
            | tracedecay_store::EvidenceAssemblyReadOperationV1::ContributionPage {
                owner, ..
            } => self.validate_owner(owner)?,
        }
        let request = evidence_runtime_read_request(self.runtime.binding(), operation)?;
        let probe = EvidenceRuntimeProbe::from_control(request.control());
        let outcome = self
            .runtime
            .dispatch_read(request, &probe)
            .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
        if !matches!(
            outcome.coverage(),
            tracedecay_store::RuntimeReadCoverageV1::Latest { .. }
                | tracedecay_store::RuntimeReadCoverageV1::Complete { .. }
        ) {
            return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable);
        }
        let result = match outcome.value() {
            Some(tracedecay_store::RuntimeReadResultV1::Repository {
                result: tracedecay_store::RepositoryReadResultV1::Project(project),
            }) => match project.as_ref() {
                tracedecay_store::ProjectReadResultV1::EvidenceAssembly(result) => result.clone(),
                _ => return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable),
            },
            _ => return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable),
        };
        match &result {
            tracedecay_store::EvidenceAssemblyReadResultV1::Publication(Some(receipt)) => {
                self.validate_owner(&receipt.owner)?;
                receipt.validate()?;
            }
            tracedecay_store::EvidenceAssemblyReadResultV1::ContributionPage(Some(page)) => {
                self.validate_owner(&page.contribution.owner)?;
                if page.span.owner != page.contribution.owner.owner {
                    return Err(evidence_runtime_invalid(
                        "evidence runtime drilldown owner mismatch",
                    ));
                }
                page.contribution.validate()?;
                page.span.validate()?;
                for occurrence in &page.occurrences {
                    occurrence.validate()?;
                    if occurrence.owner != page.contribution.owner.owner {
                        return Err(evidence_runtime_invalid(
                            "evidence runtime occurrence owner mismatch",
                        ));
                    }
                }
            }
            _ => {}
        }
        Ok(result)
    }
}

async fn resolve_anchor_snapshot(
    snapshot: &(impl QueryExecutor + Sync),
    anchor_id: &RetrievalAnchorId,
    owner: &tracedecay_store::RetrievalAnchorOwnerV1,
) -> tracedecay_store::EvidenceAssemblyStoreResult<EvidenceAssemblyAnchorResolutionV1> {
    let owner_json = serde_json::to_string(owner)
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
    let mut rows = snapshot
        .query(
            "SELECT anchor.anchor_json, anchor.projection_generation,
                    disposition.disposition_id, disposition.state,
                    disposition.superseded_by, disposition.reason_class,
                    disposition.effective_at, disposition.record_json
             FROM retrieval_anchors AS anchor
             LEFT JOIN retrieval_anchor_dispositions AS disposition
               ON disposition.sequence = (
                   SELECT latest.sequence
                   FROM retrieval_anchor_dispositions AS latest
                   WHERE latest.anchor_id = anchor.anchor_id
                     AND latest.owner_json = anchor.owner_json
                   ORDER BY latest.sequence DESC LIMIT 1
               )
             WHERE anchor.anchor_id = ?1 AND anchor.owner_json = ?2",
            params![anchor_id.as_str(), owner_json.as_str()],
        )
        .await
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?
    else {
        return Ok(EvidenceAssemblyAnchorResolutionV1::Unavailable);
    };
    let anchor_json: String = row
        .get(0)
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
    let projection_generation: String = row
        .get(1)
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
    let disposition_columns = (
        row.get::<Option<String>>(2),
        row.get::<Option<String>>(3),
        row.get::<Option<String>>(4),
        row.get::<Option<String>>(5),
        row.get::<Option<i64>>(6),
        row.get::<Option<String>>(7),
    );
    drop(rows);

    let disposition = match disposition_columns {
        (Ok(None), Ok(None), Ok(None), Ok(None), Ok(None), Ok(None)) => None,
        (
            Ok(Some(disposition_id)),
            Ok(Some(state)),
            Ok(superseded_by),
            Ok(Some(reason_class)),
            Ok(Some(effective_at)),
            Ok(Some(record_json)),
        ) => {
            let record: tracedecay_store::RetrievalAnchorDispositionRecordV1 =
                serde_json::from_str(&record_json)
                    .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
            record
                .validate()
                .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
            if record.disposition_id() != disposition_id
                || record.anchor_id() != anchor_id
                || record.owner() != owner
                || record.state().as_str() != state
                || record.superseded_by().map(RetrievalAnchorId::as_str) != superseded_by.as_deref()
                || record.reason_class().as_str() != reason_class
                || record.effective_at().0 != effective_at
            {
                return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable);
            }
            Some(record)
        }
        _ => return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable),
    };

    if let Some(disposition) = disposition {
        if matches!(
            disposition.state(),
            tracedecay_store::AnchorDispositionStateV1::Redacted
                | tracedecay_store::AnchorDispositionStateV1::Expired
                | tracedecay_store::AnchorDispositionStateV1::Quarantined
                | tracedecay_store::AnchorDispositionStateV1::Deleted
                | tracedecay_store::AnchorDispositionStateV1::Unavailable
        ) {
            let tombstone = tracedecay_store::RetrievalAnchorTombstoneV1::new(
                anchor_id.clone(),
                owner.clone(),
                disposition.state(),
                disposition.reason_class(),
                disposition.effective_at(),
            )
            .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
            return Ok(EvidenceAssemblyAnchorResolutionV1::Tombstone(tombstone));
        }
        if disposition.state() != tracedecay_store::AnchorDispositionStateV1::Active {
            return Ok(EvidenceAssemblyAnchorResolutionV1::Unavailable);
        }
    }

    let stored: tracedecay_store::StoredRetrievalAnchorRecordV1 =
        serde_json::from_str(&anchor_json)
            .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
    stored
        .validate()
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
    if stored.anchor_id() != anchor_id
        || stored.owner() != owner.clone()
        || stored.projection_generation().as_str() != projection_generation
    {
        return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable);
    }
    let tracedecay_store::StoredRetrievalAnchorRecordV1::V3(record) = stored else {
        return Ok(EvidenceAssemblyAnchorResolutionV1::Unavailable);
    };

    let mut rows = snapshot
        .query(
            "SELECT lineage.derivative_kind, lineage.derivative_id,
                    lineage.direct_evidence
             FROM retrieval_anchor_reverse_lineage AS lineage
             WHERE lineage.source_anchor_id = ?1 AND lineage.owner_json = ?2
               AND NOT EXISTS (
                   SELECT 1
                   FROM retrieval_anchor_derivative_tombstones AS tombstone
                   WHERE tombstone.source_anchor_id = lineage.source_anchor_id
                     AND tombstone.owner_json = lineage.owner_json
                     AND tombstone.derivative_kind = lineage.derivative_kind
                     AND tombstone.derivative_id = lineage.derivative_id
               )
             ORDER BY lineage.derivative_kind, lineage.derivative_id",
            params![anchor_id.as_str(), owner_json],
        )
        .await
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
    let mut derivatives = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?
    {
        let kind = tracedecay_store::AnchorDerivativeKindV1::parse(
            &row.get::<String>(0)
                .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?,
        )
        .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
        let derivative_id: String = row
            .get(1)
            .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
        let direct_evidence: i64 = row
            .get(2)
            .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?;
        if !matches!(direct_evidence, 0 | 1) {
            return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable);
        }
        derivatives.push(
            tracedecay_store::RetrievalAnchorDerivativeV1::new(
                anchor_id.clone(),
                owner.clone(),
                kind,
                derivative_id,
                direct_evidence == 1,
            )
            .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?,
        );
    }
    Ok(EvidenceAssemblyAnchorResolutionV1::Resolved {
        record,
        derivatives,
    })
}

fn authorize_runtime_anchor_resolution_at(
    context: &ProductRequestContext,
    owner: &tracedecay_store::EvidenceAssemblyOwnerV1,
    observed_at: UtcMicros,
) -> tracedecay_store::EvidenceAssemblyStoreResult<()> {
    owner.validate()?;
    if context.validate().is_err()
        || context.admission_at(observed_at) != RequestAdmission::Admitted
        || context.grant().disclosure < DisclosureClass::Evidence
        || owner.owner.project_id() != Some(&context.scope().project_id)
        || owner.scope_digest != context.scope().scope_digest
    {
        return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable);
    }
    Ok(())
}

impl tracedecay_store::EvidenceAssemblyStore for RuntimeEvidenceAssemblyStore {
    fn publish_or_replay(
        &self,
        write: tracedecay_store::EvidenceAssemblyWriteV1,
    ) -> impl std::future::Future<
        Output = tracedecay_store::EvidenceAssemblyStoreResult<
            tracedecay_store::EvidenceAssemblyPublicationOutcomeV1,
        >,
    > + Send {
        async move {
            write.validate()?;
            self.validate_owner(&write.owner)?;
            let expected = write.receipt.clone();
            let read =
                tracedecay_store::EvidenceAssemblyReadOperationV1::PublicationByIdempotency {
                    owner: write.owner.clone(),
                    idempotency_key: write.idempotency_key.clone(),
                };
            let request = evidence_runtime_submit_request(self.runtime.binding(), write)?;
            let probe = Arc::new(EvidenceRuntimeProbe::from_control(request.control()));
            let replayed = match self
                .runtime
                .dispatch_submit_authorized(request, probe, self.authority.clone())
                .await
                .map_err(|_| tracedecay_store::EvidenceAssemblyStoreError::Unavailable)?
            {
                tracedecay_store::RuntimeSubmitOutcomeV1::Committed { .. }
                | tracedecay_store::RuntimeSubmitOutcomeV1::CommittedAfterCancellation { .. } => {
                    false
                }
                tracedecay_store::RuntimeSubmitOutcomeV1::ExactReplay { .. } => true,
                tracedecay_store::RuntimeSubmitOutcomeV1::IdempotencyConflict { .. } => {
                    return Err(tracedecay_store::EvidenceAssemblyStoreError::ReplayConflict);
                }
                _ => return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable),
            };
            let receipt = match self.read(read)? {
                tracedecay_store::EvidenceAssemblyReadResultV1::Publication(Some(receipt))
                    if receipt == expected =>
                {
                    receipt
                }
                tracedecay_store::EvidenceAssemblyReadResultV1::Publication(Some(_)) => {
                    return Err(tracedecay_store::EvidenceAssemblyStoreError::ReplayConflict);
                }
                _ => return Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable),
            };
            Ok(if replayed {
                tracedecay_store::EvidenceAssemblyPublicationOutcomeV1::Replayed(receipt)
            } else {
                tracedecay_store::EvidenceAssemblyPublicationOutcomeV1::Published(receipt)
            })
        }
    }

    fn drilldown_contribution(
        &self,
        owner: &tracedecay_store::EvidenceAssemblyOwnerV1,
        contribution_id: &tracedecay_domain::RetrieverContributionIdV1,
        start_ordinal: u64,
        page_size: u64,
    ) -> impl std::future::Future<
        Output = tracedecay_store::EvidenceAssemblyStoreResult<
            Option<tracedecay_store::EvidenceAssemblyDrilldownPageV1>,
        >,
    > + Send {
        let owner = owner.clone();
        let contribution_id = contribution_id.clone();
        async move {
            match self.read(
                tracedecay_store::EvidenceAssemblyReadOperationV1::ContributionPage {
                    owner,
                    contribution_id,
                    start_ordinal,
                    page_size,
                },
            )? {
                tracedecay_store::EvidenceAssemblyReadResultV1::ContributionPage(page) => Ok(page),
                tracedecay_store::EvidenceAssemblyReadResultV1::Publication(_) => {
                    Err(tracedecay_store::EvidenceAssemblyStoreError::Unavailable)
                }
            }
        }
    }
}

fn evidence_runtime_submit_request(
    binding: &tracedecay_store::StoreRuntimeBindingV1,
    write: tracedecay_store::EvidenceAssemblyWriteV1,
) -> tracedecay_store::EvidenceAssemblyStoreResult<tracedecay_store::RuntimeSubmitRequestV1> {
    let command_digest = canonical_sha256(&write).map_err(evidence_runtime_invalid)?;
    let suffix = evidence_runtime_digest_suffix(command_digest.as_str())?;
    let idempotency_suffix =
        evidence_runtime_digest_suffix(write.idempotency_key.as_digest().as_str())?.to_owned();
    let admitted_at = evidence_runtime_now();
    let admission_bytes = serde_json::to_vec(&write)
        .map_err(evidence_runtime_invalid)?
        .len();
    let payload = tracedecay_store::RepositoryWritePayloadV1::EvidenceAssembly(Box::new(write));
    let metadata = tracedecay_store::StoreOperationMetadataV1 {
        operation_id: tracedecay_store::StoreOperationIdV1::new(format!(
            "operation.evidence-assembly.{suffix}"
        ))
        .map_err(evidence_runtime_invalid)?,
        client_id: tracedecay_store::StoreClientIdV1::new("client.evidence-assembly")
            .map_err(evidence_runtime_invalid)?,
        shard_id: binding.shard_id.clone(),
        incarnation: binding.incarnation,
        authority_epoch: binding.authority_epoch,
        idempotency: tracedecay_store::IdempotencyIdentityV1 {
            key: tracedecay_store::StoreIdempotencyKeyV1::new(format!(
                "evidence-assembly.{idempotency_suffix}"
            ))
            .map_err(evidence_runtime_invalid)?,
            command_digest: tracedecay_store::CommandDigestV1::new(command_digest.as_str())
                .map_err(evidence_runtime_invalid)?,
        },
        durability: tracedecay_store::DurabilityClassV1::Full,
        priority: tracedecay_store::OperationPriorityV1::Foreground,
        admission_bytes: u64::try_from(admission_bytes).unwrap_or(u64::MAX).max(1),
        admitted_at,
    };
    let compatibility = tracedecay_store::RuntimeBatchCompatibilityV1::from_operation(&metadata)
        .map_err(evidence_runtime_invalid)?;
    let transaction_scope = tracedecay_store::RuntimeTransactionScopeV1 {
        transaction_id: tracedecay_store::RuntimeTransactionIdV1::new(format!(
            "transaction.{}",
            metadata.operation_id.as_str()
        ))
        .map_err(evidence_runtime_invalid)?,
        compatibility,
        opened_at: admitted_at,
    };
    tracedecay_store::RuntimeSubmitRequestV1::new(
        tracedecay_store::RepositoryOperationEnvelopeV1 { metadata, payload },
        transaction_scope,
        evidence_runtime_control(suffix, admitted_at)?,
    )
    .map_err(evidence_runtime_invalid)
}

fn evidence_runtime_read_request(
    binding: &tracedecay_store::StoreRuntimeBindingV1,
    operation: tracedecay_store::EvidenceAssemblyReadOperationV1,
) -> tracedecay_store::EvidenceAssemblyStoreResult<tracedecay_store::RuntimeReadRequestV1> {
    let command_digest = canonical_sha256(&operation).map_err(evidence_runtime_invalid)?;
    let suffix = evidence_runtime_digest_suffix(command_digest.as_str())?;
    let admission_bytes = serde_json::to_vec(&operation)
        .map_err(evidence_runtime_invalid)?
        .len();
    let requested_at = evidence_runtime_now();
    tracedecay_store::RuntimeReadRequestV1::new(
        binding.clone(),
        tracedecay_store::ConsistencyModeV1::LatestAvailable,
        tracedecay_store::RuntimeReadOperationV1::Repository {
            op: tracedecay_store::RepositoryReadOperationV1::Project(
                tracedecay_store::ProjectReadOperationV1::EvidenceAssembly(operation),
            ),
        },
        tracedecay_store::OperationPriorityV1::Foreground,
        u64::try_from(admission_bytes).unwrap_or(u64::MAX).max(1),
        evidence_runtime_control(suffix, requested_at)?,
    )
    .map_err(evidence_runtime_invalid)
}

fn evidence_runtime_control(
    suffix: &str,
    requested_at: UtcMicros,
) -> tracedecay_store::EvidenceAssemblyStoreResult<tracedecay_store::RuntimeRequestControlV1> {
    Ok(tracedecay_store::RuntimeRequestControlV1 {
        requested_at,
        deadline: tracedecay_store::RuntimeDeadlineV1 {
            deadline_id: tracedecay_store::RuntimeDeadlineIdV1::new(format!(
                "deadline.evidence-assembly.{suffix}"
            ))
            .map_err(evidence_runtime_invalid)?,
        },
        cancellation: tracedecay_store::RuntimeCancellationIdentityV1 {
            cancellation_id: tracedecay_store::RuntimeCancellationIdV1::new(format!(
                "cancellation.evidence-assembly.{suffix}"
            ))
            .map_err(evidence_runtime_invalid)?,
            generation: 1,
        },
    })
}

fn evidence_runtime_digest_suffix(
    digest: &str,
) -> tracedecay_store::EvidenceAssemblyStoreResult<&str> {
    digest
        .strip_prefix("sha256:")
        .ok_or_else(|| evidence_runtime_invalid("non-SHA-256 evidence runtime digest"))
}

fn evidence_runtime_now() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    UtcMicros(i64::try_from(micros).unwrap_or(i64::MAX))
}

struct EvidenceRuntimeProbe {
    cancellation: tracedecay_store::RuntimeCancellationIdentityV1,
    deadline: tracedecay_store::RuntimeDeadlineV1,
}

impl EvidenceRuntimeProbe {
    fn from_control(control: &tracedecay_store::RuntimeRequestControlV1) -> Self {
        Self {
            cancellation: control.cancellation.clone(),
            deadline: control.deadline.clone(),
        }
    }
}

impl tracedecay_store::RuntimeRequestProbeV1 for EvidenceRuntimeProbe {
    fn cancellation_identity(&self) -> &tracedecay_store::RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &tracedecay_store::RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<tracedecay_store::RuntimeInterruptionV1> {
        None
    }
}

fn evidence_runtime_invalid(
    error: impl std::fmt::Display,
) -> tracedecay_store::EvidenceAssemblyStoreError {
    tracedecay_store::EvidenceAssemblyStoreError::InvalidData(error.to_string())
}

#[derive(Clone)]
/// V1 compatibility service retained for already-persisted evidence records.
/// New canonical V3 publications and reads must use the daemon-owned
/// [`RuntimeEvidenceAssemblyStore`] path above.
pub struct EvidenceAssemblyService {
    project_database: Database,
    session_database: Arc<RegisteredGlobalDb>,
}

impl EvidenceAssemblyService {
    pub(crate) const fn new(
        project_database: Database,
        session_database: Arc<RegisteredGlobalDb>,
    ) -> Self {
        Self {
            project_database,
            session_database,
        }
    }

    pub async fn drilldown_contribution(
        &self,
        context: &ProductRequestContext,
        contribution_id: &RetrieverContributionId,
        start_ordinal: u64,
        page_size: u64,
    ) -> Result<EvidenceDrilldownPage, EvidenceAssemblyError> {
        if page_size == 0 || page_size > 256 {
            return Err(EvidenceAssemblyError::Invalid("drilldown page size"));
        }
        let project = self.project_database.engine_conn();
        let mut contribution_rows = project
            .query(
                "SELECT record_json FROM evidence_retriever_contributions
                 WHERE contribution_id = ?1",
                params![contribution_id.as_str()],
            )
            .await
            .map_err(storage_error)?;
        let contribution_json = next_string(&mut contribution_rows)
            .await?
            .ok_or(EvidenceAssemblyError::UnauthorizedOrUnknown)?;
        let contribution: RetrieverContributionRecord = deserialize(&contribution_json)?;
        contribution.owner.validate_feedback_context(context)?;

        let mut span_rows = project
            .query(
                "SELECT record_json FROM evidence_spans WHERE span_id = ?1",
                params![contribution.span_id.as_str()],
            )
            .await
            .map_err(storage_error)?;
        let span: EvidenceSpanRecord = deserialize(
            &next_string(&mut span_rows)
                .await?
                .ok_or(EvidenceAssemblyError::UnauthorizedOrUnknown)?,
        )?;
        if span.owner != contribution.owner
            || span.span_id != contribution.span_id
            || span.occurrence_set_id != contribution.occurrence_set_id
        {
            return Err(EvidenceAssemblyError::UnauthorizedOrUnknown);
        }

        let end = start_ordinal.saturating_add(page_size);
        let mut member_rows = project
            .query(
                "SELECT occurrence.record_json
                 FROM evidence_span_members member
                 JOIN evidence_source_occurrences occurrence
                   ON occurrence.occurrence_id = member.occurrence_id
                 WHERE member.span_id = ?1
                   AND member.assembly_ordinal >= ?2
                   AND member.assembly_ordinal < ?3
                 ORDER BY member.assembly_ordinal",
                params![
                    span.span_id.as_str(),
                    i64_from_u64(start_ordinal)?,
                    i64_from_u64(end)?
                ],
            )
            .await
            .map_err(storage_error)?;
        let mut exact_sources = Vec::new();
        while let Some(row) = member_rows.next().await.map_err(storage_error)? {
            let json: String = row.get(0).map_err(storage_error)?;
            let occurrence: SourceOccurrenceRecord = deserialize(&json)?;
            let anchor = self
                .load_current_source_anchor(occurrence.exact_source_anchor())
                .await?;
            exact_sources.push(AuthorizedExactSource {
                occurrence,
                disposition: if anchor.is_some() {
                    ExactSourceDisposition::Available
                } else {
                    ExactSourceDisposition::Unavailable
                },
                anchor,
            });
        }
        let total = u64::try_from(span.exact_source_anchors.len()).unwrap_or(u64::MAX);
        let consumed =
            start_ordinal.saturating_add(u64::try_from(exact_sources.len()).unwrap_or(u64::MAX));
        Ok(EvidenceDrilldownPage {
            contribution,
            occurrence_set_id: span.occurrence_set_id.clone(),
            next_ordinal: (consumed < total).then_some(consumed),
            coverage: span.coverage.clone(),
            span,
            exact_sources,
        })
    }

    async fn load_current_source_anchor(
        &self,
        anchor_id: &RetrievalAnchorId,
    ) -> Result<Option<RetrievalAnchorRecordV2>, EvidenceAssemblyError> {
        let project = self.project_database.engine_conn();
        if let Some(anchor) = load_anchor(&project, anchor_id.as_str()).await? {
            return Ok(Some(anchor));
        }
        let session = self
            .session_database
            .read_snapshot()
            .await
            .map_err(storage_error)?;
        load_anchor(&session, anchor_id.as_str()).await
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn build_write(
    owner: EvidenceOwnerBinding,
    idempotency_key: String,
    ordered_occurrences: Vec<SourceOccurrenceRecord>,
    producer_kind: EvidenceSpanProducerKind,
    producer_revision: &str,
    adjacency_revision: &str,
    request_digest: String,
    retriever_id: &str,
    temporal_mode: TemporalModeV1,
    watermarks: RetrieverWatermarkBinding,
    coverage: EvidenceAssemblyCoverage,
    created_at: UtcMicros,
) -> Result<EvidenceAssemblyWrite, EvidenceAssemblyError> {
    if ordered_occurrences.is_empty() {
        return Err(EvidenceAssemblyError::NoEvidence);
    }
    let occurrence_set =
        CanonicalSourceOccurrenceSet::new(owner.clone(), ordered_occurrences.clone())?;
    let mut grouped = Vec::<Vec<SourceOccurrenceRecord>>::new();
    for occurrence in &ordered_occurrences {
        let extends = grouped
            .last()
            .and_then(|run| run.last())
            .is_some_and(|prior| {
                prior.timeline == occurrence.timeline
                    && prior.source_order().checked_add(1) == Some(occurrence.source_order())
            });
        if extends {
            if let Some(run) = grouped.last_mut() {
                run.push(occurrence.clone());
            }
        } else {
            grouped.push(vec![occurrence.clone()]);
        }
    }
    let runs = grouped
        .iter()
        .enumerate()
        .map(|(ordinal, members)| {
            EvidenceSpanRun::verify(
                u64::try_from(ordinal).map_err(|_| EvidenceAssemblyError::MemberLimit)?,
                members,
                adjacency_revision,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let span = EvidenceSpanRecord::new(
        owner.clone(),
        &occurrence_set,
        runs,
        producer_kind,
        producer_revision,
        coverage,
        &ordered_occurrences,
    )?;
    let projection_receipt =
        EvidenceSpanProjectionReceipt::new(&span, producer_revision, &ordered_occurrences)?;
    let contribution = RetrieverContributionRecord::new(
        owner.clone(),
        retriever_id,
        producer_revision,
        request_digest,
        temporal_mode,
        watermarks,
        &occurrence_set,
        &span,
        created_at,
    )?;
    let mut anchors = ordered_occurrences
        .iter()
        .map(|occurrence| {
            let anchor_id = derived_anchor_id(
                "source_occurrence",
                occurrence.occurrence_id.as_str(),
                &owner,
            )?;
            Ok(DerivedEvidenceAnchor {
                anchor_id,
                owner: owner.clone(),
                target_kind: "source_occurrence".to_owned(),
                target_id: occurrence.occurrence_id.as_str().to_owned(),
            })
        })
        .collect::<Result<Vec<_>, EvidenceAssemblyError>>()?;
    anchors.push(DerivedEvidenceAnchor {
        anchor_id: span.anchor_id.clone(),
        owner: owner.clone(),
        target_kind: "evidence_span".to_owned(),
        target_id: span.span_id.as_str().to_owned(),
    });
    anchors.push(DerivedEvidenceAnchor {
        anchor_id: contribution.anchor_id.clone(),
        owner: owner.clone(),
        target_kind: "retriever_contribution".to_owned(),
        target_id: contribution.contribution_id.as_str().to_owned(),
    });
    let assembly_digest = canonical_sha256(&(
        &owner,
        occurrence_set.record_digest()?,
        span.record_digest()?,
        projection_receipt.record_digest()?,
        contribution.record_digest()?,
        &anchors,
    ))
    .map_err(contract_error)?;
    let write = EvidenceAssemblyWrite {
        owner,
        idempotency_key,
        occurrence_set,
        ordered_occurrences,
        span,
        projection_receipt,
        contribution,
        anchors,
        assembly_digest,
    };
    write.validate()?;
    Ok(write)
}

async fn replay_receipt(
    transaction: &(impl Executor + Sync),
    write: &EvidenceAssemblyWrite,
) -> Result<Option<EvidenceAssemblyPublicationReceipt>, EvidenceAssemblyError> {
    let mut rows = transaction
        .query(
            "SELECT assembly_digest, receipt_json
             FROM evidence_assembly_receipts
             WHERE owner_digest = ?1
               AND privacy_domain_id = ?2
               AND key_epoch = ?3
               AND idempotency_key = ?4",
            params![
                write.owner.digest()?.as_str(),
                write.owner.privacy_domain_id.as_str(),
                i64_from_u64(write.owner.key_epoch)?,
                write.idempotency_key.as_str()
            ],
        )
        .await
        .map_err(storage_error)?;
    let Some(row) = rows.next().await.map_err(storage_error)? else {
        return Ok(None);
    };
    let digest: String = row.get(0).map_err(storage_error)?;
    if digest != write.assembly_digest.as_str() {
        return Err(EvidenceAssemblyError::ReplayConflict);
    }
    let receipt_json: String = row.get(1).map_err(storage_error)?;
    Ok(Some(deserialize(&receipt_json)?))
}

async fn persist_write(
    transaction: &(impl Executor + Sync),
    write: &EvidenceAssemblyWrite,
    receipt: &EvidenceAssemblyPublicationReceipt,
) -> Result<(), EvidenceAssemblyError> {
    write.validate()?;
    let owner_digest = write.owner.digest()?;
    for occurrence in &write.occurrence_set.members {
        insert_immutable_json(
            transaction,
            "evidence_source_occurrences",
            "occurrence_id",
            occurrence.occurrence_id.as_str(),
            occurrence.record_digest()?.as_str(),
            &serialize(occurrence)?,
            Some((
                "owner_digest, timeline_digest, source_anchor_id, source_order",
                vec![
                    owner_digest.as_str().to_owned(),
                    occurrence.timeline.digest()?.as_str().to_owned(),
                    occurrence.exact_source_anchor.as_str().to_owned(),
                    occurrence.source_order().to_string(),
                ],
            )),
        )
        .await?;
    }
    insert_immutable_json(
        transaction,
        "evidence_occurrence_sets",
        "occurrence_set_id",
        write.occurrence_set.occurrence_set_id.as_str(),
        write.occurrence_set.record_digest()?.as_str(),
        &serialize(&write.occurrence_set)?,
        Some(("owner_digest", vec![owner_digest.as_str().to_owned()])),
    )
    .await?;
    for (ordinal, occurrence) in write.occurrence_set.members.iter().enumerate() {
        transaction
            .execute(
                "INSERT OR IGNORE INTO evidence_occurrence_set_members (
                    occurrence_set_id, canonical_ordinal, occurrence_id
                 ) VALUES (?1, ?2, ?3)",
                params![
                    write.occurrence_set.occurrence_set_id.as_str(),
                    i64::try_from(ordinal).map_err(|_| EvidenceAssemblyError::MemberLimit)?,
                    occurrence.occurrence_id.as_str()
                ],
            )
            .await
            .map_err(storage_error)?;
    }
    let canonical_member_ids = write
        .occurrence_set
        .members
        .iter()
        .map(|occurrence| occurrence.occurrence_id.as_str())
        .collect::<Vec<_>>();
    verify_member_ids(
        transaction,
        "SELECT occurrence_id
         FROM evidence_occurrence_set_members
         WHERE occurrence_set_id = ?1
         ORDER BY canonical_ordinal",
        write.occurrence_set.occurrence_set_id.as_str(),
        &canonical_member_ids,
    )
    .await?;
    insert_span(transaction, write, &owner_digest).await?;
    insert_projection_receipt(transaction, write).await?;
    insert_contribution(transaction, write, &owner_digest).await?;
    for anchor in &write.anchors {
        transaction
            .execute(
                "INSERT OR IGNORE INTO evidence_derived_anchors (
                    anchor_id, owner_digest, target_kind, target_id, anchor_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    anchor.anchor_id.as_str(),
                    owner_digest.as_str(),
                    anchor.target_kind.as_str(),
                    anchor.target_id.as_str(),
                    serialize(anchor)?
                ],
            )
            .await
            .map_err(storage_error)?;
    }
    transaction
        .execute(
            "INSERT INTO evidence_assembly_receipts (
                publication_receipt_id, owner_digest, privacy_domain_id, key_epoch,
                idempotency_key, assembly_digest, occurrence_set_id, span_id,
                contribution_id, projection_receipt_id, receipt_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                receipt.publication_receipt_id.as_str(),
                owner_digest.as_str(),
                write.owner.privacy_domain_id.as_str(),
                i64_from_u64(write.owner.key_epoch)?,
                write.idempotency_key.as_str(),
                write.assembly_digest.as_str(),
                write.occurrence_set.occurrence_set_id.as_str(),
                write.span.span_id.as_str(),
                write.contribution.contribution_id.as_str(),
                write.projection_receipt.projection_receipt_id.as_str(),
                serialize(receipt)?
            ],
        )
        .await
        .map_err(storage_error)?;
    Ok(())
}

async fn insert_span(
    transaction: &(impl Executor + Sync),
    write: &EvidenceAssemblyWrite,
    owner_digest: &ManifestDigest,
) -> Result<(), EvidenceAssemblyError> {
    insert_immutable_json(
        transaction,
        "evidence_spans",
        "span_id",
        write.span.span_id.as_str(),
        write.span.record_digest()?.as_str(),
        &serialize(&write.span)?,
        Some((
            "owner_digest, occurrence_set_id, anchor_id, producer_kind",
            vec![
                owner_digest.as_str().to_owned(),
                write.occurrence_set.occurrence_set_id.as_str().to_owned(),
                write.span.anchor_id.as_str().to_owned(),
                write.span.producer_kind.as_str().to_owned(),
            ],
        )),
    )
    .await?;
    let mut assembly_ordinal = 0_u64;
    for (run_ordinal, run) in write.span.runs.iter().enumerate() {
        for (run_member_ordinal, occurrence_id) in run.occurrence_ids.iter().enumerate() {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO evidence_span_members (
                        span_id, assembly_ordinal, run_ordinal,
                        run_member_ordinal, occurrence_id
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        write.span.span_id.as_str(),
                        i64_from_u64(assembly_ordinal)?,
                        i64::try_from(run_ordinal)
                            .map_err(|_| EvidenceAssemblyError::MemberLimit)?,
                        i64::try_from(run_member_ordinal)
                            .map_err(|_| EvidenceAssemblyError::MemberLimit)?,
                        occurrence_id.as_str()
                    ],
                )
                .await
                .map_err(storage_error)?;
            assembly_ordinal = assembly_ordinal
                .checked_add(1)
                .ok_or(EvidenceAssemblyError::MemberLimit)?;
        }
    }
    let ordered_member_ids = write
        .ordered_occurrences
        .iter()
        .map(|occurrence| occurrence.occurrence_id.as_str())
        .collect::<Vec<_>>();
    verify_member_ids(
        transaction,
        "SELECT occurrence_id
         FROM evidence_span_members
         WHERE span_id = ?1
         ORDER BY assembly_ordinal",
        write.span.span_id.as_str(),
        &ordered_member_ids,
    )
    .await?;
    Ok(())
}

async fn insert_projection_receipt(
    transaction: &(impl Executor + Sync),
    write: &EvidenceAssemblyWrite,
) -> Result<(), EvidenceAssemblyError> {
    insert_immutable_json(
        transaction,
        "evidence_span_projection_receipts",
        "projection_receipt_id",
        write.projection_receipt.projection_receipt_id.as_str(),
        write.projection_receipt.record_digest()?.as_str(),
        &serialize(&write.projection_receipt)?,
        Some(("span_id", vec![write.span.span_id.as_str().to_owned()])),
    )
    .await
}

async fn insert_contribution(
    transaction: &(impl Executor + Sync),
    write: &EvidenceAssemblyWrite,
    owner_digest: &ManifestDigest,
) -> Result<(), EvidenceAssemblyError> {
    insert_immutable_json(
        transaction,
        "evidence_retriever_contributions",
        "contribution_id",
        write.contribution.contribution_id.as_str(),
        write.contribution.record_digest()?.as_str(),
        &serialize(&write.contribution)?,
        Some((
            "owner_digest, span_id, anchor_id",
            vec![
                owner_digest.as_str().to_owned(),
                write.span.span_id.as_str().to_owned(),
                write.contribution.anchor_id.as_str().to_owned(),
            ],
        )),
    )
    .await
}

async fn insert_immutable_json(
    transaction: &(impl Executor + Sync),
    table: &str,
    id_column: &str,
    id: &str,
    record_digest: &str,
    record_json: &str,
    extra: Option<(&str, Vec<String>)>,
) -> Result<(), EvidenceAssemblyError> {
    let (columns, values) = extra.unwrap_or(("", Vec::new()));
    let extra_columns = if columns.is_empty() {
        String::new()
    } else {
        format!(", {columns}")
    };
    let mut placeholders = vec!["?1".to_owned(), "?2".to_owned(), "?3".to_owned()];
    placeholders.extend((0..values.len()).map(|index| format!("?{}", index + 4)));
    let sql = format!(
        "INSERT OR IGNORE INTO {table} ({id_column}, record_digest, record_json{extra_columns})
         VALUES ({})",
        placeholders.join(", ")
    );
    let mut parameters = vec![
        Value::Text(id.to_owned()),
        Value::Text(record_digest.to_owned()),
        Value::Text(record_json.to_owned()),
    ];
    parameters.extend(values.into_iter().map(Value::Text));
    transaction
        .execute(&sql, parameters)
        .await
        .map_err(storage_error)?;
    let mut rows = transaction
        .query(
            &format!("SELECT record_digest FROM {table} WHERE {id_column} = ?1"),
            params![id],
        )
        .await
        .map_err(storage_error)?;
    let stored = next_string(&mut rows)
        .await?
        .ok_or_else(|| EvidenceAssemblyError::Storage(format!("{table} insert disappeared")))?;
    if stored != record_digest {
        return Err(EvidenceAssemblyError::ReplayConflict);
    }
    Ok(())
}

async fn verify_member_ids(
    transaction: &(impl Executor + Sync),
    sql: &str,
    parent_id: &str,
    expected: &[&str],
) -> Result<(), EvidenceAssemblyError> {
    let mut rows = transaction
        .query(sql, params![parent_id])
        .await
        .map_err(storage_error)?;
    let mut actual = Vec::new();
    while let Some(row) = rows.next().await.map_err(storage_error)? {
        actual.push(row.get::<String>(0).map_err(storage_error)?);
    }
    if actual.len() != expected.len()
        || actual
            .iter()
            .zip(expected)
            .any(|(actual, expected)| actual != expected)
    {
        return Err(EvidenceAssemblyError::ReplayConflict);
    }
    Ok(())
}

async fn load_anchor(
    connection: &(impl QueryExecutor + Sync),
    anchor_id: &str,
) -> Result<Option<RetrievalAnchorRecordV2>, EvidenceAssemblyError> {
    let mut rows = connection
        .query(
            "SELECT anchor_json FROM retrieval_anchors WHERE anchor_id = ?1",
            params![anchor_id],
        )
        .await
        .map_err(storage_error)?;
    let Some(json) = next_string(&mut rows).await? else {
        return Ok(None);
    };
    let anchor: RetrievalAnchorRecordV2 = deserialize(&json)?;
    anchor
        .validate()
        .map_err(|_| EvidenceAssemblyError::Invalid("stored retrieval anchor"))?;
    if anchor.anchor_id().as_str() != anchor_id {
        return Err(EvidenceAssemblyError::Invalid(
            "retrieval anchor identity mismatch",
        ));
    }
    Ok(Some(anchor))
}

fn derived_anchor_id(
    target_kind: &str,
    target_id: &str,
    owner: &EvidenceOwnerBinding,
) -> Result<RetrievalAnchorId, EvidenceAssemblyError> {
    let id = derived_id(
        DERIVED_ANCHOR_ID_DOMAIN,
        &(DERIVED_ANCHOR_ID_DOMAIN, target_kind, target_id, owner),
    )?;
    RetrievalAnchorId::new(id)
        .map_err(|_| EvidenceAssemblyError::Invalid("derived retrieval anchor id"))
}

fn sha256_text(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    format!("sha256:{}", hex::encode(digest.finalize()))
}

fn derived_id<T: Serialize>(
    domain: &'static str,
    material: &T,
) -> Result<String, EvidenceAssemblyError> {
    let digest = canonical_sha256(&(domain, material)).map_err(contract_error)?;
    Ok(format!(
        "{domain}.{}",
        digest
            .as_str()
            .strip_prefix("sha256:")
            .unwrap_or(digest.as_str())
    ))
}

fn serialize<T: Serialize>(value: &T) -> Result<String, EvidenceAssemblyError> {
    serde_json::to_string(value)
        .map_err(|error| EvidenceAssemblyError::Serialization(error.to_string()))
}

fn deserialize<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T, EvidenceAssemblyError> {
    serde_json::from_str(value)
        .map_err(|error| EvidenceAssemblyError::Serialization(error.to_string()))
}

fn contract_error(error: impl std::fmt::Display) -> EvidenceAssemblyError {
    EvidenceAssemblyError::Serialization(error.to_string())
}

fn storage_error(error: impl std::fmt::Display) -> EvidenceAssemblyError {
    EvidenceAssemblyError::Storage(error.to_string())
}

fn validate_label(value: &str, _field: &'static str) -> Result<(), EvidenceAssemblyError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 512
        || value.chars().any(char::is_control)
    {
        return Err(EvidenceAssemblyError::Invalid("bounded identifier"));
    }
    Ok(())
}

fn i64_from_u64(value: u64) -> Result<i64, EvidenceAssemblyError> {
    i64::try_from(value).map_err(|_| EvidenceAssemblyError::MemberLimit)
}

fn u64_from_i64(value: i64, field: &'static str) -> Result<u64, EvidenceAssemblyError> {
    u64::try_from(value).map_err(|_| EvidenceAssemblyError::Invalid(field))
}

async fn next_string(rows: &mut Rows) -> Result<Option<String>, EvidenceAssemblyError> {
    rows.next()
        .await
        .map_err(storage_error)?
        .map(|row| row.get(0).map_err(storage_error))
        .transpose()
}

#[cfg(test)]
#[path = "evidence_assembly_tests.rs"]
mod tests;
