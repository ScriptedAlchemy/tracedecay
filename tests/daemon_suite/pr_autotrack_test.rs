//! End-to-end lifecycle tests for daemon PR-branch auto-tracking.
//!
//! These drive the real discovery + reconcile path against a fixture repo whose
//! `origin` is a local bare repo carrying `refs/pull/N/head` refs (created with
//! `git update-ref`), exactly the shape GitHub exposes. Same-repo PRs are tracked
//! through the normal branch machinery (fetch → owned synthetic worktree →
//! `add_branch_tracking`); fork PRs (a `refs/pull/N/head` whose SHA matches no
//! origin head) are skipped. The tests assert: a PR branch is tracked and its
//! *own* content is indexed, a second poll is a no-op, closing the PR untracks it
//! and cleans the store + worktree, and the per-cycle new-track cap ramps.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Output;
use std::sync::Arc;

use crate::common::fixture::{GitFixture, RegisteredProject, TestProfile, git_run};
use fs2::FileExt;
use tracedecay::application::memory::{MemoryApplication, MemoryOperationContext};
use tracedecay::branch_meta::{load_branch_meta, save_branch_meta};
use tracedecay::daemon::pr_autotrack;
use tracedecay::memory::types::{AddFactRequest, MemoryCategory};
use tracedecay::store::memory::DatabaseFactStore;
use tracedecay::tracedecay::TraceDecay;
use tracedecay_domain::{FactOwnerV1, ProjectId};

/// Deletes the `.tracedecay-test-profile-*.db` family that the canonical test
/// runtime publishes beside a non-profile database it opens.
///
/// A production store has exactly one `*.db` per tracked branch under
/// `branches/`, and the project-memory cutover treats every `*.db` there as a
/// branch memory source. Leaving the harness's sidecar behind would invent a
/// schema-less extra source and make the cutover refuse.
fn remove_test_runtime_profile_sidecars(database_path: &Path) {
    let directory = database_path
        .parent()
        .expect("branch database has a parent");
    for entry in fs::read_dir(directory).expect("branch directory is readable") {
        let path = entry.expect("branch directory entry is readable").path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".tracedecay-test-profile-"))
        {
            fs::remove_file(&path).expect("test-runtime profile sidecar is removable");
        }
    }
}

/// An indexed project on `main` with a local bare `origin` it has been pushed
/// to, registered and enrolled in one fixture profile.
///
/// Every git and store operation goes through this handle, so a test cannot
/// name the wrong checkout, resolve a second store layout, or reopen a branch
/// graph in a different profile.
struct PrProject {
    repo: GitFixture,
    origin: PathBuf,
    project: RegisteredProject,
}

