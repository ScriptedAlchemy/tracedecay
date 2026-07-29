//! Regression tests for finding #2: a long-running server that pins the
//! branch resolved at open time must not write the new branch's files into the
//! old branch's DB after a mid-session `git checkout`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;

use crate::common::fixture::{ClosedRegisteredProject, GitFixture, TestProfile};
use tracedecay::branch_meta::{BranchMeta, save_branch_meta};
use tracedecay::tracedecay::TraceDecay;

/// An indexed git project on `main`, enrolled in one fixture profile, with its
/// retained graph closed so a later reopen can pin the serving branch.
struct BranchDriftProject {
    repo: GitFixture,
    project: ClosedRegisteredProject,
}

impl BranchDriftProject {
    async fn indexed() -> Self {
        let profile = TestProfile::acquire().await;
        let repo = GitFixture::primary(profile.path("project"));
        fs::create_dir_all(repo.root().join("src")).unwrap();
        fs::write(repo.root().join("src/lib.rs"), "pub fn f() -> u32 { 1 }\n").unwrap();
        repo.commit_all("initial");
        let project = profile.enroll_indexed(repo.root()).await.close().await;
        Self { repo, project }
    }

    async fn indexed_tracking_main() -> Self {
        let fixture = Self::indexed().await;
        let meta = BranchMeta::new("main");
        save_branch_meta(fixture.project.data_root(), &meta).unwrap();
        fixture
    }

    fn root(&self) -> &std::path::Path {
        self.project.root()
    }

    fn data_root(&self) -> &std::path::Path {
        self.project.data_root()
    }

    fn open_options(&self) -> tracedecay::tracedecay::TraceDecayOpenOptions {
        self.project.open_options()
    }

    async fn reopen(&self) -> TraceDecay {
        self.project.reopen().await
    }
}

async fn close_graph(cg: TraceDecay) {
    cg.checkpoint().await.unwrap();
    cg.close();
}

#[tokio::test]
async fn sync_refuses_to_write_after_mid_session_branch_checkout() {
    let fixture = BranchDriftProject::indexed_tracking_main().await;

    // Reopen so the instance resolves and pins `main`.
    let cg = fixture.reopen().await;
    assert!(
        !cg.branch_drifted(),
        "no drift expected while still on the branch we opened"
    );
    assert_eq!(cg.serving_branch(), Some("main"));

    // Mid-session checkout to a different branch.
    fixture.repo.run(&["checkout", "-b", "feature"]);

    assert!(
        cg.branch_drifted(),
        "branch_drifted must detect the working tree moved to 'feature'"
    );

    let err = cg
        .sync()
        .await
        .expect_err("sync must refuse to write the old branch's DB after a checkout");
    let msg = err.to_string();
    assert!(
        msg.contains("feature") && msg.contains("main"),
        "drift error should name both branches, got: {msg}"
    );

    // Reopening rebinds to the live branch and clears the drift.
    let reopened = cg.reopen_for_current_branch().await.unwrap();
    assert!(!reopened.branch_drifted());
    close_graph(reopened).await;
    close_graph(cg).await;
}

#[tokio::test]
async fn no_drift_and_sync_allowed_while_on_opened_branch() {
    let fixture = BranchDriftProject::indexed().await;

    let cg = fixture.reopen().await;

    // Still on the branch we opened: no drift, writes proceed normally.
    assert!(!cg.branch_drifted());
    fs::write(fixture.root().join("src/lib.rs"), "pub fn f() -> u32 { 2 }\n").unwrap();
    cg.sync()
        .await
        .expect("sync on the opened branch must not be blocked");
    close_graph(cg).await;
}

#[tokio::test]
async fn sync_allowed_in_single_db_mode_without_git() {
    // No git repo => no default branch detected => no branch metadata =>
    // single-DB mode (serving_branch == None), exempt from the drift guard.
    // Deliberately not a GitFixture: the scenario is the absent repository.
    let profile = TestProfile::acquire().await;
    let project_root = profile.path("project");
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(project_root.join("src/lib.rs"), "pub fn f() -> u32 { 1 }\n").unwrap();
    let project = profile.enroll_indexed(&project_root).await.close().await;

    let cg = project.reopen().await;
    assert_eq!(cg.serving_branch(), None);
    assert!(!cg.branch_drifted());

    fs::write(project.root().join("src/lib.rs"), "pub fn f() -> u32 { 9 }\n").unwrap();
    cg.sync()
        .await
        .expect("single-DB mode sync must never be blocked by the drift guard");
    close_graph(cg).await;
}

#[tokio::test]
async fn branch_diagnostics_reports_stale_open_and_serving_state_after_checkout() {
    let fixture = BranchDriftProject::indexed_tracking_main().await;

    let cg = fixture.reopen().await;
    fixture.repo.run(&["checkout", "-b", "feature"]);

    let diagnostics = cg.branch_diagnostics();
    assert_eq!(diagnostics.open_active_branch.as_deref(), Some("main"));
    assert_eq!(diagnostics.current_branch.as_deref(), Some("feature"));
    assert_eq!(diagnostics.serving_branch.as_deref(), Some("main"));
    assert!(diagnostics.branch_drifted);
    assert_eq!(diagnostics.branch_resolution, "stale_serving_branch");
    assert!(
        diagnostics
            .warnings
            .iter()
            .any(|warning| warning.contains("feature") && warning.contains("main")),
        "expected branch-drift warning naming both branches, got: {:?}",
        diagnostics.warnings
    );
    close_graph(cg).await;
}

