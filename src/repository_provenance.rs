//! Bounded, read-only repository provenance capture.
//!
//! This adapter deliberately exposes no generic Git command surface. It reads
//! identity and immutable object evidence through `gix`, and uses one fixed
//! porcelain-status probe for the working-state classification that `gix`
//! cannot provide with this crate's current feature set.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CommitId, EvidenceAvailabilityV1, PrivacyDomainBoundLocatorDigest, ProjectId, RefId,
    RepositoryDirtyStateV1, RepositoryEvidenceV1, RepositoryId, RepositoryProvenanceV1, TreeId,
    UtcMicros, WorktreeId,
};

const STATUS_OUTPUT_LIMIT_BYTES: usize = 256 * 1024;
const STATUS_TIMEOUT: Duration = Duration::from_secs(2);
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(5);
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Authoritative identities and privacy material supplied by the admission boundary.
pub(crate) struct RepositoryProvenanceProbeRequest<'a> {
    project_root: &'a Path,
    repository_id: &'a RepositoryId,
    project_id: Option<&'a ProjectId>,
    worktree_id: Option<&'a WorktreeId>,
    privacy_domain_key: &'a [u8; 32],
    captured_at: UtcMicros,
}

impl<'a> RepositoryProvenanceProbeRequest<'a> {
    pub(crate) const fn new(
        project_root: &'a Path,
        repository_id: &'a RepositoryId,
        project_id: Option<&'a ProjectId>,
        worktree_id: Option<&'a WorktreeId>,
        privacy_domain_key: &'a [u8; 32],
        captured_at: UtcMicros,
    ) -> Self {
        Self {
            project_root,
            repository_id,
            project_id,
            worktree_id,
            privacy_domain_key,
            captured_at,
        }
    }
}

/// Fixed native-Git provenance probe. It never writes the index or object store.
pub(crate) struct NativeRepositoryProvenanceProbe {
    status_bounds: StatusProbeBounds,
}

impl Default for NativeRepositoryProvenanceProbe {
    fn default() -> Self {
        Self {
            status_bounds: StatusProbeBounds {
                max_output_bytes: STATUS_OUTPUT_LIMIT_BYTES,
                timeout: STATUS_TIMEOUT,
            },
        }
    }
}

