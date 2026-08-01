//! Bounded, read-only repository provenance capture.
//!
//! This adapter deliberately exposes no generic Git command surface, object
//! traversal or worktree-status probing. It reads only bounded
//! repository/worktree/HEAD/ref/remote identity plus persisted index metadata
//! through `gix`; query owns status, diff, history, blame, and hunk intelligence.

#[cfg(test)]
use std::path::{Path, PathBuf};

#[cfg(test)]
use gix::bstr::ByteSlice;
#[cfg(test)]
use sha2::{Digest, Sha256};
#[cfg(test)]
use tracedecay_domain::{
    CommitId, EvidenceAvailabilityV1, PrivacyDomainBoundLocatorDigest, ProjectId, RefId,
    RepositoryDirtyStateV1, RepositoryEvidenceV1, RepositoryId, RepositoryProvenanceV1,
    RepositoryRemoteIdentityV1, TreeId, UtcMicros, WorktreeId,
};

#[cfg(not(test))]
pub(crate) use tracedecay_sessions::repository_provenance::RepositoryProvenanceAdmissionContext;

#[cfg(test)]
const MAX_REMOTE_IDENTITY_BYTES: usize = 8 * 1024;
#[cfg(test)]
const MAX_INDEX_FILE_BYTES: u64 = 16 * 1024 * 1024;
#[cfg(test)]
const MAX_INDEX_ENTRIES: usize = 250_000;
#[cfg(test)]
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
#[cfg(test)]
const PROJECT_PRIVACY_DOMAIN_SALT_NAMESPACE: &[u8] =
    b"tracedecay.repository-provenance.project-domain-salt.v1\0";

/// Owned, authoritative repository identity supplied by daemon admission.
///
/// The project identity comes from the sanitized observation scope, never
/// from this path-bearing context or mutable Git metadata.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct RepositoryProvenanceAdmissionContext {
    project_id: ProjectId,
    repository_id: RepositoryId,
    worktree_id: Option<WorktreeId>,
    /// A deterministic project-domain salt, not a secret or credential.
    privacy_domain_salt: [u8; 32],
}

#[cfg(test)]
impl RepositoryProvenanceAdmissionContext {
    /// Construct only from the daemon-authoritative project marker and typed
    /// project identity. The marker is an identity authority, never evidence.
    pub(crate) fn from_authoritative_project_marker(
        project_root: &Path,
        project_id: &ProjectId,
        marker: &crate::storage::RepositoryIdentityMarker,
    ) -> Option<Self> {
        let authoritative = tracedecay_sessions::repository_provenance::
            RepositoryProvenanceAdmissionContext::from_authoritative_project_marker(
                project_root,
                project_id,
                marker,
            )?;
        let (project_id, repository_id, worktree_id) = authoritative.admitted_identity()?;
        let privacy_domain_salt = derive_project_privacy_domain_salt(&project_id);
        Some(Self {
            project_id,
            repository_id,
            worktree_id: Some(worktree_id),
            privacy_domain_salt,
        })
    }

    pub(crate) fn admitted_identity(&self) -> Option<(ProjectId, RepositoryId, WorktreeId)> {
        Some((
            self.project_id.clone(),
            self.repository_id.clone(),
            self.worktree_id.clone()?,
        ))
    }
}

/// Authoritative identities and privacy material supplied by the admission boundary.
#[cfg(test)]
pub(crate) struct RepositoryProvenanceProbeRequest<'a> {
    project_root: &'a Path,
    repository_id: &'a RepositoryId,
    project_id: Option<&'a ProjectId>,
    worktree_id: Option<&'a WorktreeId>,
    expected_common_dir: Option<PathBuf>,
    privacy_domain_salt: &'a [u8; 32],
    captured_at: UtcMicros,
}

#[cfg(test)]
impl<'a> RepositoryProvenanceProbeRequest<'a> {
    pub(crate) fn new(
        project_root: &'a Path,
        repository_id: &'a RepositoryId,
        project_id: Option<&'a ProjectId>,
        worktree_id: Option<&'a WorktreeId>,
        privacy_domain_salt: &'a [u8; 32],
        captured_at: UtcMicros,
    ) -> Self {
        Self {
            project_root,
            repository_id,
            project_id,
            worktree_id,
            expected_common_dir: discover_canonical_common_dir(project_root),
            privacy_domain_salt,
            captured_at,
        }
    }
}