impl PrProject {
    async fn indexed_with_origin() -> Self {
        let profile = TestProfile::acquire().await;
        let repo = GitFixture::primary(profile.path("project"));
        fs::create_dir_all(repo.root().join("src")).unwrap();
        fs::write(repo.root().join("src/lib.rs"), "pub fn on_main() {}\n").unwrap();
        repo.commit_all("initial commit");

        let project = profile.enroll_indexed(repo.root()).await;
        // A sibling bare repo acts as `origin`; it lives as long as the profile's
        // scratch directory.
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

    /// The store this project's graph actually wrote, never re-resolved.
    fn data_root(&self) -> &Path {
        self.project.data_root()
    }

    fn graph(&self) -> &Arc<TraceDecay> {
        self.project.graph()
    }

    /// Writes one durable project-memory fact into a *branch* store through
    /// the ordinary production write path — the owner-bound compatibility fact
    /// authority the daemon itself writes through — rather than by
    /// hand-inserting a raw `memory_facts` row.
    ///
    /// The distinction is the whole point of the fixture: a hand-written legacy
    /// row carries no Memory V2 authority, so the surviving archive merge in
    /// `memory_cutover::apply_for_retained_project` has nothing to carry into
    /// project memory. Seeding through `add_fact_v1` produces exactly the rows
    /// a real branch-local fact has (V2 identity, assertion, lineage, legacy
    /// map, and the compatibility `memory_facts` projection), so branch
    /// retirement is exercised against production-shaped state and the "branch
    /// retirement never loses memory" contract is what the assertions actually
    /// observe. Returns the fact's compatibility id — the id project memory
    /// must still resolve after the branch store is gone.
    ///
    /// The fact is written under this project's own memory owner, the only
    /// owner the cutover receipt accepts an archive proof for.
    ///
    /// Writing needs exclusive ownership of the branch family, which the
    /// branch-graph open does not grant (it publishes a shared read-only
    /// connection). The fixture therefore takes explicit test authority over
    /// the branch database and drops the synthetic profile sidecar that the
    /// test runtime leaves beside it — the store must be back in its exact
    /// production shape before the cutover planner enumerates `branches/`.
    async fn seed_branch_only_fact(&self, branch: &str, content: &str) -> i64 {
        let owner = FactOwnerV1::Project {
            project_id: ProjectId::new(self.project.project_id().to_owned()).unwrap(),
        };
        let branch_database = self.data_root().join(
            &load_branch_meta(self.data_root())
                .expect("branch meta exists")
                .branches[branch]
                .db_file,
        );
        let (database, _) = crate::common::open_test_database(&branch_database)
            .await
            .expect("branch store opens for the fixture seed write");
        let fact = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&database))
            .unwrap()
            .add_fact_v1(
                AddFactRequest {
                    content: content.to_owned(),
                    category: MemoryCategory::Project,
                    source: Some("pr-branch".to_owned()),
                    tags: vec!["branch-cutover".to_owned()],
                    entities: Vec::new(),
                    trust: Some(0.9),
                    metadata: serde_json::json!({ "fixture": "production-pr-autotrack" }),
                },
                MemoryOperationContext::generated(&owner, "seed branch-only fact", None).unwrap(),
            )
            .await
            .unwrap()
            .fact
            .expect("the branch-only fixture fact must be stored");
        // The cutover snapshots the branch family from disk, so the seed has to
        // be durable in the main database before the reconcile that reads it.
        database.checkpoint().await.unwrap();
        database.close();
        remove_test_runtime_profile_sidecars(&branch_database);
        fact.fact_id
    }

    fn git(&self, args: &[&str]) {
        self.repo.run(args);
    }

    fn git_out(&self, args: &[&str]) -> Output {
        self.repo.output(args)
    }

    fn git_capture(&self, args: &[&str]) -> String {
        self.repo.capture(args)
    }

    /// Runs git inside the bare `origin` rather than the checkout.
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

    /// Creates a same-repo PR: a new branch on `origin` with unique content, plus
    /// a matching `refs/pull/<n>/head`. Returns the PR head branch name. Leaves
    /// the project checked out on `main`.
    fn add_same_repo_pr(&self, n: u64, symbol: &str) -> String {
        let branch = format!("feature-{n}");
        self.git(&["checkout", "-b", &branch, "main"]);
        fs::write(
            self.root().join(format!("src/pr_{n}.rs")),
            format!("pub fn {symbol}() {{}}\n"),
        )
        .unwrap();
        self.repo.commit_all(&format!("PR {n} content"));
        self.git(&["push", "origin", &branch]);
        // Mirror GitHub's refs/pull/<n>/head at the branch tip.
        self.origin_git(&[
            "update-ref",
            &format!("refs/pull/{n}/head"),
            &format!("refs/heads/{branch}"),
        ]);
        self.git(&["checkout", "main"]);
        self.git(&["branch", "-D", &branch]);
        branch
    }

    /// Creates a fork PR: a `refs/pull/<n>/head` on `origin` whose SHA matches no
    /// origin head (so discovery classifies it as a fork).
    fn add_fork_pr(&self, n: u64, symbol: &str) {
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
        // Push the bare commit object to origin under the pull ref only — no head.
        self.git(&["push", "origin", &format!("{sha}:refs/pull/{n}/head")]);
    }

    /// Advances an existing same-repo PR's head: adds a commit carrying `symbol`
    /// on top of `origin/feature-<n>`, pushes it, and re-points
    /// `refs/pull/<n>/head` at the new tip. Leaves the project on `main`.
    fn bump_pr(&self, n: u64, symbol: &str) {
        let branch = format!("feature-{n}");
        self.git(&["checkout", "-B", &branch, &format!("origin/{branch}")]);
        fs::write(
            self.root().join(format!("src/pr_{n}_{symbol}.rs")),
            format!("pub fn {symbol}() {{}}\n"),
        )
        .unwrap();
        self.repo.commit_all(&format!("bump PR {n}"));
        self.git(&["push", "origin", &branch]);
        self.origin_git(&[
            "update-ref",
            &format!("refs/pull/{n}/head"),
            &format!("refs/heads/{branch}"),
        ]);
        self.git(&["checkout", "main"]);
        self.git(&["branch", "-D", &branch]);
    }
}

#[tokio::test]
async fn production_reconciliation_publishes_managed_pr_ref() {
    let fixture = PrProject::indexed_with_origin().await;
    fixture.add_same_repo_pr(1, "managed_ref_symbol");
    let discovery = fixture.discover();

    let report = fixture.reconcile(&discovery, 10).await;

    assert_eq!(report.tracked, vec!["tracedecay/autotrack/pr/1".to_owned()]);
    assert!(
        fixture
            .git_out(&["rev-parse", "--verify", "refs/tracedecay/pr/1"])
            .status
            .success()
    );
}

