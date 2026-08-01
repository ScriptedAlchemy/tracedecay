//! Bounded, read-only repository provenance capture.
//!
//! This adapter deliberately exposes no generic Git command surface, object
//! traversal or worktree-status probing. It reads only bounded
//! repository/worktree/HEAD/ref/remote identity plus persisted index metadata
//! through `gix`; query owns status, diff, history, blame, and hunk intelligence.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

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
pub struct RepositoryProvenanceAdmissionContext {
    project_root: PathBuf,
    project_id: ProjectId,
    repository_id: RepositoryId,
    worktree_id: Option<WorktreeId>,
    expected_common_dir: Option<PathBuf>,
    /// A deterministic project-domain salt, not a secret or credential.
    privacy_domain_salt: [u8; 32],
}

impl RepositoryProvenanceAdmissionContext {
    #[cfg(test)]
    pub fn new(
        project_root: PathBuf,
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: Option<WorktreeId>,
        privacy_domain_salt: [u8; 32],
    ) -> Self {
        let expected_common_dir = discover_canonical_common_dir(&project_root);
        Self {
            project_root,
            project_id,
            repository_id,
            worktree_id,
            expected_common_dir,
            privacy_domain_salt,
        }
    }

    /// Construct only from the daemon-authoritative project marker and typed
    /// project identity. The marker is an identity authority, never evidence.
    pub fn from_authoritative_project_marker(
        project_root: &Path,
        project_id: &ProjectId,
        marker: &tracedecay_runtime_core::storage::RepositoryIdentityMarker,
    ) -> Option<Self> {
        if marker.schema_version
            != tracedecay_runtime_core::storage::REPOSITORY_IDENTITY_SCHEMA_VERSION
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
                &[tracedecay_runtime_core::os_str_bytes::native_os_str_bytes(
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
                &[tracedecay_runtime_core::os_str_bytes::native_os_str_bytes(
                    canonical_root.as_os_str(),
                )],
            ),
        ))
        .ok()?;
        Some(Self {
            project_root: canonical_root,
            project_id: project_id.clone(),
            repository_id,
            worktree_id: Some(worktree_id),
            expected_common_dir: Some(canonical_common_dir),
            privacy_domain_salt,
        })
    }

    pub fn matches_admitted_identity(
        &self,
        project_id: &ProjectId,
        repository_id: &RepositoryId,
        worktree_id: &WorktreeId,
    ) -> bool {
        &self.project_id == project_id
            && &self.repository_id == repository_id
            && self.worktree_id.as_ref() == Some(worktree_id)
    }

    pub fn admitted_identity(&self) -> Option<(ProjectId, RepositoryId, WorktreeId)> {
        Some((
            self.project_id.clone(),
            self.repository_id.clone(),
            self.worktree_id.clone()?,
        ))
    }

    /// Capture only after the observation has crossed the privacy boundary.
    pub fn capture_after_sanitization(
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
        let captured = capture_repository_provenance(
            &RepositoryProvenanceProbeRequest::new(
                &self.project_root,
                &self.repository_id,
                Some(&self.project_id),
                self.worktree_id.as_ref(),
                &self.privacy_domain_salt,
                ingested_at,
            )
            .with_expected_common_dir(self.expected_common_dir.as_deref()),
        );
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
pub struct PreparedRepositoryProvenanceV1 {
    availability: EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1>,
    anchor: Option<RetrievalAnchorRecordV2>,
}

impl PreparedRepositoryProvenanceV1 {
    pub const fn unavailable() -> Self {
        Self {
            availability: EvidenceAvailabilityV1::Unavailable,
            anchor: None,
        }
    }

    pub fn availability(&self) -> &EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1> {
        &self.availability
    }

    pub fn anchor(&self) -> Option<&RetrievalAnchorRecordV2> {
        self.anchor.as_ref()
    }
}

/// Authoritative identities and privacy material supplied by the admission boundary.
pub struct RepositoryProvenanceProbeRequest<'a> {
    project_root: &'a Path,
    repository_id: &'a RepositoryId,
    project_id: Option<&'a ProjectId>,
    worktree_id: Option<&'a WorktreeId>,
    expected_common_dir: Option<PathBuf>,
    privacy_domain_salt: &'a [u8; 32],
    captured_at: UtcMicros,
}

