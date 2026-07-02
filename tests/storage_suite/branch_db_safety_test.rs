use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use crate::common::{self, IsolatedEnv};
use tracedecay::branch::BranchAddOutcome;
use tracedecay::branch_meta::load_branch_meta;
use tracedecay::storage::resolve_layout_for_current_profile;
use tracedecay::tracedecay::TraceDecay;

// These tests resolve the profile layout from the live HOME without pinning
// it, so under threaded `cargo test` they must not overlap with suite
// modules whose guards mutate HOME/USERPROFILE mid-test.
use crate::support::HOME_ENV_LOCK;

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

fn project_data_dir(project: &Path) -> PathBuf {
    resolve_layout_for_current_profile(project)
        .unwrap_or_else(|err| panic!("failed to resolve test project storage layout: {err}"))
        .data_root
}

async fn open_untracked_project() -> (IsolatedEnv, PathBuf, TraceDecay) {
    let (env, project) = IsolatedEnv::acquire().await;

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

    (env, project, feature)
}

async fn open_detached_fallback_project() -> (IsolatedEnv, PathBuf, TraceDecay) {
    let (env, project) = IsolatedEnv::acquire().await;

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

    (env, project, fallback)
}

async fn assert_main_db_missing_symbol(project: &Path, symbol: &str, message: &str) {
    git(project, &["checkout", "main"]);
    let main = TraceDecay::open(project).await.unwrap();
    let results = main.search(symbol, 10).await.unwrap();
    assert!(results.is_empty(), "{message}");
}

fn assert_fallback_write_refused(operation: &str, err: impl std::fmt::Display) {
    let message = err.to_string();
    assert!(
        message.contains("fallback")
            && (message.contains("tracedecay branch add")
                || message.contains("Check out a tracked branch")),
        "unexpected {operation} error: {message}"
    );
}

#[tokio::test]
async fn open_auto_tracks_untracked_branch_and_syncs_its_db() {
    let _env_lock = HOME_ENV_LOCK.lock().await;
    let (_env, project, feature) = open_untracked_project().await;

    assert!(
        !feature
            .search("untracked_only", 10)
            .await
            .unwrap()
            .is_empty(),
        "auto-tracked branch should contain the branch-only symbol"
    );

    let meta = load_branch_meta(&project_data_dir(&project)).unwrap();
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
async fn fallback_writes_are_refused_by_all_sync_entry_points() {
    let _env_lock = HOME_ENV_LOCK.lock().await;
    let (_env, project, fallback) = open_detached_fallback_project().await;

    let err = fallback
        .sync()
        .await
        .expect_err("sync should refuse fallback writes");
    assert_fallback_write_refused("sync", err);

    let err = match fallback.index_all().await {
        Ok(_) => panic!("full index should refuse fallback writes"),
        Err(err) => err,
    };
    assert_fallback_write_refused("full index", err);

    let stale_files = ["src/detached_only.rs".to_string()];
    let err = fallback
        .sync_if_stale(&stale_files)
        .await
        .expect_err("stale sync should refuse fallback writes");
    assert_fallback_write_refused("stale sync", err);

    let err = fallback
        .sync_if_stale_silent(&stale_files)
        .await
        .expect_err("silent stale sync should refuse fallback writes");
    assert_fallback_write_refused("silent stale sync", err);

    drop(fallback);
    assert_main_db_missing_symbol(
        &project,
        "detached_only",
        "fallback write attempts must not index detached files into main DB",
    )
    .await;
}

#[tokio::test]
async fn add_branch_tracking_copies_from_nearest_tracked_ancestor() {
    let _env_lock = HOME_ENV_LOCK.lock().await;
    let (_env, project) = IsolatedEnv::acquire().await;
    let project = project.as_path();

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

    let feature_outcome = TraceDecay::add_branch_tracking(project, "feature/parent")
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

    let topic_outcome = TraceDecay::add_branch_tracking(project, "topic/child")
        .await
        .unwrap();
    assert_eq!(topic_outcome, BranchAddOutcome::Added);

    let meta = load_branch_meta(&project_data_dir(project)).unwrap();
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

#[tokio::test]
async fn add_branch_tracking_refuses_corrupt_metadata_without_overwriting() {
    let _env_lock = HOME_ENV_LOCK.lock().await;
    let (_env, project) = IsolatedEnv::acquire().await;
    let project = project.as_path();

    git(project, &["init", "-b", "main"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    commit_all(project, "initial commit");

    let main = TraceDecay::init(project).await.unwrap();
    main.index_all().await.unwrap();
    drop(main);

    let tracedecay_dir = project_data_dir(project);
    let meta_path = tracedecay_dir.join("branch-meta.json");
    fs::write(&meta_path, b"{not valid json").unwrap();

    git(project, &["checkout", "-b", "feature/corrupt-meta"]);
    fs::write(
        project.join("src/feature_only.rs"),
        "pub fn feature_only() {}\n",
    )
    .unwrap();
    commit_all(project, "feature commit");

    let err = TraceDecay::add_branch_tracking(project, "feature/corrupt-meta")
        .await
        .expect_err("corrupt metadata must stop branch tracking instead of being replaced");

    assert!(
        err.to_string().contains("corrupt branch metadata"),
        "unexpected error: {err}"
    );
    assert_eq!(
        fs::read(&meta_path).unwrap(),
        b"{not valid json",
        "failed branch add must preserve the original corrupt metadata for repair"
    );
}

#[test]
fn cli_branch_add_refuses_corrupt_metadata_without_overwriting() {
    let _env_lock = HOME_ENV_LOCK.blocking_lock();
    let (env, project) = IsolatedEnv::acquire_blocking();
    let project = project.as_path();

    git(project, &["init", "-b", "main"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    commit_all(project, "initial commit");

    let mut init_command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
    common::apply_tracedecay_home_env(&mut init_command, env.home());
    let init = init_command
        .arg("init")
        .arg(project)
        .output()
        .expect("tracedecay init");
    assert!(
        init.status.success(),
        "tracedecay init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );

    let tracedecay_dir = project_data_dir(project);
    let meta_path = tracedecay_dir.join("branch-meta.json");
    fs::write(&meta_path, b"{not valid json").unwrap();

    git(project, &["checkout", "-b", "feature/corrupt-meta"]);
    fs::write(
        project.join("src/feature_only.rs"),
        "pub fn feature_only() {}\n",
    )
    .unwrap();
    commit_all(project, "feature commit");

    let mut branch_add_command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
    common::apply_tracedecay_home_env(&mut branch_add_command, env.home());
    let output = branch_add_command
        .args(["branch", "add", "feature/corrupt-meta", "--path"])
        .arg(project)
        .output()
        .expect("tracedecay branch add");

    assert!(
        !output.status.success(),
        "corrupt metadata must fail CLI branch add\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("corrupt branch metadata"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(&meta_path).unwrap(),
        b"{not valid json",
        "failed CLI branch add must preserve corrupt metadata for repair"
    );
}
