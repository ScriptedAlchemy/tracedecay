use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;

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

#[tokio::test]
async fn hook_branch_tracking_writes_profile_sharded_branch_db() {
    let dir = TempDir::new().unwrap();
    let temp_root = canonical_temp_path(dir.path());
    let project = temp_root.join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn hook_marker() {}\n").unwrap();
    git(&project, &["init", "-b", "main"]);
    git(&project, &["add", "."]);
    git(
        &project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay-test@example.com",
            "commit",
            "-m",
            "initial commit",
        ],
    );
    let harness = ProductionProjectCompositionHarnessV1::open(&temp_root, [project.clone()])
        .await
        .unwrap();
    let shard_root = harness.project_data_root(&project).await.unwrap();
    git(&project, &["checkout", "-b", "feature/hook"]);

    let outcome = harness
        .track_worktree_branch(&project, &project, "feature/hook")
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
        shard_root.starts_with(harness.profile_root())
            && !project
                .join(".tracedecay/branches/feature_hook.db")
                .exists(),
        "hook branch tracking must not write branch DBs under repo-local marker storage"
    );
}
