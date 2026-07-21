use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::{
    BrainId, LocatorDigest, ProjectId, RepositoryId, UserProfileId, WorktreeId,
};

use super::StorageRuntimeContractErrorV1;

macro_rules! canonical_id {
    ($name:ident, $field:literal) => {
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub const MAX_BYTES: usize = 512;

            pub fn new(value: impl Into<String>) -> Result<Self, StorageRuntimeContractErrorV1> {
                let value = value.into();
                validate_canonical_id(&value, $field, Self::MAX_BYTES)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }

        impl TryFrom<String> for $name {
            type Error = StorageRuntimeContractErrorV1;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

canonical_id!(StoreSnapshotIdV1, "store snapshot id");
canonical_id!(SnapshotLeaseIdV1, "snapshot lease id");
canonical_id!(StoreOperationIdV1, "store operation id");
canonical_id!(StoreClientIdV1, "store client id");
canonical_id!(RuntimePublicationIdV1, "runtime publication id");
canonical_id!(RuntimeLeaseIdV1, "runtime lease id");
canonical_id!(ReaderHealthLeaseIdV1, "reader health lease id");
canonical_id!(
    RuntimeMaintenanceTransitionIdV1,
    "runtime maintenance transition id"
);
canonical_id!(RuntimeOperationPermitIdV1, "runtime operation permit id");
canonical_id!(RuntimeTransactionIdV1, "runtime transaction id");
canonical_id!(EffectIdV1, "effect id");
canonical_id!(EffectOrderingKeyV1, "effect ordering key");

pub(super) fn validate_canonical_id(
    value: &str,
    field: &'static str,
    max: usize,
) -> Result<(), StorageRuntimeContractErrorV1> {
    if value.is_empty() {
        return Err(StorageRuntimeContractErrorV1::Empty { field });
    }
    if value.len() > max {
        return Err(StorageRuntimeContractErrorV1::TooLong {
            field,
            actual: value.len(),
            max,
        });
    }
    if value.trim() != value || value.chars().any(char::is_control) {
        return Err(StorageRuntimeContractErrorV1::NonCanonical { field });
    }
    Ok(())
}

/// The part of a code-index identity below its repository.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CodeShardScopeV1 {
    Worktree {
        worktree_id: WorktreeId,
    },
    /// Immutable retained state. It is never a mutable code-index target.
    Snapshot {
        worktree_id: Option<WorktreeId>,
        snapshot_id: StoreSnapshotIdV1,
    },
}

/// Logical store family. Only code shards may be worktree or snapshot scoped.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StoreShardScopeV1 {
    Profile,
    Project {
        project_id: ProjectId,
    },
    ProjectSessions {
        project_id: ProjectId,
    },
    Code {
        project_id: ProjectId,
        repository_id: RepositoryId,
        scope: CodeShardScopeV1,
    },
}

impl StoreShardScopeV1 {
    pub fn project_id(&self) -> Option<&ProjectId> {
        match self {
            Self::Profile => None,
            Self::Project { project_id }
            | Self::ProjectSessions { project_id }
            | Self::Code { project_id, .. } => Some(project_id),
        }
    }

    pub fn is_mutable(&self) -> bool {
        match self {
            Self::Profile | Self::Project { .. } | Self::ProjectSessions { .. } => true,
            Self::Code {
                scope: CodeShardScopeV1::Worktree { .. },
                ..
            } => true,
            Self::Code {
                scope: CodeShardScopeV1::Snapshot { .. },
                ..
            } => false,
        }
    }
}

/// Canonical logical shard identity, independent of aliases and physical locators.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct StoreShardIdV1 {
    pub brain_id: BrainId,
    pub profile_id: UserProfileId,
    pub scope: StoreShardScopeV1,
}

impl StoreShardIdV1 {
    pub fn new(brain_id: BrainId, profile_id: UserProfileId, scope: StoreShardScopeV1) -> Self {
        Self {
            brain_id,
            profile_id,
            scope,
        }
    }

    pub fn profile(brain_id: BrainId, profile_id: UserProfileId) -> Self {
        Self::new(brain_id, profile_id, StoreShardScopeV1::Profile)
    }

    pub fn project(brain_id: BrainId, profile_id: UserProfileId, project_id: ProjectId) -> Self {
        Self::new(
            brain_id,
            profile_id,
            StoreShardScopeV1::Project { project_id },
        )
    }

    pub fn project_sessions(
        brain_id: BrainId,
        profile_id: UserProfileId,
        project_id: ProjectId,
    ) -> Self {
        Self::new(
            brain_id,
            profile_id,
            StoreShardScopeV1::ProjectSessions { project_id },
        )
    }

    pub fn code(
        brain_id: BrainId,
        profile_id: UserProfileId,
        project_id: ProjectId,
        repository_id: RepositoryId,
        scope: CodeShardScopeV1,
    ) -> Self {
        Self::new(
            brain_id,
            profile_id,
            StoreShardScopeV1::Code {
                project_id,
                repository_id,
                scope,
            },
        )
    }

    pub fn is_mutable(&self) -> bool {
        self.scope.is_mutable()
    }
}

/// Monotonic identity of one physical publication of a logical shard.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(try_from = "u64", into = "u64")]
pub struct StoreIncarnationV1(u64);

impl StoreIncarnationV1 {
    pub fn new(value: u64) -> Result<Self, StorageRuntimeContractErrorV1> {
        if value == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "store incarnation",
            });
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for StoreIncarnationV1 {
    type Error = StorageRuntimeContractErrorV1;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<StoreIncarnationV1> for u64 {
    fn from(value: StoreIncarnationV1) -> Self {
        value.0
    }
}

/// Monotonic fencing token for the active writer authority.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(try_from = "u64", into = "u64")]
pub struct AuthorityEpochV1(u64);

impl AuthorityEpochV1 {
    pub fn new(value: u64) -> Result<Self, StorageRuntimeContractErrorV1> {
        if value == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "authority epoch",
            });
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for AuthorityEpochV1 {
    type Error = StorageRuntimeContractErrorV1;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<AuthorityEpochV1> for u64 {
    fn from(value: AuthorityEpochV1) -> Self {
        value.0
    }
}

/// Complete runtime identity for an active logical shard publication.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoreRuntimeBindingV1 {
    pub shard_id: StoreShardIdV1,
    pub incarnation: StoreIncarnationV1,
    pub authority_epoch: AuthorityEpochV1,
}

impl StoreRuntimeBindingV1 {
    pub fn new(
        shard_id: StoreShardIdV1,
        incarnation: StoreIncarnationV1,
        authority_epoch: AuthorityEpochV1,
    ) -> Self {
        Self {
            shard_id,
            incarnation,
            authority_epoch,
        }
    }
}

/// A locator only after the daemon has verified it for a canonical identity.
///
/// The digest is intentionally opaque. This contract cannot select, normalize,
/// or open a filesystem path.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedStoreLocatorV1 {
    pub shard_id: StoreShardIdV1,
    pub incarnation: StoreIncarnationV1,
    pub locator_digest: LocatorDigest,
}

impl VerifiedStoreLocatorV1 {
    pub fn new(
        shard_id: StoreShardIdV1,
        incarnation: StoreIncarnationV1,
        locator_digest: LocatorDigest,
    ) -> Self {
        Self {
            shard_id,
            incarnation,
            locator_digest,
        }
    }
}
