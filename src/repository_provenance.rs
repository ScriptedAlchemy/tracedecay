//! Bounded, read-only repository provenance capture.
//!
//! This adapter deliberately exposes no generic Git command surface, object
//! traversal or worktree-status probing. It reads only bounded
//! repository/worktree/HEAD/ref/remote identity plus persisted index metadata
//! through `gix`; PR9 owns status, diff, history, blame, and hunk intelligence.

use std::path::{Path, PathBuf};

use gix::bstr::ByteSlice;
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    AnchorDurabilityClass, AnchorSourceGenerationV2, CommitId, CoverageReportV1,
    DurableObservationV1, EvidenceAvailabilityV1, EvidenceClass,
    GenerationBoundRepositoryProvenanceV1, PayloadAccessState, PrivacyDomainBoundLocatorDigest,
    ProjectId, ProjectionGenerationId, RefId, RepositoryDirtyStateV1, RepositoryEvidenceV1,
    RepositoryId, RepositoryProvenanceV1, RepositoryRemoteIdentityV1, ResolutionAuthorizationV1,
    RetrievalAnchorRecordV2, RetrievalAnchorRecordV2Parts, RetrievalAnchorTargetV2, TreeId,
    UtcMicros, VectorWatermark, WorktreeId,
};

const MAX_REMOTE_IDENTITY_BYTES: usize = 8 * 1024;
const MAX_INDEX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_INDEX_ENTRIES: usize = 250_000;
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
const PROJECT_PRIVACY_DOMAIN_SALT_NAMESPACE: &[u8] =
    b"tracedecay.repository-provenance.project-domain-salt.v1\0";
const REPOSITORY_ADMISSION_ID_NAMESPACE: &[u8] =
    b"tracedecay.repository-provenance.repository-id.v1\0";
const WORKTREE_ADMISSION_ID_NAMESPACE: &[u8] = b"tracedecay.repository-provenance.worktree-id.v1\0";

/// Owned, authoritative repository identity supplied by daemon admission.
///
/// The project identity comes from the sanitized observation scope, never
/// from this path-bearing context or mutable Git metadata.
#[derive(Clone)]
pub(crate) struct RepositoryProvenanceAdmissionContext {
    project_root: PathBuf,
    project_id: ProjectId,
    repository_id: RepositoryId,
    worktree_id: Option<WorktreeId>,
    /// A deterministic project-domain salt, not a secret or credential.
    privacy_domain_salt: [u8; 32],
}

impl RepositoryProvenanceAdmissionContext {
    pub(crate) fn new(
        project_root: PathBuf,
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: Option<WorktreeId>,
        privacy_domain_salt: [u8; 32],
    ) -> Self {
        Self {
            project_root,
            project_id,
            repository_id,
            worktree_id,
            privacy_domain_salt,
        }
    }

    /// Construct only from the daemon-authoritative project marker and typed
    /// project identity. The marker is an identity authority, never evidence.
    pub(crate) fn from_authoritative_project_marker(
        project_root: &Path,
        project_id: &ProjectId,
        marker: &crate::storage::RepositoryIdentityMarker,
    ) -> Option<Self> {
        if marker.schema_version != crate::storage::REPOSITORY_IDENTITY_SCHEMA_VERSION
            || marker.project_id != project_id.as_str()
        {
            return None;
        }
        let common_dir = Path::new(&marker.git_common_dir);
        if !common_dir.is_absolute() {
            return None;
        }
        let (canonical_root, root_is_partial) = canonical_path(project_root);
        let (canonical_common_dir, common_dir_is_partial) = canonical_path(common_dir);
        if root_is_partial
            || common_dir_is_partial
            || !canonical_root.is_absolute()
            || !canonical_common_dir.is_absolute()
        {
            return None;
        }

        let privacy_domain_salt = derive_project_privacy_domain_salt(project_id);
        let repository_id = RepositoryId::new(format!(
            "repository.{}",
            opaque_admission_identifier(
                &privacy_domain_salt,
                REPOSITORY_ADMISSION_ID_NAMESPACE,
                &[crate::os_str_bytes::native_os_str_bytes(
                    canonical_common_dir.as_os_str(),
                )],
            ),
        ))
        .ok()?;
        let worktree_id = WorktreeId::new(format!(
            "worktree.{}",
            opaque_admission_identifier(
                &privacy_domain_salt,
                WORKTREE_ADMISSION_ID_NAMESPACE,
                &[crate::os_str_bytes::native_os_str_bytes(
                    canonical_root.as_os_str(),
                )],
            ),
        ))
        .ok()?;
        Some(Self::new(
            canonical_root,
            project_id.clone(),
            repository_id,
            Some(worktree_id),
            privacy_domain_salt,
        ))
    }

