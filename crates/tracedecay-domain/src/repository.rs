//! Pure repository-provenance contracts.
//!
//! Capture happens outside this crate. These values preserve only canonical,
//! path-safe evidence and never infer facts that the capture boundary could
//! not establish.

use serde::{Deserialize, Deserializer, Serialize};

use crate::observation::CanonicalObservationIdV1;
use crate::research::{
    CommitId, DomainError, PrivacyDomainBoundLocatorDigest, ProjectId, ProjectionGenerationId,
    RefId, RepositoryCaptureId, RepositoryId, TreeId, UtcMicros, WorktreeId, canonical_sha256,
};

const CAPTURE_ID_NAMESPACE: &str = "repository.capture.v1";

/// Explicit availability of one repository evidence value.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(
    tag = "availability",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum EvidenceAvailabilityV1<T> {
    Known(T),
    Missing,
    Unborn,
    Detached,
    Conflicted,
    PartiallyReadable(T),
    Unsupported,
    Unavailable,
    #[default]
    Unknown,
}

impl<T> EvidenceAvailabilityV1<T> {
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Known(value) | Self::PartiallyReadable(value) => Some(value),
            Self::Missing
            | Self::Unborn
            | Self::Detached
            | Self::Conflicted
            | Self::Unsupported
            | Self::Unavailable
            | Self::Unknown => None,
        }
    }

    fn validate_with(
        &self,
        validate: impl FnOnce(&T) -> Result<(), DomainError>,
    ) -> Result<(), DomainError> {
        match self.value() {
            Some(value) => validate(value),
            None => Ok(()),
        }
    }
}

/// Repository working-state evidence when it was observable.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryDirtyStateV1 {
    Clean,
    Dirty,
    Conflicted,
}

/// Privacy-safe identity of the configured primary repository remote.
///
/// The captured digest never contains a URL, credential, query, or fragment.
/// `Missing`, `Invalid`, and `Oversized` stay distinct so callers do not infer
/// that a remote was available when the bounded probe could not retain one.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(
    tag = "availability",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RepositoryRemoteIdentityV1 {
    Known(PrivacyDomainBoundLocatorDigest),
    Missing,
    Invalid,
    Oversized,
    Unavailable,
    #[default]
    Unknown,
}

impl RepositoryRemoteIdentityV1 {
    pub fn digest(&self) -> Option<&PrivacyDomainBoundLocatorDigest> {
        match self {
            Self::Known(digest) => Some(digest),
            Self::Missing | Self::Invalid | Self::Oversized | Self::Unavailable | Self::Unknown => {
                None
            }
        }
    }

    pub const fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }

    fn validate(&self) -> Result<(), DomainError> {
        self.digest()
            .map_or(Ok(()), PrivacyDomainBoundLocatorDigest::validate)
    }
}

/// Bounded repository facts captured by the daemon/application boundary.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepositoryEvidenceV1 {
    attached_ref: EvidenceAvailabilityV1<RefId>,
    head_commit: EvidenceAvailabilityV1<CommitId>,
    index_tree: EvidenceAvailabilityV1<TreeId>,
    path_identity_digest: EvidenceAvailabilityV1<PrivacyDomainBoundLocatorDigest>,
    #[serde(
        default,
        skip_serializing_if = "RepositoryRemoteIdentityV1::is_unknown"
    )]
    remote_identity: RepositoryRemoteIdentityV1,
    dirty_state: EvidenceAvailabilityV1<RepositoryDirtyStateV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositoryEvidenceWireV1 {
    attached_ref: EvidenceAvailabilityV1<RefId>,
    head_commit: EvidenceAvailabilityV1<CommitId>,
    index_tree: EvidenceAvailabilityV1<TreeId>,
    path_identity_digest: EvidenceAvailabilityV1<PrivacyDomainBoundLocatorDigest>,
    #[serde(default)]
    remote_identity: RepositoryRemoteIdentityV1,
    dirty_state: EvidenceAvailabilityV1<RepositoryDirtyStateV1>,
}

