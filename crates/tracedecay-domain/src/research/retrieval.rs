use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::canonical::canonical_sha256;
use super::coverage::{CoverageReportV1, RetentionClass};
use super::error::DomainError;
use super::evidence::{Confidence, EvidenceClass, LogSafeText, validate_evidence_confidence};
use super::id::{
    AccessPolicyDigest, CapabilityId, CatalogGenerationId, ComponentVersion, DataVersionDigest,
    LocatorDigest, ManifestDigest, NonEmptyUniqueVec, ObservationId, PrivacyDomainId, ProvenanceId,
    QueryId, RegistryManifestDigest, ResearchAnchorId, RetrievalAnchorId, RetrievalRecipeId,
    ScopeResolutionId, SourceInstanceId, UseCaseId, ensure_unique,
};
use super::subjects::{
    ActivityResearchFacetV1, CatalogSnapshotRefV1, EntityKind, EntityRef, ResearchAnchorSubjectV1,
};
use super::time::{TimeInterval, UtcMicros};
use super::watermark::VectorWatermark;

/// Keyed locator digest whose value is meaningful only inside its privacy domain.
///
/// This is intentionally not interchangeable with [`LocatorDigest`]. Callers
/// must construct it through the validating string constructor after computing
/// the locator digest with the privacy-domain key.
///
/// ```compile_fail,E0308
/// use tracedecay_domain::research::{
///     CapabilityId, LocatorDigest, RetrievalExpansionMode, RetrievalExpansionRecipeV1,
/// };
///
/// fn cannot_use_unkeyed_digest(capability_id: CapabilityId, digest: LocatorDigest) {
///     let _ = RetrievalExpansionRecipeV1 {
///         capability_id,
///         expansion: RetrievalExpansionMode::ExactTarget,
///         bounded_arguments_digest: digest,
///     };
/// }
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct PrivacyDomainBoundLocatorDigest(LocatorDigest);

impl PrivacyDomainBoundLocatorDigest {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        Self::try_from(value.into())
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.0.validate()
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl TryFrom<String> for PrivacyDomainBoundLocatorDigest {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        LocatorDigest::try_from(value).map(Self)
    }
}

impl TryFrom<&str> for PrivacyDomainBoundLocatorDigest {
    type Error = DomainError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_owned())
    }
}

/// Digest of a sanitized artifact output, distinct from manifest identity.
///
/// The private representation prevents a generic [`ManifestDigest`] from being
/// assigned to an artifact anchor without an explicit validated construction.
///
/// ```compile_fail,E0308
/// use tracedecay_domain::research::{EntityRef, ManifestDigest, RetrievalAnchorTargetV1};
///
/// fn cannot_use_manifest_digest(artifact: EntityRef, digest: ManifestDigest) {
///     let _ = RetrievalAnchorTargetV1::Artifact {
///         artifact,
///         sanitized_output_digest: digest,
///     };
/// }
/// ```
/// Compatibility-only target shape for the V1 research-manifest wire format.
/// New authoritative anchors use `RetrievalAnchorTarget`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct SanitizedOutputDigest(ManifestDigest);

impl SanitizedOutputDigest {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        Self::try_from(value.into())
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.0.validate()
    }
}

impl TryFrom<String> for SanitizedOutputDigest {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        ManifestDigest::try_from(value).map(Self)
    }
}

impl TryFrom<&str> for SanitizedOutputDigest {
    type Error = DomainError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_owned())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "target",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RetrievalAnchorCompatibilityTargetV1 {
    Entity(EntityRef),
    Query(QueryId),
    SourcePosition {
        source: SourceInstanceId,
        position_digest: PrivacyDomainBoundLocatorDigest,
    },
    Artifact {
        artifact: EntityRef,
        sanitized_output_digest: SanitizedOutputDigest,
    },
}