impl<'a> RepositoryProvenanceProbeRequest<'a> {
    pub fn new(
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

    fn with_expected_common_dir(mut self, expected_common_dir: Option<&Path>) -> Self {
        if let Some(expected_common_dir) = expected_common_dir {
            self.expected_common_dir = Some(expected_common_dir.to_path_buf());
        }
        self
    }
}

/// Fixed native-Git provenance probe. It never writes the index or object store.
#[derive(Default)]
pub struct NativeRepositoryProvenanceProbe;

impl NativeRepositoryProvenanceProbe {
    pub fn capture(
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
            &[tracedecay_runtime_core::os_str_bytes::native_os_str_bytes(
                canonical_root.as_os_str(),
            )],
        ) else {
            return EvidenceAvailabilityV1::Unavailable;
        };
        let path_frames = [
            tracedecay_runtime_core::os_str_bytes::native_os_str_bytes(canonical_root.as_os_str()),
            tracedecay_runtime_core::os_str_bytes::native_os_str_bytes(git_dir.as_os_str()),
            tracedecay_runtime_core::os_str_bytes::native_os_str_bytes(common_dir.as_os_str()),
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

pub fn capture_repository_provenance(
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

fn discover_canonical_common_dir(project_root: &Path) -> Option<PathBuf> {
    let repository = gix::discover(project_root).ok()?;
    let (common_dir, partial) = canonical_path(repository.common_dir());
    (!partial && common_dir.is_absolute()).then_some(common_dir)
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
        // prove cleanliness without a worktree traversal, which belongs to QUERY.
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

/// Distinct remote strings the normalization memo keeps.
///
/// One daemon serves a handful of checkouts, so a few entries cover every
/// remote in flight; the memo is a short linear scan, never a growth surface.
const REMOTE_NORMALIZATION_MEMO_CAPACITY: usize = 8;

/// Recently normalized `(raw remote, normalization)` entries, most recent last.
type RemoteNormalizationMemo = Mutex<VecDeque<(String, Option<String>)>>;

fn remote_normalization_memo() -> &'static RemoteNormalizationMemo {
    static MEMO: OnceLock<RemoteNormalizationMemo> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(VecDeque::with_capacity(REMOTE_NORMALIZATION_MEMO_CAPACITY)))
}

/// Credential-stripped remote identity, memoized on the raw remote string.
///
/// Provenance is captured once per ingested observation, so this normalization
/// ran once per record — and for an `https://` remote `Url::parse` runs the
/// host through idna/ICU domain mapping, which is disproportionately expensive
/// next to the rest of a per-record capture. The remote is still read live from
/// Git config on every capture; only the pure normalization of that string is
/// reused. A changed remote is a different key and recomputes, so a memo hit is
/// by construction the same value a fresh parse would produce.
fn normalize_remote_without_credentials(remote: &str) -> Option<String> {
    if let Ok(memo) = remote_normalization_memo().lock()
        && let Some((_, normalized)) = memo.iter().find(|(key, _)| key == remote)
    {
        return normalized.clone();
    }
    let normalized = normalize_remote_uncached(remote);
    if let Ok(mut memo) = remote_normalization_memo().lock() {
        // Re-check: a concurrent capture may have inserted the same key.
        if !memo.iter().any(|(key, _)| key == remote) {
            if memo.len() == REMOTE_NORMALIZATION_MEMO_CAPACITY {
                memo.pop_front();
            }
            memo.push_back((remote.to_owned(), normalized.clone()));
        }
    }
    normalized
}

fn normalize_remote_uncached(remote: &str) -> Option<String> {
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
    PrivacyDomainBoundLocatorDigest::new(format!("sha256:{}", hex::encode(hasher.finalize()))).ok()
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
    hex::encode(hasher.finalize())
}

fn hash_frame(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod remote_normalization_tests {
    use super::{normalize_remote_uncached, normalize_remote_without_credentials};

    /// Remote strings covering every branch of the normalization: credentialed
    /// and bare HTTPS (the shapes that reach idna), SCP-style SSH, local paths,
    /// and the rejected forms.
    const REMOTES: &[&str] = &[
        "https://github.com/Owner/Repo.git",
        "https://user:secret@github.com/Owner/Repo.git/",
        "https://github.com/Owner/Repo?token=abc#frag",
        "https://xn--nxasmm1c.example.com/Owner/Repo.git",
        "git@github.com:Owner/Repo.git",
        "ssh://git@example.com:22/Owner/Repo.git",
        "/srv/git/repo.git",
        "  https://github.com/Owner/Repo.git  ",
        "",
        "   ",
    ];

    /// The memo must be transparent: every remote shape normalizes to exactly
    /// what an uncached parse produces, on the miss and again on the hit.
    #[test]
    fn memoized_normalization_matches_the_uncached_normalization() {
        for remote in REMOTES {
            let expected = normalize_remote_uncached(remote);
            assert_eq!(
                normalize_remote_without_credentials(remote),
                expected,
                "cold normalization diverged for {remote:?}"
            );
            assert_eq!(
                normalize_remote_without_credentials(remote),
                expected,
                "memoized normalization diverged for {remote:?}"
            );
        }
    }

    /// A repository whose remote changes must not be served the previous
    /// remote's identity, and evicted entries must recompute identically.
    #[test]
    fn changed_remotes_never_serve_a_previous_normalization() {
        let first = normalize_remote_without_credentials("https://github.com/Owner/First.git");
        let second = normalize_remote_without_credentials("https://github.com/Owner/Second.git");
        assert_eq!(first.as_deref(), Some("https://github.com/Owner/First"));
        assert_eq!(second.as_deref(), Some("https://github.com/Owner/Second"));

        // Overflow the memo, then re-read the first remote.
        for index in 0..32 {
            let _ = normalize_remote_without_credentials(&format!(
                "https://github.com/Owner/Filler{index}.git"
            ));
        }
        assert_eq!(
            normalize_remote_without_credentials("https://github.com/Owner/First.git"),
            first
        );
    }
}