#[tokio::test]
async fn tracks_same_repo_pr_indexes_its_content_and_untracks_on_close() {
    let fixture = PrProject::indexed_with_origin().await;
    let branch_only_content = "PR branch retirement preserves this project-memory fact identity";
    let head_branch = fixture.add_same_repo_pr(1, "pr_one_symbol");
    fixture.add_fork_pr(2, "fork_symbol");
    fixture.git(&["branch", "pr/1", "main"]);
    let user_branch_sha = fixture.git_capture(&["rev-parse", "refs/heads/pr/1"]);

    let data_root = fixture.data_root().to_path_buf();
    let tracking_label = "tracedecay/autotrack/pr/1";

    // Discovery: PR 1 is a tracked same-repo PR; PR 2 is a skipped fork.
    let discovery = fixture.discover();
    assert_eq!(discovery.open.len(), 1, "one same-repo PR expected");
    assert_eq!(discovery.open[0].number, 1);
    assert_eq!(discovery.open[0].head_branch, head_branch);
    assert!(!discovery.open[0].head_sha.is_empty());
    assert_eq!(discovery.skipped_forks, vec![2]);

    // Reconcile → PR 1 tracked.
    let report = fixture.reconcile(&discovery, 10).await;
    assert_eq!(report.tracked, vec![tracking_label.to_string()]);
    assert_eq!(report.skipped_forks, vec![2]);

    // Branch metadata + state reflect the tracked PR.
    let meta = load_branch_meta(&data_root).expect("branch meta exists");
    assert!(
        meta.is_tracked(tracking_label),
        "the internal PR ref should be a tracked branch"
    );
    let summary = pr_autotrack::managed_summary(&data_root);
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].pr, 1);
    assert_eq!(summary[0].head_branch, head_branch);
    let user_branch_after_track = fixture.git_capture(&["rev-parse", "refs/heads/pr/1"]);
    assert_eq!(user_branch_after_track, user_branch_sha);

    // Seed a legacy-only row into the real production-created branch database,
    // matching the checked-in pre-cutover fixtures. It exists nowhere in the
    // project store before the head-update cleanup.
    let tracked_entry = meta.branches.get(tracking_label).unwrap();
    let tracked_database = data_root.join(&tracked_entry.db_file);
    assert_eq!(
        rusqlite::Connection::open_with_flags(
            &tracked_database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE name = 'pr_one_symbol'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        1,
        "pr/1 store should contain the PR head's symbol (indexed from its worktree)"
    );
    let branch_fact_id = fixture
        .seed_branch_only_fact(tracking_label, branch_only_content)
        .await;
    let branch_fact = rusqlite::Connection::open_with_flags(
        &tracked_database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(
        "SELECT fact_id, content FROM memory_facts WHERE content = ?1",
        [branch_only_content],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
    )
    .unwrap();
    assert_eq!(
        branch_fact,
        (branch_fact_id, branch_only_content.to_owned())
    );
    assert!(
        fixture
            .graph()
            .list_facts(None, Some(0.0), 100)
            .await
            .unwrap()
            .iter()
            .all(|fact| fact.content != branch_only_content)
    );

    // Idempotent: a second reconcile with the same discovery changes nothing.
    let again = fixture.reconcile(&discovery, 10).await;
    assert!(again.tracked.is_empty(), "no re-track on repeat poll");
    assert!(again.untracked.is_empty());
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 1);

    // Advance the remote PR head. Reconciliation must rebuild and sync the
    // managed graph instead of serving the previous commit indefinitely.
    fixture.git(&[
        "checkout",
        "-b",
        &head_branch,
        &format!("origin/{head_branch}"),
    ]);
    fs::write(
        fixture.root().join("src/pr_1_updated.rs"),
        "pub fn pr_one_updated_symbol() {}\n",
    )
    .unwrap();
    fixture.repo.commit_all("update PR 1 content");
    fixture.git(&["push", "origin", &head_branch]);
    fixture.origin_git(&[
        "update-ref",
        "refs/pull/1/head",
        &format!("refs/heads/{head_branch}"),
    ]);
    fixture.git(&["checkout", "main"]);
    fixture.git(&["branch", "-D", &head_branch]);

    let refreshed_discovery = fixture.discover();
    assert_ne!(
        refreshed_discovery.open[0].head_sha,
        discovery.open[0].head_sha
    );
    let refreshed = fixture.reconcile(&refreshed_discovery, 10).await;
    assert_eq!(refreshed.tracked, vec![tracking_label.to_string()]);
    let refreshed_database =
        data_root.join(&load_branch_meta(&data_root).unwrap().branches[tracking_label].db_file);
    assert_eq!(
        rusqlite::Connection::open_with_flags(
            refreshed_database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE name = 'pr_one_updated_symbol'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        1,
        "changed PR head must replace the stale branch graph",
    );
    let restored = fixture
        .graph()
        .list_facts(None, Some(0.0), 100)
        .await
        .unwrap()
        .into_iter()
        .find(|fact| fact.content == branch_only_content)
        .expect("head refresh must cut branch-only memory over before retirement");
    assert_eq!(restored.content, branch_only_content);
    let restored_fact_id = restored.fact_id;

    // Close PR 1 (delete its pull ref) → discovery no longer lists it → untrack.
    let canonical_branch_paths = load_branch_meta(&data_root)
        .unwrap()
        .branches
        .values()
        .filter(|entry| entry.db_file.starts_with("branches/"))
        .map(|entry| entry.db_file.clone())
        .collect::<std::collections::BTreeSet<_>>();
    fixture.origin_git(&["update-ref", "-d", "refs/pull/1/head"]);
    let after_close = fixture.discover();
    assert!(
        after_close.open.iter().all(|p| p.number != 1),
        "closed PR must not be discovered"
    );
    let closing = fixture.reconcile(&after_close, 10).await;
    assert_eq!(closing.untracked, vec![tracking_label.to_string()]);

    let meta = load_branch_meta(&data_root).expect("branch meta exists");
    assert!(!meta.is_tracked(tracking_label));
    assert!(pr_autotrack::managed_summary(&data_root).is_empty());
    assert!(
        !data_root.join("pr-worktrees/pr-1").exists(),
        "worktree should be removed on untrack"
    );
    let user_branch_after_close = fixture.git_capture(&["rev-parse", "refs/heads/pr/1"]);
    assert_eq!(user_branch_after_close, user_branch_sha);
    let retired_fact = fixture
        .graph()
        .get_fact(restored_fact_id)
        .await
        .unwrap()
        .expect("branch retirement must preserve the fact's canonical identity");
    assert_eq!(retired_fact.content, branch_only_content);

    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(data_root.join("memory-branch-cutover.json")).unwrap())
            .unwrap();
    let covered = receipt["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| {
            assert!(
                source["generation"]
                    .as_str()
                    .is_some_and(|generation| generation.starts_with("sha256:"))
            );
            source["relative_path"].as_str().unwrap().to_owned()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        covered, canonical_branch_paths,
        "the production receipt must cover every canonical branch family"
    );
}