impl<'de> Deserialize<'de> for RepositoryEvidenceV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RepositoryEvidenceWireV1::deserialize(deserializer)?;
        Self::new(
            wire.attached_ref,
            wire.head_commit,
            wire.index_tree,
            wire.path_identity_digest,
            wire.remote_identity,
            wire.dirty_state,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl RepositoryEvidenceV1 {
    pub fn new(
        attached_ref: EvidenceAvailabilityV1<RefId>,
        head_commit: EvidenceAvailabilityV1<CommitId>,
        index_tree: EvidenceAvailabilityV1<TreeId>,
        path_identity_digest: EvidenceAvailabilityV1<PrivacyDomainBoundLocatorDigest>,
        remote_identity: RepositoryRemoteIdentityV1,
        dirty_state: EvidenceAvailabilityV1<RepositoryDirtyStateV1>,
    ) -> Result<Self, DomainError> {
        let evidence = Self {
            attached_ref,
            head_commit,
            index_tree,
            path_identity_digest,
            remote_identity,
            dirty_state,
        };
        evidence.validate()?;
        Ok(evidence)
    }

    pub fn attached_ref(&self) -> &EvidenceAvailabilityV1<RefId> {
        &self.attached_ref
    }

    pub fn head_commit(&self) -> &EvidenceAvailabilityV1<CommitId> {
        &self.head_commit
    }

    pub fn index_tree(&self) -> &EvidenceAvailabilityV1<TreeId> {
        &self.index_tree
    }

    pub fn path_identity_digest(&self) -> &EvidenceAvailabilityV1<PrivacyDomainBoundLocatorDigest> {
        &self.path_identity_digest
    }

    pub fn remote_identity(&self) -> &RepositoryRemoteIdentityV1 {
        &self.remote_identity
    }

    pub fn dirty_state(&self) -> &EvidenceAvailabilityV1<RepositoryDirtyStateV1> {
        &self.dirty_state
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.attached_ref.validate_with(RefId::validate)?;
        self.head_commit.validate_with(|value| {
            value.validate()?;
            validate_git_object_id(value.as_str(), "HEAD commit")
        })?;
        self.index_tree.validate_with(|value| {
            value.validate()?;
            validate_git_object_id(value.as_str(), "index tree")
        })?;
        self.path_identity_digest
            .validate_with(PrivacyDomainBoundLocatorDigest::validate)?;
        self.remote_identity.validate()?;
        Ok(())
    }
}

/// One immutable, path-safe capture of repository identity and state.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepositoryProvenanceV1 {
    capture_id: RepositoryCaptureId,
    repository_id: RepositoryId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_id: Option<ProjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worktree_id: Option<WorktreeId>,
    canonical_root_digest: PrivacyDomainBoundLocatorDigest,
    evidence: RepositoryEvidenceV1,
    captured_at: UtcMicros,
}

impl RepositoryProvenanceV1 {
    pub fn new(
        repository_id: RepositoryId,
        project_id: Option<ProjectId>,
        worktree_id: Option<WorktreeId>,
        canonical_root_digest: PrivacyDomainBoundLocatorDigest,
        evidence: RepositoryEvidenceV1,
        captured_at: UtcMicros,
    ) -> Result<Self, DomainError> {
        repository_id.validate()?;
        if let Some(project_id) = &project_id {
            project_id.validate()?;
        }
        if let Some(worktree_id) = &worktree_id {
            worktree_id.validate()?;
        }
        canonical_root_digest.validate()?;
        evidence.validate()?;

        let capture_id = derive_capture_id(
            &repository_id,
            project_id.as_ref(),
            worktree_id.as_ref(),
            &canonical_root_digest,
            &evidence,
            captured_at,
        )?;
        Ok(Self {
            capture_id,
            repository_id,
            project_id,
            worktree_id,
            canonical_root_digest,
            evidence,
            captured_at,
        })
    }

    pub fn capture_id(&self) -> &RepositoryCaptureId {
        &self.capture_id
    }

    pub fn repository_id(&self) -> &RepositoryId {
        &self.repository_id
    }

    pub fn project_id(&self) -> Option<&ProjectId> {
        self.project_id.as_ref()
    }

    pub fn worktree_id(&self) -> Option<&WorktreeId> {
        self.worktree_id.as_ref()
    }

    pub fn canonical_root_digest(&self) -> &PrivacyDomainBoundLocatorDigest {
        &self.canonical_root_digest
    }

    pub fn evidence(&self) -> &RepositoryEvidenceV1 {
        &self.evidence
    }