/// Source-compatible name for the legacy research-manifest target.
pub type RetrievalAnchorTargetV1 = RetrievalAnchorCompatibilityTargetV1;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceIdentityClass {
    ProfileActivity,
    ProjectEvidence,
    GraphGeneration,
    BlobArtifact,
    ExternalDelivery,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalViewV1 {
    SanitizedNative,
    Representative,
    EntityVersion,
    QueryResult,
    SourceObservation,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalExpansionMode {
    ExactTarget,
    AdjacentContext,
    RepresentedMembers,
    SourceLineage,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalExpansionRecipeV1 {
    pub capability_id: CapabilityId,
    pub expansion: RetrievalExpansionMode,
    pub bounded_arguments_digest: PrivacyDomainBoundLocatorDigest,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PayloadAccessState {
    Eligible,
    Redacted,
    Quarantined,
    RetentionExpired,
    Deleted,
    Unavailable,
    Ambiguous,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum AnchorDurabilityClass {
    DurableEvidence,
    RetentionBound { expires_at: UtcMicros },
    Archived,
}

/// Compatibility-only V1 research-manifest record.
///
/// This is not the authoritative persisted anchor model. New storage and
/// application code use `RetrievalAnchorRecord`, whose identity is derived by
/// the domain rather than supplied by a caller.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalAnchorCompatibilityRecordV1 {
    pub anchor_id: RetrievalAnchorId,
    pub target: RetrievalAnchorTargetV1,
    pub target_kind: EntityKind,
    pub resolved_scope_id: ScopeResolutionId,
    pub privacy_domain_id: PrivacyDomainId,
    pub access_policy_digest: AccessPolicyDigest,
    pub source_identity_class: SourceIdentityClass,
    pub immutable_source_refs: Vec<EntityRef>,
    pub source_observations: Vec<ObservationId>,
    pub snapshot: VectorWatermark,
    pub schema_registry_digest: RegistryManifestDigest,
    pub capability_catalog: CatalogSnapshotRefV1,
    pub data_version_digest: DataVersionDigest,
    pub projection_version: ComponentVersion,
    pub view_algorithm_version: Option<ComponentVersion>,
    pub view: RetrievalViewV1,
    pub expansion_recipe: RetrievalExpansionRecipeV1,
    pub canonical_request_digest: PrivacyDomainBoundLocatorDigest,
    pub provenance: Vec<ProvenanceId>,
    pub payload_access: PayloadAccessState,
    pub retention_class: RetentionClass,
    pub created_at: UtcMicros,
    pub durability: AnchorDurabilityClass,
}

/// Source-compatible name for the legacy research-manifest record.
pub type RetrievalAnchorRecordV1 = RetrievalAnchorCompatibilityRecordV1;

impl RetrievalAnchorRecordV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.anchor_id.validate()?;
        self.resolved_scope_id.validate()?;
        self.privacy_domain_id.validate()?;
        self.access_policy_digest.validate()?;
        self.schema_registry_digest.validate()?;
        self.capability_catalog.validate()?;
        self.data_version_digest.validate()?;
        self.projection_version.validate()?;
        if let Some(version) = &self.view_algorithm_version {
            version.validate()?;
        }
        self.expansion_recipe.capability_id.validate()?;
        self.expansion_recipe.bounded_arguments_digest.validate()?;
        self.canonical_request_digest.validate()?;
        ensure_unique(
            self.immutable_source_refs.iter().map(|source| &source.id),
            "retrieval anchor immutable_source_refs",
        )?;
        for source in &self.immutable_source_refs {
            source.validate()?;
        }
        ensure_unique(
            self.source_observations.iter(),
            "retrieval anchor source_observations",
        )?;
        ensure_unique(self.provenance.iter(), "retrieval anchor provenance")?;
        match &self.target {
            RetrievalAnchorTargetV1::Entity(entity) => {
                entity.validate()?;
                if entity.kind != self.target_kind {
                    return Err(DomainError::UnknownReference {
                        field: "retrieval anchor target_kind",
                    });
                }
            }
            RetrievalAnchorTargetV1::Query(query) => {
                query.validate()?;
                if !matches!(
                    &self.target_kind,
                    EntityKind::Other(kind) if kind.as_str() == "query"
                ) {
                    return Err(DomainError::UnknownReference {
                        field: "retrieval anchor query target_kind",
                    });
                }
            }
            RetrievalAnchorTargetV1::SourcePosition {
                source,
                position_digest,
            } => {
                source.validate()?;
                position_digest.validate()?;
                if self.target_kind != EntityKind::SourceRecord {
                    return Err(DomainError::UnknownReference {
                        field: "retrieval anchor source position target_kind",
                    });
                }
            }
            RetrievalAnchorTargetV1::Artifact {
                artifact,
                sanitized_output_digest,
            } => {
                artifact.validate()?;
                sanitized_output_digest.validate()?;
                if self.target_kind != EntityKind::Artifact || artifact.kind != EntityKind::Artifact
                {
                    return Err(DomainError::UnknownReference {
                        field: "retrieval anchor artifact target_kind",
                    });
                }
            }
        }
        Ok(())
    }
}

/// Snapshot-pinned external catalog used to validate research manifests.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalAnchorCatalogV1 {
    pub snapshot: CatalogSnapshotRefV1,
    pub records: BTreeMap<RetrievalAnchorId, RetrievalAnchorRecordV1>,
}

/// Stable V1 digest surface for an immutable retrieval-anchor record. The
/// copied `capability_catalog` reference is deliberately excluded: the
/// catalog generation is bound once by the enclosing digest payload, and the
/// full reference is checked against the verified snapshot during validation.
#[derive(Serialize)]
struct RetrievalAnchorRecordDigestV1<'a> {
    anchor_id: &'a RetrievalAnchorId,
    target: &'a RetrievalAnchorTargetV1,
    target_kind: &'a EntityKind,
    resolved_scope_id: &'a ScopeResolutionId,
    privacy_domain_id: &'a PrivacyDomainId,
    access_policy_digest: &'a AccessPolicyDigest,
    source_identity_class: &'a SourceIdentityClass,
    immutable_source_refs: &'a [EntityRef],
    source_observations: &'a [ObservationId],
    snapshot: &'a VectorWatermark,
    schema_registry_digest: &'a RegistryManifestDigest,
    data_version_digest: &'a DataVersionDigest,
    projection_version: &'a ComponentVersion,
    view_algorithm_version: &'a Option<ComponentVersion>,
    view: &'a RetrievalViewV1,
    expansion_recipe: &'a RetrievalExpansionRecipeV1,
    canonical_request_digest: &'a PrivacyDomainBoundLocatorDigest,
    provenance: &'a [ProvenanceId],
    payload_access: &'a PayloadAccessState,
    retention_class: &'a RetentionClass,
    created_at: &'a UtcMicros,
    durability: &'a AnchorDurabilityClass,
}