/// Fixed native-Git provenance probe. It never writes the index or object store.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct NativeRepositoryProvenanceProbe;

#[cfg(test)]
impl NativeRepositoryProvenanceProbe {
    pub(crate) fn capture(
        &self,
        request: &RepositoryProvenanceProbeRequest<'_>,
    ) -> EvidenceAvailabilityV1<RepositoryProvenanceV1> {
        // Admission has already resolved the exact checkout root. Opening that
        // root directly prevents a removed nested checkout from silently
        // walking up to, and capturing evidence from, an ambient repository.
        let Ok(repo) = gix::open(request.project_root) else {
            return EvidenceAvailabilityV1::Unavailable;
        };
        Self::capture_open_repository(&repo, request)
    }

    fn capture_open_repository(
        repo: &gix::Repository,
        request: &RepositoryProvenanceProbeRequest<'_>,
    ) -> EvidenceAvailabilityV1<RepositoryProvenanceV1> {
        let Some(workdir) = repo.workdir() else {
            return EvidenceAvailabilityV1::Unsupported;
        };

        let (canonical_root, root_is_partial) = canonical_path(workdir);
        if !canonical_root.is_absolute() {
            return EvidenceAvailabilityV1::Unavailable;
        }
        let (git_dir, git_dir_is_partial) = canonical_path(repo.git_dir());
        let (common_dir, common_dir_is_partial) = canonical_path(repo.common_dir());
        if request
            .expected_common_dir
            .as_ref()
            .is_some_and(|expected| expected != &common_dir)
        {
            return EvidenceAvailabilityV1::Unavailable;
        }
        let remote_identity = observe_remote_identity(repo, request.privacy_domain_salt);

        let Some(canonical_root_digest) = privacy_bound_digest(
            request.privacy_domain_salt,
            b"repository-canonical-root-v1",
            &[crate::os_str_bytes::native_os_str_bytes(
                canonical_root.as_os_str(),
            )],
        ) else {
            return EvidenceAvailabilityV1::Unavailable;
        };
        let path_frames = [
            crate::os_str_bytes::native_os_str_bytes(canonical_root.as_os_str()),
            crate::os_str_bytes::native_os_str_bytes(git_dir.as_os_str()),
            crate::os_str_bytes::native_os_str_bytes(common_dir.as_os_str()),
            remote_identity.path_frame,
        ];
        let Some(path_identity_digest) = privacy_bound_digest(
            request.privacy_domain_salt,
            b"repository-path-identity-v1",
            &path_frames,
        ) else {
            return EvidenceAvailabilityV1::Unavailable;
        };

        let head = observe_head(repo);
        let index = observe_index(repo);
        let Ok(evidence) = RepositoryEvidenceV1::new(
            head.attached_ref,
            head.commit,
            index.tree,
            EvidenceAvailabilityV1::Known(path_identity_digest),
            remote_identity.identity,
            index.dirty_state,
        ) else {
            return EvidenceAvailabilityV1::Unavailable;
        };
        let Ok(capture) = RepositoryProvenanceV1::new(
            request.repository_id.clone(),
            request.project_id.cloned(),
            request.worktree_id.cloned(),
            canonical_root_digest,
            evidence,
            request.captured_at,
        ) else {
            return EvidenceAvailabilityV1::Unavailable;
        };

        if root_is_partial || git_dir_is_partial || common_dir_is_partial {
            EvidenceAvailabilityV1::PartiallyReadable(capture)
        } else {
            EvidenceAvailabilityV1::Known(capture)
        }
    }
}

#[cfg(test)]
pub(crate) fn capture_repository_provenance(
    request: &RepositoryProvenanceProbeRequest<'_>,
) -> EvidenceAvailabilityV1<RepositoryProvenanceV1> {
    NativeRepositoryProvenanceProbe.capture(request)
}

#[cfg(test)]
#[derive(Debug)]
struct HeadObservation {
    attached_ref: EvidenceAvailabilityV1<RefId>,
    commit: EvidenceAvailabilityV1<CommitId>,
}

