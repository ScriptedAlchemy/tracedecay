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
        let output = Command::new(tracedecay_runtime_core::git::git_program())
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
    let merge = Command::new(tracedecay_runtime_core::git::git_program())
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
        "provenance reads do not run a worktree status scan"
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
    let output = Command::new(tracedecay_runtime_core::git::git_program())
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
    let marker = tracedecay_runtime_core::storage::RepositoryIdentityMarker {
        schema_version: tracedecay_runtime_core::storage::REPOSITORY_IDENTITY_SCHEMA_VERSION,
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
    let other_marker = tracedecay_runtime_core::storage::RepositoryIdentityMarker {
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
fn admission_capture_cache_reuses_exact_watermark_and_invalidates_on_index_change() {
    let fixture = GitFixture::new();
    fixture.commit("base");
    let context = RepositoryProvenanceAdmissionContext::new(
        fixture.path().to_path_buf(),
        ProjectId::new("project.capture-cache").unwrap(),
        RepositoryId::new("repository.capture-cache").unwrap(),
        Some(WorktreeId::new("worktree.capture-cache").unwrap()),
        PRIVACY_DOMAIN_SALT,
    );

    let cold_snapshot = context.capture_snapshot(UtcMicros(100));
    let warm_snapshot = context.capture_snapshot(UtcMicros(200));
    let cold = cold_snapshot.availability().value().unwrap();
    let warm = warm_snapshot.availability().value().unwrap();
    assert_eq!(cold.capture_id(), warm.capture_id());
    assert_eq!(warm.captured_at(), UtcMicros(100));

    fs::write(fixture.path().join("tracked.txt"), "staged").unwrap();
    fixture.git(&["add", "--", "tracked.txt"]);
    let changed_snapshot = context.capture_snapshot(UtcMicros(300));
    let changed = changed_snapshot.availability().value().unwrap();
    assert_ne!(cold.capture_id(), changed.capture_id());
    assert_eq!(changed.captured_at(), UtcMicros(300));
    assert_eq!(
        changed.evidence().dirty_state(),
        &EvidenceAvailabilityV1::Unknown
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
    let EvidenceAvailabilityV1::Known(retained_commit) = retained.evidence().head_commit() else {
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
    let EvidenceAvailabilityV1::Known(retained_commit) = retained.evidence().head_commit() else {
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
    let EvidenceAvailabilityV1::Known(retained_commit) = retained.evidence().head_commit() else {
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
    let EvidenceAvailabilityV1::Known(retained_commit) = retained.evidence().head_commit() else {
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
    fn visit(directory: &Path, entries: &mut BTreeMap<PathBuf, (u64, std::time::SystemTime)>) {
        let mut children = fs::read_dir(directory)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            let file_type = child.file_type().unwrap();
            if file_type.is_dir() {
                visit(&child.path(), entries);
            } else if file_type.is_file() {
                let metadata = child.metadata().unwrap();
                entries.insert(child.path(), (metadata.len(), metadata.modified().unwrap()));
            }
        }
    }

    let mut entries = BTreeMap::new();
    visit(&root.join(".git"), &mut entries);
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

#[test]
fn defunct_checkout_capture_never_falls_back_to_an_ambient_parent_repository() {
    let parent = GitFixture::new();
    parent.commit("ambient parent");
    let parent_head = head_oid(&parent);

    let child = parent.path().join("child");
    fs::create_dir_all(&child).unwrap();
    let git = |args: &[&str]| {
        let output = Command::new(tracedecay_runtime_core::git::git_program())
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
