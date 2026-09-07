#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use tracedecay_domain::GitOidV1;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::git_repository::{
    GitNativeIntegrationMode, GitNativePreflightDisposition, GitRepositoryAuthority,
};

struct RepositoryFixture {
    directory: TempDir,
}

impl RepositoryFixture {
    fn new() -> Self {
        let fixture = Self {
            directory: tempfile::tempdir().expect("temporary repository"),
        };
        fixture.git(&["init", "--quiet", "-b", "main"]);
        fixture.git(&["config", "user.name", "Fixture"]);
        fixture.git(&["config", "user.email", "fixture@example.com"]);
        fixture
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(self.path())
            .output()
            .expect("git executable");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("UTF-8 git output")
            .trim()
            .to_owned()
    }

    fn write(&self, path: &str, contents: &str) {
        std::fs::write(self.path().join(path), contents).expect("write fixture");
    }

    fn commit(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "--quiet", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn tip(&self, reference: &str) -> GitOidV1 {
        GitOidV1::new(self.git(&["rev-parse", reference])).expect("valid oid")
    }

    fn tree(&self, reference: &str) -> String {
        self.git(&["rev-parse", &format!("{reference}^{{tree}}")])
    }

    fn setup_fast_forward(&self) -> (GitOidV1, GitOidV1) {
        self.write("base.txt", "base\n");
        let destination = self.commit("base");
        self.git(&["branch", "feature"]);
        self.git(&["switch", "--quiet", "feature"]);
        self.write("feature.txt", "feature\n");
        let source = self.commit("feature");
        (
            GitOidV1::new(source).unwrap(),
            GitOidV1::new(destination).unwrap(),
        )
    }

    fn setup_divergence(&self, conflict: bool) -> (GitOidV1, GitOidV1) {
        self.write("shared.txt", "base\n");
        self.commit("base");
        self.git(&["branch", "feature"]);
        self.write("main.txt", "main\n");
        if conflict {
            self.write("shared.txt", "main\n");
        }
        let destination = self.commit("main");
        self.git(&["switch", "--quiet", "feature"]);
        self.write("feature.txt", "feature\n");
        if conflict {
            self.write("shared.txt", "feature\n");
        }
        let source = self.commit("feature");
        (
            GitOidV1::new(source).unwrap(),
            GitOidV1::new(destination).unwrap(),
        )
    }
}

fn preflight(
    fixture: &RepositoryFixture,
    source: &GitOidV1,
    destination: &GitOidV1,
    mode: GitNativeIntegrationMode,
) -> tracedecay_runtime_core::git_repository::GitNativePreflight {
    GitRepositoryAuthority::discover(fixture.path())
        .unwrap()
        .preflight_native_integration(
            "refs/heads/feature",
            "refs/heads/main",
            source,
            destination,
            mode,
            &CancellationToken::new(),
        )
        .unwrap()
}

#[test]
fn preflight_is_read_only_and_fast_forward_apply_and_rollback_use_exact_ref_cas() {
    let fixture = RepositoryFixture::new();
    let (source, destination) = fixture.setup_fast_forward();
    let refs_before = fixture.git(&["show-ref"]);
    let status_before = fixture.git(&["status", "--porcelain=v1"]);
    let objects_before = fixture.git(&["count-objects", "-v"]);

    let preview = preflight(
        &fixture,
        &source,
        &destination,
        GitNativeIntegrationMode::FastForward,
    );
    assert_eq!(preview.disposition, GitNativePreflightDisposition::Eligible);
    assert_eq!(fixture.git(&["show-ref"]), refs_before);
    assert_eq!(fixture.git(&["status", "--porcelain=v1"]), status_before);
    assert_eq!(fixture.git(&["count-objects", "-v"]), objects_before);

    let authority = GitRepositoryAuthority::discover(fixture.path()).unwrap();
    let candidate = preview.candidate_tree.as_ref().unwrap();
    let applied = authority
        .apply_native_integration(
            "refs/heads/feature",
            "refs/heads/main",
            &source,
            &destination,
            candidate,
            GitNativeIntegrationMode::FastForward,
            &CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(applied.new_tip, source);
    assert_eq!(fixture.tip("main"), source);

    authority
        .rollback_native_integration("refs/heads/main", &applied.new_tip, &destination)
        .unwrap();
    assert_eq!(fixture.tip("main"), destination);

    fixture.git(&["branch", "other", "main"]);
    fixture.git(&["switch", "--quiet", "other"]);
    fixture.write("other.txt", "other\n");
    let concurrent = fixture.commit("other");
    fixture.git(&["branch", "-f", "main", "other"]);
    fixture.git(&["switch", "--quiet", "feature"]);
    assert!(
        authority
            .rollback_native_integration("refs/heads/main", &source, &destination)
            .is_err()
    );
    assert_eq!(fixture.git(&["rev-parse", "main"]), concurrent);
}

#[test]
fn two_parent_merge_and_cherry_pick_materialize_the_previewed_tree() {
    for mode in [
        GitNativeIntegrationMode::TwoParentMerge,
        GitNativeIntegrationMode::CherryPickExactCommits,
    ] {
        let fixture = RepositoryFixture::new();
        let (source, destination) = fixture.setup_divergence(false);
        let preview = preflight(&fixture, &source, &destination, mode);
        assert_eq!(preview.disposition, GitNativePreflightDisposition::Eligible);
        let candidate = preview.candidate_tree.clone().unwrap();

        let applied = GitRepositoryAuthority::discover(fixture.path())
            .unwrap()
            .apply_native_integration(
                "refs/heads/feature",
                "refs/heads/main",
                &source,
                &destination,
                &candidate,
                mode,
                &CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(applied.final_tree, candidate);
        assert_eq!(fixture.tree("main"), candidate.as_str());
        let parent_count = fixture
            .git(&["show", "-s", "--format=%P", "main"])
            .split_whitespace()
            .count();
        assert_eq!(
            parent_count,
            if mode == GitNativeIntegrationMode::TwoParentMerge {
                2
            } else {
                1
            }
        );
    }
}

#[test]
fn conflict_cancellation_and_concurrent_destination_drift_never_move_the_ref() {
    let conflict = RepositoryFixture::new();
    let (source, destination) = conflict.setup_divergence(true);
    let preview = preflight(
        &conflict,
        &source,
        &destination,
        GitNativeIntegrationMode::TwoParentMerge,
    );
    assert_eq!(preview.disposition, GitNativePreflightDisposition::Conflict);
    assert_eq!(conflict.tip("main"), destination);

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        GitRepositoryAuthority::discover(conflict.path())
            .unwrap()
            .preflight_native_integration(
                "refs/heads/feature",
                "refs/heads/main",
                &source,
                &destination,
                GitNativeIntegrationMode::TwoParentMerge,
                &cancelled,
            )
            .is_err()
    );
    assert_eq!(conflict.tip("main"), destination);

    let drift = RepositoryFixture::new();
    let (source, destination) = drift.setup_fast_forward();
    let preview = preflight(
        &drift,
        &source,
        &destination,
        GitNativeIntegrationMode::FastForward,
    );
    drift.git(&["branch", "concurrent", "main"]);
    drift.git(&["switch", "--quiet", "concurrent"]);
    drift.write("concurrent.txt", "concurrent\n");
    let concurrent = drift.commit("concurrent");
    drift.git(&["branch", "-f", "main", "concurrent"]);
    drift.git(&["switch", "--quiet", "feature"]);
    assert!(
        GitRepositoryAuthority::discover(drift.path())
            .unwrap()
            .apply_native_integration(
                "refs/heads/feature",
                "refs/heads/main",
                &source,
                &destination,
                preview.candidate_tree.as_ref().unwrap(),
                GitNativeIntegrationMode::FastForward,
                &CancellationToken::new(),
            )
            .is_err()
    );
    assert_eq!(drift.git(&["rev-parse", "main"]), concurrent);
}

#[test]
fn configured_signing_or_write_hooks_keep_native_apply_preview_only() {
    for configure in ["signing", "hook"] {
        let fixture = RepositoryFixture::new();
        let (source, destination) = fixture.setup_fast_forward();
        if configure == "signing" {
            fixture.git(&["config", "commit.gpgSign", "true"]);
        } else {
            std::fs::write(fixture.path().join(".git/hooks/pre-commit"), "#!/bin/sh\n")
                .expect("write hook");
        }
        let preview = preflight(
            &fixture,
            &source,
            &destination,
            GitNativeIntegrationMode::FastForward,
        );
        assert!(matches!(
            preview.disposition,
            GitNativePreflightDisposition::Unsupported(_)
        ));
        assert!(preview.candidate_tree.is_none());
        assert_eq!(fixture.tip("main"), destination);
    }
}
