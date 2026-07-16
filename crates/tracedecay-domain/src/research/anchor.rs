use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

use crate::observation::{
    CanonicalObservationIdV1, ObservationScopeV1, ObservationSourceGenerationV1,
};

use super::canonical::canonical_sha256;
use super::coverage::{CoverageReportV1, RetentionClass};
use super::error::DomainError;
use super::evidence::{EvidenceClass, SanitizationReceiptRefV1};
use super::id::{
    BlobId, CommitId, ProjectionGenerationId, RepositoryCaptureId, RepositoryId, RetrievalAnchorId,
    TreeId,
};
use super::resolution::ResolutionAuthorizationV1;
use super::retrieval::{
    AnchorDurabilityClass, PayloadAccessState, PrivacyDomainBoundLocatorDigest,
};
use super::subjects::EntityRef;
use super::time::{TimeInterval, UtcMicros};
use super::watermark::VectorWatermark;

const RETRIEVAL_ANCHOR_V2_ID_DOMAIN: &str = "tracedecay.retrieval-anchor.v2";

/// Meaning of a privacy-domain-safe native locator digest.
///
/// The digest is the only locator material admitted to the anchor contract;
/// literal paths, ref names, queries, and provider payloads remain in their
/// owning stores.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NativeAliasKindV2 {
    ProviderRecord,
    LegacyIdentity,
    RepositoryRoot,
    Worktree,
    Ref,
    Path,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeAliasV2 {
    kind: NativeAliasKindV2,
    locator_digest: PrivacyDomainBoundLocatorDigest,
}

impl NativeAliasV2 {
    pub fn new(
        kind: NativeAliasKindV2,
        locator_digest: PrivacyDomainBoundLocatorDigest,
    ) -> Result<Self, DomainError> {
        locator_digest.validate()?;
        Ok(Self {
            kind,
            locator_digest,
        })
    }

    pub fn kind(&self) -> NativeAliasKindV2 {
        self.kind
    }

    pub fn locator_digest(&self) -> &PrivacyDomainBoundLocatorDigest {
        &self.locator_digest
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.locator_digest.validate()
    }
}

impl<'de> Deserialize<'de> for NativeAliasV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            kind: NativeAliasKindV2,
            locator_digest: PrivacyDomainBoundLocatorDigest,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.kind, wire.locator_digest).map_err(serde::de::Error::custom)
    }
}

/// Immutable retrieval target. Mutable Git routing names are aliases, never
/// target identities.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "target",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RetrievalAnchorTargetV2 {
    ExactObservation(CanonicalObservationIdV1),
    Entity(EntityRef),
    ExactRepositoryCommit {
        repository_id: RepositoryId,
        commit_id: CommitId,
    },
    ExactRepositoryTree {
        repository_id: RepositoryId,
        tree_id: TreeId,
    },
    ExactRepositoryBlob {
        repository_id: RepositoryId,
        blob_id: BlobId,
    },
    RepositoryCapture {
        repository_id: RepositoryId,
        capture_id: RepositoryCaptureId,
        receipt: SanitizationReceiptRefV1,
    },
}

impl RetrievalAnchorTargetV2 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::ExactObservation(_) => Ok(()),
            Self::Entity(entity) => entity.validate(),
            Self::ExactRepositoryCommit {
                repository_id,
                commit_id,
            } => {
                repository_id.validate()?;
                commit_id.validate()?;
                validate_git_object_id(commit_id.as_str(), "retrieval anchor commit")
            }
            Self::ExactRepositoryTree {
                repository_id,
                tree_id,
            } => {
                repository_id.validate()?;
                tree_id.validate()?;
                validate_git_object_id(tree_id.as_str(), "retrieval anchor tree")
            }
            Self::ExactRepositoryBlob {
                repository_id,
                blob_id,
            } => {
                repository_id.validate()?;
                blob_id.validate()?;
                validate_git_object_id(blob_id.as_str(), "retrieval anchor blob")
            }
            Self::RepositoryCapture {
                repository_id,
                capture_id,
                receipt,
            } => {
                repository_id.validate()?;
                capture_id.validate()?;
                receipt.validate()
            }
        }
    }

    fn requires_project_owner(&self) -> bool {
        matches!(
            self,
            Self::ExactRepositoryCommit { .. }
                | Self::ExactRepositoryTree { .. }
                | Self::ExactRepositoryBlob { .. }
                | Self::RepositoryCapture { .. }
        )
    }
}

