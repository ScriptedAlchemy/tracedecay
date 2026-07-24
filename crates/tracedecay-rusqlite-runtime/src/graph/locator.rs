use std::{
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_json_bytes;
use tracedecay_store::{
    CodeShardScopeV1, LocatorDigest, StoreRuntimeBindingV1, StoreShardIdV1, StoreShardScopeV1,
    VerifiedStoreLocatorV1,
};

pub const GRAPH_DATABASE_FILENAME: &str = "graph.db";

const COMPONENT_DIGEST_DOMAIN: &[u8] = b"tracedecay.code-shard.component.v1\0";
const LOCATOR_DIGEST_DOMAIN: &[u8] = b"tracedecay.code-shard.locator.v1\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeShardAccessV1 {
    MutableWorktree,
    ImmutableSnapshot,
}

#[derive(Clone, Debug)]
pub struct CodeShardPhysicalLocator {
    binding: StoreRuntimeBindingV1,
    verified: VerifiedStoreLocatorV1,
    canonical_path: PathBuf,
    access: CodeShardAccessV1,
}

impl CodeShardPhysicalLocator {
    /// Adapts a daemon-verified existing locator without deriving identity from
    /// its path. The caller remains the locator authority; this constructor
    /// only checks binding parity, immutable/mutable scope, and file safety.
    pub fn from_verified_existing(
        binding: StoreRuntimeBindingV1,
        verified: VerifiedStoreLocatorV1,
        path: PathBuf,
    ) -> Result<Self, CodeShardLocatorError> {
        if verified.shard_id != binding.shard_id || verified.incarnation != binding.incarnation {
            return Err(CodeShardLocatorError::LocatorBindingMismatch);
        }
        if !path.is_absolute() {
            return Err(CodeShardLocatorError::DatabasePathIsNotAbsolute(path));
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| CodeShardLocatorError::DatabaseUnavailable(path.clone()))?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(CodeShardLocatorError::UnsafeDatabase(path));
        }
        let canonical_path = fs::canonicalize(&path)
            .map_err(|_| CodeShardLocatorError::DatabaseUnavailable(path.clone()))?;
        if canonical_path != path {
            return Err(CodeShardLocatorError::UnsafeDatabase(path));
        }
        let access = code_shard_access(&binding.shard_id)?;
        Ok(Self {
            binding,
            verified,
            canonical_path,
            access,
        })
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    pub fn verified(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified
    }

    pub fn path(&self) -> &Path {
        &self.canonical_path
    }

    pub const fn access(&self) -> CodeShardAccessV1 {
        self.access
    }

    pub const fn is_mutable(&self) -> bool {
        matches!(self.access, CodeShardAccessV1::MutableWorktree)
    }
}

/// Resolves physical code-shard paths exclusively from canonical typed IDs.
///
/// `root` is a configured storage boundary. It never selects a project,
/// repository, worktree, or snapshot; all selection comes from the supplied
/// [`StoreShardIdV1`]. IDs are hashed into safe path components because
/// repository and worktree IDs may themselves contain path-shaped values.
#[derive(Clone, Debug)]
pub struct CodeShardPhysicalLocatorFactory {
    canonical_root: PathBuf,
}

impl CodeShardPhysicalLocatorFactory {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, CodeShardLocatorError> {
        let root = root.as_ref();
        let metadata = fs::symlink_metadata(root)
            .map_err(|_| CodeShardLocatorError::RootUnavailable(root.to_path_buf()))?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
            return Err(CodeShardLocatorError::UnsafeRoot(root.to_path_buf()));
        }
        let canonical_root = fs::canonicalize(root)
            .map_err(|_| CodeShardLocatorError::RootUnavailable(root.to_path_buf()))?;
        Ok(Self { canonical_root })
    }

    pub fn root(&self) -> &Path {
        &self.canonical_root
    }

    /// Returns the deterministic prospective path without creating anything.
    pub fn prospective_path(
        &self,
        shard_id: &StoreShardIdV1,
    ) -> Result<PathBuf, CodeShardLocatorError> {
        let StoreShardScopeV1::Code {
            project_id,
            repository_id,
            scope,
        } = &shard_id.scope
        else {
            return Err(CodeShardLocatorError::NotCodeShard);
        };

        let base = self
            .canonical_root
            .join("code")
            .join(component("brain", shard_id.brain_id.as_str()))
            .join(component("profile", shard_id.profile_id.as_str()))
            .join(component("project", project_id.as_str()))
            .join(component("repository", repository_id.as_str()));
        let path = match scope {
            CodeShardScopeV1::Worktree { worktree_id } => base
                .join("worktrees")
                .join(component("worktree", worktree_id.as_str())),
            CodeShardScopeV1::Branch {
                worktree_id,
                ref_id,
            } => base
                .join("worktrees")
                .join(component("worktree", worktree_id.as_str()))
                .join("refs")
                .join(component("ref", ref_id.as_str())),
            CodeShardScopeV1::Snapshot {
                worktree_id,
                snapshot_id,
            } => {
                let worktree = worktree_id.as_ref().map(|id| id.as_str()).unwrap_or("");
                base.join("snapshots")
                    .join(component_many("snapshot", [worktree, snapshot_id.as_str()]))
            }
        };
        Ok(path.join(GRAPH_DATABASE_FILENAME))
    }

    /// Verifies an already-created graph database without opening it.
    pub fn resolve_existing(
        &self,
        binding: &StoreRuntimeBindingV1,
    ) -> Result<CodeShardPhysicalLocator, CodeShardLocatorError> {
        let path = self.prospective_path(&binding.shard_id)?;
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| CodeShardLocatorError::DatabaseUnavailable(path.clone()))?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(CodeShardLocatorError::UnsafeDatabase(path));
        }
        let canonical_path = fs::canonicalize(&path)
            .map_err(|_| CodeShardLocatorError::DatabaseUnavailable(path.clone()))?;
        if canonical_path != path || !canonical_path.starts_with(&self.canonical_root) {
            return Err(CodeShardLocatorError::UnsafeDatabase(path));
        }

        let digest = locator_digest(&binding.shard_id, &canonical_path)?;
        CodeShardPhysicalLocator::from_verified_existing(
            binding.clone(),
            VerifiedStoreLocatorV1::new(binding.shard_id.clone(), binding.incarnation, digest),
            canonical_path,
        )
    }
}