    pub fn captured_at(&self) -> UtcMicros {
        self.captured_at
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        let expected = derive_capture_id(
            &self.repository_id,
            self.project_id.as_ref(),
            self.worktree_id.as_ref(),
            &self.canonical_root_digest,
            &self.evidence,
            self.captured_at,
        )?;
        if self.capture_id != expected {
            return Err(DomainError::SnapshotMismatch {
                field: "repository capture identity",
            });
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositoryProvenanceWireV1 {
    capture_id: RepositoryCaptureId,
    repository_id: RepositoryId,
    #[serde(default)]
    project_id: Option<ProjectId>,
    #[serde(default)]
    worktree_id: Option<WorktreeId>,
    canonical_root_digest: PrivacyDomainBoundLocatorDigest,
    evidence: RepositoryEvidenceV1,
    captured_at: UtcMicros,
}

impl<'de> Deserialize<'de> for RepositoryProvenanceV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RepositoryProvenanceWireV1::deserialize(deserializer)?;
        let capture = Self::new(
            wire.repository_id,
            wire.project_id,
            wire.worktree_id,
            wire.canonical_root_digest,
            wire.evidence,
            wire.captured_at,
        )
        .map_err(serde::de::Error::custom)?;
        if wire.capture_id != capture.capture_id {
            return Err(serde::de::Error::custom(
                "repository capture identity does not match canonical evidence",
            ));
        }
        Ok(capture)
    }
}

/// Repository capture pinned to one immutable projection generation.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationBoundRepositoryProvenanceV1 {
    generation_id: ProjectionGenerationId,
    capture_id: RepositoryCaptureId,
    capture: RepositoryProvenanceV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_observation: Option<CanonicalObservationIdV1>,
}

impl GenerationBoundRepositoryProvenanceV1 {
    pub fn new(
        generation_id: ProjectionGenerationId,
        capture: RepositoryProvenanceV1,
        source_observation: Option<CanonicalObservationIdV1>,
    ) -> Result<Self, DomainError> {
        generation_id.validate()?;
        capture.validate()?;
        Ok(Self {
            generation_id,
            capture_id: capture.capture_id.clone(),
            capture,
            source_observation,
        })
    }

    pub fn generation_id(&self) -> &ProjectionGenerationId {
        &self.generation_id
    }

    pub fn capture_id(&self) -> &RepositoryCaptureId {
        &self.capture_id
    }

    pub fn capture(&self) -> &RepositoryProvenanceV1 {
        &self.capture
    }

    pub fn source_observation(&self) -> Option<&CanonicalObservationIdV1> {
        self.source_observation.as_ref()
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.generation_id.validate()?;
        self.capture.validate()?;
        if self.capture_id != self.capture.capture_id {
            return Err(DomainError::SnapshotMismatch {
                field: "generation repository capture identity",
            });
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationBoundRepositoryProvenanceWireV1 {
    generation_id: ProjectionGenerationId,
    capture_id: RepositoryCaptureId,
    capture: RepositoryProvenanceV1,
    #[serde(default)]
    source_observation: Option<CanonicalObservationIdV1>,
}

impl<'de> Deserialize<'de> for GenerationBoundRepositoryProvenanceV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = GenerationBoundRepositoryProvenanceWireV1::deserialize(deserializer)?;
        let binding = Self::new(wire.generation_id, wire.capture, wire.source_observation)
            .map_err(serde::de::Error::custom)?;
        if wire.capture_id != binding.capture_id {
            return Err(serde::de::Error::custom(
                "generation binding capture identity does not match its capture",
            ));
        }
        Ok(binding)
    }
}

#[derive(Serialize)]
struct RepositoryCaptureIdentityMaterialV1<'a> {
    repository_id: &'a RepositoryId,
    project_id: Option<&'a ProjectId>,
    worktree_id: Option<&'a WorktreeId>,
    canonical_root_digest: &'a PrivacyDomainBoundLocatorDigest,
    evidence: &'a RepositoryEvidenceV1,
    captured_at: UtcMicros,
}

fn derive_capture_id(
    repository_id: &RepositoryId,
    project_id: Option<&ProjectId>,
    worktree_id: Option<&WorktreeId>,
    canonical_root_digest: &PrivacyDomainBoundLocatorDigest,
    evidence: &RepositoryEvidenceV1,
    captured_at: UtcMicros,
) -> Result<RepositoryCaptureId, DomainError> {
    let digest = canonical_sha256(&(
        CAPTURE_ID_NAMESPACE,
        RepositoryCaptureIdentityMaterialV1 {
            repository_id,
            project_id,
            worktree_id,
            canonical_root_digest,
            evidence,
            captured_at,
        },
    ))?;
    let encoded = crate::canonical_text::sha256_hex_body(
        digest.as_str(),
        "repository capture identity digest",
    )?;
    RepositoryCaptureId::new(format!("{CAPTURE_ID_NAMESPACE}.{encoded}"))
}

