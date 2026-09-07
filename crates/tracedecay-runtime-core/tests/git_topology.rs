use std::path::Path;
use std::process::Command;

use tracedecay_runtime_core::branch::current_branch;
use tracedecay_runtime_core::worktree::{
    detached_worktree_graph_scope, git_common_dir, git_worktree_root,
};

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn linked_worktree_topology_is_resolved_in_process() {
    let fixture = tempfile::tempdir().expect("fixture root");
    let primary = fixture.path().join("primary");
    let attached = fixture.path().join("attached");
    let detached = fixture.path().join("detached");
    std::fs::create_dir_all(&primary).expect("primary checkout");
    git(&primary, &["init", "-q"]);
    git(&primary, &["config", "user.email", "test@example.com"]);
    git(&primary, &["config", "user.name", "TraceDecay Test"]);
    std::fs::write(primary.join("tracked.txt"), "base\n").expect("tracked file");
    git(&primary, &["add", "tracked.txt"]);
    git(&primary, &["commit", "-qm", "base"]);
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            attached.to_str().expect("attached path"),
        ],
    );
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            detached.to_str().expect("detached path"),
            "HEAD",
        ],
    );

    assert_eq!(current_branch(&attached).as_deref(), Some("feature"));
    assert_eq!(git_worktree_root(&attached), attached.canonicalize().ok());
    assert_eq!(git_common_dir(&attached), git_common_dir(&primary));
    assert!(current_branch(&detached).is_none());
    assert!(
        detached_worktree_graph_scope(&detached)
            .is_some_and(|scope| scope.starts_with("detached-worktree/"))
    );
}
