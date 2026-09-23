use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

use crate::configuration::UserProfileId;
use crate::observation::{
    CanonicalObservationIdV1, ObservationScopeV1, ObservationSourceGenerationV1,
};

use super::canonical::canonical_sha256;
use super::coverage::{CoverageReportV1, RetentionClass};
use super::error::DomainError;
use super::evidence::{EvidenceClass, SanitizationReceiptRefV1};
use super::git_topology::{GitTopologyAnchorTargetV1, GitTopologyGenerationRefV1};
use super::id::{
    BlobId, CommitId, PrivacyDomainId, ProjectId, ProjectionGenerationId, RepositoryCaptureId,
    RepositoryId, RetrievalAnchorId, TreeId,
};
use super::resolution::ResolutionAuthorizationV1;
use super::retrieval::{
    AnchorDurabilityClass, PayloadAccessState, PrivacyDomainBoundLocatorDigest,
};
use super::subjects::EntityRef;
use super::time::{TimeInterval, UtcMicros};
use super::watermark::VectorWatermark;

const RETRIEVAL_ANCHOR_V2_ID_DOMAIN: &str = "tracedecay.retrieval-anchor.v2";
const RETRIEVAL_ANCHOR_V3_ID_DOMAIN: &str = "tracedecay.retrieval-anchor.v3";
const MAX_ANCHOR_ALIASES: usize = 64;
const MAX_ANCHOR_SOURCE_OBSERVATIONS: usize = 256;
const MAX_ANCHOR_SOURCE_ANCHORS: usize = 256;

/// Meaning of a privacy-domain-safe native locator digest.
///
/// The digest is the only locator material admitted to the anchor contract;
/// literal paths, ref names, queries, and provider payloads remain in their
/// owning stores.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NativeAliasKind {
    ProviderRecord,
    LegacyIdentity,
    RepositoryRoot,
    Worktree,
    Ref,
    Path,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct NativeAlias {
    kind: NativeAliasKind,
    locator_digest: PrivacyDomainBoundLocatorDigest,
}

impl NativeAlias {
    pub fn new(
        kind: NativeAliasKind,
        locator_digest: PrivacyDomainBoundLocatorDigest,
    ) -> Result<Self, DomainError> {
        locator_digest.validate()?;
        Ok(Self {
            kind,
            locator_digest,
        })
    }

    pub fn kind(&self) -> NativeAliasKind {
        self.kind
    }

    pub fn locator_digest(&self) -> &PrivacyDomainBoundLocatorDigest {
        &self.locator_digest
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.locator_digest.validate()
    }
}

impl<'de> Deserialize<'de> for NativeAlias {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            kind: NativeAliasKind,
            locator_digest: PrivacyDomainBoundLocatorDigest,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.kind, wire.locator_digest).map_err(serde::de::Error::custom)
    }
}

/// Immutable retrieval target. Mutable Git routing names are aliases, never
/// target identities.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "target",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RetrievalAnchorTarget {
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
    GitTopology(Box<GitTopologyAnchorTargetV1>),
}

/// Exact profile/project and privacy owner binding.
///
/// Ambient paths, labels, store filenames, host profiles, and process state
/// cannot fill this identity.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AnchorOwnerBindingV1 {
    Profile {
        profile_id: UserProfileId,
        privacy_domain_id: PrivacyDomainId,
    },
    Project {
        profile_id: UserProfileId,
        project_id: ProjectId,
        privacy_domain_id: PrivacyDomainId,
    },
}

impl AnchorOwnerBindingV1 {
    pub fn for_profile(
        profile_id: UserProfileId,
        privacy_domain_id: PrivacyDomainId,
    ) -> Result<Self, DomainError> {
        let owner = Self::Profile {
            profile_id,
            privacy_domain_id,
        };
        owner.validate()?;
        Ok(owner)
    }

    pub fn for_project(
        profile_id: UserProfileId,
        project_id: ProjectId,
        privacy_domain_id: PrivacyDomainId,
    ) -> Result<Self, DomainError> {
        let owner = Self::Project {
            profile_id,
            project_id,
            privacy_domain_id,
        };
        owner.validate()?;
        Ok(owner)
    }

    pub fn profile_id(&self) -> &UserProfileId {
        match self {
            Self::Profile { profile_id, .. } | Self::Project { profile_id, .. } => profile_id,
        }
    }