use crate::canonical_text::validate_git_object_id;

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    const DIGEST_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const TREE: &str = "89abcdef0123456789abcdef0123456789abcdef";

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String, Error = DomainError>,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn evidence() -> RepositoryEvidenceV1 {
        RepositoryEvidenceV1::new(
            EvidenceAvailabilityV1::Known(id("refs/heads/main")),
            EvidenceAvailabilityV1::Known(id(COMMIT)),
            EvidenceAvailabilityV1::Known(id(TREE)),
            EvidenceAvailabilityV1::Known(id(DIGEST_B)),
            RepositoryRemoteIdentityV1::Known(id(DIGEST_A)),
            EvidenceAvailabilityV1::Known(RepositoryDirtyStateV1::Clean),
        )
        .unwrap()
    }

    fn capture() -> RepositoryProvenanceV1 {
        RepositoryProvenanceV1::new(
            id("repository.fixture"),
            Some(id("project.fixture")),
            Some(id("worktree.fixture")),
            id(DIGEST_A),
            evidence(),
            UtcMicros(42),
        )
        .unwrap()
    }

    #[test]
    fn capture_identity_is_deterministic() {
        assert_eq!(capture().capture_id(), capture().capture_id());
    }

    #[test]
    fn detached_unborn_and_unavailable_are_preserved() {
        let evidence = RepositoryEvidenceV1::new(
            EvidenceAvailabilityV1::Detached,
            EvidenceAvailabilityV1::Unborn,
            EvidenceAvailabilityV1::Unavailable,
            EvidenceAvailabilityV1::Unknown,
            RepositoryRemoteIdentityV1::Unknown,
            EvidenceAvailabilityV1::Unknown,
        )
        .unwrap();
        let round_trip: RepositoryEvidenceV1 =
            serde_json::from_value(serde_json::to_value(&evidence).unwrap()).unwrap();

        assert_eq!(round_trip.attached_ref(), &EvidenceAvailabilityV1::Detached);
        assert_eq!(round_trip.head_commit(), &EvidenceAvailabilityV1::Unborn);
        assert_eq!(
            round_trip.index_tree(),
            &EvidenceAvailabilityV1::Unavailable
        );
        assert_eq!(
            round_trip.remote_identity(),
            &RepositoryRemoteIdentityV1::Unknown
        );
    }

    #[test]
    fn legacy_unknown_remote_identity_preserves_capture_identity() {
        let legacy_evidence = RepositoryEvidenceV1::new(
            EvidenceAvailabilityV1::Known(id("refs/heads/main")),
            EvidenceAvailabilityV1::Known(id(COMMIT)),
            EvidenceAvailabilityV1::Known(id(TREE)),
            EvidenceAvailabilityV1::Known(id(DIGEST_B)),
            RepositoryRemoteIdentityV1::Unknown,
            EvidenceAvailabilityV1::Known(RepositoryDirtyStateV1::Clean),
        )
        .unwrap();
        let capture = RepositoryProvenanceV1::new(
            id("repository.legacy-fixture"),
            Some(id("project.fixture")),
            Some(id("worktree.fixture")),
            id(DIGEST_A),
            legacy_evidence,
            UtcMicros(42),
        )
        .unwrap();
        let encoded = serde_json::to_value(&capture).unwrap();
        assert!(encoded["evidence"].get("remote_identity").is_none());
        let decoded: RepositoryProvenanceV1 = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.capture_id(), capture.capture_id());
        assert_eq!(
            decoded.evidence().remote_identity(),
            &RepositoryRemoteIdentityV1::Unknown
        );
    }

    #[test]
    fn standalone_evidence_deserialization_rejects_noncanonical_git_object_ids() {
        let mut value = serde_json::to_value(evidence()).unwrap();
        value["head_commit"]["value"] = Value::String("0123456789abcdef".into());

        assert!(serde_json::from_value::<RepositoryEvidenceV1>(value).is_err());
    }

    #[test]
    fn project_and_worktree_aliases_do_not_define_repository_identity() {
        let first = capture();
        let second = RepositoryProvenanceV1::new(
            first.repository_id().clone(),
            Some(id("project.alias")),
            Some(id("worktree.alias")),
            first.canonical_root_digest().clone(),
            first.evidence().clone(),
            first.captured_at(),
        )
        .unwrap();

        assert_eq!(first.repository_id(), second.repository_id());
        assert_ne!(first.capture_id(), second.capture_id());
    }

    #[test]
    fn generation_binding_rejects_capture_mismatch_and_tampering() {
        let binding = GenerationBoundRepositoryProvenanceV1::new(
            id("projection.fixture.v1"),
            capture(),
            None,
        )
        .unwrap();
        let mut mismatched = serde_json::to_value(&binding).unwrap();
        mismatched["capture_id"] = Value::String("repository.capture.v1.invalid".into());
        assert!(
            serde_json::from_value::<GenerationBoundRepositoryProvenanceV1>(mismatched).is_err()
        );

        let mut tampered = serde_json::to_value(&binding).unwrap();
        tampered["capture"]["captured_at"] = Value::from(43);
        assert!(serde_json::from_value::<GenerationBoundRepositoryProvenanceV1>(tampered).is_err());
    }
}
