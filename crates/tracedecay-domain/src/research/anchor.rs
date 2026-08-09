use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

use crate::configuration::UserProfileId;
use crate::observation::{
    CanonicalObservationIdV1, ObservationScopeV1, ObservationSourceGenerationV1,
};
use crate::retrieval::SourceOccurrenceId;
use crate::session_derived::EvidenceSpanIdV1;

use super::canonical::canonical_sha256;
use super::coverage::{CoverageReportV1, RetentionClass};
use super::error::DomainError;
use super::evidence::{EvidenceClass, SanitizationReceiptRefV1};
use super::git_topology::{GitTopologyAnchorTargetV1, GitTopologyGenerationRefV1};
use super::id::{
    BlobId, CommitId, PrivacyDomainId, ProjectId, ProjectionGenerationId, RepositoryCaptureId,
    RepositoryId, RetrievalAnchorId, RetrieverContributionIdV1, TreeId,
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
pub enum NativeAliasKindV2 {
    ProviderRecord,
    LegacyIdentity,
    RepositoryRoot,
    Worktree,
    Ref,
    Path,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
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
    GitTopology(Box<GitTopologyAnchorTargetV1>),
}

/// Exact profile/project and privacy owner for V3 anchors and lineage.
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

    fn observation_scope(&self) -> ObservationScopeV1 {
        match self {
            Self::Profile { .. } => ObservationScopeV1::Profile,
            Self::Project { project_id, .. } => ObservationScopeV1::Project {
                project_id: project_id.clone(),
            },
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

/// Canonical V3 target type for authoritative retrieval anchors.
///
/// Legacy variants intentionally keep their V2 wire representation. The V3
/// evidence targets add immutable, payload-free references without changing
/// persisted V2 decoding.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "target",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RetrievalAnchorTargetV3 {
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
    ExactSourceOccurrence(SourceOccurrenceId),
    ExactEvidenceSpan(EvidenceSpanIdV1),
    RetrieverContribution(RetrieverContributionIdV1),
}

pub type RetrievalAnchorTarget = RetrievalAnchorTargetV3;

impl RetrievalAnchorTargetV3 {
    pub fn validate(&self) -> Result<(), DomainError> {
        if let Some(legacy) = self.as_v2() {
            return legacy.validate();
        }
        match self {
            Self::ExactSourceOccurrence(occurrence_id) => {
                occurrence_id
                    .validate()
                    .map_err(|_| DomainError::NonCanonical {
                        field: "source occurrence anchor target",
                    })
            }
            Self::ExactEvidenceSpan(_) => Ok(()),
            Self::RetrieverContribution(contribution_id) => contribution_id.validate(),
            _ => unreachable!("legacy targets return before V3 evidence validation"),
        }
    }

    fn as_v2(&self) -> Option<RetrievalAnchorTargetV2> {
        Some(match self {
            Self::ExactObservation(observation_id) => {
                RetrievalAnchorTargetV2::ExactObservation(observation_id.clone())
            }
            Self::Entity(entity) => RetrievalAnchorTargetV2::Entity(entity.clone()),
            Self::ExactRepositoryCommit {
                repository_id,
                commit_id,
            } => RetrievalAnchorTargetV2::ExactRepositoryCommit {
                repository_id: repository_id.clone(),
                commit_id: commit_id.clone(),
            },
            Self::ExactRepositoryTree {
                repository_id,
                tree_id,
            } => RetrievalAnchorTargetV2::ExactRepositoryTree {
                repository_id: repository_id.clone(),
                tree_id: tree_id.clone(),
            },
            Self::ExactRepositoryBlob {
                repository_id,
                blob_id,
            } => RetrievalAnchorTargetV2::ExactRepositoryBlob {
                repository_id: repository_id.clone(),
                blob_id: blob_id.clone(),
            },
            Self::RepositoryCapture {
                repository_id,
                capture_id,
                receipt,
            } => RetrievalAnchorTargetV2::RepositoryCapture {
                repository_id: repository_id.clone(),
                capture_id: capture_id.clone(),
                receipt: receipt.clone(),
            },
            Self::GitTopology(target) => RetrievalAnchorTargetV2::GitTopology(target.clone()),
            Self::ExactSourceOccurrence(_)
            | Self::ExactEvidenceSpan(_)
            | Self::RetrieverContribution(_) => return None,
        })
    }
}

impl From<RetrievalAnchorTargetV2> for RetrievalAnchorTargetV3 {
    fn from(target: RetrievalAnchorTargetV2) -> Self {
        match target {
            RetrievalAnchorTargetV2::ExactObservation(observation_id) => {
                Self::ExactObservation(observation_id)
            }
            RetrievalAnchorTargetV2::Entity(entity) => Self::Entity(entity),
            RetrievalAnchorTargetV2::ExactRepositoryCommit {
                repository_id,
                commit_id,
            } => Self::ExactRepositoryCommit {
                repository_id,
                commit_id,
            },
            RetrievalAnchorTargetV2::ExactRepositoryTree {
                repository_id,
                tree_id,
            } => Self::ExactRepositoryTree {
                repository_id,
                tree_id,
            },
            RetrievalAnchorTargetV2::ExactRepositoryBlob {
                repository_id,
                blob_id,
            } => Self::ExactRepositoryBlob {
                repository_id,
                blob_id,
            },
            RetrievalAnchorTargetV2::RepositoryCapture {
                repository_id,
                capture_id,
                receipt,
            } => Self::RepositoryCapture {
                repository_id,
                capture_id,
                receipt,
            },
            RetrievalAnchorTargetV2::GitTopology(target) => Self::GitTopology(target),
        }
    }
}

impl<'de> Deserialize<'de> for RetrievalAnchorTargetV3 {
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
        enum EvidenceWire {
            ExactSourceOccurrence(SourceOccurrenceId),
            ExactEvidenceSpan(EvidenceSpanIdV1),
            RetrieverContribution(RetrieverContributionIdV1),
        }

        let value = serde_json::Value::deserialize(deserializer)?;
        let target = if let Ok(legacy) =
            serde_json::from_value::<RetrievalAnchorTargetV2>(value.clone())
        {
            legacy.into()
        } else {
            match serde_json::from_value::<EvidenceWire>(value).map_err(serde::de::Error::custom)? {
                EvidenceWire::ExactSourceOccurrence(occurrence_id) => {
                    Self::ExactSourceOccurrence(occurrence_id)
                }
                EvidenceWire::ExactEvidenceSpan(span_id) => Self::ExactEvidenceSpan(span_id),
                EvidenceWire::RetrieverContribution(contribution_id) => {
                    Self::RetrieverContribution(contribution_id)
                }
            }
        };
        target.validate().map_err(serde::de::Error::custom)?;
        Ok(target)
    }
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

impl<'de> Deserialize<'de> for RetrievalAnchorTargetV2 {
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
pub enum AnchorSourceGenerationV2 {
    Observation(ObservationSourceGenerationV1),
    RepositoryCapture(RepositoryCaptureId),
    GitTopology(GitTopologyGenerationRefV1),
    Unavailable,
    Unknown,
}

pub type AnchorSourceGenerationV3 = AnchorSourceGenerationV2;

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
            (Self::GitTopology(source), RetrievalAnchorTargetV2::GitTopology(target)) => {
                source == &target.generation()
            }
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
        if let Self::GitTopology(generation) = self {
            generation.validate()?;
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
    Corrects,
    Contradicts,
    Supersedes,
    Supports,
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

/// Ordered, owner- and privacy-bound lineage for V3 evidence assemblies.
///
/// `source_ordinal` is assembly order, not chronology. Keeping it in the
/// immutable record prevents sorted V2 lineage from silently replacing
/// lossless cross-source order.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct AnchorLineageRefV3 {
    source_ordinal: u64,
    relation: AnchorProvenanceRelationV2,
    anchor_id: RetrievalAnchorId,
    owner: AnchorOwnerBindingV1,
}

impl AnchorLineageRefV3 {
    pub fn new(
        source_ordinal: u64,
        relation: AnchorProvenanceRelationV2,
        anchor_id: RetrievalAnchorId,
        owner: AnchorOwnerBindingV1,
    ) -> Result<Self, DomainError> {
        let lineage = Self {
            source_ordinal,
            relation,
            anchor_id,
            owner,
        };
        lineage.validate()?;
        Ok(lineage)
    }

    pub const fn source_ordinal(&self) -> u64 {
        self.source_ordinal
    }

    pub const fn relation(&self) -> AnchorProvenanceRelationV2 {
        self.relation
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        &self.anchor_id
    }

    pub fn owner(&self) -> &AnchorOwnerBindingV1 {
        &self.owner
    }

    pub fn privacy_domain_id(&self) -> &PrivacyDomainId {
        self.owner.privacy_domain_id()
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.anchor_id.validate()?;
        self.owner.validate()
    }
}

impl<'de> Deserialize<'de> for AnchorLineageRefV3 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            source_ordinal: u64,
            relation: AnchorProvenanceRelationV2,
            anchor_id: RetrievalAnchorId,
            owner: AnchorOwnerBindingV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.source_ordinal,
            wire.relation,
            wire.anchor_id,
            wire.owner,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// Validate lossless V3 assembly order without inferring chronology.
pub fn validate_anchor_lineage_v3(lineage: &[AnchorLineageRefV3]) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    for (expected_ordinal, source) in lineage.iter().enumerate() {
        source.validate()?;
        if source.source_ordinal
            != u64::try_from(expected_ordinal).map_err(|_| DomainError::NonCanonical {
                field: "retrieval anchor V3 source lineage order",
            })?
        {
            return Err(DomainError::NonCanonical {
                field: "retrieval anchor V3 source lineage order",
            });
        }
        if !seen.insert((source.anchor_id(), source.owner())) {
            return Err(DomainError::DuplicateId {
                field: "retrieval anchor V3 source lineage",
            });
        }
    }
    Ok(())
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

/// Constructor material for an owner- and privacy-bound V3 anchor record.
///
/// Source lineage order is authoritative assembly order and is therefore not
/// canonicalized by sorting.
#[derive(Clone, Debug)]
pub struct RetrievalAnchorRecordV3Parts {
    pub target: RetrievalAnchorTargetV3,
    pub owner: AnchorOwnerBindingV1,
    pub aliases: Vec<NativeAliasV2>,
    pub occurred_at: Option<TimeInterval>,
    pub ingested_at: UtcMicros,
    pub evidence_class: EvidenceClass,
    pub source_generation: AnchorSourceGenerationV3,
    pub projection_generation: ProjectionGenerationId,
    pub projection_watermark: VectorWatermark,
    pub coverage: CoverageReportV1,
    pub source_observations: Vec<CanonicalObservationIdV1>,
    pub source_anchors: Vec<AnchorLineageRefV3>,
    pub authorization: ResolutionAuthorizationV1,
    pub payload_access: PayloadAccessState,
    pub retention_class: RetentionClass,
    pub durability: AnchorDurabilityClass,
}

/// Authoritative V3 record for exact evidence and retriever provenance.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalAnchorRecordV3 {
    anchor_id: RetrievalAnchorId,
    target: RetrievalAnchorTargetV3,
    owner: AnchorOwnerBindingV1,
    aliases: Vec<NativeAliasV2>,
    occurred_at: Option<TimeInterval>,
    ingested_at: UtcMicros,
    evidence_class: EvidenceClass,
    source_generation: AnchorSourceGenerationV3,
    projection_generation: ProjectionGenerationId,
    projection_watermark: VectorWatermark,
    coverage: CoverageReportV1,
    source_observations: Vec<CanonicalObservationIdV1>,
    source_anchors: Vec<AnchorLineageRefV3>,
    authorization: ResolutionAuthorizationV1,
    payload_access: PayloadAccessState,
    retention_class: RetentionClass,
    durability: AnchorDurabilityClass,
}

/// Canonical authoritative retrieval-anchor record.
///
/// Existing product paths remain on the byte-compatible V2 record while V3
/// evidence assemblies migrate through [`RetrievalAnchorRecordV3`].
pub type RetrievalAnchorRecord = RetrievalAnchorRecordV2;

impl RetrievalAnchorRecordV2 {
    pub fn new(mut parts: RetrievalAnchorRecordV2Parts) -> Result<Self, DomainError> {
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
        if let (
            RetrievalAnchorTargetV2::GitTopology(target),
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
        if let RetrievalAnchorTargetV2::ExactObservation(target) = &self.target
            && !self.source_observations.contains(target)
        {
            return Err(DomainError::UnknownReference {
                field: "exact observation source lineage",
            });
        }
        ensure_unique_lineage(&self.source_anchors)?;
        if let RetrievalAnchorTargetV2::GitTopology(target) = &self.target {
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

impl RetrievalAnchorRecordV3 {
    pub fn new(mut parts: RetrievalAnchorRecordV3Parts) -> Result<Self, DomainError> {
        validate_collection_bounds_v3(&parts)?;
        parts.aliases.sort_unstable_by(|left, right| {
            (left.locator_digest(), left.kind()).cmp(&(right.locator_digest(), right.kind()))
        });
        parts.source_observations.sort_unstable();
        let anchor_id = derive_v3_anchor_id(&parts.owner, &parts.target)?;
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

    pub fn target(&self) -> &RetrievalAnchorTargetV3 {
        &self.target
    }

    pub fn owner(&self) -> &AnchorOwnerBindingV1 {
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

    /// V3 owner/privacy binding is immutable; the local ingest clock is not.
    /// Exact re-observation may therefore replay one anchor under a later
    /// materialization timestamp while every authority-bearing field remains
    /// byte-equivalent.
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

    pub fn source_generation(&self) -> &AnchorSourceGenerationV3 {
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

    pub fn source_anchors(&self) -> &[AnchorLineageRefV3] {
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
        self.owner.validate()?;
        validate_source_generation_v3(&self.source_generation, &self.target)?;
        if let Some(legacy) = self.target.as_v2() {
            if legacy.requires_project_owner() && self.owner.project_id().is_none() {
                return Err(DomainError::UnknownReference {
                    field: "repository anchor V3 owner",
                });
            }
            if let RetrievalAnchorTargetV2::GitTopology(target) = legacy
                && self.owner.project_id() != Some(target.project_id())
            {
                return Err(DomainError::UnknownReference {
                    field: "git topology anchor V3 project owner",
                });
            }
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
        if &self.authorization.privacy_domain_id != self.owner.privacy_domain_id() {
            return Err(DomainError::UnknownReference {
                field: "retrieval anchor V3 authorization owner",
            });
        }
        for alias in &self.aliases {
            alias.validate()?;
        }
        ensure_unique_aliases(&self.aliases)?;
        ensure_unique_observations(&self.source_observations)?;
        if let RetrievalAnchorTargetV3::ExactObservation(target) = &self.target
            && !self.source_observations.contains(target)
        {
            return Err(DomainError::UnknownReference {
                field: "exact observation source lineage",
            });
        }
        validate_anchor_lineage_v3(&self.source_anchors)?;
        if matches!(
            self.target,
            RetrievalAnchorTargetV3::ExactSourceOccurrence(_)
                | RetrievalAnchorTargetV3::ExactEvidenceSpan(_)
                | RetrievalAnchorTargetV3::RetrieverContribution(_)
        ) && self.source_anchors.is_empty()
        {
            return Err(DomainError::UnknownReference {
                field: "exact evidence source lineage",
            });
        }
        if let RetrievalAnchorTargetV3::GitTopology(target) = &self.target {
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
            if source.owner() != &self.owner {
                return Err(DomainError::UnknownReference {
                    field: "retrieval anchor V3 lineage owner",
                });
            }
            if source.anchor_id() == &self.anchor_id {
                return Err(DomainError::SelfSupersession);
            }
        }
        if self.anchor_id != derive_v3_anchor_id(&self.owner, &self.target)? {
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
        &RetrievalAnchorTargetV2::ExactObservation(observation_id.clone()),
    )
}

/// Derive the canonical V3 identity for one immutable Git-topology target.
pub fn derive_git_topology_anchor_id(
    owner: &ObservationScopeV1,
    target: &GitTopologyAnchorTargetV1,
) -> Result<RetrievalAnchorId, DomainError> {
    derive_anchor_id(
        owner,
        &RetrievalAnchorTargetV2::GitTopology(Box::new(target.clone())),
    )
}

/// Derive the canonical public anchor for one exact source occurrence.
pub fn derive_exact_source_occurrence_anchor_id(
    owner: &AnchorOwnerBindingV1,
    occurrence_id: &SourceOccurrenceId,
) -> Result<RetrievalAnchorId, DomainError> {
    derive_v3_anchor_id(
        owner,
        &RetrievalAnchorTargetV3::ExactSourceOccurrence(occurrence_id.clone()),
    )
}

/// Derive the canonical public anchor for one immutable evidence span.
pub fn derive_exact_evidence_span_anchor_id(
    owner: &AnchorOwnerBindingV1,
    span_id: &EvidenceSpanIdV1,
) -> Result<RetrievalAnchorId, DomainError> {
    derive_v3_anchor_id(
        owner,
        &RetrievalAnchorTargetV3::ExactEvidenceSpan(span_id.clone()),
    )
}

/// Derive the canonical public anchor for one retriever contribution.
pub fn derive_retriever_contribution_anchor_id(
    owner: &AnchorOwnerBindingV1,
    contribution_id: &RetrieverContributionIdV1,
) -> Result<RetrievalAnchorId, DomainError> {
    derive_v3_anchor_id(
        owner,
        &RetrievalAnchorTargetV3::RetrieverContribution(contribution_id.clone()),
    )
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

impl<'de> Deserialize<'de> for RetrievalAnchorRecordV3 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            anchor_id: RetrievalAnchorId,
            target: RetrievalAnchorTargetV3,
            owner: AnchorOwnerBindingV1,
            aliases: Vec<NativeAliasV2>,
            occurred_at: Option<TimeInterval>,
            ingested_at: UtcMicros,
            evidence_class: EvidenceClass,
            source_generation: AnchorSourceGenerationV3,
            projection_generation: ProjectionGenerationId,
            projection_watermark: VectorWatermark,
            coverage: CoverageReportV1,
            source_observations: Vec<CanonicalObservationIdV1>,
            source_anchors: Vec<AnchorLineageRefV3>,
            authorization: ResolutionAuthorizationV1,
            payload_access: PayloadAccessState,
            retention_class: RetentionClass,
            durability: AnchorDurabilityClass,
        }

        let wire = Wire::deserialize(deserializer)?;
        let claimed_id = wire.anchor_id;
        let record = Self::new(RetrievalAnchorRecordV3Parts {
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
    let domain = if matches!(target, RetrievalAnchorTargetV2::GitTopology(_)) {
        RETRIEVAL_ANCHOR_V3_ID_DOMAIN
    } else {
        RETRIEVAL_ANCHOR_V2_ID_DOMAIN
    };
    let digest = canonical_sha256(&Identity {
        domain,
        owner,
        target,
    })?;
    let version = if matches!(target, RetrievalAnchorTargetV2::GitTopology(_)) {
        "v3"
    } else {
        "v2"
    };
    RetrievalAnchorId::new(format!("retrieval.{version}.{}", digest.as_str()))
}

fn derive_v3_anchor_id(
    owner: &AnchorOwnerBindingV1,
    target: &RetrievalAnchorTargetV3,
) -> Result<RetrievalAnchorId, DomainError> {
    #[derive(Serialize)]
    struct Identity<'a> {
        domain: &'static str,
        owner: &'a AnchorOwnerBindingV1,
        target: &'a RetrievalAnchorTargetV3,
    }

    owner.validate()?;
    target.validate()?;
    if !matches!(target, RetrievalAnchorTargetV3::GitTopology(_))
        && let Some(legacy) = target.as_v2()
    {
        return derive_anchor_id(&owner.observation_scope(), &legacy);
    }
    let digest = canonical_sha256(&Identity {
        domain: RETRIEVAL_ANCHOR_V3_ID_DOMAIN,
        owner,
        target,
    })?;
    RetrievalAnchorId::new(format!("retrieval.v3.{}", digest.as_str()))
}

/// Derives the exact profile/project/privacy-bound identity for a Git topology
/// target before publication. Callers still must publish and resolve the full
/// V3 record through the canonical retrieval-anchor authority.
pub fn derive_git_topology_anchor_id_v3(
    owner: &AnchorOwnerBindingV1,
    target: &GitTopologyAnchorTargetV1,
) -> Result<RetrievalAnchorId, DomainError> {
    derive_v3_anchor_id(
        owner,
        &RetrievalAnchorTargetV3::GitTopology(Box::new(target.clone())),
    )
}

fn validate_owner(owner: &ObservationScopeV1) -> Result<(), DomainError> {
    owner.validate().map_err(|_| DomainError::UnknownReference {
        field: "retrieval anchor owner",
    })
}

use crate::canonical_text::validate_git_object_id;

fn ensure_unique_aliases(aliases: &[NativeAliasV2]) -> Result<(), DomainError> {
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

fn validate_collection_bounds(parts: &RetrievalAnchorRecordV2Parts) -> Result<(), DomainError> {
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

fn validate_collection_bounds_v3(parts: &RetrievalAnchorRecordV3Parts) -> Result<(), DomainError> {
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
            field: "retrieval anchor V3 source lineage",
        });
    }
    Ok(())
}

fn validate_source_generation_v3(
    source: &AnchorSourceGenerationV3,
    target: &RetrievalAnchorTargetV3,
) -> Result<(), DomainError> {
    if let Some(legacy) = target.as_v2() {
        return source.validate_for_target(&legacy);
    }
    match source {
        AnchorSourceGenerationV3::RepositoryCapture(capture_id) => capture_id.validate(),
        AnchorSourceGenerationV3::GitTopology(generation) => generation.validate(),
        AnchorSourceGenerationV3::Observation(_)
        | AnchorSourceGenerationV3::Unavailable
        | AnchorSourceGenerationV3::Unknown => Ok(()),
    }
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
#[path = "anchor_test.rs"]
mod anchor_test;