#[tokio::test]
async fn branch_diagnostics_reports_auto_tracked_live_branch() {
    let fixture = BranchDriftProject::indexed_tracking_main().await;

    fixture.repo.run(&["checkout", "-b", "feature/untracked"]);

    let cg = fixture.reopen().await;
    let diagnostics = cg.branch_diagnostics();
    assert!(!diagnostics.is_fallback);
    assert_eq!(diagnostics.branch_resolution, "exact");
    assert_eq!(
        diagnostics.current_branch.as_deref(),
        Some("feature/untracked")
    );
    assert_eq!(diagnostics.fallback_target, None);
    assert_eq!(diagnostics.nearest_tracked_ancestor, None);
    assert_eq!(
        diagnostics.serving_branch.as_deref(),
        Some("feature/untracked")
    );
    assert!(diagnostics.live_branch_tracked);
    assert_eq!(diagnostics.live_branch_db_exists, Some(true));
    close_graph(cg).await;
}

#[tokio::test]
async fn open_repairs_missing_tracked_branch_db_before_diagnostics() {
    let fixture = BranchDriftProject::indexed_tracking_main().await;

    fixture.repo.run(&["checkout", "-b", "feature/tracked"]);
    fs::write(fixture.root().join("src/lib.rs"), "pub fn f() -> u32 { 2 }\n").unwrap();
    fs::write(
        fixture.root().join("src/tracked_only.rs"),
        "pub fn tracked_only() {}\n",
    )
    .unwrap();
    fixture.repo.commit_all("feature");

    // Track the branch by writing its metadata entry directly instead of
    // going through TraceDecay::add_branch_tracking, which would build and
    // sync a branch DB only for the test to delete it again. The repair
    // under test keys purely off "tracked in metadata, DB file missing", so
    // the state is identical and the fixture skips a whole DB build.
    let tracedecay_dir = fixture.data_root();
    let mut meta = tracedecay::branch_meta::load_branch_meta(tracedecay_dir).unwrap();
    let stem = tracedecay::branch::sanitize_branch_name("feature/tracked");
    meta.add_branch("feature/tracked", &format!("branches/{stem}.db"), "main");
    save_branch_meta(tracedecay_dir, &meta).unwrap();
    let feature_db =
        tracedecay::branch::resolve_branch_db_path(tracedecay_dir, "feature/tracked", &meta)
            .unwrap();
    assert!(
        !feature_db.exists(),
        "tracked branch DB must be missing before the repair-on-open under test"
    );

    let cg = fixture.reopen().await;
    let diagnostics = cg.branch_diagnostics();
    assert!(diagnostics.live_branch_tracked);
    assert_eq!(diagnostics.live_branch_db_exists, Some(true));
    assert!(!diagnostics.is_fallback);
    assert_eq!(diagnostics.fallback_target, None);
    assert_eq!(
        diagnostics.serving_branch.as_deref(),
        Some("feature/tracked")
    );
    assert!(
        diagnostics.warnings.is_empty(),
        "expected auto-repaired branch DB without warnings, got: {:?}",
        diagnostics.warnings
    );
    assert!(
        !cg.search("tracked_only", 10).await.unwrap().is_empty(),
        "repaired branch DB should be synced with branch-only symbols"
    );
    close_graph(cg).await;
}

#[tokio::test]
async fn branch_serving_instance_writes_facts_to_the_project_wide_store() {
    let fixture = BranchDriftProject::indexed().await;
    // A tracked non-default branch resolves to its own shard database; the
    // default branch serves the project store directly.
    fixture.repo.run(&["checkout", "-b", "feature"]);
    TraceDecay::add_branch_tracking_with_options(fixture.root(), "feature", fixture.open_options())
        .await
        .unwrap();

    let cg = fixture.reopen().await;
    assert_eq!(cg.serving_branch(), Some("feature"));
    let branch_db_path = cg.db_path();
    let project_db_path = cg.store_layout().graph_db_path.clone();
    assert_ne!(
        branch_db_path, project_db_path,
        "fixture must serve a branch shard distinct from the project store"
    );

    // Project facts are project-wide: writing through a branch-serving
    // instance must land in the shared project store, not the branch shard.
    let outcome = cg
        .add_fact(tracedecay::memory::types::AddFactRequest {
            content: "Branch shards must not fork the project fact store".to_string(),
            category: tracedecay::memory::types::MemoryCategory::Project,
            source: Some("branch-shard-regression".to_string()),
            tags: Vec::new(),
            entities: Vec::new(),
            trust: Some(0.8),
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    assert!(outcome.fact.is_some(), "fact must be accepted");
    let facts = cg
        .search_facts(tracedecay::memory::types::SearchFactsRequest {
            query: "project fact store".to_string(),
            category: None,
            limit: Some(5),
            min_trust: None,
            include_why: false,
        })
        .await
        .unwrap();
    assert_eq!(facts.len(), 1, "branch-serving search must see the fact");
    close_graph(cg).await;

    let count = |path: PathBuf| async move {
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM memory_v2_current_facts", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    };
    assert_eq!(
        count(project_db_path).await,
        1,
        "the canonical fact must live in the project-wide store"
    );
    assert_eq!(
        count(branch_db_path).await,
        0,
        "the branch shard must not fork the project fact store"
    );
}