    pub fn project_id(&self) -> Option<&ProjectId> {
        match self {
            Self::Profile { .. } => None,
            Self::Project { project_id, .. } => Some(project_id),
        }
    }

    pub fn privacy_domain_id(&self) -> &PrivacyDomainId {
        match self {
            Self::Profile {
                privacy_domain_id, ..
            }
            | Self::Project {
                privacy_domain_id, ..
            } => privacy_domain_id,
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.profile_id().validate()?;
        if let Some(project_id) = self.project_id() {
            project_id.validate()?;
        }
        self.privacy_domain_id().validate()
    }
}

impl<'de> Deserialize<'de> for AnchorOwnerBindingV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum Wire {
            Profile {
                profile_id: UserProfileId,
                privacy_domain_id: PrivacyDomainId,
            },
            Project {
                profile_id: UserProfileId,
                project_id: ProjectId,
                privacy_domain_id: PrivacyDomainId,
            },
        }

        let owner = match Wire::deserialize(deserializer)? {
            Wire::Profile {
                profile_id,
                privacy_domain_id,
            } => Self::Profile {
                profile_id,
                privacy_domain_id,
            },
            Wire::Project {
                profile_id,
                project_id,
                privacy_domain_id,
            } => Self::Project {
                profile_id,
                project_id,
                privacy_domain_id,
            },
        };
        owner.validate().map_err(serde::de::Error::custom)?;
        Ok(owner)
    }
}

impl RetrievalAnchorTarget {
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
            Self::GitTopology(target) => target.validate(),
        }
    }

    fn requires_project_owner(&self) -> bool {
        matches!(
            self,
            Self::ExactRepositoryCommit { .. }
                | Self::ExactRepositoryTree { .. }
                | Self::ExactRepositoryBlob { .. }
                | Self::RepositoryCapture { .. }
                | Self::GitTopology(_)
        )
    }
}

impl<'de> Deserialize<'de> for RetrievalAnchorTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(
            tag = "kind",
            content = "target",
            rename_all = "snake_case",
            deny_unknown_fields
        )]
        enum Wire {
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
            GitTopology(Box<GitTopologyAnchorTargetV1>),
        }

        let target = match Wire::deserialize(deserializer)? {
            Wire::ExactObservation(observation_id) => Self::ExactObservation(observation_id),
            Wire::Entity(entity) => Self::Entity(entity),
            Wire::ExactRepositoryCommit {
                repository_id,
                commit_id,
            } => Self::ExactRepositoryCommit {
                repository_id,
                commit_id,
            },
            Wire::ExactRepositoryTree {
                repository_id,
                tree_id,
            } => Self::ExactRepositoryTree {
                repository_id,
                tree_id,
            },
            Wire::ExactRepositoryBlob {
                repository_id,
                blob_id,
            } => Self::ExactRepositoryBlob {
                repository_id,
                blob_id,
            },
            Wire::RepositoryCapture {
                repository_id,
                capture_id,
                receipt,
            } => Self::RepositoryCapture {
                repository_id,
                capture_id,
                receipt,
            },
            Wire::GitTopology(target) => Self::GitTopology(target),
        };
        target.validate().map_err(serde::de::Error::custom)?;
        Ok(target)
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
pub enum AnchorSourceGeneration {
    Observation(ObservationSourceGenerationV1),
    RepositoryCapture(RepositoryCaptureId),
    GitTopology(GitTopologyGenerationRefV1),
    Unavailable,
    Unknown,
}

