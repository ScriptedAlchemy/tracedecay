//! Regression tests for finding #2: a long-running server that pins the
//! branch resolved at open time must not write the new branch's files into the
//! old branch's DB after a mid-session `git checkout`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use tracedecay::branch_meta::{save_branch_meta, BranchMeta};
use tracedecay::config::USER_DATA_DIR_ENV;
use tracedecay::storage::resolve_layout_for_current_profile;
use tracedecay::tracedecay::TraceDecay;

static HOME_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct HomeEnvGuard {
    previous_home: Option<OsString>,
    previous_userprofile: Option<OsString>,
    previous_data_dir: Option<OsString>,
}

impl HomeEnvGuard {
    fn set(home: &Path) -> Self {
        let previous_home = std::env::var_os("HOME");
        let previous_userprofile = std::env::var_os("USERPROFILE");
        let previous_data_dir = std::env::var_os(USER_DATA_DIR_ENV);
        let home = canonical_temp_path(home);
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        std::env::set_var(USER_DATA_DIR_ENV, home.join(".tracedecay"));
        Self {
            previous_home,
            previous_userprofile,
            previous_data_dir,
        }
    }
}

impl Drop for HomeEnvGuard {
    fn drop(&mut self) {
        match self.previous_home.take() {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match self.previous_userprofile.take() {
            Some(value) => std::env::set_var("USERPROFILE", value),
            None => std::env::remove_var("USERPROFILE"),
        }
        match self.previous_data_dir.take() {
            Some(value) => std::env::set_var(USER_DATA_DIR_ENV, value),
            None => std::env::remove_var(USER_DATA_DIR_ENV),
        }
    }
}

fn canonical_temp_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        path.to_path_buf()
    }
    #[cfg(not(windows))]
    {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }
}

fn git(project: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "core.hooksPath=.git/no-hooks"])
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
fn init_repo_on_main(project: &Path) {
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

fn project_data_dir(project: &Path) -> PathBuf {
    resolve_layout_for_current_profile(project)
        .unwrap_or_else(|err| panic!("failed to resolve test project storage layout: {err}"))
        .data_root
}

async fn close_graph(cg: TraceDecay) {
    cg.checkpoint().await.unwrap();
    cg.close();
}

#[tokio::test]
async fn sync_refuses_to_write_after_mid_session_branch_checkout() {
    let _env_lock = HOME_ENV_LOCK.lock().await;
    let home = TempDir::new().unwrap();
    let _home_env = HomeEnvGuard::set(home.path());
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    init_repo_on_main(project);

    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    // Track `main` so the project is in branch-aware mode (serving_branch=Some).
    let meta = BranchMeta::new("main");
    save_branch_meta(&project_data_dir(project), &meta).unwrap();
    close_graph(cg).await;

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
    close_graph(reopened).await;
    close_graph(cg).await;
}

#[tokio::test]
async fn no_drift_and_sync_allowed_while_on_opened_branch() {
    let _env_lock = HOME_ENV_LOCK.lock().await;
    let home = TempDir::new().unwrap();
    let _home_env = HomeEnvGuard::set(home.path());
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    init_repo_on_main(project);

    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    close_graph(cg).await;

    let cg = TraceDecay::open(project).await.unwrap();

    // Still on the branch we opened: no drift, writes proceed normally.
    assert!(!cg.branch_drifted());
    fs::write(project.join("src/lib.rs"), "pub fn f() -> u32 { 2 }\n").unwrap();
    cg.sync()
        .await
        .expect("sync on the opened branch must not be blocked");
    close_graph(cg).await;
}

#[tokio::test]
async fn sync_allowed_in_single_db_mode_without_git() {
    let _env_lock = HOME_ENV_LOCK.lock().await;
    let home = TempDir::new().unwrap();
    let _home_env = HomeEnvGuard::set(home.path());
    // No git repo => no default branch detected => no branch metadata =>
    // single-DB mode (serving_branch == None), exempt from the drift guard.
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn f() -> u32 { 1 }\n").unwrap();

    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    close_graph(cg).await;

    let cg = TraceDecay::open(project).await.unwrap();
    assert_eq!(cg.serving_branch(), None);
    assert!(!cg.branch_drifted());

    fs::write(project.join("src/lib.rs"), "pub fn f() -> u32 { 9 }\n").unwrap();
    cg.sync()
        .await
        .expect("single-DB mode sync must never be blocked by the drift guard");
    close_graph(cg).await;
}

#[tokio::test]
async fn branch_diagnostics_reports_stale_open_and_serving_state_after_checkout() {
    let _env_lock = HOME_ENV_LOCK.lock().await;
    let home = TempDir::new().unwrap();
    let _home_env = HomeEnvGuard::set(home.path());
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    init_repo_on_main(project);

    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let meta = BranchMeta::new("main");
    save_branch_meta(&project_data_dir(project), &meta).unwrap();
    close_graph(cg).await;

    let cg = TraceDecay::open(project).await.unwrap();
    git(project, &["checkout", "-b", "feature"]);

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
    let _env_lock = HOME_ENV_LOCK.lock().await;
    let home = TempDir::new().unwrap();
    let _home_env = HomeEnvGuard::set(home.path());
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    init_repo_on_main(project);

    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let meta = BranchMeta::new("main");
    save_branch_meta(&project_data_dir(project), &meta).unwrap();
    close_graph(cg).await;

    git(project, &["checkout", "-b", "feature/untracked"]);

    let cg = TraceDecay::open(project).await.unwrap();
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
    let _env_lock = HOME_ENV_LOCK.lock().await;
    let home = TempDir::new().unwrap();
    let _home_env = HomeEnvGuard::set(home.path());
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    init_repo_on_main(project);

    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    let meta = BranchMeta::new("main");
    save_branch_meta(&project_data_dir(project), &meta).unwrap();
    close_graph(cg).await;

    git(project, &["checkout", "-b", "feature/tracked"]);
    fs::write(project.join("src/lib.rs"), "pub fn f() -> u32 { 2 }\n").unwrap();
    fs::write(
        project.join("src/tracked_only.rs"),
        "pub fn tracked_only() {}\n",
    )
    .unwrap();
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "feature"]);

    TraceDecay::add_branch_tracking(project, "feature/tracked")
        .await
        .unwrap();

    let tracedecay_dir = project_data_dir(project);
    let meta = tracedecay::branch_meta::load_branch_meta(&tracedecay_dir).unwrap();
    let feature_db =
        tracedecay::branch::resolve_branch_db_path(&tracedecay_dir, "feature/tracked", &meta)
            .unwrap();
    fs::remove_file(&feature_db).unwrap();

    let cg = TraceDecay::open(project).await.unwrap();
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