impl<'a> From<&'a RetrievalAnchorRecordV1> for RetrievalAnchorRecordDigestV1<'a> {
    fn from(record: &'a RetrievalAnchorRecordV1) -> Self {
        Self {
            anchor_id: &record.anchor_id,
            target: &record.target,
            target_kind: &record.target_kind,
            resolved_scope_id: &record.resolved_scope_id,
            privacy_domain_id: &record.privacy_domain_id,
            access_policy_digest: &record.access_policy_digest,
            source_identity_class: &record.source_identity_class,
            immutable_source_refs: &record.immutable_source_refs,
            source_observations: &record.source_observations,
            snapshot: &record.snapshot,
            schema_registry_digest: &record.schema_registry_digest,
            data_version_digest: &record.data_version_digest,
            projection_version: &record.projection_version,
            view_algorithm_version: &record.view_algorithm_version,
            view: &record.view,
            expansion_recipe: &record.expansion_recipe,
            canonical_request_digest: &record.canonical_request_digest,
            provenance: &record.provenance,
            payload_access: &record.payload_access,
            retention_class: &record.retention_class,
            created_at: &record.created_at,
            durability: &record.durability,
        }
    }
}

const RETRIEVAL_ANCHOR_CATALOG_DIGEST_DOMAIN: &str = "tracedecay.retrieval-anchor-catalog.v1";

/// Canonical digest envelope for a retrieval-anchor catalog. Keeping this
/// projection named and versioned makes the exact authenticated byte surface
/// explicit and prevents unrelated wire-format additions from silently
/// changing the snapshot digest.
#[derive(Serialize)]
struct RetrievalAnchorCatalogDigestV1<'a> {
    domain: &'static str,
    generation: &'a CatalogGenerationId,
    records: BTreeMap<&'a RetrievalAnchorId, RetrievalAnchorRecordDigestV1<'a>>,
}