/// Immutable generation identity of the source that produced an anchor.
/// Repository capture generations are never confused with observation source
/// generations, projection generations, or store watermarks.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "generation",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum AnchorSourceGenerationV2 {
    Observation(ObservationSourceGenerationV1),
    RepositoryCapture(RepositoryCaptureId),
    Unavailable,
    Unknown,
}

impl AnchorSourceGenerationV2 {
    fn validate_for_target(&self, target: &RetrievalAnchorTargetV2) -> Result<(), DomainError> {
        let valid = match (self, target) {
            (Self::Observation(_), RetrievalAnchorTargetV2::ExactObservation(_)) => true,
            (
                Self::RepositoryCapture(source),
                RetrievalAnchorTargetV2::RepositoryCapture { capture_id, .. },
            ) => source == capture_id,
            (
                Self::RepositoryCapture(_) | Self::Unavailable | Self::Unknown,
                RetrievalAnchorTargetV2::ExactRepositoryCommit { .. }
                | RetrievalAnchorTargetV2::ExactRepositoryTree { .. }
                | RetrievalAnchorTargetV2::ExactRepositoryBlob { .. },
            ) => true,
            (_, RetrievalAnchorTargetV2::Entity(_)) => true,
            _ => false,
        };
        if !valid {
            return Err(DomainError::UnknownReference {
                field: "retrieval anchor source generation",
            });
        }
        if let Self::RepositoryCapture(capture_id) = self {
            capture_id.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AnchorProvenanceRelationV2 {
    CapturedFrom,
    Produced,
    Observed,
    ExecutedIn,
    Discussed,
    CopiedFrom,
    DerivedFrom,
}

/// Owner-bound reference to an earlier anchor in the provenance graph.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct AnchorLineageRefV2 {
    relation: AnchorProvenanceRelationV2,
    anchor_id: RetrievalAnchorId,
    owner: ObservationScopeV1,
}

impl AnchorLineageRefV2 {
    pub fn new(
        relation: AnchorProvenanceRelationV2,
        anchor_id: RetrievalAnchorId,
        owner: ObservationScopeV1,
    ) -> Result<Self, DomainError> {
        anchor_id.validate()?;
        validate_owner(&owner)?;
        Ok(Self {
            relation,
            anchor_id,
            owner,
        })
    }

    pub fn relation(&self) -> AnchorProvenanceRelationV2 {
        self.relation
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        &self.anchor_id
    }

    pub fn owner(&self) -> &ObservationScopeV1 {
        &self.owner
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.anchor_id.validate()?;
        validate_owner(&self.owner)
    }
}

impl<'de> Deserialize<'de> for AnchorLineageRefV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            relation: AnchorProvenanceRelationV2,
            anchor_id: RetrievalAnchorId,
            owner: ObservationScopeV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.relation, wire.anchor_id, wire.owner).map_err(serde::de::Error::custom)
    }
}