impl AnchorSourceGeneration {
    fn validate_for_target(&self, target: &RetrievalAnchorTarget) -> Result<(), DomainError> {
        let valid = match (self, target) {
            (Self::Observation(_), RetrievalAnchorTarget::ExactObservation(_)) => true,
            (
                Self::RepositoryCapture(source),
                RetrievalAnchorTarget::RepositoryCapture { capture_id, .. },
            ) => source == capture_id,
            (
                Self::RepositoryCapture(_) | Self::Unavailable | Self::Unknown,
                RetrievalAnchorTarget::ExactRepositoryCommit { .. }
                | RetrievalAnchorTarget::ExactRepositoryTree { .. }
                | RetrievalAnchorTarget::ExactRepositoryBlob { .. },
            ) => true,
            (Self::GitTopology(source), RetrievalAnchorTarget::GitTopology(target)) => {
                source == &target.generation()
            }
            (_, RetrievalAnchorTarget::Entity(_)) => true,
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
        if let Self::GitTopology(generation) = self {
            generation.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AnchorProvenanceRelation {
    CapturedFrom,
    Produced,
    Observed,
    ExecutedIn,
    Discussed,
    CopiedFrom,
    DerivedFrom,
    Corrects,
    Contradicts,
    Supersedes,
    Supports,
}

/// Owner-bound reference to an earlier anchor in the provenance graph.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct AnchorLineageRef {
    relation: AnchorProvenanceRelation,
    anchor_id: RetrievalAnchorId,
    owner: ObservationScopeV1,
}

impl AnchorLineageRef {
    pub fn new(
        relation: AnchorProvenanceRelation,
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

    pub fn relation(&self) -> AnchorProvenanceRelation {
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

impl<'de> Deserialize<'de> for AnchorLineageRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            relation: AnchorProvenanceRelation,
            anchor_id: RetrievalAnchorId,
            owner: ObservationScopeV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.relation, wire.anchor_id, wire.owner).map_err(serde::de::Error::custom)
    }
}

/// Constructor material for a validated record. `anchor_id` is omitted
/// because it is derived exclusively from the owner and immutable target.
#[derive(Clone, Debug)]
pub struct RetrievalAnchorRecordParts {
    pub target: RetrievalAnchorTarget,
    pub owner: ObservationScopeV1,
    pub aliases: Vec<NativeAlias>,
    pub occurred_at: Option<TimeInterval>,
    pub ingested_at: UtcMicros,
    pub evidence_class: EvidenceClass,
    pub source_generation: AnchorSourceGeneration,
    pub projection_generation: ProjectionGenerationId,
    pub projection_watermark: VectorWatermark,
    pub coverage: CoverageReportV1,
    pub source_observations: Vec<CanonicalObservationIdV1>,
    pub source_anchors: Vec<AnchorLineageRef>,
    pub authorization: ResolutionAuthorizationV1,
    pub payload_access: PayloadAccessState,
    pub retention_class: RetentionClass,
    pub durability: AnchorDurabilityClass,
}

/// The encoded record omits what it can re-derive: a default `coverage`, and
/// the namespace-constant fields of an `authorization` that is exactly its
/// namespace's derivation.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalAnchorRecord {
    anchor_id: RetrievalAnchorId,
    target: RetrievalAnchorTarget,
    owner: ObservationScopeV1,
    aliases: Vec<NativeAlias>,
    occurred_at: Option<TimeInterval>,
    ingested_at: UtcMicros,
    evidence_class: EvidenceClass,
    source_generation: AnchorSourceGeneration,
    projection_generation: ProjectionGenerationId,
    projection_watermark: VectorWatermark,
    #[serde(skip_serializing_if = "coverage_is_default")]
    coverage: CoverageReportV1,
    source_observations: Vec<CanonicalObservationIdV1>,
    source_anchors: Vec<AnchorLineageRef>,
    #[serde(serialize_with = "serialize_anchor_authorization")]
    authorization: ResolutionAuthorizationV1,
    payload_access: PayloadAccessState,
    retention_class: RetentionClass,
    durability: AnchorDurabilityClass,
}

fn coverage_is_default(coverage: &CoverageReportV1) -> bool {
    *coverage == CoverageReportV1::default()
}

/// Stored form of an anchor's authorization: just the namespace and request
/// digest when the rest is that namespace's derivation, the full record
/// otherwise.
#[derive(Deserialize)]
#[serde(untagged)]
enum AnchorAuthorizationWire {
    Derived(DerivedAuthorizationWire),
    Explicit(ResolutionAuthorizationV1),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DerivedAuthorizationWire {
    authority: String,
    canonical_request_digest: PrivacyDomainBoundLocatorDigest,
}

fn serialize_anchor_authorization<S>(
    authorization: &ResolutionAuthorizationV1,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match authorization.derived_authority() {
        Some(authority) => DerivedAuthorizationWire {
            authority: authority.to_owned(),
            canonical_request_digest: authorization.canonical_request_digest.clone(),
        }
        .serialize(serializer),
        None => authorization.serialize(serializer),
    }
}

impl AnchorAuthorizationWire {
    fn into_authorization(self) -> Result<ResolutionAuthorizationV1, DomainError> {
        match self {
            Self::Derived(derived) => ResolutionAuthorizationV1::for_authority(
                &derived.authority,
                derived.canonical_request_digest,
            ),
            Self::Explicit(authorization) => Ok(authorization),
        }
    }
}

impl RetrievalAnchorRecord {
    pub fn new(mut parts: RetrievalAnchorRecordParts) -> Result<Self, DomainError> {
        validate_collection_bounds(&parts)?;
        parts.aliases.sort_unstable_by(|left, right| {
            (left.locator_digest(), left.kind()).cmp(&(right.locator_digest(), right.kind()))
        });
        parts.source_observations.sort_unstable();
        parts.source_anchors.sort_unstable();
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

    pub fn target(&self) -> &RetrievalAnchorTarget {
        &self.target
    }

    pub fn owner(&self) -> &ObservationScopeV1 {
        &self.owner
    }

    /// JSON stored in `retrieval_anchors.owner_json`. Callers compare or insert
    /// this text; they do not re-serialize the owner column themselves.
    pub fn owner_column_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self.owner())
    }

    /// Whether a stored `owner_json` cell is this record's owner column.
    pub fn owner_column_matches(&self, stored: &str) -> bool {
        self.owner_column_json().ok().as_deref() == Some(stored)
    }

    pub fn aliases(&self) -> &[NativeAlias] {
        &self.aliases
    }

    pub fn occurred_at(&self) -> Option<TimeInterval> {
        self.occurred_at
    }

    pub fn ingested_at(&self) -> UtcMicros {
        self.ingested_at
    }

    /// Whether two records describe the same immutable retrieval evidence.
    ///
    /// `ingested_at` records the local attempt that first materialized the
    /// anchor; it is not part of the owner-bound anchor identity. Concurrent
    /// first writers may therefore observe different ingest clocks while
    /// carrying exactly the same target and authority. Every other field must
    /// remain byte-equivalent for the later writer to be an idempotent replay.
    pub fn is_semantic_replay_of(&self, other: &Self) -> bool {
        self.anchor_id == other.anchor_id
            && self.target == other.target
            && self.owner == other.owner
            && self.aliases == other.aliases
            && self.occurred_at == other.occurred_at
            && self.evidence_class == other.evidence_class
            && self.source_generation == other.source_generation
            && self.projection_generation == other.projection_generation
            && self.projection_watermark == other.projection_watermark
            && self.coverage == other.coverage
            && self.source_observations == other.source_observations
            && self.source_anchors == other.source_anchors
            && self.authorization == other.authorization
            && self.payload_access == other.payload_access
            && self.retention_class == other.retention_class
            && self.durability == other.durability
    }

    pub fn evidence_class(&self) -> EvidenceClass {
        self.evidence_class
    }

    pub fn source_generation(&self) -> &AnchorSourceGeneration {
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

    pub fn source_anchors(&self) -> &[AnchorLineageRef] {
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
        if let (
            RetrievalAnchorTarget::GitTopology(target),
            ObservationScopeV1::Project { project_id },
        ) = (&self.target, &self.owner)
            && target.project_id() != project_id
        {
            return Err(DomainError::UnknownReference {
                field: "git topology anchor project owner",
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
        if let RetrievalAnchorTarget::ExactObservation(target) = &self.target
            && !self.source_observations.contains(target)
        {
            return Err(DomainError::UnknownReference {
                field: "exact observation source lineage",
            });
        }
        ensure_unique_lineage(&self.source_anchors)?;
        if let RetrievalAnchorTarget::GitTopology(target) = &self.target {
            for expected in target.ordered_sources() {
                if !self
                    .source_anchors
                    .iter()
                    .any(|source| source.anchor_id() == &expected.anchor_id)
                {
                    return Err(DomainError::UnknownReference {
                        field: "git topology ordered source lineage",
                    });
                }
            }
        }
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

/// Derive the canonical retrieval anchor for one durable observation.
///
/// Projection generations and rebuild watermarks are deliberately excluded:
/// rebuilding a view must never re-key its source observation.
pub fn derive_exact_observation_anchor_id(
    owner: &ObservationScopeV1,
    observation_id: &CanonicalObservationIdV1,
) -> Result<RetrievalAnchorId, DomainError> {
    derive_anchor_id(
        owner,
        &RetrievalAnchorTarget::ExactObservation(observation_id.clone()),
    )
}

/// Derive the canonical identity for one immutable Git-topology target.
pub fn derive_git_topology_anchor_id(
    owner: &ObservationScopeV1,
    target: &GitTopologyAnchorTargetV1,
) -> Result<RetrievalAnchorId, DomainError> {
    derive_anchor_id(
        owner,
        &RetrievalAnchorTarget::GitTopology(Box::new(target.clone())),
    )
}

impl<'de> Deserialize<'de> for RetrievalAnchorRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            anchor_id: RetrievalAnchorId,
            target: RetrievalAnchorTarget,
            owner: ObservationScopeV1,
            aliases: Vec<NativeAlias>,
            occurred_at: Option<TimeInterval>,
            ingested_at: UtcMicros,
            evidence_class: EvidenceClass,
            source_generation: AnchorSourceGeneration,
            projection_generation: ProjectionGenerationId,
            projection_watermark: VectorWatermark,
            #[serde(default)]
            coverage: CoverageReportV1,
            source_observations: Vec<CanonicalObservationIdV1>,
            source_anchors: Vec<AnchorLineageRef>,
            authorization: AnchorAuthorizationWire,
            payload_access: PayloadAccessState,
            retention_class: RetentionClass,
            durability: AnchorDurabilityClass,
        }

        let wire = Wire::deserialize(deserializer)?;
        let claimed_id = wire.anchor_id;
        let authorization = wire
            .authorization
            .into_authorization()
            .map_err(serde::de::Error::custom)?;
        let record = Self::new(RetrievalAnchorRecordParts {
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
            authorization,
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
    target: &RetrievalAnchorTarget,
) -> Result<RetrievalAnchorId, DomainError> {
    #[derive(Serialize)]
    struct Identity<'a> {
        domain: &'static str,
        owner: &'a ObservationScopeV1,
        target: &'a RetrievalAnchorTarget,
    }

    validate_owner(owner)?;
    target.validate()?;
    let domain = if matches!(target, RetrievalAnchorTarget::GitTopology(_)) {
        RETRIEVAL_ANCHOR_V3_ID_DOMAIN
    } else {
        RETRIEVAL_ANCHOR_V2_ID_DOMAIN
    };
    let digest = canonical_sha256(&Identity {
        domain,
        owner,
        target,
    })?;
    let version = if matches!(target, RetrievalAnchorTarget::GitTopology(_)) {
        "v3"
    } else {
        "v2"
    };
    RetrievalAnchorId::new(format!("retrieval.{version}.{}", digest.as_str()))
}

fn validate_owner(owner: &ObservationScopeV1) -> Result<(), DomainError> {
    owner.validate().map_err(|_| DomainError::UnknownReference {
        field: "retrieval anchor owner",
    })
}

use crate::canonical_text::validate_git_object_id;

fn ensure_unique_aliases(aliases: &[NativeAlias]) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    for alias in aliases {
        if !seen.insert(alias.locator_digest()) {
            return Err(DomainError::DuplicateId {
                field: "retrieval anchor aliases",
            });
        }
    }
    Ok(())
}

fn validate_collection_bounds(parts: &RetrievalAnchorRecordParts) -> Result<(), DomainError> {
    if parts.aliases.len() > MAX_ANCHOR_ALIASES {
        return Err(DomainError::NonCanonical {
            field: "retrieval anchor aliases",
        });
    }
    if parts.source_observations.len() > MAX_ANCHOR_SOURCE_OBSERVATIONS {
        return Err(DomainError::NonCanonical {
            field: "retrieval anchor source observations",
        });
    }
    if parts.source_anchors.len() > MAX_ANCHOR_SOURCE_ANCHORS {
        return Err(DomainError::NonCanonical {
            field: "retrieval anchor source lineage",
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

fn ensure_unique_lineage(lineage: &[AnchorLineageRef]) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    if lineage.iter().any(|source| !seen.insert(source)) {
        return Err(DomainError::DuplicateId {
            field: "retrieval anchor source lineage",
        });
    }
    Ok(())
}

#[cfg(test)]
#[path = "anchor_test.rs"]
mod anchor_test;