impl RetrievalAnchorCatalogV1 {
    /// Compute the domain-separated canonical digest over the catalog
    /// generation and exact keyed V1 record projection.
    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        let records = self
            .records
            .iter()
            .map(|(anchor_id, record)| (anchor_id, record.into()))
            .collect();
        canonical_sha256(&RetrievalAnchorCatalogDigestV1 {
            domain: RETRIEVAL_ANCHOR_CATALOG_DIGEST_DOMAIN,
            generation: &self.snapshot.generation,
            records,
        })
    }

    pub fn verify_digest(&self) -> Result<(), DomainError> {
        if self.compute_digest()? != self.snapshot.digest {
            return Err(DomainError::SnapshotMismatch {
                field: "retrieval catalog snapshot digest",
            });
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.snapshot.validate()?;
        self.verify_digest()?;
        for (anchor_id, record) in &self.records {
            if anchor_id != &record.anchor_id {
                return Err(DomainError::UnknownReference {
                    field: "retrieval catalog record key",
                });
            }
            record.validate()?;
            if record.capability_catalog != self.snapshot {
                return Err(DomainError::SnapshotMismatch {
                    field: "retrieval catalog record capability_catalog",
                });
            }
        }
        Ok(())
    }

    pub fn get(&self, anchor_id: &RetrievalAnchorId) -> Option<&RetrievalAnchorRecordV1> {
        self.records.get(anchor_id)
    }
}

/// Protected, versioned recipe metadata; it contains IDs and proven-safe text only.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalRecipeV1 {
    pub recipe_id: RetrievalRecipeId,
    pub use_case: UseCaseId,
    pub anchors: NonEmptyUniqueVec<RetrievalAnchorId>,
    pub purpose: LogSafeText,
    pub snapshot: VectorWatermark,
}

impl RetrievalRecipeV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.recipe_id.validate()?;
        self.use_case.validate()?;
        for anchor in self.anchors.iter() {
            anchor.validate()?;
        }
        Ok(())
    }
}

/// One immutable entry in a versioned research manifest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResearchContextAnchorV1 {
    pub entry_id: ResearchAnchorId,
    pub retrieval_anchors: NonEmptyUniqueVec<RetrievalAnchorId>,
    pub purpose: LogSafeText,
    pub subject: ResearchAnchorSubjectV1,
    pub related_activity: Option<ActivityResearchFacetV1>,
    pub occurred_window: Option<TimeInterval>,
    pub source_observation_ids: Vec<ObservationId>,
    pub evidence_class: EvidenceClass,
    pub confidence: Confidence,
    pub expected_subject: LogSafeText,
    pub retrieval_recipe_id: RetrievalRecipeId,
    pub snapshot: VectorWatermark,
    pub coverage: CoverageReportV1,
}