/// A contended coordinator removal must leave every owned artifact in place so
/// the following poll can retry safely instead of serving an untracked state
/// whose worktree/ref was already deleted.
#[tokio::test]
async fn busy_untrack_retains_managed_state_until_a_later_poll_succeeds() {
    let fixture = PrProject::indexed_with_origin().await;
    let retry_content = "Retried PR cleanup merges this fact exactly once";
    fixture.add_same_repo_pr(6, "pr_six_symbol");
    let data_root = fixture.data_root().to_path_buf();
    let label = "tracedecay/autotrack/pr/6";
    let worktree = data_root.join("pr-worktrees/pr-6");
    let discovery = fixture.discover();
    let tracked = fixture.reconcile(&discovery, 10).await;
    assert_eq!(tracked.tracked, vec![label.to_string()]);
    fixture.seed_branch_only_fact(label, retry_content).await;

    // The branch-administration coordinator takes this same metadata lock. It
    // must fail closed while another mutation owns it, not delete Git artifacts
    // after an unsuccessful store removal.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(data_root.join(".branch-add.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();

    let busy = fixture
        .reconcile(&pr_autotrack::PrDiscovery::default(), 10)
        .await;
    assert!(busy.untracked.is_empty());
    assert!(
        busy.failures.iter().any(|(branch, _)| branch == label),
        "the failed coordinator removal must be reported"
    );
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 1);
    assert!(load_branch_meta(&data_root).unwrap().is_tracked(label));
    assert!(worktree.exists(), "busy removal must retain its worktree");
    assert!(
        fixture
            .git_out(&[
                "rev-parse",
                "--verify",
                "refs/heads/tracedecay/autotrack/pr/6",
            ])
            .status
            .success(),
        "busy removal must retain its synthetic branch"
    );
    assert!(
        fixture
            .git_out(&["rev-parse", "--verify", "refs/tracedecay/pr/6"])
            .status
            .success(),
        "busy removal must retain its owned fetch ref"
    );
    let restored_on_busy = fixture
        .graph()
        .list_facts(None, Some(0.0), 100)
        .await
        .unwrap()
        .into_iter()
        .find(|fact| fact.content == retry_content)
        .expect("the cutover completes before fail-closed branch cleanup");

    lock.unlock().unwrap();

    let retried = fixture
        .reconcile(&pr_autotrack::PrDiscovery::default(), 10)
        .await;
    assert_eq!(retried.untracked, vec![label.to_string()]);
    assert!(pr_autotrack::managed_summary(&data_root).is_empty());
    assert!(!load_branch_meta(&data_root).unwrap().is_tracked(label));
    assert!(!worktree.exists(), "successful retry removes the worktree");
    assert!(
        !fixture
            .git_out(&[
                "rev-parse",
                "--verify",
                "refs/heads/tracedecay/autotrack/pr/6",
            ])
            .status
            .success(),
        "successful retry removes the synthetic branch"
    );
    assert!(
        !fixture
            .git_out(&["rev-parse", "--verify", "refs/tracedecay/pr/6"])
            .status
            .success(),
        "successful retry removes the owned fetch ref"
    );
    let matching_facts = fixture
        .graph()
        .list_facts(None, Some(0.0), 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|fact| fact.content == retry_content)
        .collect::<Vec<_>>();
    assert_eq!(
        matching_facts.len(),
        1,
        "cutover retries must be idempotent"
    );
    assert_eq!(matching_facts[0].fact_id, restored_on_busy.fact_id);
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn target_durability_failure_blocks_cleanup_and_retry_is_idempotent() {
    let fixture = PrProject::indexed_with_origin().await;
    let content = "Target durability failure keeps this fact readable";
    fixture.add_same_repo_pr(16, "pr_sixteen_symbol");
    let data_root = fixture.data_root().to_path_buf();
    let label = "tracedecay/autotrack/pr/16";
    let discovery = fixture.discover();
    let tracked = fixture.reconcile(&discovery, 10).await;
    assert_eq!(tracked.tracked, vec![label.to_string()]);
    let branch = load_branch_meta(&data_root).unwrap().branches[label].clone();
    let database = data_root.join(&branch.db_file);
    fixture.seed_branch_only_fact(label, content).await;
    fixture.origin_git(&["update-ref", "-d", "refs/pull/16/head"]);
    let closed = fixture.discover();

    tracedecay::migrate::memory_cutover::set_cutover_fault_for_test(
        tracedecay::migrate::memory_cutover::CutoverFaultForTest::TargetDurabilityBarrier,
    );
    let blocked = fixture.reconcile(&closed, 10).await;
    assert!(blocked.untracked.is_empty());
    assert!(load_branch_meta(&data_root).unwrap().is_tracked(label));
    assert!(database.is_file());
    assert!(!data_root.join("memory-branch-cutover.json").exists());
    assert_eq!(
        fixture
            .graph()
            .list_facts(None, Some(0.0), 100)
            .await
            .unwrap()
            .iter()
            .filter(|fact| fact.content == content)
            .count(),
        1,
        "the committed merge remains intact before receipt publication"
    );

    let retried = fixture.reconcile(&closed, 10).await;
    assert_eq!(retried.untracked, vec![label.to_string()]);
    assert!(!database.exists());
    assert_eq!(
        fixture
            .graph()
            .list_facts(None, Some(0.0), 100)
            .await
            .unwrap()
            .iter()
            .filter(|fact| fact.content == content)
            .count(),
        1,
        "retry must not duplicate the merged fact"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn receipt_durability_failure_blocks_cleanup_until_durable_retry() {
    let fixture = PrProject::indexed_with_origin().await;
    let content = "Receipt durability failure keeps this fact readable";
    fixture.add_same_repo_pr(17, "pr_seventeen_symbol");
    let data_root = fixture.data_root().to_path_buf();
    let label = "tracedecay/autotrack/pr/17";
    let discovery = fixture.discover();
    let tracked = fixture.reconcile(&discovery, 10).await;
    assert_eq!(tracked.tracked, vec![label.to_string()]);
    let branch = load_branch_meta(&data_root).unwrap().branches[label].clone();
    let database = data_root.join(&branch.db_file);
    fixture.seed_branch_only_fact(label, content).await;
    fixture.origin_git(&["update-ref", "-d", "refs/pull/17/head"]);
    let closed = fixture.discover();

    tracedecay::migrate::memory_cutover::set_cutover_fault_for_test(
        tracedecay::migrate::memory_cutover::CutoverFaultForTest::ReceiptDurability,
    );
    let blocked = fixture.reconcile(&closed, 10).await;
    assert!(blocked.untracked.is_empty());
    assert!(load_branch_meta(&data_root).unwrap().is_tracked(label));
    assert!(database.is_file());
    assert!(!data_root.join("memory-branch-cutover.json").exists());

    tracedecay::migrate::memory_cutover::set_cutover_fault_for_test(
        tracedecay::migrate::memory_cutover::CutoverFaultForTest::ReceiptAfterRename,
    );
    let blocked_after_rename = fixture.reconcile(&closed, 10).await;
    assert!(blocked_after_rename.untracked.is_empty());
    assert!(load_branch_meta(&data_root).unwrap().is_tracked(label));
    assert!(database.is_file());
    assert!(
        !data_root.join("memory-branch-cutover.json").exists(),
        "rename-to-parent-sync failure must roll back the unusable receipt"
    );

    let retried = fixture.reconcile(&closed, 10).await;
    assert_eq!(retried.untracked, vec![label.to_string()]);
    assert!(!database.exists());
    assert_eq!(
        fixture
            .graph()
            .list_facts(None, Some(0.0), 100)
            .await
            .unwrap()
            .iter()
            .filter(|fact| fact.content == content)
            .count(),
        1,
        "fact must remain readable after receipt-backed deletion"
    );
}

#[tokio::test]
async fn deferred_tracking_is_not_persisted_and_retries_next_cycle() {
    let fixture = PrProject::indexed_with_origin().await;
    fixture.add_same_repo_pr(7, "pr_seven_symbol");
    let data_root = fixture.data_root().to_path_buf();
    let discovery = fixture.discover();

    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(data_root.join(".branch-add.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();

    let deferred = fixture.reconcile(&discovery, 10).await;
    assert!(deferred.tracked.is_empty());
    assert!(pr_autotrack::managed_summary(&data_root).is_empty());
    assert!(
        data_root.join("pr-worktrees/pr-7").exists(),
        "contended rollback must retain its worktree for the next poll"
    );

    lock.unlock().unwrap();
    let retried = fixture.reconcile(&discovery, 10).await;
    assert_eq!(
        retried.tracked,
        vec!["tracedecay/autotrack/pr/7".to_string()]
    );
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 1);
}

#[tokio::test]
async fn complete_orphan_is_rebuilt_after_interrupted_state_write() {
    let fixture = PrProject::indexed_with_origin().await;
    fixture.add_same_repo_pr(8, "pr_eight_symbol");
    let data_root = fixture.data_root().to_path_buf();
    let discovery = fixture.discover();
    let label = "tracedecay/autotrack/pr/8";

    let first = fixture.reconcile(&discovery, 10).await;
    assert_eq!(first.tracked, vec![label.to_string()]);
    fs::remove_file(data_root.join("pr-autotrack.json")).unwrap();

    let recovered = fixture.reconcile(&discovery, 10).await;
    assert_eq!(recovered.tracked, vec![label.to_string()]);
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 1);
    assert!(load_branch_meta(&data_root).unwrap().is_tracked(label));
}

#[tokio::test]
async fn validated_branch_ref_orphan_without_worktree_is_rebuilt() {
    let fixture = PrProject::indexed_with_origin().await;
    fixture.add_same_repo_pr(11, "pr_eleven_symbol");
    let data_root = fixture.data_root().to_path_buf();
    let discovery = fixture.discover();
    let label = "tracedecay/autotrack/pr/11";
    let worktree = data_root.join("pr-worktrees/pr-11");

    let first = fixture.reconcile(&discovery, 10).await;
    assert_eq!(first.tracked, vec![label.to_string()]);
    let head_sha = discovery.open[0].head_sha.clone();
    let removed = fixture
        .reconcile(&pr_autotrack::PrDiscovery::default(), 10)
        .await;
    assert_eq!(removed.untracked, vec![label.to_string()]);
    fixture.git(&[
        "update-ref",
        &format!("refs/heads/{label}"),
        head_sha.as_str(),
    ]);
    assert!(!worktree.exists());

    let recovered = fixture.reconcile(&discovery, 10).await;
    assert_eq!(recovered.tracked, vec![label.to_string()]);
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 1);
    assert!(worktree.exists());
}

#[tokio::test]
async fn state_persistence_failure_rolls_back_completed_tracking() {
    let fixture = PrProject::indexed_with_origin().await;
    fixture.add_same_repo_pr(10, "pr_ten_symbol");
    let data_root = fixture.data_root().to_path_buf();
    let discovery = fixture.discover();
    let label = "tracedecay/autotrack/pr/10";
    fs::create_dir(data_root.join("pr-autotrack.json")).unwrap();

    let report = fixture.reconcile(&discovery, 10).await;
    assert!(report.tracked.is_empty());
    assert!(
        report
            .failures
            .iter()
            .any(|(branch, reason)| branch == label && reason.contains("persist"))
    );
    assert!(!load_branch_meta(&data_root).unwrap().is_tracked(label));
    assert!(!data_root.join("pr-worktrees/pr-10").exists());
    assert!(
        !fixture
            .git_out(&[
                "rev-parse",
                "--verify",
                "refs/heads/tracedecay/autotrack/pr/10"
            ])
            .status
            .success()
    );
}

#[tokio::test]
async fn legacy_close_recovers_empty_head_sha_before_owned_ref_cleanup() {
    let fixture = PrProject::indexed_with_origin().await;
    fixture.add_same_repo_pr(9, "pr_nine_symbol");
    let data_root = fixture.data_root().to_path_buf();
    let discovery = fixture.discover();
    let current_label = "tracedecay/autotrack/pr/9";
    let legacy_label = "pr/9";
    let first = fixture.reconcile(&discovery, 10).await;
    assert_eq!(first.tracked, vec![current_label.to_string()]);

    let mut state = pr_autotrack::load_state(&data_root);
    let mut managed = state.managed.remove(current_label).unwrap();
    managed.head_sha.clear();
    state.managed.insert(legacy_label.to_string(), managed);
    fs::write(
        data_root.join("pr-autotrack.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();

    let mut meta = load_branch_meta(&data_root).unwrap();
    let entry = meta.remove_branch(current_label).unwrap();
    let parent = entry.parent.as_deref().unwrap_or("main");
    meta.add_branch(legacy_label, &entry.db_file, parent);
    save_branch_meta(&data_root, &meta).unwrap();
    let worktree = data_root.join("pr-worktrees/pr-9");
    git_run(&worktree, &["branch", "-m", legacy_label]);

    fixture.origin_git(&["update-ref", "-d", "refs/pull/9/head"]);
    let closed = fixture.discover();
    let report = fixture.reconcile(&closed, 10).await;
    assert_eq!(report.untracked, vec![legacy_label.to_string()]);
    assert!(
        !load_branch_meta(&data_root)
            .unwrap()
            .is_tracked(legacy_label)
    );
    assert!(!worktree.exists());
    assert!(
        !fixture
            .git_out(&["rev-parse", "--verify", "refs/tracedecay/pr/9"])
            .status
            .success(),
        "owned fetch ref must be removed after recovering its legacy SHA"
    );
    assert!(
        fixture
            .git_out(&["rev-parse", "--verify", "refs/heads/pr/9"])
            .status
            .success(),
        "ambiguous legacy local branch must be preserved"
    );
}

#[tokio::test]
async fn caps_new_tracks_per_cycle_and_ramps() {
    let fixture = PrProject::indexed_with_origin().await;
    fixture.add_same_repo_pr(1, "pr_one");
    fixture.add_same_repo_pr(2, "pr_two");
    fixture.add_same_repo_pr(3, "pr_three");

    let data_root = fixture.data_root().to_path_buf();
    let discovery = fixture.discover();
    assert_eq!(discovery.open.len(), 3);

    // First cycle with cap=2 tracks only two and flags the cap.
    let first = fixture.reconcile(&discovery, 2).await;
    assert_eq!(first.tracked.len(), 2, "cap holds back the third PR");
    assert!(first.capped, "cap flag set when additions are held back");
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 2);

    // Second cycle tracks the remaining PR.
    let second = fixture.reconcile(&discovery, 2).await;
    assert_eq!(second.tracked.len(), 1);
    assert!(!second.capped);
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 3);
}

/// Finding 1: a *failed* discovery command must be distinguishable from "zero
/// open PRs". If `discover_open_prs` collapsed a git/gh failure into an empty
/// discovery, reconcile would mass-untrack every managed PR on one transient
/// network/credential blip. It must return `Err` instead so the daemon skips
/// the cycle and leaves the managed set intact.
#[tokio::test]
async fn failed_discovery_returns_error_and_never_mass_untracks() {
    let fixture = PrProject::indexed_with_origin().await;
    fixture.add_same_repo_pr(1, "pr_one_symbol");
    let data_root = fixture.data_root().to_path_buf();
    let label = "tracedecay/autotrack/pr/1";

    let discovery = fixture.discover();
    let report = fixture.reconcile(&discovery, 10).await;
    assert_eq!(report.tracked, vec![label.to_string()]);

    // Break `origin` so every ls-remote/gh discovery command fails.
    fixture.git(&["remote", "set-url", "origin", "/definitely/not/a/repo.git"]);

    let result = pr_autotrack::discover_open_prs(fixture.root());
    assert!(
        result.is_err(),
        "a failed discovery command must surface as Err, not an empty Ok"
    );
    // The daemon skips reconcile on Err, so the managed PR is untouched.
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 1);
    assert!(load_branch_meta(&data_root).unwrap().is_tracked(label));
}