fn code_shard_access(
    shard_id: &StoreShardIdV1,
) -> Result<CodeShardAccessV1, CodeShardLocatorError> {
    match &shard_id.scope {
        StoreShardScopeV1::Code {
            scope: CodeShardScopeV1::Worktree { .. } | CodeShardScopeV1::Branch { .. },
            ..
        } => Ok(CodeShardAccessV1::MutableWorktree),
        StoreShardScopeV1::Code {
            scope: CodeShardScopeV1::Snapshot { .. },
            ..
        } => Ok(CodeShardAccessV1::ImmutableSnapshot),
        _ => Err(CodeShardLocatorError::NotCodeShard),
    }
}

fn component(kind: &str, value: &str) -> String {
    component_many(kind, [value])
}

fn component_many<'a>(kind: &str, values: impl IntoIterator<Item = &'a str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(COMPONENT_DIGEST_DOMAIN);
    update_length_prefixed(&mut hasher, kind.as_bytes());
    for value in values {
        update_length_prefixed(&mut hasher, value.as_bytes());
    }
    format!("{kind}-{}", hex_lower(&hasher.finalize()))
}

fn locator_digest(
    shard_id: &StoreShardIdV1,
    canonical_path: &Path,
) -> Result<LocatorDigest, CodeShardLocatorError> {
    let shard_json = canonical_json_bytes(shard_id)
        .map_err(|_| CodeShardLocatorError::IdentityEncodingUnavailable)?;
    let path = canonical_path
        .to_str()
        .ok_or(CodeShardLocatorError::PathEncodingUnavailable)?;
    let mut hasher = Sha256::new();
    hasher.update(LOCATOR_DIGEST_DOMAIN);
    update_length_prefixed(&mut hasher, &shard_json);
    update_length_prefixed(&mut hasher, path.as_bytes());
    LocatorDigest::new(format!("sha256:{}", hex_lower(&hasher.finalize())))
        .map_err(|_| CodeShardLocatorError::IdentityEncodingUnavailable)
}

fn update_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeShardLocatorError {
    RootUnavailable(PathBuf),
    UnsafeRoot(PathBuf),
    NotCodeShard,
    LocatorBindingMismatch,
    DatabasePathIsNotAbsolute(PathBuf),
    DatabaseUnavailable(PathBuf),
    UnsafeDatabase(PathBuf),
    IdentityEncodingUnavailable,
    PathEncodingUnavailable,
}

impl fmt::Display for CodeShardLocatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RootUnavailable(path) => {
                write!(
                    formatter,
                    "code-shard storage root is unavailable: {}",
                    path.display()
                )
            }
            Self::UnsafeRoot(path) => {
                write!(
                    formatter,
                    "code-shard storage root is unsafe: {}",
                    path.display()
                )
            }
            Self::NotCodeShard => formatter.write_str("canonical shard is not a code shard"),
            Self::LocatorBindingMismatch => {
                formatter.write_str("verified code-shard locator does not match its binding")
            }
            Self::DatabasePathIsNotAbsolute(path) => {
                write!(
                    formatter,
                    "code-shard database path is not absolute: {}",
                    path.display()
                )
            }
            Self::DatabaseUnavailable(path) => {
                write!(
                    formatter,
                    "code-shard database is unavailable: {}",
                    path.display()
                )
            }
            Self::UnsafeDatabase(path) => {
                write!(
                    formatter,
                    "code-shard database path is unsafe: {}",
                    path.display()
                )
            }
            Self::IdentityEncodingUnavailable => {
                formatter.write_str("canonical code-shard identity could not be encoded")
            }
            Self::PathEncodingUnavailable => {
                formatter.write_str("canonical code-shard path is not valid UTF-8")
            }
        }
    }
}

impl Error for CodeShardLocatorError {}