    /// Capture only after the observation has crossed the privacy boundary.
    pub(crate) fn capture_after_sanitization(
        &self,
        observation: &DurableObservationV1,
        projection_generation: &ProjectionGenerationId,
        ingested_at: UtcMicros,
        authorization: ResolutionAuthorizationV1,
    ) -> PreparedRepositoryProvenanceV1 {
        let ObservationProjectId::Known(observation_project_id) =
            ObservationProjectId::from_observation(observation)
        else {
            return PreparedRepositoryProvenanceV1::unavailable();
        };
        if observation_project_id != &self.project_id {
            return PreparedRepositoryProvenanceV1::unavailable();
        }
        let captured = capture_repository_provenance(&RepositoryProvenanceProbeRequest::new(
            &self.project_root,
            &self.repository_id,
            Some(&self.project_id),
            self.worktree_id.as_ref(),
            &self.privacy_domain_salt,
            ingested_at,
        ));
        prepare_generation_binding(
            captured,
            observation,
            projection_generation,
            ingested_at,
            authorization,
        )
    }
}

enum ObservationProjectId<'a> {
    Known(&'a ProjectId),
    Unavailable,
}

impl<'a> ObservationProjectId<'a> {
    fn from_observation(observation: &'a DurableObservationV1) -> Self {
        match observation.scope() {
            tracedecay_domain::ObservationScopeV1::Project { project_id } => {
                Self::Known(project_id)
            }
            tracedecay_domain::ObservationScopeV1::Profile => Self::Unavailable,
        }
    }
}

/// Atomic-writer attachment prepared at the post-sanitization boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedRepositoryProvenanceV1 {
    availability: EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1>,
    anchor: Option<RetrievalAnchorRecordV2>,
}

impl PreparedRepositoryProvenanceV1 {
    pub(crate) const fn unavailable() -> Self {
        Self {
            availability: EvidenceAvailabilityV1::Unavailable,
            anchor: None,
        }
    }

    pub(crate) fn availability(
        &self,
    ) -> &EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1> {
        &self.availability
    }

    pub(crate) fn anchor(&self) -> Option<&RetrievalAnchorRecordV2> {
        self.anchor.as_ref()
    }
}

/// Authoritative identities and privacy material supplied by the admission boundary.
pub(crate) struct RepositoryProvenanceProbeRequest<'a> {
    project_root: &'a Path,
    repository_id: &'a RepositoryId,
    project_id: Option<&'a ProjectId>,
    worktree_id: Option<&'a WorktreeId>,
    privacy_domain_salt: &'a [u8; 32],
    captured_at: UtcMicros,
}

impl<'a> RepositoryProvenanceProbeRequest<'a> {
    pub(crate) const fn new(
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
            privacy_domain_salt,
            captured_at,
        }
    }
}

/// Fixed native-Git provenance probe. It never writes the index or object store.
#[derive(Default)]
pub(crate) struct NativeRepositoryProvenanceProbe;