/// Constructor material for a validated V2 record. `anchor_id` is omitted
/// because it is derived exclusively from the owner and immutable target.
#[derive(Clone, Debug)]
pub struct RetrievalAnchorRecordV2Parts {
    pub target: RetrievalAnchorTargetV2,
    pub owner: ObservationScopeV1,
    pub aliases: Vec<NativeAliasV2>,
    pub occurred_at: Option<TimeInterval>,
    pub ingested_at: UtcMicros,
    pub evidence_class: EvidenceClass,
    pub source_generation: AnchorSourceGenerationV2,
    pub projection_generation: ProjectionGenerationId,
    pub projection_watermark: VectorWatermark,
    pub coverage: CoverageReportV1,
    pub source_observations: Vec<CanonicalObservationIdV1>,
    pub source_anchors: Vec<AnchorLineageRefV2>,
    pub authorization: ResolutionAuthorizationV1,
    pub payload_access: PayloadAccessState,
    pub retention_class: RetentionClass,
    pub durability: AnchorDurabilityClass,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalAnchorRecordV2 {
    anchor_id: RetrievalAnchorId,
    target: RetrievalAnchorTargetV2,
    owner: ObservationScopeV1,
    aliases: Vec<NativeAliasV2>,
    occurred_at: Option<TimeInterval>,
    ingested_at: UtcMicros,
    evidence_class: EvidenceClass,
    source_generation: AnchorSourceGenerationV2,
    projection_generation: ProjectionGenerationId,
    projection_watermark: VectorWatermark,
    coverage: CoverageReportV1,
    source_observations: Vec<CanonicalObservationIdV1>,
    source_anchors: Vec<AnchorLineageRefV2>,
    authorization: ResolutionAuthorizationV1,
    payload_access: PayloadAccessState,
    retention_class: RetentionClass,
    durability: AnchorDurabilityClass,
}

impl RetrievalAnchorRecordV2 {
    pub fn new(parts: RetrievalAnchorRecordV2Parts) -> Result<Self, DomainError> {
        let anchor_id = derive_anchor_id(&parts.owner, &parts.target)?;
        let record = Self {
            anchor_id,
            target: parts.target,
            owner: parts.owner,
            aliases: parts.aliases,
            occurred_at: parts.occurred_at,
            ingested_at: parts.ingested_at,
            evidence_class: parts.evidence_class,
            source_generation: parts.source_generation,
            projection_generation: parts.projection_generation,
            projection_watermark: parts.projection_watermark,
            coverage: parts.coverage,
            source_observations: parts.source_observations,
            source_anchors: parts.source_anchors,
            authorization: parts.authorization,
            payload_access: parts.payload_access,
            retention_class: parts.retention_class,
            durability: parts.durability,
        };
        record.validate()?;
        Ok(record)
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        &self.anchor_id
    }

    pub fn target(&self) -> &RetrievalAnchorTargetV2 {
        &self.target
    }

    pub fn owner(&self) -> &ObservationScopeV1 {
        &self.owner
    }

    pub fn aliases(&self) -> &[NativeAliasV2] {
        &self.aliases
    }

    pub fn occurred_at(&self) -> Option<TimeInterval> {
        self.occurred_at
    }

    pub fn ingested_at(&self) -> UtcMicros {
        self.ingested_at
    }

    pub fn evidence_class(&self) -> EvidenceClass {
        self.evidence_class
    }

    pub fn source_generation(&self) -> &AnchorSourceGenerationV2 {
        &self.source_generation
    }

    pub fn projection_generation(&self) -> &ProjectionGenerationId {
        &self.projection_generation
    }

    pub fn projection_watermark(&self) -> &VectorWatermark {
        &self.projection_watermark
    }

    pub fn coverage(&self) -> &CoverageReportV1 {
        &self.coverage
    }

    pub fn source_observations(&self) -> &[CanonicalObservationIdV1] {
        &self.source_observations
    }

    pub fn source_anchors(&self) -> &[AnchorLineageRefV2] {
        &self.source_anchors
    }

    pub fn authorization(&self) -> &ResolutionAuthorizationV1 {
        &self.authorization
    }

    pub fn payload_access(&self) -> PayloadAccessState {
        self.payload_access
    }

    pub fn retention_class(&self) -> &RetentionClass {
        &self.retention_class
    }

