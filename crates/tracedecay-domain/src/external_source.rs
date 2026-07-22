//! Provider-neutral external-source identities, frontiers, and safe snapshots.
//!
//! These contracts carry only typed owners and privacy-bound digests. Provider
//! locators, credentials, paths, and payloads remain outside this boundary.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::configuration::{SourceBindingId, UserProfileId};
use crate::research::{
    DomainError, LocatorDigest, ManifestDigest, PrivacyDomainId, ProjectId, ProviderId,
    SourceInstanceId, canonical_sha256,
};

pub const MAX_SOURCE_PARTITIONS_V1: u16 = 64;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct SourcePartitionIdV1(ManifestDigest);

impl SourcePartitionIdV1 {
    pub fn new(digest: ManifestDigest) -> Self {
        Self(digest)
    }

    pub fn digest(&self) -> &ManifestDigest {
        &self.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.0.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct SourceCursorV1(ManifestDigest);

impl SourceCursorV1 {
    pub fn new(digest: ManifestDigest) -> Self {
        Self(digest)
    }

    pub fn digest(&self) -> &ManifestDigest {
        &self.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.0.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct SourceSnapshotIdV1(ManifestDigest);

impl SourceSnapshotIdV1 {
    pub fn new(digest: ManifestDigest) -> Self {
        Self(digest)
    }

    pub fn digest(&self) -> &ManifestDigest {
        &self.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.0.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct SourceNativeObjectIdV1(ManifestDigest);

impl SourceNativeObjectIdV1 {
    pub fn new(digest: ManifestDigest) -> Self {
        Self(digest)
    }

    pub fn digest(&self) -> &ManifestDigest {
        &self.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.0.validate()
    }
}

/// Stable provider revision identity for one native object.
///
/// This intentionally does not derive an ordering relation: object revisions
/// are comparable only by equality unless a provider-specific contract says
/// otherwise.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct SourceObjectRevisionV1(ManifestDigest);

impl SourceObjectRevisionV1 {
    pub fn new(digest: ManifestDigest) -> Self {
        Self(digest)
    }

    pub fn digest(&self) -> &ManifestDigest {
        &self.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.0.validate()
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceCaptureModeV1 {
    Event,
    Poll,
    Hybrid,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceRefetchStrategyV1 {
    WholeRoot,
    IncrementalRevision,
    IncrementalWithWholeRootFallback,
}

impl SourceRefetchStrategyV1 {
    pub const fn supports_whole_root(self) -> bool {
        matches!(
            self,
            Self::WholeRoot | Self::IncrementalWithWholeRootFallback
        )
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceDeletionSemanticsV1 {
    ExplicitOnly,
    CompleteSnapshotAbsence,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceCoverageV1 {
    Complete,
    Partial,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceContentStateV1 {
    Live,
    AuthoritativeDeleted,
    Partial,
    TemporarilyUnavailable,
}

/// Immutable provider-neutral source definition.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceDefinitionV1 {
    pub source_id: SourceInstanceId,
    pub provider: ProviderId,
    pub revision: u64,
    pub capture_mode: SourceCaptureModeV1,
    pub refetch_strategy: SourceRefetchStrategyV1,
    pub deletion_semantics: SourceDeletionSemanticsV1,
    pub max_partitions: u16,
    pub definition_digest: ManifestDigest,
}

impl SourceDefinitionV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_id: SourceInstanceId,
        provider: ProviderId,
        revision: u64,
        capture_mode: SourceCaptureModeV1,
        refetch_strategy: SourceRefetchStrategyV1,
        deletion_semantics: SourceDeletionSemanticsV1,
        max_partitions: u16,
    ) -> Result<Self, DomainError> {
        let definition_digest = Self::compute_digest(
            &source_id,
            &provider,
            revision,
            capture_mode,
            refetch_strategy,
            deletion_semantics,
            max_partitions,
        )?;
        let definition = Self {
            source_id,
            provider,
            revision,
            capture_mode,
            refetch_strategy,
            deletion_semantics,
            max_partitions,
            definition_digest,
        };
        definition.validate()?;
        Ok(definition)
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_digest(
        source_id: &SourceInstanceId,
        provider: &ProviderId,
        revision: u64,
        capture_mode: SourceCaptureModeV1,
        refetch_strategy: SourceRefetchStrategyV1,
        deletion_semantics: SourceDeletionSemanticsV1,
        max_partitions: u16,
    ) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            "tracedecay.external-source.definition.v1",
            source_id,
            provider,
            revision,
            capture_mode,
            refetch_strategy,
            deletion_semantics,
            max_partitions,
        ))
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.source_id.validate()?;
        self.provider.validate()?;
        self.definition_digest.validate()?;
        if self.revision == 0
            || self.max_partitions == 0
            || self.max_partitions > MAX_SOURCE_PARTITIONS_V1
            || (self.deletion_semantics == SourceDeletionSemanticsV1::CompleteSnapshotAbsence
                && !self.refetch_strategy.supports_whole_root())
        {
            return Err(DomainError::NonCanonical {
                field: "external source definition",
            });
        }
        if Self::compute_digest(
            &self.source_id,
            &self.provider,
            self.revision,
            self.capture_mode,
            self.refetch_strategy,
            self.deletion_semantics,
            self.max_partitions,
        )? != self.definition_digest
        {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum SourceBindingOwnerV1 {
    Project(ProjectId),
    Profile(UserProfileId),
}

impl SourceBindingOwnerV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Project(project_id) => project_id.validate(),
            Self::Profile(profile_id) => profile_id.validate(),
        }
    }
}

/// The immutable dimensions that prevent sources from crossing owners or
/// privacy domains.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct SourceBindingIdentityV1 {
    pub binding_id: SourceBindingId,
    pub source_id: SourceInstanceId,
    pub owner: SourceBindingOwnerV1,
    pub privacy_domain: PrivacyDomainId,
    pub native_root: LocatorDigest,
}

impl SourceBindingIdentityV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.binding_id.validate()?;
        self.source_id.validate()?;
        self.owner.validate()?;
        self.privacy_domain.validate()?;
        self.native_root.validate()
    }
}

/// Immutable source-to-owner binding snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceBindingV1 {
    pub binding_id: SourceBindingId,
    pub source_id: SourceInstanceId,
    pub definition_revision: u64,
    pub definition_digest: ManifestDigest,
    pub binding_revision: u64,
    pub owner: SourceBindingOwnerV1,
    pub privacy_domain: PrivacyDomainId,
    pub native_root: LocatorDigest,
    pub binding_digest: ManifestDigest,
}

impl SourceBindingV1 {
    pub fn new(
        definition: &SourceDefinitionV1,
        owner: SourceBindingOwnerV1,
        privacy_domain: PrivacyDomainId,
        native_root: LocatorDigest,
        binding_revision: u64,
    ) -> Result<Self, DomainError> {
        definition.validate()?;
        owner.validate()?;
        privacy_domain.validate()?;
        native_root.validate()?;
        if binding_revision == 0 {
            return Err(DomainError::NonCanonical {
                field: "external source binding revision",
            });
        }
        let binding_id =
            Self::derive_binding_id(&definition.source_id, &owner, &privacy_domain, &native_root)?;
        let binding_digest = Self::compute_digest(
            &binding_id,
            &definition.source_id,
            definition.revision,
            &definition.definition_digest,
            binding_revision,
            &owner,
            &privacy_domain,
            &native_root,
        )?;
        let binding = Self {
            binding_id,
            source_id: definition.source_id.clone(),
            definition_revision: definition.revision,
            definition_digest: definition.definition_digest.clone(),
            binding_revision,
            owner,
            privacy_domain,
            native_root,
            binding_digest,
        };
        binding.validate_against(definition)?;
        Ok(binding)
    }

    pub fn immutable_identity(&self) -> Result<SourceBindingIdentityV1, DomainError> {
        let identity = SourceBindingIdentityV1 {
            binding_id: self.binding_id.clone(),
            source_id: self.source_id.clone(),
            owner: self.owner.clone(),
            privacy_domain: self.privacy_domain.clone(),
            native_root: self.native_root.clone(),
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.immutable_identity()?.validate()?;
        self.definition_digest.validate()?;
        self.binding_digest.validate()?;
        if self.definition_revision == 0 || self.binding_revision == 0 {
            return Err(DomainError::NonCanonical {
                field: "external source binding revision",
            });
        }
        let expected_id = Self::derive_binding_id(
            &self.source_id,
            &self.owner,
            &self.privacy_domain,
            &self.native_root,
        )?;
        if self.binding_id != expected_id {
            return Err(DomainError::NonCanonical {
                field: "external source binding identity",
            });
        }
        let expected_digest = Self::compute_digest(
            &self.binding_id,
            &self.source_id,
            self.definition_revision,
            &self.definition_digest,
            self.binding_revision,
            &self.owner,
            &self.privacy_domain,
            &self.native_root,
        )?;
        if expected_digest != self.binding_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }

    pub fn validate_against(&self, definition: &SourceDefinitionV1) -> Result<(), DomainError> {
        self.validate()?;
        definition.validate()?;
        if self.source_id != definition.source_id
            || self.definition_revision != definition.revision
            || self.definition_digest != definition.definition_digest
        {
            return Err(DomainError::SnapshotMismatch {
                field: "external source binding definition",
            });
        }
        Ok(())
    }

    fn derive_binding_id(
        source_id: &SourceInstanceId,
        owner: &SourceBindingOwnerV1,
        privacy_domain: &PrivacyDomainId,
        native_root: &LocatorDigest,
    ) -> Result<SourceBindingId, DomainError> {
        let digest = canonical_sha256(&(
            "tracedecay.external-source.binding-id.v1",
            source_id,
            owner,
            privacy_domain,
            native_root,
        ))?;
        SourceBindingId::new(format!(
            "external-source.{}",
            digest.as_str().trim_start_matches("sha256:")
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_digest(
        binding_id: &SourceBindingId,
        source_id: &SourceInstanceId,
        definition_revision: u64,
        definition_digest: &ManifestDigest,
        binding_revision: u64,
        owner: &SourceBindingOwnerV1,
        privacy_domain: &PrivacyDomainId,
        native_root: &LocatorDigest,
    ) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            "tracedecay.external-source.binding.v1",
            binding_id,
            source_id,
            definition_revision,
            definition_digest,
            binding_revision,
            owner,
            privacy_domain,
            native_root,
        ))
    }
}

/// One partition's committed source frontier. Cursor and snapshot identities
/// are opaque provider-bound digests, never raw provider cursors or URLs.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourcePartitionFrontierV1 {
    binding: SourceBindingIdentityV1,
    partition: SourcePartitionIdV1,
    cursor: Option<SourceCursorV1>,
    snapshot: Option<SourceSnapshotIdV1>,
    continuation: Option<SourceCursorV1>,
    coverage: SourceCoverageV1,
    sequence: u64,
    last_complete_snapshot: Option<SourceSnapshotIdV1>,
    input_digest: ManifestDigest,
}

impl SourcePartitionFrontierV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        binding: SourceBindingIdentityV1,
        partition: SourcePartitionIdV1,
        cursor: Option<SourceCursorV1>,
        snapshot: Option<SourceSnapshotIdV1>,
        continuation: Option<SourceCursorV1>,
        coverage: SourceCoverageV1,
        sequence: u64,
        previous_complete_snapshot: Option<SourceSnapshotIdV1>,
        input_digest: ManifestDigest,
    ) -> Result<Self, DomainError> {
        binding.validate()?;
        partition.validate()?;
        cursor.as_ref().map_or(Ok(()), SourceCursorV1::validate)?;
        snapshot
            .as_ref()
            .map_or(Ok(()), SourceSnapshotIdV1::validate)?;
        continuation
            .as_ref()
            .map_or(Ok(()), SourceCursorV1::validate)?;
        input_digest.validate()?;
        if sequence == 0 {
            return Err(DomainError::NonCanonical {
                field: "external source partition sequence",
            });
        }
        let last_complete_snapshot = match coverage {
            SourceCoverageV1::Complete => {
                if continuation.is_some() {
                    return Err(DomainError::NonCanonical {
                        field: "complete external source continuation",
                    });
                }
                Some(snapshot.clone().ok_or(DomainError::NonCanonical {
                    field: "complete external source snapshot",
                })?)
            }
            SourceCoverageV1::Partial => {
                if continuation.is_none() {
                    return Err(DomainError::NonCanonical {
                        field: "partial external source continuation",
                    });
                }
                previous_complete_snapshot
            }
            SourceCoverageV1::Unknown => {
                if snapshot.is_some() || continuation.is_some() {
                    return Err(DomainError::NonCanonical {
                        field: "unknown external source frontier",
                    });
                }
                previous_complete_snapshot
            }
        };
        let frontier = Self {
            binding,
            partition,
            cursor,
            snapshot,
            continuation,
            coverage,
            sequence,
            last_complete_snapshot,
            input_digest,
        };
        frontier.validate()?;
        Ok(frontier)
    }

    pub fn binding(&self) -> &SourceBindingIdentityV1 {
        &self.binding
    }

    pub fn partition(&self) -> &SourcePartitionIdV1 {
        &self.partition
    }

    pub fn cursor(&self) -> Option<&SourceCursorV1> {
        self.cursor.as_ref()
    }

    pub fn snapshot(&self) -> Option<&SourceSnapshotIdV1> {
        self.snapshot.as_ref()
    }

    pub fn continuation(&self) -> Option<&SourceCursorV1> {
        self.continuation.as_ref()
    }

    pub fn coverage(&self) -> SourceCoverageV1 {
        self.coverage
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn last_complete_snapshot(&self) -> Option<SourceSnapshotIdV1> {
        self.last_complete_snapshot.clone()
    }

    pub fn input_digest(&self) -> &ManifestDigest {
        &self.input_digest
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.binding.validate()?;
        self.partition.validate()?;
        self.cursor
            .as_ref()
            .map_or(Ok(()), SourceCursorV1::validate)?;
        self.snapshot
            .as_ref()
            .map_or(Ok(()), SourceSnapshotIdV1::validate)?;
        self.continuation
            .as_ref()
            .map_or(Ok(()), SourceCursorV1::validate)?;
        self.last_complete_snapshot
            .as_ref()
            .map_or(Ok(()), SourceSnapshotIdV1::validate)?;
        self.input_digest.validate()?;
        if self.sequence == 0 {
            return Err(DomainError::NonCanonical {
                field: "external source partition sequence",
            });
        }
        match self.coverage {
            SourceCoverageV1::Complete => {
                if self.continuation.is_some()
                    || self.snapshot.is_none()
                    || self.last_complete_snapshot != self.snapshot
                {
                    return Err(DomainError::NonCanonical {
                        field: "complete external source frontier",
                    });
                }
            }
            SourceCoverageV1::Partial if self.continuation.is_none() => {
                return Err(DomainError::NonCanonical {
                    field: "partial external source continuation",
                });
            }
            SourceCoverageV1::Unknown if self.snapshot.is_some() || self.continuation.is_some() => {
                return Err(DomainError::NonCanonical {
                    field: "unknown external source frontier",
                });
            }
            SourceCoverageV1::Partial | SourceCoverageV1::Unknown => {}
        }
        Ok(())
    }
}

/// Domain-separated aggregate over the sorted current partition heads.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceAggregateFrontierV1 {
    binding: SourceBindingIdentityV1,
    partitions: BTreeMap<SourcePartitionIdV1, SourcePartitionFrontierV1>,
    digest: ManifestDigest,
}

impl SourceAggregateFrontierV1 {
    pub fn with_updated_partition(
        binding: SourceBindingIdentityV1,
        previous: Option<&Self>,
        next: SourcePartitionFrontierV1,
    ) -> Result<Self, DomainError> {
        binding.validate()?;
        next.validate()?;
        if next.binding() != &binding {
            return Err(DomainError::SnapshotMismatch {
                field: "external source partition binding",
            });
        }
        let mut partitions =
            previous.map_or_else(BTreeMap::new, |frontier| frontier.partitions.clone());
        if let Some(previous) = previous {
            previous.validate()?;
            if previous.binding != binding {
                return Err(DomainError::SnapshotMismatch {
                    field: "external source aggregate binding",
                });
            }
        }
        partitions.insert(next.partition().clone(), next);
        Self::new(binding, partitions)
    }

    pub fn new(
        binding: SourceBindingIdentityV1,
        partitions: BTreeMap<SourcePartitionIdV1, SourcePartitionFrontierV1>,
    ) -> Result<Self, DomainError> {
        binding.validate()?;
        if partitions.is_empty() || partitions.len() > usize::from(MAX_SOURCE_PARTITIONS_V1) {
            return Err(DomainError::NonCanonical {
                field: "external source aggregate partitions",
            });
        }
        for (partition, frontier) in &partitions {
            partition.validate()?;
            frontier.validate()?;
            if partition != frontier.partition() || frontier.binding() != &binding {
                return Err(DomainError::SnapshotMismatch {
                    field: "external source aggregate partition",
                });
            }
        }
        let digest = canonical_sha256(&(
            "tracedecay.external-source.aggregate-frontier.v1",
            &binding,
            &partitions,
        ))?;
        let frontier = Self {
            binding,
            partitions,
            digest,
        };
        frontier.validate()?;
        Ok(frontier)
    }

    pub fn binding(&self) -> &SourceBindingIdentityV1 {
        &self.binding
    }

    pub fn partition(&self, partition: &SourcePartitionIdV1) -> Option<&SourcePartitionFrontierV1> {
        self.partitions.get(partition)
    }

    pub fn partitions(&self) -> &BTreeMap<SourcePartitionIdV1, SourcePartitionFrontierV1> {
        &self.partitions
    }

    pub fn digest(&self) -> &ManifestDigest {
        &self.digest
    }

    pub fn coverage(&self) -> SourceCoverageV1 {
        if self
            .partitions
            .values()
            .all(|frontier| frontier.coverage() == SourceCoverageV1::Complete)
        {
            SourceCoverageV1::Complete
        } else if self
            .partitions
            .values()
            .any(|frontier| frontier.coverage() == SourceCoverageV1::Unknown)
        {
            SourceCoverageV1::Unknown
        } else {
            SourceCoverageV1::Partial
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.binding.validate()?;
        if self.partitions.is_empty()
            || self.partitions.len() > usize::from(MAX_SOURCE_PARTITIONS_V1)
        {
            return Err(DomainError::NonCanonical {
                field: "external source aggregate partitions",
            });
        }
        for (partition, frontier) in &self.partitions {
            partition.validate()?;
            frontier.validate()?;
            if partition != frontier.partition() || frontier.binding() != &self.binding {
                return Err(DomainError::SnapshotMismatch {
                    field: "external source aggregate partition",
                });
            }
        }
        let digest = canonical_sha256(&(
            "tracedecay.external-source.aggregate-frontier.v1",
            &self.binding,
            &self.partitions,
        ))?;
        if digest != self.digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

/// Immutable sanitized evidence for one provider-native object revision.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceObjectObservationV1 {
    native_object: SourceNativeObjectIdV1,
    revision: SourceObjectRevisionV1,
    sanitized_digest: ManifestDigest,
    content_state: SourceContentStateV1,
}

impl SourceObjectObservationV1 {
    pub fn new(
        native_object: SourceNativeObjectIdV1,
        revision: SourceObjectRevisionV1,
        sanitized_digest: ManifestDigest,
        content_state: SourceContentStateV1,
    ) -> Result<Self, DomainError> {
        let observation = Self {
            native_object,
            revision,
            sanitized_digest,
            content_state,
        };
        observation.validate()?;
        Ok(observation)
    }

    pub fn native_object(&self) -> &SourceNativeObjectIdV1 {
        &self.native_object
    }

    pub fn revision(&self) -> &SourceObjectRevisionV1 {
        &self.revision
    }

    pub fn sanitized_digest(&self) -> &ManifestDigest {
        &self.sanitized_digest
    }

    pub fn content_state(&self) -> SourceContentStateV1 {
        self.content_state
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.native_object.validate()?;
        self.revision.validate()?;
        self.sanitized_digest.validate()
    }
}

/// Payload-free evidence that one whole-root snapshot is complete.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceSnapshotCompletionV1 {
    partition: SourcePartitionIdV1,
    snapshot: SourceSnapshotIdV1,
    present_objects: BTreeSet<SourceNativeObjectIdV1>,
    completion_digest: ManifestDigest,
}

impl SourceSnapshotCompletionV1 {
    pub fn new(
        partition: SourcePartitionIdV1,
        snapshot: SourceSnapshotIdV1,
        present_objects: BTreeSet<SourceNativeObjectIdV1>,
    ) -> Result<Self, DomainError> {
        partition.validate()?;
        snapshot.validate()?;
        for object in &present_objects {
            object.validate()?;
        }
        let completion_digest = canonical_sha256(&(
            "tracedecay.external-source.snapshot-completion.v1",
            &partition,
            &snapshot,
            &present_objects,
        ))?;
        let completion = Self {
            partition,
            snapshot,
            present_objects,
            completion_digest,
        };
        completion.validate()?;
        Ok(completion)
    }

    pub fn partition(&self) -> &SourcePartitionIdV1 {
        &self.partition
    }

    pub fn snapshot(&self) -> &SourceSnapshotIdV1 {
        &self.snapshot
    }

    pub fn present_objects(&self) -> &BTreeSet<SourceNativeObjectIdV1> {
        &self.present_objects
    }

    pub fn completion_digest(&self) -> &ManifestDigest {
        &self.completion_digest
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.partition.validate()?;
        self.snapshot.validate()?;
        self.completion_digest.validate()?;
        for object in &self.present_objects {
            object.validate()?;
        }
        let digest = canonical_sha256(&(
            "tracedecay.external-source.snapshot-completion.v1",
            &self.partition,
            &self.snapshot,
            &self.present_objects,
        ))?;
        if digest != self.completion_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}