#[cfg(test)]
fn observe_head(repo: &gix::Repository) -> HeadObservation {
    let Ok(head) = repo.head() else {
        return HeadObservation {
            attached_ref: EvidenceAvailabilityV1::Unavailable,
            commit: EvidenceAvailabilityV1::Unavailable,
        };
    };
    let attached_ref = if head.is_detached() {
        EvidenceAvailabilityV1::Detached
    } else {
        head.referent_name()
            .and_then(|name| std::str::from_utf8(name.as_bstr()).ok())
            .and_then(|name| RefId::new(name.to_owned()).ok())
            .map_or(
                EvidenceAvailabilityV1::Unknown,
                EvidenceAvailabilityV1::Known,
            )
    };
    if head.is_unborn() {
        return HeadObservation {
            attached_ref,
            commit: EvidenceAvailabilityV1::Unborn,
        };
    }

    let commit_id = head
        .id()
        .and_then(|id| CommitId::new(id.to_hex().to_string()).ok())
        .map_or(
            EvidenceAvailabilityV1::Unknown,
            EvidenceAvailabilityV1::Known,
        );
    HeadObservation {
        attached_ref,
        commit: commit_id,
    }
}

#[cfg(test)]
fn canonical_path(path: &Path) -> (PathBuf, bool) {
    path.canonicalize()
        .map_or_else(|_| (path.to_path_buf(), true), |path| (path, false))
}

#[cfg(test)]
fn discover_canonical_common_dir(project_root: &Path) -> Option<PathBuf> {
    let repository = gix::discover(project_root).ok()?;
    let (common_dir, partial) = canonical_path(repository.common_dir());
    (!partial && common_dir.is_absolute()).then_some(common_dir)
}

#[cfg(test)]
struct RemoteIdentityObservation {
    identity: RepositoryRemoteIdentityV1,
    path_frame: Vec<u8>,
}

#[cfg(test)]
fn observe_remote_identity(
    repo: &gix::Repository,
    privacy_domain_salt: &[u8; 32],
) -> RemoteIdentityObservation {
    let Some(remote) = repo.config_snapshot().string("remote.origin.url") else {
        return remote_identity_observation(RepositoryRemoteIdentityV1::Missing);
    };
    if remote.len() > MAX_REMOTE_IDENTITY_BYTES {
        return remote_identity_observation(RepositoryRemoteIdentityV1::Oversized);
    }
    let Ok(remote) = remote.to_str() else {
        return remote_identity_observation(RepositoryRemoteIdentityV1::Invalid);
    };
    let Some(normalized) = normalize_remote_without_credentials(remote) else {
        return remote_identity_observation(RepositoryRemoteIdentityV1::Invalid);
    };
    if normalized.len() > MAX_REMOTE_IDENTITY_BYTES {
        return remote_identity_observation(RepositoryRemoteIdentityV1::Oversized);
    }
    let Some(digest) = privacy_bound_digest(
        privacy_domain_salt,
        b"repository-remote-identity-v1",
        &[normalized.into_bytes()],
    ) else {
        return remote_identity_observation(RepositoryRemoteIdentityV1::Unavailable);
    };
    remote_identity_observation(RepositoryRemoteIdentityV1::Known(digest))
}

#[cfg(test)]
fn remote_identity_observation(identity: RepositoryRemoteIdentityV1) -> RemoteIdentityObservation {
    let path_frame = match &identity {
        RepositoryRemoteIdentityV1::Known(digest) => {
            let mut frame = b"known\0".to_vec();
            frame.extend_from_slice(digest.as_str().as_bytes());
            frame
        }
        RepositoryRemoteIdentityV1::Missing => b"missing\0".to_vec(),
        RepositoryRemoteIdentityV1::Invalid => b"invalid\0".to_vec(),
        RepositoryRemoteIdentityV1::Oversized => b"oversized\0".to_vec(),
        RepositoryRemoteIdentityV1::Unavailable => b"unavailable\0".to_vec(),
        RepositoryRemoteIdentityV1::Unknown => b"unknown\0".to_vec(),
    };
    RemoteIdentityObservation {
        identity,
        path_frame,
    }
}

#[cfg(test)]
struct IndexObservation {
    tree: EvidenceAvailabilityV1<TreeId>,
    dirty_state: EvidenceAvailabilityV1<RepositoryDirtyStateV1>,
}

