use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use tracedecay::branch_meta::{self, BranchMeta};
use tracedecay::config::{TraceDecayConfig, USER_DATA_DIR_ENV};
use tracedecay::db::Database;
use tracedecay::hooks::{
    cursor_branch_switch_target, cursor_shell_command_targets_project, cursor_shell_sync_plan,
    cursor_shell_sync_plan_with_current_branch, CursorShellSyncPlan,
};
use tracedecay::storage::{write_enrollment_marker, EnrollmentMarker, StorageMode};
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
        std::env::set_var("HOME", home);
        std::env::set_var("USERPROFILE", home);
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
    let output = Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .expect("git command should run");
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn git_global_c_option_still_extracts_branch_switch_target() {
    assert_eq!(
        cursor_branch_switch_target("git -C repo switch feature/foo"),
        Some("feature/foo".to_string())
    );
    assert_eq!(
        cursor_shell_sync_plan("git -C repo switch feature/foo"),
        CursorShellSyncPlan::BranchAdd("feature/foo".to_string())
    );
}

#[test]
fn quoted_branch_names_are_preserved_when_extracting_switch_target() {
    assert_eq!(
        cursor_branch_switch_target("git switch 'feature/quoted name'"),
        Some("feature/quoted name".to_string())
    );
    assert_eq!(
        cursor_branch_switch_target("git checkout -b \"feature/double quoted\""),
        Some("feature/double quoted".to_string())
    );
}

#[test]
fn checkout_path_restore_is_not_treated_as_branch_switch() {
    assert_eq!(
        cursor_branch_switch_target("git checkout -- src/lib.rs"),
        None
    );
    assert_eq!(cursor_branch_switch_target("git checkout ."), None);
    assert_eq!(
        cursor_shell_sync_plan_with_current_branch(
            "git checkout -- src/lib.rs",
            Some("feature/current")
        ),
        CursorShellSyncPlan::CurrentBranchSync("feature/current".to_string())
    );
}

#[test]
fn git_dash_c_outside_project_is_not_routed_to_current_repo() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let other = dir.path().join("other");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&other).unwrap();

    assert!(cursor_shell_command_targets_project(
        "git -C . switch feature/current",
        &project,
        &project
    ));
    assert!(
        !cursor_shell_command_targets_project(
            &format!("git -C {} switch feature/other", other.display()),
            &project,
            &project,
        ),
        "git -C pointing at another work tree must not sync the current workspace"
    );
}

#[test]
fn ambiguous_state_changes_fall_back_to_current_branch_when_available() {
    assert_eq!(
        cursor_shell_sync_plan_with_current_branch("git pull --rebase", Some("feature/current")),
        CursorShellSyncPlan::CurrentBranchSync("feature/current".to_string())
    );
    assert_eq!(
        cursor_shell_sync_plan_with_current_branch(
            "git merge origin/main",
            Some("feature/current")
        ),
        CursorShellSyncPlan::CurrentBranchSync("feature/current".to_string())
    );
    assert_eq!(
        cursor_shell_sync_plan_with_current_branch("git pull --rebase", None),
        CursorShellSyncPlan::IncrementalSync
    );
}

#[tokio::test]
async fn hook_branch_tracking_writes_profile_sharded_branch_db() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let temp_root = canonical_temp_path(dir.path());
    let home = temp_root.join("home");
    let profile_root = home.join(".tracedecay");
    let project = temp_root.join("project");
    let shard_root = profile_root.join("projects/proj_hook");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn hook_marker() {}\n").unwrap();
    let _home_guard = HomeEnvGuard::set(&home);
    write_enrollment_marker(
        &project,
        &EnrollmentMarker {
            project_id: "proj_hook".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let config = TraceDecayConfig {
        root_dir: project.to_string_lossy().to_string(),
        ..TraceDecayConfig::default()
    };
    std::fs::create_dir_all(&shard_root).unwrap();
    std::fs::write(
        shard_root.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
    Database::initialize(&shard_root.join("tracedecay.db"))
        .await
        .unwrap();
    let meta = BranchMeta::new_for_dir(&shard_root, "main");
    branch_meta::save_branch_meta(&shard_root, &meta).unwrap();
    git(&project, &["init", "-b", "main"]);
    git(&project, &["checkout", "-b", "feature/hook"]);

    let outcome = TraceDecay::add_branch_tracking(&project, "feature/hook")
        .await
        .unwrap();

    assert_eq!(outcome, tracedecay::branch::BranchAddOutcome::Added);
    assert!(
        shard_root.join("branches/feature_hook.db").exists(),
        "hook branch tracking must copy the branch DB into the profile shard"
    );
    assert!(
        shard_root.join(".branch-add.lock").exists(),
        "branch-add lock should live under the profile shard"
    );
    assert!(
        !project
            .join(".tracedecay/branches/feature_hook.db")
            .exists(),
        "hook branch tracking must not write branch DBs under repo-local marker storage"
    );
}
