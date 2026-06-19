use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;
use tracedecay::branch::{self, BranchAddOutcome};
use tracedecay::branch_meta::load_branch_meta;
use tracedecay::tracedecay::TraceDecay;

fn git(project: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(["-c", "core.hooksPath=.git/no-hooks"])
        .args(args)
        .current_dir(project)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit_all(project: &Path, message: &str) {
    git(project, &["add", "."]);
    git(
        project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay-test@example.com",
            "commit",
            "-m",
            message,
        ],
    );
}

async fn open_untracked_project() -> (TempDir, PathBuf, TraceDecay) {
    let dir = TempDir::new().unwrap();
    let project = dir.path().to_path_buf();

    git(&project, &["init", "-b", "main"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn indexed_on_main() {}\n").unwrap();
    commit_all(&project, "initial commit");

    let main = TraceDecay::init(&project).await.unwrap();
    main.index_all().await.unwrap();
    drop(main);

    git(&project, &["checkout", "-b", "feature/untracked"]);
    fs::write(
        project.join("src/untracked_only.rs"),
        "pub fn untracked_only() {}\n",
    )
    .unwrap();

    let feature = TraceDecay::open(&project).await.unwrap();
    assert_eq!(feature.active_branch(), Some("feature/untracked"));
    assert_eq!(feature.serving_branch(), Some("feature/untracked"));
    assert!(!feature.is_fallback());

    (dir, project, feature)
}

async fn open_detached_fallback_project() -> (TempDir, PathBuf, TraceDecay) {
    let dir = TempDir::new().unwrap();
    let project = dir.path().to_path_buf();

    git(&project, &["init", "-b", "main"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn indexed_on_main() {}\n").unwrap();
    commit_all(&project, "initial commit");

    let main = TraceDecay::init(&project).await.unwrap();
    main.index_all().await.unwrap();
    drop(main);

    git(&project, &["checkout", "--detach"]);

    fs::write(
        project.join("src/detached_only.rs"),
        "pub fn detached_only() {}\n",
    )
    .unwrap();

    let fallback = TraceDecay::open(&project).await.unwrap();
    assert_eq!(fallback.active_branch(), None);
    assert_eq!(fallback.serving_branch(), Some("main"));
    assert!(fallback.is_fallback());
    assert!(
        fallback
            .fallback_warning()
            .unwrap_or_default()
            .contains("detached HEAD"),
        "detached HEAD should explain the fallback branch"
    );

    (dir, project, fallback)
}

async fn assert_main_db_missing_symbol(project: &Path, symbol: &str, message: &str) {
    git(project, &["checkout", "main"]);
    let main = TraceDecay::open(project).await.unwrap();
    let results = main.search(symbol, 10).await.unwrap();
    assert!(results.is_empty(), "{message}");
}

fn assert_fallback_write_refused(err: impl std::fmt::Display) {
    let message = err.to_string();
    assert!(
        message.contains("fallback")
            && (message.contains("tracedecay branch add")
                || message.contains("Check out a tracked branch")),
        "unexpected error: {message}"
    );
}

#[tokio::test]
async fn open_auto_tracks_untracked_branch_and_syncs_its_db() {
    let (_dir, project, feature) = open_untracked_project().await;

    assert!(
        !feature
            .search("untracked_only", 10)
            .await
            .unwrap()
            .is_empty(),
        "auto-tracked branch should contain the branch-only symbol"
    );

    let meta = load_branch_meta(&project.join(".tracedecay")).unwrap();
    let feature_entry = meta
        .branches
        .get("feature/untracked")
        .expect("open should add the live branch to branch metadata");
    assert_eq!(feature_entry.parent.as_deref(), Some("main"));

    drop(feature);
    assert_main_db_missing_symbol(
        &project,
        "untracked_only",
        "auto-tracked branch sync must not index branch files into main DB",
    )
    .await;
}

#[tokio::test]
async fn sync_refuses_to_write_when_opened_on_fallback_branch() {
    let (_dir, project, fallback) = open_detached_fallback_project().await;

    let err = fallback.sync().await.unwrap_err();
    assert_fallback_write_refused(err);

    drop(fallback);
    assert_main_db_missing_symbol(
        &project,
        "detached_only",
        "fallback sync must not index detached files into main DB",
    )
    .await;
}

#[tokio::test]
async fn full_index_refuses_to_write_when_opened_on_fallback_branch() {
    let (_dir, project, fallback) = open_detached_fallback_project().await;

    let err = match fallback.index_all().await {
        Ok(_) => panic!("full index should refuse fallback writes"),
        Err(err) => err,
    };
    assert_fallback_write_refused(err);

    drop(fallback);
    assert_main_db_missing_symbol(
        &project,
        "detached_only",
        "fallback full index must not index detached files into main DB",
    )
    .await;
}

#[tokio::test]
async fn stale_sync_refuses_to_write_when_opened_on_fallback_branch() {
    let (_dir, project, fallback) = open_detached_fallback_project().await;

    let err = fallback
        .sync_if_stale(&["src/detached_only.rs".to_string()])
        .await
        .unwrap_err();
    assert_fallback_write_refused(err);

    drop(fallback);
    assert_main_db_missing_symbol(
        &project,
        "detached_only",
        "fallback stale sync must not index detached files into main DB",
    )
    .await;
}

#[tokio::test]
async fn silent_stale_sync_refuses_to_write_when_opened_on_fallback_branch() {
    let (_dir, project, fallback) = open_detached_fallback_project().await;

    let err = fallback
        .sync_if_stale_silent(&["src/detached_only.rs".to_string()])
        .await
        .unwrap_err();
    assert_fallback_write_refused(err);

    drop(fallback);
    assert_main_db_missing_symbol(
        &project,
        "detached_only",
        "fallback silent stale sync must not index detached files into main DB",
    )
    .await;
}

#[tokio::test]
async fn add_branch_tracking_copies_from_nearest_tracked_ancestor() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();

    git(project, &["init", "-b", "main"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    commit_all(project, "initial commit");

    let main = TraceDecay::init(project).await.unwrap();
    main.index_all().await.unwrap();
    main.set_tokens_saved(111).await.unwrap();
    main.checkpoint().await.unwrap();
    drop(main);

    git(project, &["checkout", "-b", "feature/parent"]);
    fs::write(
        project.join("src/feature_only.rs"),
        "pub fn feature_only() {}\n",
    )
    .unwrap();
    commit_all(project, "feature commit");

    let feature_outcome = branch::add_branch_tracking(project, "feature/parent")
        .await
        .unwrap();
    assert_eq!(feature_outcome, BranchAddOutcome::Added);

    let feature_cg = TraceDecay::open_branch(project, "feature/parent")
        .await
        .unwrap();
    feature_cg.set_tokens_saved(777).await.unwrap();
    feature_cg.checkpoint().await.unwrap();
    drop(feature_cg);

    git(project, &["checkout", "-b", "topic/child"]);
    fs::write(
        project.join("src/topic_only.rs"),
        "pub fn topic_only() {}\n",
    )
    .unwrap();
    commit_all(project, "topic commit");

    let topic_outcome = branch::add_branch_tracking(project, "topic/child")
        .await
        .unwrap();
    assert_eq!(topic_outcome, BranchAddOutcome::Added);

    let meta = load_branch_meta(&project.join(".tracedecay")).unwrap();
    let topic_entry = meta
        .branches
        .get("topic/child")
        .expect("topic branch should be recorded in branch metadata");
    assert_eq!(topic_entry.parent.as_deref(), Some("feature/parent"));

    let topic_cg = TraceDecay::open_branch(project, "topic/child")
        .await
        .unwrap();
    assert_eq!(
        topic_cg.get_tokens_saved().await.unwrap(),
        777,
        "new branch DB should inherit the nearest tracked ancestor's persisted metadata"
    );
    assert!(
        !topic_cg
            .search("feature_only", 10)
            .await
            .unwrap()
            .is_empty(),
        "topic branch DB should include symbols carried forward from the tracked ancestor"
    );
    assert!(
        !topic_cg.search("topic_only", 10).await.unwrap().is_empty(),
        "new branch tracking should still sync the current branch's own files"
    );
}