    pub fn durability(&self) -> &AnchorDurabilityClass {
        &self.durability
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.anchor_id.validate()?;
        self.target.validate()?;
        self.source_generation.validate_for_target(&self.target)?;
        validate_owner(&self.owner)?;
        if self.target.requires_project_owner()
            && !matches!(self.owner, ObservationScopeV1::Project { .. })
        {
            return Err(DomainError::UnknownReference {
                field: "repository anchor owner",
            });
        }
        if let Some(occurred_at) = &self.occurred_at {
            occurred_at.validate()?;
        }
        self.projection_generation.validate()?;
        for shard in self.projection_watermark.components.keys() {
            shard.validate()?;
        }
        self.coverage.validate()?;
        self.authorization.validate()?;
        for alias in &self.aliases {
            alias.validate()?;
        }
        ensure_unique_aliases(&self.aliases)?;
        ensure_unique_observations(&self.source_observations)?;
        if let RetrievalAnchorTargetV2::ExactObservation(target) = &self.target
            && !self.source_observations.contains(target)
        {
            return Err(DomainError::UnknownReference {
                field: "exact observation source lineage",
            });
        }
        ensure_unique_lineage(&self.source_anchors)?;
        for source in &self.source_anchors {
            source.validate()?;
            if source.owner() != &self.owner {
                return Err(DomainError::UnknownReference {
                    field: "retrieval anchor lineage owner",
                });
            }
            if source.anchor_id() == &self.anchor_id {
                return Err(DomainError::SelfSupersession);
            }
        }
        let expected = derive_anchor_id(&self.owner, &self.target)?;
        if self.anchor_id != expected {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for RetrievalAnchorRecordV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            anchor_id: RetrievalAnchorId,
            target: RetrievalAnchorTargetV2,
            owner: ObservationScopeV1,
            aliases: Vec<NativeAliasV2>,
            occurred_at: Option<TimeInterval>,
            ingested_at: UtcMicros,
            evidence_class: EvidenceClass,
            source_generation: AnchorSourceGenerationV2,
            projection_generation: ProjectionGenerationId,
            projection_watermark: VectorWatermark,
            coverage: CoverageReportV1,
            source_observations: Vec<CanonicalObservationIdV1>,
            source_anchors: Vec<AnchorLineageRefV2>,
            authorization: ResolutionAuthorizationV1,
            payload_access: PayloadAccessState,
            retention_class: RetentionClass,
            durability: AnchorDurabilityClass,
        }

        let wire = Wire::deserialize(deserializer)?;
        let claimed_id = wire.anchor_id;
        let record = Self::new(RetrievalAnchorRecordV2Parts {
            target: wire.target,
            owner: wire.owner,
            aliases: wire.aliases,
            occurred_at: wire.occurred_at,
            ingested_at: wire.ingested_at,
            evidence_class: wire.evidence_class,
            source_generation: wire.source_generation,
            projection_generation: wire.projection_generation,
            projection_watermark: wire.projection_watermark,
            coverage: wire.coverage,
            source_observations: wire.source_observations,
            source_anchors: wire.source_anchors,
            authorization: wire.authorization,
            payload_access: wire.payload_access,
            retention_class: wire.retention_class,
            durability: wire.durability,
        })
        .map_err(serde::de::Error::custom)?;
        if claimed_id != record.anchor_id {
            return Err(serde::de::Error::custom(DomainError::DigestMismatch));
        }
        Ok(record)
    }
}

fn derive_anchor_id(
    owner: &ObservationScopeV1,
    target: &RetrievalAnchorTargetV2,
) -> Result<RetrievalAnchorId, DomainError> {
    #[derive(Serialize)]
    struct Identity<'a> {
        domain: &'static str,
        owner: &'a ObservationScopeV1,
        target: &'a RetrievalAnchorTargetV2,
    }

    validate_owner(owner)?;
    target.validate()?;
    let digest = canonical_sha256(&Identity {
        domain: RETRIEVAL_ANCHOR_V2_ID_DOMAIN,
        owner,
        target,
    })?;
    RetrievalAnchorId::new(format!("retrieval.v2.{}", digest.as_str()))
}

fn validate_owner(owner: &ObservationScopeV1) -> Result<(), DomainError> {
    owner.validate().map_err(|_| DomainError::UnknownReference {
        field: "retrieval anchor owner",
    })
}

fn validate_git_object_id(value: &str, field: &'static str) -> Result<(), DomainError> {
    if !matches!(value.len(), 40 | 64)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

fn ensure_unique_aliases(aliases: &[NativeAliasV2]) -> Result<(), DomainError> {
    if aliases.iter().enumerate().any(|(index, alias)| {
        aliases[..index]
            .iter()
            .any(|prior| prior.locator_digest == alias.locator_digest)
    }) {
        return Err(DomainError::DuplicateId {
            field: "retrieval anchor aliases",
        });
    }
    Ok(())
}

fn ensure_unique_observations(
    observations: &[CanonicalObservationIdV1],
) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    for observation in observations {
        if !seen.insert(observation) {
            return Err(DomainError::DuplicateId {
                field: "retrieval anchor source observations",
            });
        }
    }
    Ok(())
}