/// Finding 2: daemon death mid-track leaves the synthetic branch at an old SHA
/// with no state and no DB. Once the PR head advances, the old `-b` path wedged
/// forever on "branch already exists" (and leaked the worktree). Tracking must
/// now be idempotent: adopt/reset the orphan branch to the new head and recover.
#[tokio::test]
async fn interrupted_track_with_advanced_head_recovers_instead_of_wedging() {
    let fixture = PrProject::indexed_with_origin().await;
    fixture.add_same_repo_pr(12, "pr_twelve_v1");
    let data_root = fixture.data_root().to_path_buf();
    let label = "tracedecay/autotrack/pr/12";
    let worktree = data_root.join("pr-worktrees/pr-12");

    let discovery = fixture.discover();
    let first = fixture.reconcile(&discovery, 10).await;
    assert_eq!(first.tracked, vec![label.to_string()]);

    // Reproduce the durable aftermath of a mid-track daemon crash without
    // mutating a database that remains mounted in this process: retire the
    // managed runtime through production cleanup, then restore only the stale
    // synthetic ref that would survive the interrupted state write.
    let old_head_sha = discovery.open[0].head_sha.clone();
    let removed = fixture
        .reconcile(&pr_autotrack::PrDiscovery::default(), 10)
        .await;
    assert_eq!(removed.untracked, vec![label.to_string()]);
    fixture.git(&[
        "update-ref",
        &format!("refs/heads/{label}"),
        old_head_sha.as_str(),
    ]);
    assert!(!worktree.exists());
    assert!(
        fixture
            .git_out(&[
                "rev-parse",
                "--verify",
                "refs/heads/tracedecay/autotrack/pr/12"
            ])
            .status
            .success(),
        "orphan synthetic branch must survive to reproduce the wedge"
    );

    // Advance the PR head so the orphan branch (old SHA) no longer matches.
    fixture.bump_pr(12, "pr_twelve_v2");
    let refreshed = fixture.discover();
    assert_ne!(refreshed.open[0].head_sha, discovery.open[0].head_sha);

    let recovered = fixture.reconcile(&refreshed, 10).await;
    assert_eq!(
        recovered.tracked,
        vec![label.to_string()],
        "an interrupted track with an advanced head must recover, not wedge"
    );
    assert!(worktree.exists(), "recovery re-creates the worktree");
    let cg = fixture.project.open_branch(label).await;
    assert!(
        !cg.search("pr_twelve_v2", 10).await.unwrap().is_empty(),
        "recovered branch graph serves the advanced head's content"
    );
}