#[cfg(test)]
fn observe_index(repo: &gix::Repository) -> IndexObservation {
    let index_path = repo.index_path();
    let metadata = match std::fs::metadata(index_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return IndexObservation {
                tree: EvidenceAvailabilityV1::Missing,
                dirty_state: EvidenceAvailabilityV1::Missing,
            };
        }
        Err(_) => return unavailable_index_observation(),
    };
    if metadata.len() > MAX_INDEX_FILE_BYTES {
        return unavailable_index_observation();
    }
    let Ok(index) = repo.open_index() else {
        return unavailable_index_observation();
    };
    if index.entries().len() > MAX_INDEX_ENTRIES {
        return unavailable_index_observation();
    }
    let tree = index
        .tree()
        .filter(|tree| tree.num_entries.is_some())
        .and_then(|tree| TreeId::new(tree.id.to_hex().to_string()).ok())
        .map_or(
            EvidenceAvailabilityV1::Unknown,
            EvidenceAvailabilityV1::Known,
        );
    let dirty_state = if index
        .entries()
        .iter()
        .any(|entry| entry.stage() != gix::index::entry::Stage::Unconflicted)
    {
        EvidenceAvailabilityV1::Known(RepositoryDirtyStateV1::Conflicted)
    } else if matches!(
        (&tree, head_tree_id(repo)),
        (EvidenceAvailabilityV1::Known(index_tree), Some(head_tree)) if index_tree != &head_tree
    ) {
        // A differing persisted index proves staged dirtiness. Equality cannot
        // prove cleanliness without a worktree traversal, which belongs to QUERY.
        EvidenceAvailabilityV1::Known(RepositoryDirtyStateV1::Dirty)
    } else {
        EvidenceAvailabilityV1::Unknown
    };
    IndexObservation { tree, dirty_state }
}

#[cfg(test)]
fn unavailable_index_observation() -> IndexObservation {
    IndexObservation {
        tree: EvidenceAvailabilityV1::Unavailable,
        dirty_state: EvidenceAvailabilityV1::Unavailable,
    }
}

#[cfg(test)]
fn head_tree_id(repo: &gix::Repository) -> Option<TreeId> {
    let tree = repo.head_commit().ok()?.tree_id().ok()?;
    TreeId::new(tree.to_hex().to_string()).ok()
}

#[cfg(test)]
fn normalize_remote_without_credentials(remote: &str) -> Option<String> {
    let remote = remote.trim();
    if remote.is_empty() {
        return None;
    }
    if let Ok(mut url) = url::Url::parse(remote) {
        url.set_username("").ok()?;
        url.set_password(None).ok()?;
        url.set_query(None);
        url.set_fragment(None);
        let path = url.path().trim_end_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path).to_owned();
        url.set_path(&path);
        return Some(url.to_string().trim_end_matches('/').to_owned());
    }
    if let Some((authority, path)) = remote.split_once(':')
        && !authority.contains(['/', '\\'])
        && !path.is_empty()
        && !(authority.len() == 1 && authority.as_bytes()[0].is_ascii_alphabetic())
    {
        let host = authority.rsplit('@').next()?.trim();
        let path = path
            .split(['?', '#'])
            .next()?
            .trim_matches('/')
            .trim_end_matches(".git");
        if host.is_empty() || path.is_empty() {
            return None;
        }
        return Some(format!("ssh://{}/{path}", host.to_ascii_lowercase()));
    }
    Some(format!("local:{remote}"))
}

#[cfg(test)]
fn privacy_bound_digest(
    privacy_domain_salt: &[u8; 32],
    domain: &[u8],
    frames: &[Vec<u8>],
) -> Option<PrivacyDomainBoundLocatorDigest> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay-privacy-bound-locator-v1\0");
    hash_frame(&mut hasher, privacy_domain_salt);
    hash_frame(&mut hasher, domain);
    for frame in frames {
        hash_frame(&mut hasher, frame);
    }
    PrivacyDomainBoundLocatorDigest::new(format!("sha256:{}", hex_digest(hasher.finalize()))).ok()
}

#[cfg(test)]
fn derive_project_privacy_domain_salt(project_id: &ProjectId) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(PROJECT_PRIVACY_DOMAIN_SALT_NAMESPACE);
    hash_frame(&mut hasher, project_id.as_str().as_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for &byte in bytes {
        encoded.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
fn hash_frame(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
#[path = "repository_provenance_test.rs"]
mod repository_provenance_test;