impl NativeRepositoryProvenanceProbe {
    pub(crate) fn capture(
        &self,
        request: &RepositoryProvenanceProbeRequest<'_>,
    ) -> EvidenceAvailabilityV1<RepositoryProvenanceV1> {
        let Ok(repo) = gix::discover(request.project_root) else {
            return EvidenceAvailabilityV1::Unavailable;
        };
        let Some(workdir) = repo.workdir() else {
            return EvidenceAvailabilityV1::Unsupported;
        };

        let (canonical_root, root_is_partial) = canonical_path(workdir);
        if !canonical_root.is_absolute() {
            return EvidenceAvailabilityV1::Unavailable;
        }
        let (git_dir, git_dir_is_partial) = canonical_path(repo.git_dir());
        let (common_dir, common_dir_is_partial) = canonical_path(repo.common_dir());
        let remote_identity = credential_free_remote_identity(&repo);

        let Some(canonical_root_digest) = privacy_bound_digest(
            request.privacy_domain_key,
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
            remote_identity.unwrap_or_else(|| b"<remote-unavailable>".to_vec()),
        ];
        let Some(path_identity_digest) = privacy_bound_digest(
            request.privacy_domain_key,
            b"repository-path-identity-v1",
            &path_frames,
        ) else {
            return EvidenceAvailabilityV1::Unavailable;
        };

        let head = observe_head(&repo);
        let status = fixed_status_probe(&canonical_root, self.status_bounds);
        let index_tree = observe_index_tree(&repo, &head.tree, &status);
        let Ok(evidence) = RepositoryEvidenceV1::new(
            head.attached_ref,
            head.commit,
            index_tree,
            EvidenceAvailabilityV1::Known(path_identity_digest),
            status.dirty_state,
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
    NativeRepositoryProvenanceProbe::default().capture(request)
}

#[derive(Clone, Copy)]
struct StatusProbeBounds {
    max_output_bytes: usize,
    timeout: Duration,
}

#[derive(Debug)]
struct HeadObservation {
    attached_ref: EvidenceAvailabilityV1<RefId>,
    commit: EvidenceAvailabilityV1<CommitId>,
    tree: EvidenceAvailabilityV1<TreeId>,
}

fn observe_head(repo: &gix::Repository) -> HeadObservation {
    let Ok(head) = repo.head() else {
        return HeadObservation {
            attached_ref: EvidenceAvailabilityV1::Unavailable,
            commit: EvidenceAvailabilityV1::Unavailable,
            tree: EvidenceAvailabilityV1::Unavailable,
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
            tree: EvidenceAvailabilityV1::Unborn,
        };
    }

    let Ok(commit) = repo.head_commit() else {
        return HeadObservation {
            attached_ref,
            commit: EvidenceAvailabilityV1::Unavailable,
            tree: EvidenceAvailabilityV1::Unavailable,
        };
    };
    let commit_id = CommitId::new(commit.id().to_hex().to_string()).map_or(
        EvidenceAvailabilityV1::Unknown,
        EvidenceAvailabilityV1::Known,
    );
    let tree = commit
        .tree_id()
        .ok()
        .and_then(|id| TreeId::new(id.to_hex().to_string()).ok())
        .map_or(
            EvidenceAvailabilityV1::Unavailable,
            EvidenceAvailabilityV1::Known,
        );
    HeadObservation {
        attached_ref,
        commit: commit_id,
        tree,
    }
}

fn observe_index_tree(
    repo: &gix::Repository,
    head_tree: &EvidenceAvailabilityV1<TreeId>,
    status: &StatusObservation,
) -> EvidenceAvailabilityV1<TreeId> {
    let index = match repo.try_index() {
        Ok(Some(index)) => index,
        Ok(None) => return EvidenceAvailabilityV1::Missing,
        Err(_) => return EvidenceAvailabilityV1::Unavailable,
    };
    if index.entries().iter().any(|entry| entry.stage_raw() != 0) || status.conflicted {
        return EvidenceAvailabilityV1::Conflicted;
    }
    match (head_tree, status.index_matches_head) {
        (EvidenceAvailabilityV1::Known(tree), Some(true)) => {
            EvidenceAvailabilityV1::Known(tree.clone())
        }
        (EvidenceAvailabilityV1::Unborn, _) => EvidenceAvailabilityV1::Unborn,
        (EvidenceAvailabilityV1::Unavailable, _) => EvidenceAvailabilityV1::Unavailable,
        (EvidenceAvailabilityV1::Unknown, _) => EvidenceAvailabilityV1::Unknown,
        (_, Some(false)) => EvidenceAvailabilityV1::Unsupported,
        _ => EvidenceAvailabilityV1::Unknown,
    }
}

#[derive(Debug)]
struct StatusObservation {
    dirty_state: EvidenceAvailabilityV1<RepositoryDirtyStateV1>,
    index_matches_head: Option<bool>,
    conflicted: bool,
}

impl StatusObservation {
    fn unavailable() -> Self {
        Self {
            dirty_state: EvidenceAvailabilityV1::Unavailable,
            index_matches_head: None,
            conflicted: false,
        }
    }
}

fn fixed_status_probe(project_root: &Path, bounds: StatusProbeBounds) -> StatusObservation {
    let mut command = Command::new(crate::git::git_program());
    command
        .args([
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=normal",
            "--ignore-submodules=none",
            "--",
        ])
        .current_dir(project_root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let Ok(mut child) = command.spawn() else {
        return StatusObservation::unavailable();
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return StatusObservation::unavailable();
    };
    let max_output_bytes = bounds.max_output_bytes.max(1);
    let reader = thread::spawn(move || read_bounded(stdout, max_output_bytes));
    let deadline = Instant::now() + bounds.timeout;
    let mut timed_out = false;
    let mut wait_failed = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => thread::sleep(STATUS_POLL_INTERVAL),
            Err(_) => {
                wait_failed = true;
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let Ok(output) = reader.join() else {
        return StatusObservation::unavailable();
    };
    let Ok(output) = output else {
        return StatusObservation::unavailable();
    };
    if wait_failed {
        return StatusObservation::unavailable();
    }
    if timed_out || output.truncated {
        return partial_status(&output.bytes);
    }
    if !status.is_some_and(|status| status.success()) {
        return StatusObservation::unavailable();
    }
    complete_status(&output.bytes)
}

struct BoundedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

fn read_bounded(mut reader: impl Read, max_output_bytes: usize) -> std::io::Result<BoundedOutput> {
    let mut bytes = Vec::with_capacity(max_output_bytes.min(8 * 1024));
    let mut truncated = false;
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let remaining = max_output_bytes.saturating_sub(bytes.len());
        let retained = remaining.min(read);
        bytes.extend_from_slice(&chunk[..retained]);
        truncated |= retained < read;
    }
    Ok(BoundedOutput { bytes, truncated })
}

#[derive(Clone, Copy)]
struct PorcelainSummary {
    dirty: bool,
    conflicted: bool,
    staged: bool,
}

fn summarize_porcelain(bytes: &[u8]) -> PorcelainSummary {
    let mut summary = PorcelainSummary {
        dirty: !bytes.is_empty(),
        conflicted: false,
        staged: false,
    };
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        if record.starts_with(b"u ") {
            summary.conflicted = true;
            summary.staged = true;
        } else if (record.starts_with(b"1 ") || record.starts_with(b"2 "))
            && record.get(2).is_some_and(|state| *state != b'.')
        {
            summary.staged = true;
        }
    }
    summary
}

fn complete_status(bytes: &[u8]) -> StatusObservation {
    let summary = summarize_porcelain(bytes);
    let state = if summary.conflicted {
        RepositoryDirtyStateV1::Conflicted
    } else if summary.dirty {
        RepositoryDirtyStateV1::Dirty
    } else {
        RepositoryDirtyStateV1::Clean
    };
    StatusObservation {
        dirty_state: EvidenceAvailabilityV1::Known(state),
        index_matches_head: Some(!summary.staged),
        conflicted: summary.conflicted,
    }
}

fn partial_status(bytes: &[u8]) -> StatusObservation {
    if bytes.is_empty() {
        return StatusObservation::unavailable();
    }
    let summary = summarize_porcelain(bytes);
    let state = if summary.conflicted {
        RepositoryDirtyStateV1::Conflicted
    } else {
        RepositoryDirtyStateV1::Dirty
    };
    StatusObservation {
        dirty_state: EvidenceAvailabilityV1::PartiallyReadable(state),
        index_matches_head: None,
        conflicted: summary.conflicted,
    }
}

fn canonical_path(path: &Path) -> (PathBuf, bool) {
    path.canonicalize()
        .map_or_else(|_| (path.to_path_buf(), true), |path| (path, false))
}

fn credential_free_remote_identity(repo: &gix::Repository) -> Option<Vec<u8>> {
    let remote = repo
        .config_snapshot()
        .string("remote.origin.url")?
        .to_string();
    normalize_remote_without_credentials(&remote).map(String::into_bytes)
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
        let path = path.trim_matches('/').trim_end_matches(".git");
        if host.is_empty() || path.is_empty() {
            return None;
        }
        return Some(format!("ssh://{}/{path}", host.to_ascii_lowercase()));
    }
    Some(format!("local:{remote}"))
}

fn privacy_bound_digest(
    privacy_domain_key: &[u8; 32],
    domain: &[u8],
    frames: &[Vec<u8>],
) -> Option<PrivacyDomainBoundLocatorDigest> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay-privacy-bound-locator-v1\0");
    hash_frame(&mut hasher, privacy_domain_key);
    hash_frame(&mut hasher, domain);
    for frame in frames {
        hash_frame(&mut hasher, frame);
    }
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    PrivacyDomainBoundLocatorDigest::new(format!("sha256:{encoded}")).ok()
}