/// Finding 4: the per-cycle new-track cap must bound only NEW tracks — an
/// already-managed PR whose head advanced must still be refreshed the same
/// cycle, even when its label sorts after the capped new PRs. (Managed PR 9
/// sorts lexically after new PRs 10/11/12.)
#[tokio::test]
async fn cap_does_not_starve_head_updates_for_managed_prs() {
    let fixture = PrProject::indexed_with_origin().await;
    fixture.add_same_repo_pr(9, "pr_nine_v1");
    let label = "tracedecay/autotrack/pr/9";

    let d0 = fixture.discover();
    let r0 = fixture.reconcile(&d0, 10).await;
    assert_eq!(r0.tracked, vec![label.to_string()]);

    // Open three NEW PRs (cap will be 2) and advance managed PR 9's head.
    fixture.add_same_repo_pr(10, "pr_ten");
    fixture.add_same_repo_pr(11, "pr_eleven");
    fixture.add_same_repo_pr(12, "pr_twelve");
    fixture.bump_pr(9, "pr_nine_v2");

    let d1 = fixture.discover();
    let r1 = fixture.reconcile(&d1, 2).await;

    assert!(r1.capped, "three new PRs under cap=2 flags the cap");
    assert!(
        r1.tracked.contains(&label.to_string()),
        "managed PR 9's head refresh must run despite the new-track cap"
    );
    let new_tracked = r1.tracked.iter().filter(|l| l.as_str() != label).count();
    assert_eq!(new_tracked, 2, "cap still bounds NEW tracks to 2");

    let cg = fixture.project.open_branch(label).await;
    assert!(
        !cg.search("pr_nine_v2", 10).await.unwrap().is_empty(),
        "the refreshed managed branch serves the advanced head, not the stale one"
    );
}
/// Finding 7: turning the feature off must tear down managed PR state rather
/// than stranding worktrees, refs, synthetic branches and stores forever.
#[tokio::test]
async fn teardown_removes_all_managed_state_on_disable() {
    let fixture = PrProject::indexed_with_origin().await;
    fixture.add_same_repo_pr(4, "pr_four_symbol");
    let data_root = fixture.data_root().to_path_buf();
    let label = "tracedecay/autotrack/pr/4";

    let discovery = fixture.discover();
    let report = fixture.reconcile(&discovery, 10).await;
    assert_eq!(report.tracked, vec![label.to_string()]);

    // The daemon runs this when it observes the feature disabled.
    pr_autotrack::teardown_disabled_project(Arc::clone(fixture.graph()), fixture.root()).await;

    assert!(pr_autotrack::managed_summary(&data_root).is_empty());
    assert!(!load_branch_meta(&data_root).unwrap().is_tracked(label));
    assert!(!data_root.join("pr-worktrees/pr-4").exists());
    assert!(
        !fixture
            .git_out(&[
                "rev-parse",
                "--verify",
                "refs/heads/tracedecay/autotrack/pr/4"
            ])
            .status
            .success(),
        "synthetic branch must be removed on teardown"
    );
    assert!(
        !fixture
            .git_out(&["rev-parse", "--verify", "refs/tracedecay/pr/4"])
            .status
            .success(),
        "owned fetch ref must be removed on teardown"
    );

    // Idempotent: a second teardown with nothing managed is a no-op.
    pr_autotrack::teardown_disabled_project(Arc::clone(fixture.graph()), fixture.root()).await;
    assert!(pr_autotrack::managed_summary(&data_root).is_empty());
}