fn ensure_unique_lineage(lineage: &[AnchorLineageRefV2]) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    if lineage.iter().any(|source| !seen.insert(source)) {
        return Err(DomainError::DuplicateId {
            field: "retrieval anchor source lineage",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::research::{
        AccessPolicyDigest, ComponentVersion, EntityId, EntityKind, PrivacyDomainId, ProjectId,
        SanitizationReceiptId, ScopeResolutionId,
    };

    const DIGEST_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn observation(seed: char) -> CanonicalObservationIdV1 {
        CanonicalObservationIdV1::new(format!(
            "sha256:{}",
            std::iter::repeat_n(seed, 64).collect::<String>()
        ))
        .unwrap()
    }

    fn owner(project: &str) -> ObservationScopeV1 {
        ObservationScopeV1::Project {
            project_id: ProjectId::new(project).unwrap(),
        }
    }

    fn authorization() -> ResolutionAuthorizationV1 {
        ResolutionAuthorizationV1 {
            resolved_scope_id: ScopeResolutionId::new("scope.fixture").unwrap(),
            privacy_domain_id: PrivacyDomainId::new("privacy.fixture").unwrap(),
            access_policy_digest: AccessPolicyDigest::new(DIGEST_A).unwrap(),
            capability_id: crate::research::CapabilityId::new("capability.fixture").unwrap(),
            canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(DIGEST_B).unwrap(),
        }
    }

    fn record_parts(
        target: RetrievalAnchorTargetV2,
        owner: ObservationScopeV1,
    ) -> RetrievalAnchorRecordV2Parts {
        let source_observations = match &target {
            RetrievalAnchorTargetV2::ExactObservation(id) => vec![id.clone()],
            _ => vec![observation('c')],
        };
        RetrievalAnchorRecordV2Parts {
            target,
            owner,
            aliases: vec![],
            occurred_at: Some(TimeInterval {
                start: UtcMicros(1),
                end: UtcMicros(2),
            }),
            ingested_at: UtcMicros(3),
            evidence_class: EvidenceClass::Observed,
            source_generation: AnchorSourceGenerationV2::Observation(
                ObservationSourceGenerationV1::new(7).unwrap(),
            ),
            projection_generation: ProjectionGenerationId::new("projection.fixture").unwrap(),
            projection_watermark: VectorWatermark::default(),
            coverage: CoverageReportV1::default(),
            source_observations,
            source_anchors: vec![],
            authorization: authorization(),
            payload_access: PayloadAccessState::Eligible,
            retention_class: RetentionClass::new("retention.fixture").unwrap(),
            durability: AnchorDurabilityClass::DurableEvidence,
        }
    }

    fn entity_target(id: &str) -> RetrievalAnchorTargetV2 {
        RetrievalAnchorTargetV2::Entity(EntityRef {
            id: EntityId::new(id).unwrap(),
            kind: EntityKind::Document,
        })
    }

    #[test]
    fn replay_derives_the_same_anchor_identity() {
        let first = RetrievalAnchorRecordV2::new(record_parts(
            entity_target("document.fixture"),
            owner("project.fixture"),
        ))
        .unwrap();
        let mut replay_parts =
            record_parts(entity_target("document.fixture"), owner("project.fixture"));
        replay_parts.ingested_at = UtcMicros(999);
        replay_parts.aliases = vec![
            NativeAliasV2::new(
                NativeAliasKindV2::Path,
                PrivacyDomainBoundLocatorDigest::new(DIGEST_A).unwrap(),
            )
            .unwrap(),
        ];
        let replay = RetrievalAnchorRecordV2::new(replay_parts).unwrap();

        assert_eq!(first.anchor_id(), replay.anchor_id());
    }

    #[test]
    fn owner_is_part_of_anchor_identity() {
        let first = RetrievalAnchorRecordV2::new(record_parts(
            entity_target("document.fixture"),
            owner("project.one"),
        ))
        .unwrap();
        let second = RetrievalAnchorRecordV2::new(record_parts(
            entity_target("document.fixture"),
            owner("project.two"),
        ))
        .unwrap();

        assert_ne!(first.anchor_id(), second.anchor_id());
    }

    #[test]
    fn rejects_alias_digest_collisions_across_alias_kinds() {
        let mut parts = record_parts(entity_target("document.fixture"), owner("project.fixture"));
        parts.aliases = vec![
            NativeAliasV2::new(
                NativeAliasKindV2::Path,
                PrivacyDomainBoundLocatorDigest::new(DIGEST_A).unwrap(),
            )
            .unwrap(),
            NativeAliasV2::new(
                NativeAliasKindV2::Ref,
                PrivacyDomainBoundLocatorDigest::new(DIGEST_A).unwrap(),
            )
            .unwrap(),
        ];

        assert_eq!(
            RetrievalAnchorRecordV2::new(parts).unwrap_err(),
            DomainError::DuplicateId {
                field: "retrieval anchor aliases"
            }
        );
    }

    #[test]
    fn copied_lineage_does_not_reuse_source_anchor_identity() {
        let source = RetrievalAnchorRecordV2::new(record_parts(
            entity_target("document.source"),
            owner("project.fixture"),
        ))
        .unwrap();
        let mut copied_parts =
            record_parts(entity_target("document.copy"), owner("project.fixture"));
        copied_parts.source_anchors = vec![
            AnchorLineageRefV2::new(
                AnchorProvenanceRelationV2::CopiedFrom,
                source.anchor_id().clone(),
                owner("project.fixture"),
            )
            .unwrap(),
        ];
        let copied = RetrievalAnchorRecordV2::new(copied_parts).unwrap();

        assert_ne!(source.anchor_id(), copied.anchor_id());
        assert_eq!(
            copied.source_anchors()[0].relation(),
            AnchorProvenanceRelationV2::CopiedFrom
        );
    }

    #[test]
    fn repository_capture_requires_a_project_owner() {
        let capture_id = RepositoryCaptureId::new("capture.fixture").unwrap();
        let target = RetrievalAnchorTargetV2::RepositoryCapture {
            repository_id: RepositoryId::new("repository.fixture").unwrap(),
            capture_id: capture_id.clone(),
            receipt: SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new("receipt.fixture").unwrap(),
                ComponentVersion::new("sanitizer.fixture").unwrap(),
            )
            .unwrap(),
        };
        let mut parts = record_parts(target, ObservationScopeV1::Profile);
        parts.source_generation = AnchorSourceGenerationV2::RepositoryCapture(capture_id);

        assert!(RetrievalAnchorRecordV2::new(parts).is_err());
    }

    #[test]
    fn exact_git_targets_require_canonical_object_ids() {
        let mut parts = record_parts(
            RetrievalAnchorTargetV2::ExactRepositoryCommit {
                repository_id: RepositoryId::new("repository.fixture").unwrap(),
                commit_id: CommitId::new("main").unwrap(),
            },
            owner("project.fixture"),
        );
        parts.source_generation = AnchorSourceGenerationV2::Unknown;

        assert_eq!(
            RetrievalAnchorRecordV2::new(parts).unwrap_err(),
            DomainError::NonCanonical {
                field: "retrieval anchor commit"
            }
        );
    }

    #[test]
    fn repository_capture_requires_the_matching_source_generation() {
        let target = RetrievalAnchorTargetV2::RepositoryCapture {
            repository_id: RepositoryId::new("repository.fixture").unwrap(),
            capture_id: RepositoryCaptureId::new("capture.target").unwrap(),
            receipt: SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new("receipt.fixture").unwrap(),
                ComponentVersion::new("sanitizer.fixture").unwrap(),
            )
            .unwrap(),
        };
        let mut parts = record_parts(target, owner("project.fixture"));
        parts.source_generation = AnchorSourceGenerationV2::RepositoryCapture(
            RepositoryCaptureId::new("capture.other").unwrap(),
        );

        assert_eq!(
            RetrievalAnchorRecordV2::new(parts).unwrap_err(),
            DomainError::UnknownReference {
                field: "retrieval anchor source generation"
            }
        );
    }

    #[test]
    fn deserialization_rejects_a_tampered_anchor_identity() {
        let record = RetrievalAnchorRecordV2::new(record_parts(
            entity_target("document.fixture"),
            owner("project.fixture"),
        ))
        .unwrap();
        let mut wire = serde_json::to_value(record).unwrap();
        wire["anchor_id"] = json!("retrieval.v2.tampered");

        assert!(serde_json::from_value::<RetrievalAnchorRecordV2>(wire).is_err());
    }
}