impl NativeRepositoryProvenanceProbe {
    pub(crate) fn capture(
        &self,
        request: &RepositoryProvenanceProbeRequest<'_>,
    ) -> EvidenceAvailabilityV1<RepositoryProvenanceV1> {
        let Ok(repo) = gix::discover(request.project_root) else {
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

pub(crate) fn capture_repository_provenance(
    request: &RepositoryProvenanceProbeRequest<'_>,
) -> EvidenceAvailabilityV1<RepositoryProvenanceV1> {
    NativeRepositoryProvenanceProbe.capture(request)
}

fn prepare_generation_binding(
    captured: EvidenceAvailabilityV1<RepositoryProvenanceV1>,
    observation: &DurableObservationV1,
    projection_generation: &ProjectionGenerationId,
    ingested_at: UtcMicros,
    authorization: ResolutionAuthorizationV1,
) -> PreparedRepositoryProvenanceV1 {
    let availability = match captured {
        EvidenceAvailabilityV1::Known(capture) => {
            bind_capture(capture, observation, projection_generation, false)
        }
        EvidenceAvailabilityV1::PartiallyReadable(capture) => {
            bind_capture(capture, observation, projection_generation, true)
        }
        EvidenceAvailabilityV1::Missing => EvidenceAvailabilityV1::Missing,
        EvidenceAvailabilityV1::Unborn => EvidenceAvailabilityV1::Unborn,
        EvidenceAvailabilityV1::Detached => EvidenceAvailabilityV1::Detached,
        EvidenceAvailabilityV1::Conflicted => EvidenceAvailabilityV1::Conflicted,
        EvidenceAvailabilityV1::Unsupported => EvidenceAvailabilityV1::Unsupported,
        EvidenceAvailabilityV1::Unavailable => EvidenceAvailabilityV1::Unavailable,
        EvidenceAvailabilityV1::Unknown => EvidenceAvailabilityV1::Unknown,
    };
    let Some(binding) = availability.value() else {
        return PreparedRepositoryProvenanceV1 {
            availability,
            anchor: None,
        };
    };
    let capture = binding.capture();
    let target = RetrievalAnchorTargetV2::RepositoryCapture {
        repository_id: capture.repository_id().clone(),
        capture_id: binding.capture_id().clone(),
        receipt: observation.receipt().receipt().clone(),
    };
    let anchor = RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target,
        owner: observation.scope().clone(),
        aliases: vec![],
        occurred_at: None,
        ingested_at,
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGenerationV2::RepositoryCapture(
            binding.capture_id().clone(),
        ),
        projection_generation: projection_generation.clone(),
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations: vec![observation.observation_id().clone()],
        source_anchors: vec![],
        authorization,
        payload_access: PayloadAccessState::Eligible,
        retention_class: observation.retention_class().clone(),
        durability: AnchorDurabilityClass::DurableEvidence,
    });
    match anchor {
        Ok(anchor) => PreparedRepositoryProvenanceV1 {
            availability,
            anchor: Some(anchor),
        },
        Err(_) => PreparedRepositoryProvenanceV1::unavailable(),
    }
}

fn bind_capture(
    capture: RepositoryProvenanceV1,
    observation: &DurableObservationV1,
    projection_generation: &ProjectionGenerationId,
    partially_readable: bool,
) -> EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1> {
    let Ok(binding) = GenerationBoundRepositoryProvenanceV1::new(
        projection_generation.clone(),
        capture,
        Some(observation.observation_id().clone()),
    ) else {
        return EvidenceAvailabilityV1::Unavailable;
    };
    if partially_readable {
        EvidenceAvailabilityV1::PartiallyReadable(binding)
    } else {
        EvidenceAvailabilityV1::Known(binding)
    }
}

#[derive(Debug)]
struct HeadObservation {
    attached_ref: EvidenceAvailabilityV1<RefId>,
    commit: EvidenceAvailabilityV1<CommitId>,
}

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

fn canonical_path(path: &Path) -> (PathBuf, bool) {
    path.canonicalize()
        .map_or_else(|_| (path.to_path_buf(), true), |path| (path, false))
}

struct RemoteIdentityObservation {
    identity: RepositoryRemoteIdentityV1,
    path_frame: Vec<u8>,
}

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

struct IndexObservation {
    tree: EvidenceAvailabilityV1<TreeId>,
    dirty_state: EvidenceAvailabilityV1<RepositoryDirtyStateV1>,
}

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
        // prove cleanliness without a worktree traversal, which belongs to PR9.
        EvidenceAvailabilityV1::Known(RepositoryDirtyStateV1::Dirty)
    } else {
        EvidenceAvailabilityV1::Unknown
    };
    IndexObservation { tree, dirty_state }
}

fn unavailable_index_observation() -> IndexObservation {
    IndexObservation {
        tree: EvidenceAvailabilityV1::Unavailable,
        dirty_state: EvidenceAvailabilityV1::Unavailable,
    }
}

fn head_tree_id(repo: &gix::Repository) -> Option<TreeId> {
    let tree = repo.head_commit().ok()?.tree_id().ok()?;
    TreeId::new(tree.to_hex().to_string()).ok()
}

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

fn derive_project_privacy_domain_salt(project_id: &ProjectId) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(PROJECT_PRIVACY_DOMAIN_SALT_NAMESPACE);
    hash_frame(&mut hasher, project_id.as_str().as_bytes());
    hasher.finalize().into()
}