impl ResearchContextAnchorV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.entry_id.validate()?;
        for anchor in self.retrieval_anchors.iter() {
            anchor.validate()?;
        }
        self.subject.validate()?;
        if matches!(self.subject, ResearchAnchorSubjectV1::Activity(_))
            && self.related_activity.is_some()
        {
            return Err(DomainError::ActivityFacetOnActivitySubject);
        }
        if let Some(activity) = &self.related_activity {
            activity.validate()?;
        }
        if let Some(window) = &self.occurred_window {
            window.validate()?;
        }
        ensure_unique(self.source_observation_ids.iter(), "source_observation_ids")?;
        self.retrieval_recipe_id.validate()?;
        self.coverage.validate()?;
        validate_evidence_confidence(self.evidence_class, self.confidence)
    }

    pub(crate) fn provider_activity(&self) -> Option<&ActivityResearchFacetV1> {
        self.subject
            .activity_facet()
            .or(self.related_activity.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZERO_SHA256: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String, Error = DomainError>,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    #[test]
    fn anchor_digest_newtypes_validate_and_round_trip_serialization() {
        let locator = PrivacyDomainBoundLocatorDigest::new(ZERO_SHA256).unwrap();
        let locator_json = serde_json::to_string(&locator).unwrap();
        assert_eq!(locator_json, format!("\"{ZERO_SHA256}\""));
        assert_eq!(
            serde_json::from_str::<PrivacyDomainBoundLocatorDigest>(&locator_json).unwrap(),
            locator
        );

        let sanitized = SanitizedOutputDigest::new(ZERO_SHA256).unwrap();
        let sanitized_json = serde_json::to_string(&sanitized).unwrap();
        assert_eq!(sanitized_json, format!("\"{ZERO_SHA256}\""));
        assert_eq!(
            serde_json::from_str::<SanitizedOutputDigest>(&sanitized_json).unwrap(),
            sanitized
        );

        assert!(PrivacyDomainBoundLocatorDigest::new("not-a-digest").is_err());
        assert!(SanitizedOutputDigest::new("not-a-digest").is_err());
    }

    #[test]
    fn anchor_digest_newtypes_are_not_generic_digest_aliases() {
        use std::any::TypeId;

        assert_ne!(
            TypeId::of::<PrivacyDomainBoundLocatorDigest>(),
            TypeId::of::<LocatorDigest>()
        );
        assert_ne!(
            TypeId::of::<SanitizedOutputDigest>(),
            TypeId::of::<ManifestDigest>()
        );
    }

    fn valid_catalog() -> RetrievalAnchorCatalogV1 {
        let anchor_id: RetrievalAnchorId = id("retrieval.fixture");
        let document = EntityRef {
            id: id("document.fixture"),
            kind: EntityKind::Document,
        };
        let snapshot = CatalogSnapshotRefV1 {
            generation: id("catalog.fixture.v1"),
            digest: id(ZERO_SHA256),
        };
        let record = RetrievalAnchorRecordV1 {
            anchor_id: anchor_id.clone(),
            target: RetrievalAnchorTargetV1::Entity(document.clone()),
            target_kind: EntityKind::Document,
            resolved_scope_id: id("scope.fixture"),
            privacy_domain_id: id("privacy.fixture"),
            access_policy_digest: id(ZERO_SHA256),
            source_identity_class: SourceIdentityClass::ProjectEvidence,
            immutable_source_refs: vec![document],
            source_observations: vec![id("observation.fixture")],
            snapshot: VectorWatermark {
                components: BTreeMap::from([(id("shard.fixture"), 7)]),
            },
            schema_registry_digest: id(ZERO_SHA256),
            capability_catalog: snapshot.clone(),
            data_version_digest: id(ZERO_SHA256),
            projection_version: id("projection.fixture.v1"),
            view_algorithm_version: None,
            view: RetrievalViewV1::EntityVersion,
            expansion_recipe: RetrievalExpansionRecipeV1 {
                capability_id: id("capability.research.exact"),
                expansion: RetrievalExpansionMode::ExactTarget,
                bounded_arguments_digest: id(ZERO_SHA256),
            },
            canonical_request_digest: id(ZERO_SHA256),
            provenance: vec![id("provenance.fixture")],
            payload_access: PayloadAccessState::Eligible,
            retention_class: RetentionClass::new("retention.fixture").unwrap(),
            created_at: UtcMicros(1),
            durability: AnchorDurabilityClass::DurableEvidence,
        };
        let mut catalog = RetrievalAnchorCatalogV1 {
            snapshot,
            records: BTreeMap::from([(anchor_id, record)]),
        };
        catalog.snapshot.digest = catalog.compute_digest().unwrap();
        for record in catalog.records.values_mut() {
            record.capability_catalog = catalog.snapshot.clone();
        }
        catalog
    }

    fn assert_snapshot_rejects_record_mutation(mutate: impl FnOnce(&mut RetrievalAnchorRecordV1)) {
        let mut catalog = valid_catalog();
        catalog.validate().expect("sealed catalog is valid");
        let unchanged_snapshot = catalog.snapshot.clone();
        let record = catalog.records.values_mut().next().unwrap();
        mutate(record);

        assert_eq!(catalog.snapshot, unchanged_snapshot);
        assert_eq!(
            catalog.validate(),
            Err(DomainError::SnapshotMismatch {
                field: "retrieval catalog snapshot digest",
            })
        );
    }

    #[test]
    fn catalog_snapshot_digest_authenticates_security_relevant_record_families() {
        assert_snapshot_rejects_record_mutation(|record| {
            record.target = RetrievalAnchorTargetV1::Entity(EntityRef {
                id: id("document.mutated"),
                kind: EntityKind::Document,
            });
        });
        assert_snapshot_rejects_record_mutation(|record| {
            record.provenance = vec![id("provenance.mutated")];
        });
        assert_snapshot_rejects_record_mutation(|record| {
            record.access_policy_digest =
                id("sha256:1111111111111111111111111111111111111111111111111111111111111111");
        });
        assert_snapshot_rejects_record_mutation(|record| {
            record.payload_access = PayloadAccessState::Redacted;
        });
        assert_snapshot_rejects_record_mutation(|record| {
            record.expansion_recipe.expansion = RetrievalExpansionMode::AdjacentContext;
        });
    }
}
