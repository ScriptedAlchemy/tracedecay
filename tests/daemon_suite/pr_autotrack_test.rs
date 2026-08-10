//! Production-boundary tests for PR discovery and scheduler admission.
//!
//! PR worktree activation is owned by the retained daemon code-index scheduler.
//! The public reconciliation boundary therefore fails closed until that
//! scheduler is injected; it must not fall back to the retired per-branch
//! SQLite graph implementation or mutate Git state before admission.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::common::fixture::{GitFixture, RegisteredProject, TestProfile, git_run};
use tracedecay::daemon::pr_autotrack;
use tracedecay::tracedecay::TraceDecay;

struct PrProject {
    repo: GitFixture,
    origin: PathBuf,
    project: RegisteredProject,
}

impl PrProject {
    async fn enrolled_with_origin() -> Self {
        let profile = TestProfile::acquire().await;
        let repo = GitFixture::primary(profile.path("project"));
        fs::create_dir_all(repo.root().join("src")).unwrap();
        fs::write(repo.root().join("src/lib.rs"), "pub fn on_main() {}\n").unwrap();
        repo.commit_all("initial commit");

        let project = profile.enroll(repo.root()).await;
        let origin = repo.with_bare_origin();

        Self {
            repo,
            origin,
            project,
        }
    }

    fn root(&self) -> &Path {
        self.project.root()
    }

    fn data_root(&self) -> &Path {
        self.project.data_root()
    }

    fn graph(&self) -> &Arc<TraceDecay> {
        self.project.graph()
    }

    fn git(&self, args: &[&str]) {
        self.repo.run(args);
    }

    fn origin_git(&self, args: &[&str]) {
        git_run(&self.origin, args);
    }

    fn discover(&self) -> pr_autotrack::PrDiscovery {
        pr_autotrack::discover_open_prs(self.root()).expect("PR discovery succeeds")
    }

    async fn reconcile(
        &self,
        discovery: &pr_autotrack::PrDiscovery,
        cap: usize,
    ) -> pr_autotrack::ReconcileReport {
        pr_autotrack::reconcile_project(
            Arc::clone(self.graph()),
            self.root(),
            self.data_root(),
            discovery,
            cap,
        )
        .await
    }

    fn add_same_repo_pr(&self, number: u64, symbol: &str) -> String {
        let branch = format!("feature-{number}");
        self.git(&["checkout", "-b", &branch, "main"]);
        fs::write(
            self.root().join(format!("src/pr_{number}.rs")),
            format!("pub fn {symbol}() {{}}\n"),
        )
        .unwrap();
        self.repo.commit_all(&format!("PR {number} content"));
        self.git(&["push", "origin", &branch]);
        self.origin_git(&[
            "update-ref",
            &format!("refs/pull/{number}/head"),
            &format!("refs/heads/{branch}"),
        ]);
        self.git(&["checkout", "main"]);
        self.git(&["branch", "-D", &branch]);
        branch
    }

    fn add_fork_pr(&self, number: u64, symbol: &str) {
        self.git(&["checkout", "-b", "tmp-fork", "main"]);
        fs::write(
            self.root().join("src/fork.rs"),
            format!("pub fn {symbol}() {{}}\n"),
        )
        .unwrap();
        self.repo.commit_all("fork content");
        let sha = self.repo.head_sha();
        self.git(&["checkout", "main"]);
        self.git(&["branch", "-D", "tmp-fork"]);
        fs::remove_file(self.root().join("src/fork.rs")).ok();
        self.git(&["push", "origin", &format!("{sha}:refs/pull/{number}/head")]);
    }
}

#[tokio::test]
async fn discovery_classifies_same_repo_and_fork_pull_heads() {
    let fixture = PrProject::enrolled_with_origin().await;
    let head_branch = fixture.add_same_repo_pr(1, "pr_one_symbol");
    fixture.add_fork_pr(2, "fork_symbol");

    let discovery = fixture.discover();

    assert_eq!(discovery.open.len(), 1);
    assert_eq!(discovery.open[0].number, 1);
    assert_eq!(discovery.open[0].head_branch, head_branch);
    assert!(!discovery.open[0].head_sha.is_empty());
    assert_eq!(discovery.skipped_forks, vec![2]);
}

#[tokio::test]
async fn reconciliation_without_scheduler_fails_before_git_or_state_mutation() {
    let fixture = PrProject::enrolled_with_origin().await;
    fixture.add_same_repo_pr(7, "pr_seven_symbol");
    let discovery = fixture.discover();

    let report = fixture.reconcile(&discovery, 10).await;

    assert!(report.tracked.is_empty());
    assert!(report.untracked.is_empty());
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].0, "project");
    assert!(
        report.failures[0]
            .1
            .starts_with("code_index_scheduler_unavailable:")
    );
    assert!(pr_autotrack::managed_summary(fixture.data_root()).is_empty());
    assert!(!fixture.data_root().join("pr-worktrees").exists());
    assert!(
        !fixture
            .repo
            .output(&["rev-parse", "--verify", "refs/tracedecay/pr/7"])
            .status
            .success()
    );
}

#[tokio::test]
async fn failed_discovery_is_not_reported_as_an_empty_success() {
    let fixture = PrProject::enrolled_with_origin().await;
    fixture.git(&["remote", "set-url", "origin", "/definitely/not/a/repo.git"]);

    let result = pr_autotrack::discover_open_prs(fixture.root());

    assert!(
        result.is_err(),
        "a failed discovery command must surface as Err so callers cannot interpret it as every PR closing"
    );
}
