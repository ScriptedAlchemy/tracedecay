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

fn git_head_oid(project: &Path) -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(project)
        .output()
        .expect("git rev-parse should run");
    assert!(output.status.success(), "git rev-parse HEAD failed");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A non-default-branch hook write lands in the single project graph store:
/// the branch is tracked as metadata referencing the canonical main database,
/// its content publishes as a sealed branch-graph generation (publication
/// epoch + exact ref/OID provenance), and no per-branch database exists
/// anywhere. A subsequent write on another branch rolls the store to a newer
/// generation under the new ref while the first branch keeps its sealed
/// provenance record.
#[tokio::test]
async fn hook_branch_write_lands_in_a_sealed_single_store_generation() {
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
    let head_oid = git_head_oid(&project);

    let outcome = harness
        .track_worktree_branch(&project, &project, "feature/hook")
        .await
        .unwrap();

    assert_eq!(outcome, tracedecay::branch::BranchAddOutcome::Added);
    assert!(
        shard_root.join("tracedecay.db").exists(),
        "the single project graph store must serve the branch"
    );
    assert!(
        !shard_root.join("branches").exists(),
        "hook branch tracking must not create a per-branch database"
    );
    assert!(
        shard_root.join(".branch-add.lock").exists(),
        "branch-add lock should live under the profile shard"
    );
    assert!(
        shard_root.starts_with(harness.profile_root())
            && !project.join(".tracedecay/branches").exists(),
        "hook branch tracking must not write branch stores under repo-local marker storage"
    );

    let meta = tracedecay::branch_meta::load_branch_meta(&shard_root)
        .expect("branch metadata must be published");
    let entry = meta
        .branches
        .get("feature/hook")
        .expect("hook branch must be tracked");
    assert!(
        entry.served_by_project_store(),
        "tracked branch must reference the canonical main database, found '{}'",
        entry.db_file
    );
    assert_eq!(entry.parent.as_deref(), Some("main"));
    let source = entry
        .graph_source
        .as_ref()
        .expect("hook branch sync must seal a branch-graph generation")
        .clone();
    assert_eq!(source.reference, "refs/heads/feature/hook");
    assert_eq!(source.source_oid, head_oid);
    let canonical_project = project.canonicalize().unwrap();
    assert_eq!(
        source.worktree_root,
        canonical_project.to_string_lossy().into_owned(),
        "sealed branch provenance must retain the exact mounted worktree root"
    );
    let first_epoch = source.publication_epoch.get();
    assert!(first_epoch >= 1, "sealed generation must carry an epoch");

    let replay_outcome = harness
        .track_worktree_branch(&project, &project, "feature/hook")
        .await
        .unwrap();
    assert_eq!(
        replay_outcome,
        tracedecay::branch::BranchAddOutcome::AlreadyTracked,
        "replaying the exact branch/worktree route must preserve its sealed generation"
    );
    let replay_source = tracedecay::branch_meta::load_branch_meta(&shard_root)
        .expect("replayed branch metadata must remain published")
        .branches
        .get("feature/hook")
        .and_then(|entry| entry.graph_source.as_ref())
        .cloned()
        .expect("replayed branch must keep its sealed provenance");
    assert_eq!(
        replay_source, source,
        "the exact branch/worktree/store route must not publish a replacement generation"
    );

    // A retained generation at the same ref but an older commit is stale. The
    // branch publisher must not accept the branch name alone as idempotence:
    // it has to capture the new Git snapshot, await that exact generation, and
    // replace the stale provenance.
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn hook_marker() {}\npub fn refreshed_hook_marker() {}\n",
    )
    .unwrap();
    git(&project, &["add", "src/lib.rs"]);
    git(
        &project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay-test@example.com",
            "commit",
            "-m",
            "refresh branch source",
        ],
    );
    let refreshed_head_oid = git_head_oid(&project);
    let refreshed = harness
        .track_worktree_branch(&project, &project, "feature/hook")
        .await
        .unwrap();
    assert_eq!(
        refreshed,
        tracedecay::branch::BranchAddOutcome::Added,
        "a new branch head must not be mistaken for an idempotent replay"
    );
    let refreshed_source = tracedecay::branch_meta::load_branch_meta(&shard_root)
        .expect("refreshed branch metadata must be published")
        .branches
        .get("feature/hook")
        .and_then(|entry| entry.graph_source.as_ref())
        .cloned()
        .expect("refreshed branch must replace the stale provenance");
    assert_eq!(refreshed_source.reference, "refs/heads/feature/hook");
    assert_eq!(refreshed_source.source_oid, refreshed_head_oid);
    assert_eq!(
        refreshed_source.worktree_root,
        canonical_project.to_string_lossy().into_owned(),
        "the refreshed publication must retain the same exact worktree route"
    );
    assert!(
        refreshed_source.publication_epoch.get() > first_epoch,
        "a refreshed commit must receive a newer publication epoch"
    );

    // A write on a second branch rolls the store to a newer generation under
    // the new ref; the first branch keeps its sealed provenance record.
    git(&project, &["checkout", "-b", "feature/second"]);
    let outcome = harness
        .track_worktree_branch(&project, &project, "feature/second")
        .await
        .unwrap();
    assert_eq!(outcome, tracedecay::branch::BranchAddOutcome::Added);

    let meta = tracedecay::branch_meta::load_branch_meta(&shard_root)
        .expect("branch metadata must remain published");
    let second = meta
        .branches
        .get("feature/second")
        .and_then(|entry| entry.graph_source.as_ref())
        .expect("second branch sync must seal its own generation");
    assert_eq!(second.reference, "refs/heads/feature/second");
    assert!(
        second.publication_epoch.get() > refreshed_source.publication_epoch.get(),
        "a later branch write must land in a newer publication epoch \
         (refreshed {}, second {})",
        refreshed_source.publication_epoch.get(),
        second.publication_epoch.get()
    );
    let first = meta
        .branches
        .get("feature/hook")
        .and_then(|entry| entry.graph_source.as_ref())
        .expect("first branch must keep its sealed provenance");
    assert_eq!(
        first.publication_epoch.get(),
        refreshed_source.publication_epoch.get()
    );
    assert!(
        !shard_root.join("branches").exists(),
        "no write may create a per-branch database"
    );
}