fn hash_frame(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Output;

    use tempfile::TempDir;

    use super::*;

    const PRIVACY_KEY: [u8; 32] = [0x5a; 32];

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
                &PRIVACY_KEY,
                UtcMicros(123),
            ))
        }

        fn capture(&self) -> RepositoryProvenanceV1 {
            match self.capture_with(&NativeRepositoryProvenanceProbe::default()) {
                EvidenceAvailabilityV1::Known(capture) => capture,
                other => panic!("expected known capture, got {other:?}"),
            }
        }
    }

    #[test]
    fn clean_capture_has_exact_head_ref_index_and_private_locator_evidence() {
        let fixture = GitFixture::new();
        fixture.commit("initial");
        fixture.git(&[
            "remote",
            "add",
            "origin",
            "https://alice:top-secret@example.com/Owner/Repo.git?token=hidden",
        ]);

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
        assert_eq!(
            capture.evidence().dirty_state(),
            &EvidenceAvailabilityV1::Known(RepositoryDirtyStateV1::Clean)
        );
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
    fn conflicted_index_and_worktree_are_explicit() {
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
        assert_eq!(
            capture.evidence().dirty_state(),
            &EvidenceAvailabilityV1::Known(RepositoryDirtyStateV1::Conflicted)
        );
        assert!(matches!(
            capture.evidence().index_tree(),
            EvidenceAvailabilityV1::Conflicted
        ));
    }

    #[test]
    fn changed_index_is_typed_unsupported_without_materializing_a_tree() {
        let fixture = GitFixture::new();
        fixture.commit("base");
        fs::write(fixture.path().join("tracked.txt"), "staged").unwrap();
        fixture.git(&["add", "--", "tracked.txt"]);

        let capture = fixture.capture();
        assert_eq!(
            capture.evidence().dirty_state(),
            &EvidenceAvailabilityV1::Known(RepositoryDirtyStateV1::Dirty)
        );
        assert!(matches!(
            capture.evidence().index_tree(),
            EvidenceAvailabilityV1::Unsupported
        ));
    }

    #[test]
    fn output_cap_reports_partial_without_claiming_an_index_tree() {
        let fixture = GitFixture::new();
        fixture.commit("base");
        fs::write(fixture.path().join("untracked.txt"), "dirty").unwrap();
        let probe = NativeRepositoryProvenanceProbe {
            status_bounds: StatusProbeBounds {
                max_output_bytes: 1,
                timeout: STATUS_TIMEOUT,
            },
        };
        let capture = match fixture.capture_with(&probe) {
            EvidenceAvailabilityV1::Known(capture) => capture,
            other => panic!("expected known capture, got {other:?}"),
        };
        assert_eq!(
            capture.evidence().dirty_state(),
            &EvidenceAvailabilityV1::PartiallyReadable(RepositoryDirtyStateV1::Dirty)
        );
        assert!(matches!(
            capture.evidence().index_tree(),
            EvidenceAvailabilityV1::Unknown
        ));
    }

    #[test]
    fn capture_does_not_write_the_index_or_object_database() {
        let fixture = GitFixture::new();
        fixture.commit("base");
        let git_dir = fixture.path().join(".git");
        let index_before = fs::read(git_dir.join("index")).unwrap();
        let objects_before = object_files(&git_dir.join("objects"));

        let _ = fixture.capture();

        assert_eq!(fs::read(git_dir.join("index")).unwrap(), index_before);
        assert_eq!(object_files(&git_dir.join("objects")), objects_before);
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
    }

    fn object_files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        fn visit(root: &Path, path: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
            for entry in fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    visit(root, &path, files);
                } else if path.is_file() {
                    files.push((
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        fs::read(path).unwrap(),
                    ));
                }
            }
        }
        let mut files = Vec::new();
        visit(root, root, &mut files);
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    }

    #[test]
    fn bounded_reader_discards_excess_bytes_without_growing_the_retained_buffer() {
        let output = read_bounded(&b"abcdef"[..], 3).unwrap();
        assert_eq!(output.bytes, b"abc");
        assert!(output.truncated);
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
            &PRIVACY_KEY,
            UtcMicros(123),
        ));
        assert!(matches!(result, EvidenceAvailabilityV1::Unavailable));
    }
}