fn opaque_admission_identifier(
    privacy_domain_salt: &[u8; 32],
    namespace: &[u8],
    frames: &[Vec<u8>],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(namespace);
    hash_frame(&mut hasher, privacy_domain_salt);
    for frame in frames {
        hash_frame(&mut hasher, frame);
    }
    hex_digest(hasher.finalize())
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for &byte in bytes {
        encoded.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn hash_frame(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::process::{Command, Output};

    use tempfile::TempDir;

    use super::*;

    const PRIVACY_DOMAIN_SALT: [u8; 32] = [0x5a; 32];

    struct GitFixture {
        root: TempDir,
    }

    impl GitFixture {
        fn new() -> Self {
            let root = TempDir::new().unwrap();
            let fixture = Self { root };
            fixture.git(&["init", "-q", "-b", "main"]);
            fixture.git(&["config", "user.name", "TraceDecay Test"]);
            fixture.git(&["config", "user.email", "tracedecay@example.invalid"]);
            fixture
        }

        fn path(&self) -> &Path {
            self.root.path()
        }

        fn git(&self, args: &[&str]) -> Output {
            let output = Command::new(crate::git::git_program())
                .args(args)
                .current_dir(self.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output
        }

        fn commit(&self, contents: &str) {
            fs::write(self.path().join("tracked.txt"), contents).unwrap();
            self.git(&["add", "--", "tracked.txt"]);
            self.git(&["commit", "-q", "-m", contents]);
        }

        fn capture_with(
            &self,
            probe: &NativeRepositoryProvenanceProbe,
        ) -> EvidenceAvailabilityV1<RepositoryProvenanceV1> {
            let repository_id = RepositoryId::new("repository.fixture").unwrap();
            let project_id = ProjectId::new("project.fixture").unwrap();
            let worktree_id = WorktreeId::new("worktree.fixture").unwrap();
            probe.capture(&RepositoryProvenanceProbeRequest::new(
                self.path(),
                &repository_id,
                Some(&project_id),
                Some(&worktree_id),
                &PRIVACY_DOMAIN_SALT,
                UtcMicros(123),
            ))
        }

        fn capture(&self) -> RepositoryProvenanceV1 {
            match self.capture_with(&NativeRepositoryProvenanceProbe) {
                EvidenceAvailabilityV1::Known(capture) => capture,
                other => panic!("expected known capture, got {other:?}"),
            }
        }
    }

    #[test]
    fn identity_capture_keeps_head_ref_and_private_locator_evidence() {
        let fixture = GitFixture::new();
        fixture.commit("initial");
        fixture.git(&[
            "remote",
            "add",
            "origin",
            "https://alice:top-secret@example.com/Owner/Repo.git?token=hidden",
        ]);
        fixture.git(&["write-tree"]);

        let capture = fixture.capture();
        assert!(matches!(
            capture.evidence().attached_ref(),
            EvidenceAvailabilityV1::Known(_)
        ));
        assert!(matches!(
            capture.evidence().head_commit(),
            EvidenceAvailabilityV1::Known(_)
        ));
        assert!(matches!(
            capture.evidence().index_tree(),
            EvidenceAvailabilityV1::Known(_)
        ));
        assert!(matches!(
            capture.evidence().dirty_state(),
            EvidenceAvailabilityV1::Unknown
        ));
        assert!(matches!(
            capture.evidence().remote_identity(),
            RepositoryRemoteIdentityV1::Known(_)
        ));
        let encoded = serde_json::to_string(&capture).unwrap();
        assert!(!encoded.contains("alice"));
        assert!(!encoded.contains("top-secret"));
        assert!(!encoded.contains("token=hidden"));
        assert!(!encoded.contains(fixture.path().to_string_lossy().as_ref()));

        fixture.git(&[
            "remote",
            "set-url",
            "origin",
            "https://bob:different-secret@example.com/Owner/Repo.git?token=changed",
        ]);
        let recaptured = fixture.capture();
        assert_eq!(
            recaptured.evidence().path_identity_digest(),
            capture.evidence().path_identity_digest()
        );
        assert_eq!(recaptured.capture_id(), capture.capture_id());
    }

    #[test]
    fn unborn_and_detached_head_states_are_not_guessed() {
        let fixture = GitFixture::new();
        let unborn = fixture.capture();
        assert!(matches!(
            unborn.evidence().head_commit(),
            EvidenceAvailabilityV1::Unborn
        ));
        assert!(matches!(
            unborn.evidence().attached_ref(),
            EvidenceAvailabilityV1::Known(_)
        ));

        fixture.commit("born");
        fixture.git(&["checkout", "-q", "--detach", "HEAD"]);
        let detached = fixture.capture();
        assert!(matches!(
            detached.evidence().attached_ref(),
            EvidenceAvailabilityV1::Detached
        ));
        assert!(matches!(
            detached.evidence().head_commit(),
            EvidenceAvailabilityV1::Known(_)
        ));
    }

    #[test]
    fn conflicted_index_is_explicit_without_a_status_probe() {
        let fixture = GitFixture::new();
        fixture.commit("base");
        fixture.git(&["checkout", "-q", "-b", "side"]);
        fixture.commit("side");
        fixture.git(&["checkout", "-q", "main"]);
        fixture.commit("main");
        let merge = Command::new(crate::git::git_program())
            .args(["merge", "--no-edit", "side"])
            .current_dir(fixture.path())
            .output()
            .unwrap();
        assert!(!merge.status.success());

        let capture = fixture.capture();
        assert!(matches!(
            capture.evidence().dirty_state(),
            EvidenceAvailabilityV1::Known(RepositoryDirtyStateV1::Conflicted)
        ));
    }

    #[test]
    fn remote_availability_never_collapses_missing_invalid_and_oversized() {
        let fixture = GitFixture::new();
        fixture.commit("base");
        let missing = fixture.capture();
        assert_eq!(
            missing.evidence().remote_identity(),
            &RepositoryRemoteIdentityV1::Missing
        );

        fixture.git(&["config", "remote.origin.url", ""]);
        let invalid = fixture.capture();
        assert_eq!(
            invalid.evidence().remote_identity(),
            &RepositoryRemoteIdentityV1::Invalid
        );

        let remote = format!(
            "https://example.invalid/{}",
            "x".repeat(MAX_REMOTE_IDENTITY_BYTES)
        );
        fixture.git(&["config", "remote.origin.url", &remote]);

        let oversized = fixture.capture();
        assert_eq!(
            oversized.evidence().remote_identity(),
            &RepositoryRemoteIdentityV1::Oversized
        );
        assert_ne!(
            missing.evidence().path_identity_digest(),
            invalid.evidence().path_identity_digest()
        );
        assert_ne!(
            invalid.evidence().path_identity_digest(),
            oversized.evidence().path_identity_digest()
        );
        assert_ne!(missing.capture_id(), invalid.capture_id());
        assert_ne!(invalid.capture_id(), oversized.capture_id());
    }

    #[test]
    fn persisted_index_tree_reports_staged_dirtiness_without_worktree_status() {
        let fixture = GitFixture::new();
        fixture.commit("base");
        fixture.git(&["write-tree"]);
        let baseline = fixture.capture();
        assert!(matches!(
            baseline.evidence().index_tree(),
            EvidenceAvailabilityV1::Known(_)
        ));
        assert!(matches!(
            baseline.evidence().dirty_state(),
            EvidenceAvailabilityV1::Unknown
        ));

        fs::write(fixture.path().join("tracked.txt"), "staged").unwrap();
        fixture.git(&["add", "--", "tracked.txt"]);
        fixture.git(&["write-tree"]);
        let staged = fixture.capture();
        assert!(matches!(
            staged.evidence().index_tree(),
            EvidenceAvailabilityV1::Known(_)
        ));
        assert_eq!(
            staged.evidence().dirty_state(),
            &EvidenceAvailabilityV1::Known(RepositoryDirtyStateV1::Dirty)
        );
        assert_ne!(
            staged.evidence().index_tree(),
            baseline.evidence().index_tree()
        );
    }

    #[test]
    fn unstaged_changes_never_claim_a_clean_repository() {
        let fixture = GitFixture::new();
        fixture.commit("base");
        fixture.git(&["write-tree"]);
        fs::write(fixture.path().join("tracked.txt"), "unstaged").unwrap();

        let capture = fixture.capture();
        assert_eq!(
            capture.evidence().dirty_state(),
            &EvidenceAvailabilityV1::Unknown,
            "PR7 does not run a worktree status scan"
        );
    }

    #[test]
    fn remote_credentials_are_removed_before_identity_hashing() {
        assert_eq!(
            normalize_remote_without_credentials(
                "https://alice:secret@Example.COM/Owner/Repo.git?token=hidden#fragment"
            )
            .unwrap(),
            "https://example.com/Owner/Repo"
        );
        assert_eq!(
            normalize_remote_without_credentials("git@example.com:Owner/Repo.git").unwrap(),
            "ssh://example.com/Owner/Repo"
        );
        assert_eq!(
            normalize_remote_without_credentials(
                "git@example.com:Owner/Repo.git?token=hidden#fragment"
            )
            .unwrap(),
            "ssh://example.com/Owner/Repo"
        );
    }

    #[test]
    fn bare_repository_is_typed_unsupported() {
        let root = TempDir::new().unwrap();
        let output = Command::new(crate::git::git_program())
            .args(["init", "--bare", "-q"])
            .current_dir(root.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        let repository_id = RepositoryId::new("repository.bare-fixture").unwrap();
        let result = capture_repository_provenance(&RepositoryProvenanceProbeRequest::new(
            root.path(),
            &repository_id,
            None,
            None,
            &PRIVACY_DOMAIN_SALT,
            UtcMicros(123),
        ));
        assert!(matches!(result, EvidenceAvailabilityV1::Unsupported));
    }

    #[test]
    fn missing_path_is_marked_partially_readable() {
        let root = TempDir::new().unwrap();
        let missing = root.path().join("missing");
        let (canonical, partially_readable) = canonical_path(&missing);
        assert!(partially_readable);
        assert_eq!(canonical, missing);
    }

    #[cfg(unix)]
    #[test]
    fn removed_opened_worktree_is_captured_as_partially_readable() {
        let fixture = GitFixture::new();
        fixture.commit("base");
        let repo = gix::discover(fixture.path()).unwrap();
        let workdir = repo.workdir().unwrap().to_path_buf();
        fs::remove_dir_all(&workdir).unwrap();

        let repository_id = RepositoryId::new("repository.partial-fixture").unwrap();
        let project_id = ProjectId::new("project.partial-fixture").unwrap();
        let worktree_id = WorktreeId::new("worktree.partial-fixture").unwrap();
        let result = NativeRepositoryProvenanceProbe::capture_open_repository(
            &repo,
            &RepositoryProvenanceProbeRequest::new(
                fixture.path(),
                &repository_id,
                Some(&project_id),
                Some(&worktree_id),
                &PRIVACY_DOMAIN_SALT,
                UtcMicros(123),
            ),
        );
        assert!(matches!(
            result,
            EvidenceAvailabilityV1::PartiallyReadable(_)
        ));
    }

    #[test]
    fn admission_context_is_deterministic_separated_and_path_private() {
        let root = TempDir::new().unwrap();
        let alternate_root = TempDir::new().unwrap();
        let common_dir = TempDir::new().unwrap();
        let project = ProjectId::new("project.provenance-admission").unwrap();
        let marker = crate::storage::RepositoryIdentityMarker {
            schema_version: crate::storage::REPOSITORY_IDENTITY_SCHEMA_VERSION,
            project_id: project.as_str().to_owned(),
            git_common_dir: common_dir.path().to_string_lossy().to_string(),
        };

        let first = RepositoryProvenanceAdmissionContext::from_authoritative_project_marker(
            root.path(),
            &project,
            &marker,
        )
        .unwrap();
        let repeated = RepositoryProvenanceAdmissionContext::from_authoritative_project_marker(
            root.path(),
            &project,
            &marker,
        )
        .unwrap();
        let alternate_worktree =
            RepositoryProvenanceAdmissionContext::from_authoritative_project_marker(
                alternate_root.path(),
                &project,
                &marker,
            )
            .unwrap();
        assert_eq!(first.repository_id, repeated.repository_id);
        assert_eq!(first.worktree_id, repeated.worktree_id);
        assert_eq!(first.privacy_domain_salt, repeated.privacy_domain_salt);
        assert_eq!(first.repository_id, alternate_worktree.repository_id);
        assert_ne!(first.worktree_id, alternate_worktree.worktree_id);

        let other_project = ProjectId::new("project.provenance-other").unwrap();
        let other_marker = crate::storage::RepositoryIdentityMarker {
            project_id: other_project.as_str().to_owned(),
            ..marker.clone()
        };
        let separated = RepositoryProvenanceAdmissionContext::from_authoritative_project_marker(
            root.path(),
            &other_project,
            &other_marker,
        )
        .unwrap();
        assert_ne!(first.privacy_domain_salt, separated.privacy_domain_salt);
        assert_ne!(first.repository_id, separated.repository_id);
        assert_ne!(first.worktree_id, separated.worktree_id);
        assert!(
            !first
                .repository_id
                .as_str()
                .contains(root.path().to_string_lossy().as_ref())
        );
        assert!(
            !first
                .repository_id
                .as_str()
                .contains(common_dir.path().to_string_lossy().as_ref())
        );
        assert!(
            !first
                .worktree_id
                .as_ref()
                .unwrap()
                .as_str()
                .contains(root.path().to_string_lossy().as_ref())
        );

        assert!(
            RepositoryProvenanceAdmissionContext::from_authoritative_project_marker(
                root.path(),
                &project,
                &other_marker,
            )
            .is_none()
        );
    }

    #[test]
    fn non_repository_is_typed_unavailable() {
        let root = TempDir::new().unwrap();
        let repository_id = RepositoryId::new("repository.fixture").unwrap();
        let result = capture_repository_provenance(&RepositoryProvenanceProbeRequest::new(
            root.path(),
            &repository_id,
            None,
            None,
            &PRIVACY_DOMAIN_SALT,
            UtcMicros(123),
        ));
        assert!(matches!(result, EvidenceAvailabilityV1::Unavailable));
    }

    fn head_oid(fixture: &GitFixture) -> String {
        let output = fixture.git(&["rev-parse", "HEAD"]);
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn ref_movement_does_not_retarget_retained_provenance() {
        let fixture = GitFixture::new();
        fixture.commit("commit-a");
        let commit_a_oid = head_oid(&fixture);
        let retained = fixture.capture();
        let EvidenceAvailabilityV1::Known(retained_commit) = retained.evidence().head_commit()
        else {
            panic!(
                "expected known head commit at A, got {:?}",
                retained.evidence().head_commit()
            );
        };
        assert_eq!(commit_a_oid, retained_commit.as_str());

        // Build commit B on a scratch branch, then retarget `main` to it with a
        // hard reset. The retained capture is immutable evidence; it must not
        // follow the moving ref.
        fixture.git(&["checkout", "-q", "-b", "scratch"]);
        fixture.commit("commit-b");
        let commit_b_oid = head_oid(&fixture);
        assert_ne!(commit_a_oid, commit_b_oid);
        fixture.git(&["checkout", "-q", "main"]);
        fixture.git(&["reset", "--hard", "scratch"]);
        assert_eq!(head_oid(&fixture), commit_b_oid);

        let fresh = fixture.capture();
        let EvidenceAvailabilityV1::Known(fresh_commit) = fresh.evidence().head_commit() else {
            panic!(
                "expected known head commit at B, got {:?}",
                fresh.evidence().head_commit()
            );
        };
        assert_eq!(commit_b_oid, fresh_commit.as_str());

        // The first capture still names A even though the ref now names B.
        let EvidenceAvailabilityV1::Known(retained_commit) = retained.evidence().head_commit()
        else {
            panic!("retained head commit was mutated by the ref move");
        };
        assert_eq!(commit_a_oid, retained_commit.as_str());
        assert_ne!(
            retained.evidence().head_commit(),
            fresh.evidence().head_commit()
        );
    }

    #[test]
    fn branch_rewrite_and_detach_do_not_retarget_retained_provenance() {
        let fixture = GitFixture::new();
        fixture.commit("commit-a");
        let commit_a_oid = head_oid(&fixture);
        let retained = fixture.capture();
        assert!(matches!(
            retained.evidence().attached_ref(),
            EvidenceAvailabilityV1::Known(_)
        ));
        let EvidenceAvailabilityV1::Known(retained_commit) = retained.evidence().head_commit()
        else {
            panic!(
                "expected known head commit at A, got {:?}",
                retained.evidence().head_commit()
            );
        };
        assert_eq!(commit_a_oid, retained_commit.as_str());

        // Detach HEAD and rewrite the commit in place. The rewrite produces a new
        // object B while HEAD stays detached.
        fixture.git(&["checkout", "-q", "--detach", "HEAD"]);
        fs::write(fixture.path().join("tracked.txt"), "rewritten").unwrap();
        fixture.git(&["add", "--", "tracked.txt"]);
        fixture.git(&["commit", "-q", "--amend", "-m", "rewritten"]);
        let commit_b_oid = head_oid(&fixture);
        assert_ne!(commit_a_oid, commit_b_oid);

        let fresh = fixture.capture();
        // The fresh capture reports the detached state explicitly and names B.
        assert!(matches!(
            fresh.evidence().attached_ref(),
            EvidenceAvailabilityV1::Detached
        ));
        let EvidenceAvailabilityV1::Known(fresh_commit) = fresh.evidence().head_commit() else {
            panic!(
                "expected known detached head commit at B, got {:?}",
                fresh.evidence().head_commit()
            );
        };
        assert_eq!(commit_b_oid, fresh_commit.as_str());

        // The retained capture is unchanged: still attached to its ref and naming A.
        assert!(matches!(
            retained.evidence().attached_ref(),
            EvidenceAvailabilityV1::Known(_)
        ));
        let EvidenceAvailabilityV1::Known(retained_commit) = retained.evidence().head_commit()
        else {
            panic!("retained head commit was mutated by the detach/rewrite");
        };
        assert_eq!(commit_a_oid, retained_commit.as_str());
        assert_ne!(
            retained.evidence().head_commit(),
            fresh.evidence().head_commit()
        );
    }

    #[test]
    fn removed_checkout_yields_typed_absence_without_ambient_head() {
        let fixture = GitFixture::new();
        fixture.commit("base");
        let retained = fixture.capture();
        assert!(matches!(
            retained.evidence().head_commit(),
            EvidenceAvailabilityV1::Known(_)
        ));
        assert!(matches!(
            retained.evidence().attached_ref(),
            EvidenceAvailabilityV1::Known(_)
        ));
        assert!(matches!(
            retained.evidence().path_identity_digest(),
            EvidenceAvailabilityV1::Known(_)
        ));

        // Remove the checkout entirely. A fresh capture must not walk up to an
        // ambient repository; it reports typed absence instead.
        fs::remove_dir_all(fixture.path()).unwrap();
        let fresh = fixture.capture_with(&NativeRepositoryProvenanceProbe);
        assert!(matches!(fresh, EvidenceAvailabilityV1::Unavailable));

        // The capture taken before deletion remains fully readable evidence.
        assert!(matches!(
            retained.evidence().head_commit(),
            EvidenceAvailabilityV1::Known(_)
        ));
        assert!(matches!(
            retained.evidence().path_identity_digest(),
            EvidenceAvailabilityV1::Known(_)
        ));
        serde_json::to_string(&retained).unwrap();
    }

    fn git_dir_fingerprint(root: &Path) -> BTreeMap<PathBuf, (u64, std::time::SystemTime)> {
        let mut entries = BTreeMap::new();
        for entry in walkdir::WalkDir::new(root.join(".git")).sort_by_file_name() {
            let entry = entry.unwrap();
            if !entry.file_type().is_file() {
                continue;
            }
            let metadata = entry.metadata().unwrap();
            entries.insert(
                entry.path().to_path_buf(),
                (metadata.len(), metadata.modified().unwrap()),
            );
        }
        entries
    }

    #[test]
    fn provenance_capture_copies_no_git_objects() {
        let fixture = GitFixture::new();
        fixture.commit("initial");
        fixture.git(&["write-tree"]);
        let before = git_dir_fingerprint(fixture.path());

        let first = fixture.capture();
        let second = fixture.capture();
        assert_eq!(first.capture_id(), second.capture_id());

        let after = git_dir_fingerprint(fixture.path());
        assert_eq!(
            before, after,
            "provenance capture must be read-only: it copies no git objects and \
             leaves the object store untouched"
        );
    }

    // PR7 contract gap (report, then unignore): when the admitted checkout's
    // `.git` is removed but its path still exists inside an ambient ancestor
    // repository, `gix::discover` walks up and the probe returns the ambient
    // parent's HEAD as `Known` evidence bound to the defunct checkout's
    // repository/worktree identity. The contract requires a typed
    // missing/unavailable state; the probe needs to verify the discovered
    // repository against the admission-pinned identity before capturing.
    #[test]
    #[ignore = "PR7 gap: probe resolves a defunct checkout against the ambient parent HEAD instead of a typed unavailable state"]
    fn defunct_checkout_capture_never_falls_back_to_an_ambient_parent_repository() {
        let parent = GitFixture::new();
        parent.commit("ambient parent");
        let parent_head = head_oid(&parent);

        let child = parent.path().join("child");
        fs::create_dir_all(&child).unwrap();
        let git = |args: &[&str]| {
            let output = Command::new(crate::git::git_program())
                .args(args)
                .current_dir(&child)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.name", "TraceDecay Test"]);
        git(&["config", "user.email", "tracedecay@example.invalid"]);
        fs::write(child.join("tracked.txt"), "nested").unwrap();
        git(&["add", "--", "tracked.txt"]);
        git(&["commit", "-q", "-m", "nested"]);
        let child_head = {
            let output = git(&["rev-parse", "HEAD"]);
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        assert_ne!(parent_head, child_head);

        let repository_id = RepositoryId::new("repository.nested-fixture").unwrap();
        let project_id = ProjectId::new("project.nested-fixture").unwrap();
        let worktree_id = WorktreeId::new("worktree.nested-fixture").unwrap();
        let request = RepositoryProvenanceProbeRequest::new(
            &child,
            &repository_id,
            Some(&project_id),
            Some(&worktree_id),
            &PRIVACY_DOMAIN_SALT,
            UtcMicros(123),
        );
        let before = capture_repository_provenance(&request);
        let Some(before_capture) = before.value() else {
            panic!("nested checkout must capture its own HEAD, got {before:?}");
        };
        assert_eq!(
            before_capture
                .evidence()
                .head_commit()
                .value()
                .map(CommitId::as_str),
            Some(child_head.as_str())
        );

        // The nested checkout's repository is gone, but its path still exists
        // inside the ambient parent worktree. The contract requires a safe typed
        // state — never the ambient parent's HEAD.
        fs::remove_dir_all(child.join(".git")).unwrap();
        fs::remove_file(child.join("tracked.txt")).unwrap();
        let after = capture_repository_provenance(&request);
        assert!(
            matches!(
                after,
                EvidenceAvailabilityV1::Unavailable
                    | EvidenceAvailabilityV1::Missing
                    | EvidenceAvailabilityV1::Unsupported
            ),
            "a defunct checkout must be typed unavailable, never resolved against \
             the ambient parent HEAD {parent_head}: {after:?}"
        );
    }
}
