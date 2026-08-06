use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_text::is_canonical_text;
pub use tracedecay_domain::{
    AuthorityEpoch, BrainId, BrainNodeId, LocatorDigest, ProjectId, RefId, RepositoryId,
    UserProfileId, WorktreeId,
};

use super::StorageRuntimeContractErrorV1;

const LOCATOR_DIGEST_DOMAIN: &[u8] = b"tracedecay.store-runtime.local-locator.v1\0";

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

        impl TryFrom<&str> for $name {
            type Error = StorageRuntimeContractErrorV1;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

pub(super) use canonical_id;

// A retained database snapshot is not an evaluation, configuration, or Git
// repository-state snapshot, so it intentionally does not reuse those domain IDs.
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
// Application-layer effect and idempotency identities cannot be imported here:
// `tracedecay-store` deliberately depends only on `tracedecay-domain`. These
// names make the storage ownership explicit, while the checked string
// conversions above provide the lossless adapter boundary.
canonical_id!(StoreEffectIdV1, "store effect id");
canonical_id!(StoreEffectOrderingKeyV1, "store effect ordering key");
canonical_id!(StoreIdempotencyKeyV1, "store idempotency key");

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
    if !is_canonical_text(value) {
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
    Branch {
        worktree_id: WorktreeId,
        ref_id: RefId,
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
    ProfileMemory,
    ProfileSessions,
    RemoteNode {
        node_id: BrainNodeId,
    },
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
            Self::Profile
            | Self::ProfileMemory
            | Self::ProfileSessions
            | Self::RemoteNode { .. } => None,
            Self::Project { project_id }
            | Self::ProjectSessions { project_id }
            | Self::Code { project_id, .. } => Some(project_id),
        }
    }

    pub fn is_mutable(&self) -> bool {
        match self {
            Self::Profile
            | Self::ProfileMemory
            | Self::ProfileSessions
            | Self::RemoteNode { .. }
            | Self::Project { .. }
            | Self::ProjectSessions { .. } => true,
            Self::Code {
                scope: CodeShardScopeV1::Worktree { .. } | CodeShardScopeV1::Branch { .. },
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
///
/// Its profile, project, repository, and worktree components are the domain
/// types re-exported by this module; the store does not mint parallel IDs.
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

    pub fn profile_memory(brain_id: BrainId, profile_id: UserProfileId) -> Self {
        Self::new(brain_id, profile_id, StoreShardScopeV1::ProfileMemory)
    }

    pub fn profile_sessions(brain_id: BrainId, profile_id: UserProfileId) -> Self {
        Self::new(brain_id, profile_id, StoreShardScopeV1::ProfileSessions)
    }

    pub fn remote_node(brain_id: BrainId, profile_id: UserProfileId, node_id: BrainNodeId) -> Self {
        Self::new(
            brain_id,
            profile_id,
            StoreShardScopeV1::RemoteNode { node_id },
        )
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

/// Non-zero storage-runtime projection of the canonical writer authority epoch.
///
/// [`AuthorityEpoch`] is the domain authority and permits zero as an
/// uninitialized/default value. An active storage binding cannot. Conversion
/// into this type therefore validates, while conversion back is lossless.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(try_from = "u64", into = "u64")]
pub struct StoreAuthorityEpochV1(u64);

impl StoreAuthorityEpochV1 {
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

impl TryFrom<u64> for StoreAuthorityEpochV1 {
    type Error = StorageRuntimeContractErrorV1;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<StoreAuthorityEpochV1> for u64 {
    fn from(value: StoreAuthorityEpochV1) -> Self {
        value.0
    }
}

impl TryFrom<AuthorityEpoch> for StoreAuthorityEpochV1 {
    type Error = StorageRuntimeContractErrorV1;

    fn try_from(value: AuthorityEpoch) -> Result<Self, Self::Error> {
        Self::new(value.0)
    }
}

impl From<StoreAuthorityEpochV1> for AuthorityEpoch {
    fn from(value: StoreAuthorityEpochV1) -> Self {
        Self(value.0)
    }
}

/// Complete runtime identity for an active logical shard publication.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoreRuntimeBindingV1 {
    pub shard_id: StoreShardIdV1,
    pub incarnation: StoreIncarnationV1,
    pub authority_epoch: StoreAuthorityEpochV1,
}

impl StoreRuntimeBindingV1 {
    pub fn new(
        shard_id: StoreShardIdV1,
        incarnation: StoreIncarnationV1,
        authority_epoch: StoreAuthorityEpochV1,
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

/// Canonical storage-registry lease retained by a graph runtime.
///
/// The lease is the sole authority for the logical binding and physical graph
/// locator. Consumers cannot supply those fields independently, and dropping
/// the last retained lease releases only its exact authority epoch.
pub trait RetainedGraphStoreLeaseV1: Send + Sync + fmt::Debug {
    fn binding(&self) -> &StoreRuntimeBindingV1;
    fn verified_locator(&self) -> &VerifiedStoreLocatorV1;
    fn canonical_path(&self) -> &Path;
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

/// Binds one exact canonical or prospective physical path to a verified
/// runtime locator. Filesystem resolution remains daemon-owned; this pure
/// function is the sole digest authority shared by resolvers and consumers.
pub fn canonical_store_locator_digest(
    path: &Path,
) -> Result<LocatorDigest, StorageRuntimeContractErrorV1> {
    if !path.is_absolute() {
        return Err(StorageRuntimeContractErrorV1::NonCanonical {
            field: "store locator path",
        });
    }
    let path = path
        .to_str()
        .ok_or(StorageRuntimeContractErrorV1::NonCanonical {
            field: "store locator path",
        })?;
    let mut hasher = Sha256::new();
    hasher.update(LOCATOR_DIGEST_DOMAIN);
    hasher.update((path.len() as u64).to_be_bytes());
    hasher.update(path.as_bytes());
    LocatorDigest::new(format!("sha256:{}", hex::encode(hasher.finalize()))).map_err(|_| {
        StorageRuntimeContractErrorV1::NonCanonical {
            field: "store locator digest",
        }
    })
}

/// Derives the sole persistent Graph locator paired with one relational shard.
///
/// Both inputs have already been selected and canonicalized by daemon store
/// authority. This pure contract places one ordinary `.grafeo` database file
/// beside its relational store; it never creates or opens filesystem artifacts.
/// Grafeo owns the file and its documented transient WAL sidecar.
pub fn graph_store_locator_path(
    canonical_store_root: &Path,
    relational_store_path: &Path,
) -> Result<PathBuf, StorageRuntimeContractErrorV1> {
    if !canonical_store_root.is_absolute()
        || !relational_store_path.is_absolute()
        || !relational_store_path.starts_with(canonical_store_root)
    {
        return Err(StorageRuntimeContractErrorV1::NonCanonical {
            field: "graph store locator path",
        });
    }
    let filename = relational_store_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .ok_or(StorageRuntimeContractErrorV1::NonCanonical {
            field: "graph store locator path",
        })?;
    Ok(canonical_store_root.join(format!("{filename}.grafeo")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("canonical fixture identity")
    }

    #[test]
    fn profile_memory_has_a_distinct_mutable_wire_identity() {
        let shard = StoreShardIdV1::profile_memory(
            id::<BrainId>("brain.identity"),
            id::<UserProfileId>("profile.identity"),
        );

        assert!(shard.is_mutable());
        assert_eq!(shard.scope.project_id(), None);
        assert_ne!(
            shard,
            StoreShardIdV1::profile(
                id::<BrainId>("brain.identity"),
                id::<UserProfileId>("profile.identity"),
            )
        );

        let encoded = serde_json::to_value(&shard).expect("serialize profile-memory shard");
        assert_eq!(encoded["scope"]["kind"], "profile_memory");
        assert_eq!(
            serde_json::from_value::<StoreShardIdV1>(encoded).expect("deserialize shard"),
            shard
        );
    }

    #[test]
    fn remote_node_has_a_distinct_mutable_wire_identity() {
        let shard = StoreShardIdV1::remote_node(
            id::<BrainId>("brain.identity"),
            id::<UserProfileId>("profile.identity"),
            id::<BrainNodeId>("node.identity"),
        );

        assert!(shard.is_mutable());
        assert_eq!(shard.scope.project_id(), None);
        let encoded = serde_json::to_value(&shard).expect("serialize remote-node shard");
        assert_eq!(encoded["scope"]["kind"], "remote_node");
        assert_eq!(encoded["scope"]["node_id"], "node.identity");
        assert_eq!(
            serde_json::from_value::<StoreShardIdV1>(encoded).expect("deserialize shard"),
            shard
        );
    }

    #[test]
    fn canonical_locator_digest_binds_the_exact_absolute_path() {
        let first = canonical_store_locator_digest(Path::new("/stores/a/graph-store"))
            .expect("absolute locator");
        let second = canonical_store_locator_digest(Path::new("/stores/b/graph-store"))
            .expect("absolute locator");

        assert_ne!(first, second);
        assert!(canonical_store_locator_digest(Path::new("relative/graph-store")).is_err());
    }

    #[test]
    fn graph_locator_is_an_ordinary_database_file_and_shard_specific() {
        let root = Path::new("/stores/project-a");
        assert_eq!(
            graph_store_locator_path(root, &root.join("sessions.db"))
                .expect("canonical graph locator"),
            root.join("sessions.grafeo")
        );
        assert!(graph_store_locator_path(root, Path::new("/stores/project-b/project.db")).is_err());
    }

    #[test]
    fn branch_scope_is_mutable_and_does_not_alias_its_worktree() {
        let project_id = id::<ProjectId>("project.identity");
        let worktree_id = id::<WorktreeId>("worktree.identity");
        let branch = StoreShardIdV1::code(
            id::<BrainId>("brain.identity"),
            id::<UserProfileId>("profile.identity"),
            project_id.clone(),
            id::<RepositoryId>("repository.identity"),
            CodeShardScopeV1::Branch {
                worktree_id: worktree_id.clone(),
                ref_id: id::<RefId>("refs/heads/main"),
            },
        );
        let worktree = StoreShardIdV1::code(
            id::<BrainId>("brain.identity"),
            id::<UserProfileId>("profile.identity"),
            project_id.clone(),
            id::<RepositoryId>("repository.identity"),
            CodeShardScopeV1::Worktree { worktree_id },
        );

        assert!(branch.is_mutable());
        assert_eq!(branch.scope.project_id(), Some(&project_id));
        assert_ne!(branch, worktree);

        let encoded = serde_json::to_value(&branch).expect("serialize branch shard");
        assert_eq!(encoded["scope"]["scope"]["kind"], "branch");
        assert_eq!(encoded["scope"]["scope"]["ref_id"], "refs/heads/main");
        assert_eq!(
            serde_json::from_value::<StoreShardIdV1>(encoded).expect("deserialize shard"),
            branch
        );
    }
}
