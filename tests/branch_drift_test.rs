//! Regression tests for finding #2: a long-running server that pins the
//! branch resolved at open time must not write the new branch's files into the
//! old branch's DB after a mid-session `git checkout`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;
use tempfile::TempDir;
use tracedecay::branch_meta::{save_branch_meta, BranchMeta};
use tracedecay::tracedecay::TraceDecay;

fn git(project: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .expect("git command failed to spawn");
    assert!(
        status.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

/// Initialize a git repo on branch `main` with one committed source file.
fn init_repo_on_main(project: &std::path::Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn f() -> u32 { 1 }\n").unwrap();
    git(project, &["init"]);
    git(project, &["config", "user.email", "test@test.com"]);
    git(project, &["config", "user.name", "Test"]);
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "initial"]);
    // Guarantee the branch is named `main` regardless of git's init default.
    git(project, &["branch", "-M", "main"]);
}

#[tokio::test]
async fn sync_refuses_to_write_after_mid_session_branch_checkout() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    init_repo_on_main(project);

    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    // Track `main` so the project is in branch-aware mode (serving_branch=Some).
    let meta = BranchMeta::new("main");
    save_branch_meta(&project.join(".tracedecay"), &meta).unwrap();

    // Reopen so the instance resolves and pins `main`.
    let cg = TraceDecay::open(project).await.unwrap();
    assert!(
        !cg.branch_drifted(),
        "no drift expected while still on the branch we opened"
    );
    assert_eq!(cg.serving_branch(), Some("main"));

    // Mid-session checkout to a different branch.
    git(project, &["checkout", "-b", "feature"]);

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
}

#[tokio::test]
async fn no_drift_and_sync_allowed_while_on_opened_branch() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    init_repo_on_main(project);

    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let cg = TraceDecay::open(project).await.unwrap();

    // Still on the branch we opened: no drift, writes proceed normally.
    assert!(!cg.branch_drifted());
    fs::write(project.join("src/lib.rs"), "pub fn f() -> u32 { 2 }\n").unwrap();
    cg.sync()
        .await
        .expect("sync on the opened branch must not be blocked");
}

#[tokio::test]
async fn sync_allowed_in_single_db_mode_without_git() {
    // No git repo => no default branch detected => no branch metadata =>
    // single-DB mode (serving_branch == None), exempt from the drift guard.
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn f() -> u32 { 1 }\n").unwrap();

    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let cg = TraceDecay::open(project).await.unwrap();
    assert_eq!(cg.serving_branch(), None);
    assert!(!cg.branch_drifted());

    fs::write(project.join("src/lib.rs"), "pub fn f() -> u32 { 9 }\n").unwrap();
    cg.sync()
        .await
        .expect("single-DB mode sync must never be blocked by the drift guard");
}
